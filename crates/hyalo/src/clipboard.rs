//! The clipboard: what Enter on an answer, Ctrl+C and Ctrl+X put on it,
//! what Ctrl+V takes from it, and the clipboard history: what is read
//! off it as it changes, and an entry put back on it, all its formats.
//! Which formats are read, and what they mean, is `hyalo_core::clipboard`.
//!
//! Tests that touch it run in a window station of their own
//! (`test_desktop::in_own_window_station`): the user's clipboard is never
//! overwritten, nor read.

use std::sync::Arc;
use std::time::Duration;

use hyalo_core::clipboard::picture::{self, Pixels};
use hyalo_core::clipboard::{self as history, Content, MOST_BYTES, Marks, Offered, Wanted};
use hyalo_core::clipboard::{dropped, html};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows::core::{Error, HSTRING, Result};

use crate::apps::{self, Icon};
use crate::com::Com;

/// The clipboard formats Windows numbers: a bitmap with its header,
/// files dropped, UTF-16 text. The windows crate keeps their names in
/// the large Ole module.
const DIB: u32 = 8;
const HDROP: u32 = 15;
const UNICODE_TEXT: u32 = 13;

/// The formats apps name, registered by those names.
const HTML: &str = "HTML Format";
const RTF: &str = "Rich Text Format";
const PNG: &str = "PNG";
const DROP_EFFECT: &str = "Preferred DropEffect";
const EXCLUDED: &str = "ExcludeClipboardContentFromMonitorProcessing";
const IGNORED: &str = "Clipboard Viewer Ignore";
const HISTORY: &str = "CanIncludeInClipboardHistory";

/// `DROPEFFECT_COPY`: files put back paste as copies, even if they were
/// cut.
const COPY: u32 = 1;

/// The most bytes of a bitmap read: an 8K picture, 7680 by 4320 pixels
/// of 32 bits, with its header. Kept as PNG it must then fit in
/// `MOST_BYTES`.
const MOST_DIB: usize = 7680 * 4320 * 4 + 1024;

/// The number of the format apps call `name`.
fn registered(name: &str) -> u32 {
    // SAFETY: a plain call; a name Windows cannot register gives 0,
    // which no clipboard holds.
    unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) }
}

/// Whether the clipboard holds `format`.
fn available(format: u32) -> bool {
    // SAFETY: a plain call.
    unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

/// Puts `text` on the clipboard in place of what was there.
pub fn copy(text: &str) -> Result<()> {
    put_formats(vec![(UNICODE_TEXT, utf16(text))])
}

/// Puts `content` back on the clipboard in place of what was there, in
/// each format it was kept in: text with its HTML and RTF; a picture as
/// a bitmap and as PNG; files as Explorer copies them. COM must be
/// initialized on this thread, for a picture.
pub fn put(content: &Content) -> Result<()> {
    let formats = match content {
        Content::Text { text, html, rtf } => {
            let beside = [(HTML, html), (RTF, rtf)].into_iter();
            let beside =
                beside.filter_map(|(name, bytes)| Some((registered(name), bytes.clone()?)));
            [(UNICODE_TEXT, utf16(text))]
                .into_iter()
                .chain(beside)
                .collect()
        }
        Content::Image { png, .. } => {
            let bitmap = picture::write(&crate::picture::decode(png)?);
            vec![(DIB, bitmap), (registered(PNG), png.clone())]
        }
        Content::Files(paths) => vec![
            (HDROP, dropped::dropped(paths)),
            (registered(DROP_EFFECT), COPY.to_le_bytes().to_vec()),
        ],
    };
    put_formats(formats)
}

/// `text` as NUL-terminated UTF-16 bytes, the form the clipboard takes
/// it in. Windows also writes a NUL over the last unit when the text is
/// set, but the clipboard's contract is to hand it NUL-terminated text.
fn utf16(text: &str) -> Vec<u8> {
    let units = text.encode_utf16().chain([0]);
    units.flat_map(u16::to_le_bytes).collect()
}

/// Empties the clipboard and puts each of `formats` on it, its number
/// and its bytes.
fn put_formats(formats: Vec<(u32, Vec<u8>)>) -> Result<()> {
    let _open = Clipboard::open()?;
    // SAFETY: the clipboard is open on this thread.
    unsafe { EmptyClipboard()? };
    for (format, bytes) in formats {
        let memory = global(&bytes)?;
        // SAFETY: `memory` holds the format's bytes. Once set, it
        // belongs to the clipboard; if setting fails, it is still ours
        // to free.
        unsafe {
            SetClipboardData(format, Some(HANDLE(memory.0))).inspect_err(|_| {
                let _ = GlobalFree(Some(memory));
            })?;
        }
    }
    Ok(())
}

/// `bytes` in movable global memory, the form the clipboard takes them
/// in.
fn global(bytes: &[u8]) -> Result<HGLOBAL> {
    // SAFETY: the memory holds `bytes.len()` bytes (one at least, as an
    // empty block cannot be locked) and stays locked while they are
    // copied in.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let target = GlobalLock(memory).cast::<u8>();
        if target.is_null() {
            let error = Error::from_thread();
            let _ = GlobalFree(Some(memory));
            return Err(error);
        }
        target.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        // Releasing the last lock reports FALSE with no error: not a failure.
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }
}

