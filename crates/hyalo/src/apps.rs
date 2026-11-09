//! Apps: everything Start lists, desktop and Store apps alike, read from
//! the shell's Applications folder (`shell:AppsFolder`).
//!
//! Each entry has a display name and a parsing name: an AppUserModelID
//! for Store apps and pinned desktop apps, a full path or a URL for some
//! shortcuts. `shell:AppsFolder\<parsing name>` opens any of them.
//!
//! COM must be initialized on the calling thread (see `com::Com`). The
//! `windows` crate marks COM and GDI calls `unsafe`; each block below
//! states the invariant it relies on.

use hyalo_core::item::{Action, Item, ItemId, Kind};
use windows::Win32::Foundation::{E_INVALIDARG, SIZE};
use windows::Win32::Graphics::Gdi::{DIBSECTION, DeleteObject, GetObjectW, HBITMAP};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, IShellItemImageFactory,
    KF_FLAG_DEFAULT, SHGetKnownFolderItem, SIGDN, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEPARSING,
    SIIGBF_ICONONLY,
};
use windows::core::{Interface, Result};

/// Prefixed to a parsing name, opens that entry of the Applications folder.
const APPS_FOLDER: &str = "shell:AppsFolder\\";

/// The item for one entry of the Applications folder; `None` for an
/// entry without a name or without a parsing name.
pub fn item(name: &str, parsing_name: &str) -> Option<Item> {
    if name.trim().is_empty() || parsing_name.is_empty() {
        return None;
    }
    Some(Item {
        id: ItemId(parsing_name.to_string()),
        title: name.to_string(),
        subtitle: subtitle(parsing_name),
        kind: Kind::App,
        action: Action::Open(format!("{APPS_FOLDER}{parsing_name}")),
    })
}

/// A path or an address tells where the app lives. An AppUserModelID
/// tells a person nothing, so the row just says it is an app.
fn subtitle(parsing_name: &str) -> String {
    let location = parsing_name.contains(":\\") || parsing_name.contains("://");
    match location {
        true => parsing_name.to_string(),
        false => "App".to_string(),
    }
}

/// One entry of the Applications folder: the item it lists, and the shell
/// item its icon comes from. Lives on the thread that found it.
pub struct Entry {
    pub item: Item,
    shell: IShellItem,
}

/// Every entry of the Applications folder that `item` keeps, in the
/// shell's order.
pub fn find() -> Result<Vec<Entry>> {
    // SAFETY: COM is initialized on this thread (see the module docs) and
    // every interface here is a live object owned by this function.
    unsafe {
        let folder: IShellItem = SHGetKnownFolderItem(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)?;
        let listing: IEnumShellItems = folder.BindToHandler(None, &BHID_EnumItems)?;
        let mut entries = Vec::new();
        let mut next = [None];
        loop {
            listing.Next(&mut next, None)?;
            let Some(shell) = next[0].take() else {
                return Ok(entries);
            };
            let name = display_name(&shell, SIGDN_NORMALDISPLAY)?;
            let parsing_name = display_name(&shell, SIGDN_PARENTRELATIVEPARSING)?;
            if let Some(item) = item(&name, &parsing_name) {
                entries.push(Entry { item, shell });
            }
        }
    }
}

/// One of an entry's names, copied out of the buffer the shell allocated.
fn display_name(entry: &IShellItem, kind: SIGDN) -> Result<String> {
    // SAFETY: GetDisplayName returns a NUL-terminated string allocated with
    // CoTaskMemAlloc; it is read once, then freed once.
    unsafe {
        let name = entry.GetDisplayName(kind)?;
        let text = String::from_utf16_lossy(name.as_wide());
        CoTaskMemFree(Some(name.0.cast()));
        Ok(text)
    }
}

/// An icon's pixels: premultiplied BGRA, top row first, rows unpadded.
#[derive(Debug, Clone, PartialEq)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

impl Entry {
    /// The entry's icon, `size` pixels square. Taking it from the shell
    /// item found by `find` is several times faster than parsing the
    /// name again (0.5 s against 2 to 7 s for 239 apps).
    pub fn icon(&self, size: u32) -> Result<Icon> {
        let side = size as i32;
        // SAFETY: COM is initialized on this thread; the bitmap GetImage
        // hands over is ours, read once and deleted once.
        unsafe {
            let factory: IShellItemImageFactory = self.shell.cast()?;
            let bitmap = factory.GetImage(SIZE { cx: side, cy: side }, SIIGBF_ICONONLY)?;
            let icon = pixels(bitmap);
            let _ = DeleteObject(bitmap.into());
            let mut icon = icon?;
            premultiply(&mut icon.bgra);
            Ok(icon)
        }
    }
}

