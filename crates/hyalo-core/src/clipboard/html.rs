//! The text of what an app copied as HTML alone (`HTML Format`): its
//! fragment, the tags taken out, the entities read, the blanks squeezed
//! as a browser shows them.

/// The text of `html`, the clipboard's `HTML Format`: a header of
/// `Name:value` lines, then the page, the part copied marked by the
/// header's byte offsets or by `<!--StartFragment-->` comments.
pub fn text(html: &[u8]) -> String {
    let end = html
        .iter()
        .rposition(|&byte| byte != 0)
        .map_or(0, |last| last + 1);
    let html = &html[..end];
    let fragment = String::from_utf8_lossy(fragment(html));
    let mut out = Out::default();
    let mut rest = fragment.as_ref();
    while let Some(at) = rest.find(['<', '&']) {
        out.words(&rest[..at]);
        rest = match rest[at..].starts_with('<') {
            true => out.tag(&rest[at..]),
            false => out.entity(&rest[at..]),
        };
    }
    out.words(rest);
    out.finished()
}

/// The part copied: between the header's `StartFragment` and
/// `EndFragment` offsets, else between the comments that mark it, else
/// all from the page's first tag.
fn fragment(html: &[u8]) -> &[u8] {
    let offset = |name: &str| -> Option<usize> {
        let header = &html[..html
            .iter()
            .position(|&byte| byte == b'<')
            .unwrap_or(html.len())];
        let header = std::str::from_utf8(header).ok()?;
        let line = header
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))?;
        line.trim().parse().ok()
    };
    let by_offsets = offset("StartFragment").zip(offset("EndFragment"));
    if let Some(part) = by_offsets.and_then(|(start, end)| html.get(start..end)) {
        return part;
    }
    let after = |mark: &[u8]| find(html, mark).map(|at| at + mark.len());
    let start = after(b"<!--StartFragment-->");
    let end = find(html, b"<!--EndFragment-->");
    match start.zip(end).and_then(|(start, end)| html.get(start..end)) {
        Some(part) => part,
        None => {
            &html[html
                .iter()
                .position(|&byte| byte == b'<')
                .unwrap_or(html.len())..]
        }
    }
}

/// Where `needle` first is in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|window| window == needle)
}

/// Tags that start a line of their own.
const BLOCKS: [&str; 18] = [
    "br",
    "p",
    "div",
    "li",
    "tr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "table",
    "ul",
    "ol",
    "blockquote",
    "pre",
    "section",
];

/// Tags whose content is never shown.
const HIDDEN: [&str; 3] = ["script", "style", "title"];

/// The text so far, lines of words each squeezed to one space.
#[derive(Default)]
struct Out {
    lines: Vec<String>,
    line: String,
}

impl Out {
    /// Words of the page: each run of blanks is one space.
    fn words(&mut self, words: &str) {
        for (at, word) in words.split(char::is_whitespace).enumerate() {
            if at > 0 && !self.line.ends_with(' ') {
                self.line.push(' ');
            }
            self.line.push_str(word);
        }
    }

    /// A tag at the start of `rest`, or a comment: breaks the line where
    /// it starts a block, skips what it hides; what follows it.
    fn tag<'a>(&mut self, rest: &'a str) -> &'a str {
        if let Some(comment) = rest.strip_prefix("<!--") {
            return comment.split_once("-->").map_or("", |(_, after)| after);
        }
        let Some((tag, after)) = rest[1..].split_once('>') else {
            return "";
        };
        let name = tag
            .trim_start_matches('/')
            .split([' ', '/', '\t', '\r', '\n'])
            .next();
        let name = name.unwrap_or_default().to_ascii_lowercase();
        if BLOCKS.contains(&name.as_str()) {
            self.lines.push(std::mem::take(&mut self.line));
        }
        match HIDDEN.contains(&name.as_str()) && !tag.starts_with('/') {
            true => {
                let closed = after.split_once(&format!("</{name}"));
                closed
                    .and_then(|(_, rest)| rest.split_once('>'))
                    .map_or("", |(_, rest)| rest)
            }
            false => after,
        }
    }

    /// An entity at the start of `rest`: the letter it stands for, or the
    /// `&` itself when it reads as none; what follows it.
    fn entity<'a>(&mut self, rest: &'a str) -> &'a str {
        let read = rest[1..]
            .split_once(';')
            .and_then(|(name, after)| Some((letter(name)?, after)));
        match read {
            Some((letter, after)) => {
                self.line.push(letter);
                after
            }
            None => {
                self.line.push('&');
                &rest[1..]
            }
        }
    }

    /// The lines, each trimmed, blank ones gone.
    fn finished(mut self) -> String {
        self.lines.push(self.line);
        let lines = self
            .lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty());
        lines.collect::<Vec<_>>().join("\n")
    }
}