/// The clipboard, open on this thread until dropped.
struct Clipboard;

/// How often opening the clipboard is tried again, and how far apart:
/// another app that watches it may be reading it at the very moment.
const AGAIN: usize = 9;
const BETWEEN: Duration = Duration::from_millis(10);

impl Clipboard {
    /// Opens it with no owner window: what is put on it is all there at
    /// once, so no one needs to be asked to render it later. Held by
    /// another, it is tried again a few times before giving up.
    fn open() -> Result<Self> {
        for wait in std::iter::repeat_n(BETWEEN, AGAIN) {
            if Self::try_open().is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(wait);
        }
        Self::try_open().map(|()| Self)
    }

    fn try_open() -> Result<()> {
        // SAFETY: a plain call; Drop closes what it opened.
        unsafe { OpenClipboard(None) }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

/// The text on the clipboard; `None` when it holds no text or cannot be
/// opened (another app holding it, say).
pub fn text() -> Option<String> {
    if !available(UNICODE_TEXT) {
        return None;
    }
    let _open = Clipboard::open().ok()?;
    open_text(usize::MAX)
}

/// The text on the open clipboard, to its first NUL; `None` when it
/// holds more than `most` bytes of UTF-16.
fn open_text(most: usize) -> Option<String> {
    let bytes = open_bytes(UNICODE_TEXT, most)?;
    let (pairs, _) = bytes.as_chunks::<2>();
    let units = pairs.iter().map(|&pair| u16::from_le_bytes(pair));
    let text = char::decode_utf16(units.take_while(|&unit| unit != 0))
        .map(|letter| letter.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    Some(text)
}

/// The bytes of `format` on the open clipboard; `None` when it holds
/// none, or more than `most`.
fn open_bytes(format: u32, most: usize) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open on this thread; the handle it gives
    // stays the clipboard's, and is only read while locked, no further
    // than its size.
    unsafe {
        let memory = HGLOBAL(GetClipboardData(format).ok()?.0);
        let size = GlobalSize(memory);
        if size > most {
            return None;
        }
        let bytes = GlobalLock(memory).cast::<u8>().cast_const();
        if bytes.is_null() {
            return None;
        }
        let copied = std::slice::from_raw_parts(bytes, size).to_vec();
        let _ = GlobalUnlock(memory);
        Some(copied)
    }
}

/// What the clipboard history reads: Windows' clipboard (`Watch::real`),
/// or a stand-in in tests, which hands over what the test copied.
#[derive(Clone)]
pub struct Watch {
    /// Asks Windows to tell the window of each change to the clipboard.
    pub listen: fn(HWND) -> Result<()>,
    pub read: Reader,
}

/// Reads what is on the clipboard now, its row's picture `side` pixels
/// square; `None` for what the history does not keep.
pub type Reader = Arc<dyn Fn(u32) -> Option<Read> + Send + Sync>;

/// What a reader read: what the history keeps of it, the app it came
/// from (empty when not known) and its row's picture.
pub type Read = (Content, String, Option<Icon>);

impl Watch {
    /// Windows' clipboard, the app in front taken for the one it came
    /// from.
    pub fn real() -> Self {
        Watch {
            listen,
            read: Arc::new(|side| {
                let _com = Com::init().ok()?;
                let (content, icon) = read(side)?;
                let app = crate::switcher::foreground_app().unwrap_or_default();
                Some((content, app, icon))
            }),
        }
    }
}

/// Asks Windows to tell `window` of each change to the clipboard, with
/// `WM_CLIPBOARDUPDATE`.
fn listen(window: HWND) -> Result<()> {
    // SAFETY: a plain call; the listening ends with the window.
    unsafe { AddClipboardFormatListener(window) }
}

/// What is on the clipboard now, as the history keeps it, and the
/// picture its row shows, `side` pixels square: a picture's thumbnail,
/// the first file's icon. `None` when an app marked it private, or it
/// holds nothing hyalo keeps or can read. COM must be initialized on
/// this thread.
pub fn read(side: u32) -> Option<(Content, Option<Icon>)> {
    let taken = take()?;
    shown(taken, side)
}

/// What was taken off the clipboard, before it is made an entry.
enum Taken {
    Files(Vec<u8>),
    Text {
        text: String,
        html: Option<Vec<u8>>,
        rtf: Option<Vec<u8>>,
    },
    Image(Vec<u8>),
    Html(Vec<u8>),
}

/// Takes what the history wants off the clipboard, holding it open no
/// longer than that.
fn take() -> Option<Taken> {
    let _open = Clipboard::open().ok()?;
    let marks = Marks {
        excluded: available(registered(EXCLUDED)),
        ignored: available(registered(IGNORED)),
        history: mark(registered(HISTORY)),
    };
    let offered = Offered {
        files: available(HDROP),
        text: available(UNICODE_TEXT),
        image: available(DIB),
        html: available(registered(HTML)),
    };
    take_open(history::wanted(marks, offered)?)
}

/// Takes the `wanted` kind off the open clipboard.
fn take_open(wanted: Wanted) -> Option<Taken> {
    let html = registered(HTML);
    Some(match wanted {
        Wanted::Files => Taken::Files(open_bytes(HDROP, MOST_BYTES)?),
        Wanted::Text => Taken::Text {
            text: open_text(MOST_BYTES * 2)?,
            html: open_bytes(html, MOST_BYTES),
            rtf: open_bytes(registered(RTF), MOST_BYTES),
        },
        Wanted::Image => Taken::Image(open_bytes(DIB, MOST_DIB)?),
        Wanted::Html => Taken::Html(open_bytes(html, MOST_BYTES)?),
    })
}

/// The value of the mark `format`, a DWORD, when the open clipboard
/// holds it; one that does not read counts as 0, keeping it out.
fn mark(format: u32) -> Option<u32> {
    if !available(format) {
        return None;
    }
    let bytes = open_bytes(format, 64).unwrap_or_default();
    let value = bytes
        .first_chunk::<4>()
        .map(|&four| u32::from_le_bytes(four));
    Some(value.unwrap_or(0))
}

/// What was taken as the history keeps it, and its row's picture.
fn shown(taken: Taken, side: u32) -> Option<(Content, Option<Icon>)> {
    match taken {
        Taken::Files(bytes) => {
            let paths = dropped::files(&bytes)?;
            let icon = paths
                .first()
                .and_then(|first| apps::icon_at(first, side).ok());
            Some((Content::Files(paths), icon))
        }
        Taken::Text { text, html, rtf } => Some((Content::Text { text, html, rtf }, None)),
        Taken::Image(bitmap) => {
            let pixels = picture::read(&bitmap)?;
            let png = crate::picture::encode(&pixels).ok()?;
            let content = Content::Image {
                width: pixels.width,
                height: pixels.height,
                png,
            };
            Some((content, Some(icon(picture::thumbnail(&pixels, side)))))
        }
        Taken::Html(bytes) => {
            let text = html::text(&bytes);
            let content = Content::Text {
                text,
                html: Some(bytes),
                rtf: None,
            };
            Some((content, None))
        }
    }
}

/// Pixels, premultiplied, as a row's icon.
fn icon(pixels: Pixels) -> Icon {
    Icon {
        width: pixels.width,
        height: pixels.height,
        bgra: pixels.bgra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_desktop::in_own_window_station;

    #[test]
    fn text_copied_is_the_text_read_back_and_an_empty_clipboard_has_none() {
        let name =
            "clipboard::tests::text_copied_is_the_text_read_back_and_an_empty_clipboard_has_none";
        in_own_window_station(name, || {
            copy("pad\nplus 🔍").expect("copy");
            assert_eq!(text().as_deref(), Some("pad\nplus 🔍"));
            copy("").expect("copy nothing");
            assert_eq!(text().as_deref(), Some(""));
            {
                let _open = Clipboard::open().expect("open");
                // SAFETY: the clipboard is open on this thread.
                unsafe { EmptyClipboard() }.expect("empty");
            }
            assert_eq!(text(), None);
        });
    }

    /// A picture of `width` by `height`, each pixel different.
    fn picture_of(width: u32, height: u32) -> Pixels {
        let mut bgra = Vec::new();
        for y in 0..height {
            for x in 0..width {
                bgra.extend([x as u8, y as u8, (x + y) as u8, 255]);
            }
        }
        Pixels {
            width,
            height,
            bgra,
        }
    }

    #[test]
    fn what_is_put_back_reads_again_as_it_was_with_the_picture_its_row_shows() {
        let name = "clipboard::tests::what_is_put_back_reads_again_as_it_was_with_the_picture_its_row_shows";
        in_own_window_station(name, || {
            let _com = Com::init().expect("COM");
            let styled = Content::Text {
                text: "Dear sir".to_string(),
                html: Some(b"Version:0.9\r\n<b>Dear</b> sir\0".to_vec()),
                rtf: Some(b"{\\rtf1 Dear sir}".to_vec()),
            };
            put(&styled).expect("put the text");
            assert_eq!(read(32), Some((styled, None)));
            assert_eq!(
                text().as_deref(),
                Some("Dear sir"),
                "any app reads the text"
            );
            let png = crate::picture::encode(&picture_of(64, 32)).expect("encode");
            let image = Content::Image {
                width: 64,
                height: 32,
                png,
            };
            put(&image).expect("put the picture");
            let (read_back, thumbnail) = read(32).expect("a picture");
            assert_eq!(read_back, image);
            let thumbnail = thumbnail.expect("a thumbnail");
            assert_eq!((thumbnail.width, thumbnail.height), (32, 32));
            let row = |y: usize| &thumbnail.bgra[y * 32 * 4..(y + 1) * 32 * 4];
            assert!(row(0).iter().all(|&byte| byte == 0), "clear above");
            assert!(
                row(8).chunks(4).all(|pixel| pixel[3] == 255),
                "16 high, in the middle"
            );
            let exe = std::env::current_exe().expect("the test binary");
            let folder = exe
                .parent()
                .expect("its folder")
                .to_string_lossy()
                .to_string();
            let files = Content::Files(vec![exe.to_string_lossy().to_string(), folder]);
            put(&files).expect("put the files");
            let (read_back, icon) = read(32).expect("files");
            assert_eq!(read_back, files);
            assert_eq!(icon.map(|icon| icon.width), Some(32), "the shell's icon");
        });
    }

    #[test]
    fn html_alone_is_kept_with_its_text_and_what_an_app_marks_private_is_not_read() {
        let name = "clipboard::tests::html_alone_is_kept_with_its_text_and_what_an_app_marks_private_is_not_read";
        in_own_window_station(name, || {
            let _com = Com::init().expect("COM");
            let page = b"Version:0.9\r\n<p>only</p><p>html</p>".to_vec();
            put_formats(vec![(registered(HTML), page.clone())]).expect("put HTML");
            let kept = Content::Text {
                text: "only\nhtml".to_string(),
                html: Some(page),
                rtf: None,
            };
            assert_eq!(read(32), Some((kept, None)));
            for (mark, value) in [(EXCLUDED, 1_u32), (IGNORED, 1), (HISTORY, 0)] {
                let secret = vec![
                    (UNICODE_TEXT, utf16("hunter2")),
                    (registered(mark), value.to_le_bytes().to_vec()),
                ];
                put_formats(secret).expect("put a secret");
                assert_eq!(read(32), None, "{mark}");
            }
            let allowed = vec![
                (UNICODE_TEXT, utf16("fine")),
                (registered(HISTORY), 1_u32.to_le_bytes().to_vec()),
            ];
            put_formats(allowed).expect("put text history may keep");
            assert!(read(32).is_some());
            put_formats(vec![(registered("hyalo test"), vec![1, 2, 3])])
                .expect("put a format of its own");
            assert_eq!(read(32), None, "nothing hyalo could put back");
            let unreadable = vec![
                (UNICODE_TEXT, utf16("hunter2")),
                (registered(HISTORY), vec![1, 0]),
            ];
            put_formats(unreadable).expect("put a mark too short to read");
            assert_eq!(read(32), None, "a mark that does not read keeps it out");
        });
    }

    #[test]
    fn text_goes_on_the_clipboard_ending_in_a_nul() {
        assert_eq!(utf16("ab"), [b'a', 0, b'b', 0, 0, 0]);
        assert_eq!(utf16(""), [0, 0]);
    }

    #[test]
    fn a_long_text_is_read_whole_up_to_what_an_entry_holds_and_a_format_up_to_its_limit() {
        let name = "clipboard::tests::a_long_text_is_read_whole_up_to_what_an_entry_holds_and_a_format_up_to_its_limit";
        in_own_window_station(name, || {
            let _com = Com::init().expect("COM");
            let long = "a".repeat(MOST_BYTES * 3 / 4);
            copy(&long).expect("copy");
            let read_back = read(32).map(|(content, _)| content);
            let whole = Content::Text {
                text: long,
                html: None,
                rtf: None,
            };
            assert!(read_back == Some(whole), "3 MB of text, 6 MB as UTF-16");
            let empty = Content::Text {
                text: "a".to_string(),
                html: Some(Vec::new()),
                rtf: None,
            };
            put(&empty).expect("an empty format goes on too");
            let format = registered("hyalo test");
            put_formats(vec![(format, vec![7; 13])]).expect("put 13 bytes");
            let _open = Clipboard::open().expect("open");
            let size = open_bytes(format, usize::MAX).expect("the bytes").len();
            assert!(size >= 13, "{size}");
            assert!(open_bytes(format, size).is_some(), "as many as it may");
            assert!(
                open_bytes(format, size - 1).is_none(),
                "one more than it may"
            );
        });
    }
}