/// The shell draws icons with straight alpha; Direct2D blends
/// premultiplied colours. Scales each colour by its pixel's alpha,
/// rounding to the nearest step.
fn premultiply(bgra: &mut [u8]) {
    let (pixels, _) = bgra.as_chunks_mut::<4>();
    for pixel in pixels {
        let alpha = u16::from(pixel[3]);
        for colour in &mut pixel[..3] {
            *colour = ((u16::from(*colour) * alpha + 127) / 255) as u8;
        }
    }
}

/// The pixels of a 32-bit DIB section, the kind of bitmap the shell
/// draws icons into.
fn pixels(bitmap: HBITMAP) -> Result<Icon> {
    let mut section = DIBSECTION::default();
    let size = size_of::<DIBSECTION>() as i32;
    // SAFETY: `section` is a live DIBSECTION and `size` is its size.
    let filled = unsafe { GetObjectW(bitmap.into(), size, Some((&raw mut section).cast())) };
    if filled != size {
        return Err(E_INVALIDARG.into());
    }
    let shape = section.dsBm;
    if shape.bmBitsPixel != 32 {
        return Err(E_INVALIDARG.into());
    }
    let (width, height, stride) = (shape.bmWidth, shape.bmHeight, shape.bmWidthBytes);
    // SAFETY: the bits of a DIB section are `height` rows of `stride`
    // bytes, alive as long as `bitmap`, which the caller still holds.
    let rows =
        unsafe { std::slice::from_raw_parts(shape.bmBits.cast(), (stride * height) as usize) };
    let bottom_up = section.dsBmih.biHeight.is_positive();
    Ok(Icon {
        width: width as u32,
        height: height as u32,
        bgra: top_down(rows, width as usize, stride as usize, bottom_up),
    })
}

