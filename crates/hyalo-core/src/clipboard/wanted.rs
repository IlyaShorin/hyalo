//! Which of what is on the clipboard hyalo reads: nothing an app marked
//! private, else the one kind of content a row will show.

/// The marks an app puts on the clipboard to keep what it copied out of
/// any history, as password managers do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Marks {
    /// `ExcludeClipboardContentFromMonitorProcessing` is there: keep it
    /// out of everything that watches the clipboard.
    pub excluded: bool,
    /// `Clipboard Viewer Ignore` is there, the older mark of the same.
    pub ignored: bool,
    /// `CanIncludeInClipboardHistory`'s value, when it is there: 0 keeps
    /// it out of the history, 1 lets it in.
    pub history: Option<u32>,
}

/// Which of the formats hyalo knows the clipboard holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Offered {
    /// Files and folders: `CF_HDROP`.
    pub files: bool,
    /// UTF-16 text: `CF_UNICODETEXT`.
    pub text: bool,
    /// A bitmap: `CF_DIB`, which Windows makes of any other bitmap.
    pub image: bool,
    /// `HTML Format`.
    pub html: bool,
}

/// The kind of content hyalo reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    Files,
    /// The text, with the HTML and RTF beside it.
    Text,
    Image,
    /// HTML with no text beside it: its text is read out of it.
    Html,
}

/// What hyalo reads of the clipboard `offered` holds under `marks`:
/// nothing an app marked private; else files before text, text before
/// an image (a spreadsheet's cells come as both), and HTML alone last
/// (a browser's copied image comes with an `<img>`); nothing of formats
/// hyalo could not put back.
pub fn wanted(marks: Marks, offered: Offered) -> Option<Wanted> {
    if marks.excluded || marks.ignored || marks.history == Some(0) {
        return None;
    }
    let kinds = [
        (offered.files, Wanted::Files),
        (offered.text, Wanted::Text),
        (offered.image, Wanted::Image),
        (offered.html, Wanted::Html),
    ];
    kinds
        .into_iter()
        .find(|&(there, _)| there)
        .map(|(_, kind)| kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Offered = Offered {
        files: true,
        text: true,
        image: true,
        html: true,
    };

    #[test]
    fn files_come_before_text_text_before_an_image_and_html_alone_last() {
        let free = Marks::default();
        assert_eq!(wanted(free, ALL), Some(Wanted::Files));
        let no_files = Offered {
            files: false,
            ..ALL
        };
        assert_eq!(wanted(free, no_files), Some(Wanted::Text));
        let no_text = Offered {
            text: false,
            ..no_files
        };
        assert_eq!(wanted(free, no_text), Some(Wanted::Image));
        let html = Offered {
            image: false,
            ..no_text
        };
        assert_eq!(wanted(free, html), Some(Wanted::Html));
        assert_eq!(
            wanted(free, Offered::default()),
            None,
            "nothing hyalo knows"
        );
    }

    #[test]
    fn what_an_app_marks_private_is_never_read() {
        let marked = [
            Marks {
                excluded: true,
                ..Marks::default()
            },
            Marks {
                ignored: true,
                ..Marks::default()
            },
            Marks {
                history: Some(0),
                ..Marks::default()
            },
        ];
        for marks in marked {
            assert_eq!(wanted(marks, ALL), None, "{marks:?}");
        }
        let let_in = Marks {
            history: Some(1),
            ..Marks::default()
        };
        assert_eq!(wanted(let_in, ALL), Some(Wanted::Files));
    }
}