/// The letter the entity `&name;` stands for: one of the common names,
/// or a number in decimal or hex. A non-breaking space is a space.
fn letter(name: &str) -> Option<char> {
    let named = [
        ("amp", '&'),
        ("lt", '<'),
        ("gt", '>'),
        ("quot", '"'),
        ("apos", '\''),
    ];
    if let Some(&(_, letter)) = named.iter().find(|(known, _)| *known == name) {
        return Some(letter);
    }
    if name == "nbsp" {
        return Some(' ');
    }
    let number = name.strip_prefix('#')?;
    let code = match number.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `body` as the clipboard holds it: a header with the offsets of
    /// the part between the comments, as Chrome and Word write it.
    fn clipboard(body: &str) -> Vec<u8> {
        let page =
            format!("<html><body><!--StartFragment-->{body}<!--EndFragment--></body></html>");
        let header_size = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
        let start = header_size + page.find("-->").expect("mark") + 3;
        let end = start + body.len();
        let html = header_size + page.len();
        format!(
            "Version:0.9\r\nStartHTML:{header_size:010}\r\nEndHTML:{html:010}\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n{page}"
        )
        .into_bytes()
    }

    #[test]
    fn the_part_copied_loses_its_tags_and_keeps_its_words() {
        let html = clipboard("<b>Dear</b>   <i>sir</i>,\r\n  the <a href=\"x\">report</a>");
        assert_eq!(text(&html), "Dear sir, the report");
    }

    #[test]
    fn blocks_and_breaks_start_lines_and_blank_lines_go() {
        let body = "<p>one</p><p>two<br>three</p><ul><li>four</li><li>five</li></ul><div></div>";
        assert_eq!(text(&clipboard(body)), "one\ntwo\nthree\nfour\nfive");
        let upper = "<P>one</P><BR/>two";
        assert_eq!(text(&clipboard(upper)), "one\ntwo");
    }

    #[test]
    fn entities_are_read_and_an_ampersand_that_is_none_stays() {
        let body = "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&#39; f&nbsp;g &#x416; &#1078; R&D; &bogus; &";
        assert_eq!(
            text(&clipboard(body)),
            "a & b <c> \"d\" 'e' f g Ж ж R&D; &bogus; &"
        );
    }

    #[test]
    fn scripts_styles_and_comments_are_never_text() {
        let body =
            "<style>p { color: red }</style>one<!-- not this --><script>var a = 1;</script> two";
        assert_eq!(text(&clipboard(body)), "one two");
    }

    #[test]
    fn without_offsets_the_comments_mark_the_part_else_the_whole_page_counts() {
        let marked = "Version:0.9\r\n<html><body>before<!--StartFragment-->in<!--EndFragment-->after</body></html>";
        assert_eq!(text(marked.as_bytes()), "in");
        let plain = "Version:0.9\r\n<p>all</p> of it";
        assert_eq!(text(plain.as_bytes()), "all\nof it");
        let page = "<html><body>before<b>in</b>after</body></html>";
        let header = "Version:0.9\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
        let start = header.len() + page.find("<b>").expect("bold");
        let end = start + "<b>in</b>".len();
        let offsets =
            format!("Version:0.9\r\nStartFragment:{start:010}\r\nEndFragment:{end:010}\r\n{page}");
        assert_eq!(text(offsets.as_bytes()), "in", "the offsets alone");
        let wrong = "Version:0.9\r\nStartFragment:900\r\nEndFragment:999\r\n<p>still</p>";
        assert_eq!(text(wrong.as_bytes()), "still");
        assert_eq!(
            text(
                b"Version:0.9
<p>ends</p>\0\0"
            ),
            "ends",
            "the NUL the clipboard ends it with"
        );
        assert_eq!(text(b""), "");
        assert_eq!(text(b"<b unclosed"), "");
    }
}