/// Unpadded top-down rows of `width` BGRA pixels, from GDI rows of
/// `stride` bytes that run bottom-up when `bottom_up`.
fn top_down(rows: &[u8], width: usize, stride: usize, bottom_up: bool) -> Vec<u8> {
    let rows = rows.chunks_exact(stride).map(|row| &row[..width * 4]);
    match bottom_up {
        true => rows.rev().flatten().copied().collect(),
        false => rows.flatten().copied().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::com::Com;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    };

    fn app(name: &str, parsing_name: &str) -> Option<Item> {
        item(name, parsing_name)
    }

    #[test]
    fn a_store_app_opens_through_the_apps_folder() {
        let calculator = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App";
        assert_eq!(
            app("Calculator", calculator),
            Some(Item {
                id: ItemId(calculator.to_string()),
                title: "Calculator".to_string(),
                subtitle: "App".to_string(),
                kind: Kind::App,
                action: Action::Open(format!("shell:AppsFolder\\{calculator}")),
            })
        );
    }

    #[test]
    fn an_app_known_by_its_path_shows_the_path() {
        let path = r"C:\Users\me\AppData\Roaming\Spotify\Spotify.exe";
        let spotify = app("Spotify", path).expect("an item");
        assert_eq!(spotify.subtitle, path);
        assert_eq!(
            spotify.action,
            Action::Open(format!("shell:AppsFolder\\{path}"))
        );
    }

    #[test]
    fn a_web_link_shows_the_address() {
        let link = app("Node.js website", "https://nodejs.org/").expect("an item");
        assert_eq!(link.subtitle, "https://nodejs.org/");
    }

    #[test]
    fn an_entry_without_a_name_or_a_parsing_name_is_left_out() {
        assert_eq!(app("", "Microsoft.Windows.Explorer"), None);
        assert_eq!(app("  ", "Microsoft.Windows.Explorer"), None);
        assert_eq!(app("File Explorer", ""), None);
    }

    /// Two pixels per row, rows padded to 12 bytes, as GDI pads them.
    const PADDED: [u8; 24] = [
        1, 1, 1, 1, 2, 2, 2, 2, 0, 0, 0, 0, //
        3, 3, 3, 3, 4, 4, 4, 4, 0, 0, 0, 0,
    ];

    #[test]
    fn top_down_rows_keep_their_order_without_padding() {
        let pixels = top_down(&PADDED, 2, 12, false);
        assert_eq!(pixels, [1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4]);
    }

    #[test]
    fn bottom_up_rows_are_turned_over() {
        let pixels = top_down(&PADDED, 2, 12, true);
        assert_eq!(pixels, [3, 3, 3, 3, 4, 4, 4, 4, 1, 1, 1, 1, 2, 2, 2, 2]);
    }

    #[test]
    fn premultiplying_scales_each_colour_by_its_alpha_rounded() {
        let mut pixels = [
            200, 100, 1, 255, // opaque: unchanged
            200, 100, 1, 0, // transparent: black
            255, 2, 1, 128, // half: halved, 1 rounds up
        ];
        premultiply(&mut pixels);
        assert_eq!(pixels, [200, 100, 1, 255, 0, 0, 0, 0, 128, 1, 1, 128]);
    }

    /// The entry for `id`, which every Windows 11 lists.
    fn entry(id: &str) -> Entry {
        let entries = find().expect("list the apps").into_iter();
        let mut found = entries.filter(|entry| entry.item.id.0 == id);
        found.next().unwrap_or_else(|| panic!("{id} is listed"))
    }

    const EXPLORER: &str = "Microsoft.Windows.Explorer";
    const SETTINGS: &str =
        "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

    #[test]
    fn finds_file_explorer_among_many_apps() {
        let _com = Com::init().expect("COM");
        assert!(find().expect("list the apps").len() > 1);
        assert_eq!(entry(EXPLORER).item.kind, Kind::App);
    }

    #[test]
    fn an_icon_has_the_size_asked_for_and_something_on_it() {
        let _com = Com::init().expect("COM");
        let icon = entry(EXPLORER).icon(40).expect("the icon");
        assert_eq!((icon.width, icon.height), (40, 40));
        assert_eq!(icon.bgra.len(), 40 * 40 * 4);
        let opaque = icon.bgra.chunks(4).filter(|pixel| pixel[3] == 255).count();
        assert!(opaque > 100, "{opaque} opaque pixels");
    }

    #[test]
    fn icon_pixels_are_premultiplied() {
        let _com = Com::init().expect("COM");
        for id in [EXPLORER, SETTINGS] {
            let icon = entry(id).icon(48).expect("the icon");
            let pixels = icon.bgra.chunks(4);
            let mut colours = pixels.flat_map(|pixel| pixel[..3].iter().map(|c| (*c, pixel[3])));
            assert!(colours.all(|(colour, alpha)| colour <= alpha), "{id}");
        }
    }

    #[test]
    fn reads_a_bottom_up_bitmap_top_row_first() {
        // Two rows of one pixel; GDI stores the bottom row first.
        let bitmap = test_bitmap(&[0x40, 0x40, 0x40, 0x40, 0xC0, 0xC0, 0xC0, 0xC0], 32);
        let icon = pixels(bitmap.0).expect("the pixels");
        assert_eq!((icon.width, icon.height), (1, 2));
        assert_eq!(icon.bgra, [0xC0, 0xC0, 0xC0, 0xC0, 0x40, 0x40, 0x40, 0x40]);
    }

    #[test]
    fn refuses_a_bitmap_without_an_alpha_channel() {
        let bitmap = test_bitmap(&[0x40; 8], 24);
        assert!(pixels(bitmap.0).is_err());
    }

    #[test]
    fn refuses_a_bitmap_that_is_not_a_dib_section() {
        let bitmap = TestBitmap(unsafe { CreateBitmap(1, 1, 1, 32, None) });
        assert!(pixels(bitmap.0).is_err());
    }

    /// A GDI bitmap, deleted when the test is done with it.
    struct TestBitmap(HBITMAP);

    impl Drop for TestBitmap {
        fn drop(&mut self) {
            let _ = unsafe { DeleteObject(self.0.into()) };
        }
    }

    /// A bottom-up DIB section one pixel wide, `bit_count` bits per pixel,
    /// holding `bits` (rows padded to 4 bytes).
    fn test_bitmap(bits: &[u8], bit_count: u16) -> TestBitmap {
        let rows = (bits.len() / 4) as i32;
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 1,
                biHeight: rows,
                biPlanes: 1,
                biBitCount: bit_count,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut memory = std::ptr::null_mut();
        // SAFETY: `info` describes a 1 x rows 32-bit DIB; CreateDIBSection
        // points `memory` at rows * 4 bytes it owns, which we fill once.
        unsafe {
            let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut memory, None, 0)
                .expect("a DIB section");
            std::ptr::copy_nonoverlapping(bits.as_ptr(), memory.cast(), bits.len());
            TestBitmap(bitmap)
        }
    }
}
