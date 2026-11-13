//! Files on the clipboard as Explorer puts them there (`CF_HDROP`): a
//! `DROPFILES` header, then each full path, each ending in a NUL, the
//! list ending in one more.

/// The size of `DROPFILES`: where the list starts (u32), a point (two
/// i32), whether that is in the non-client area (u32), whether the
/// paths are UTF-16 (u32).
const HEADER: usize = 20;

/// The paths in `bytes`, a `CF_HDROP`; `None` when it does not read, or
/// holds paths in an ANSI code page, which Explorer never writes.
pub fn files(bytes: &[u8]) -> Option<Vec<String>> {
    let word = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    if word(16)? == 0 {
        return None;
    }
    let list = bytes.get(word(0)? as usize..)?;
    let (pairs, _) = list.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
    let paths = units
        .split(|&unit| unit == 0)
        .take_while(|path| !path.is_empty());
    Some(paths.map(String::from_utf16_lossy).collect())
}

/// `paths` as a `CF_HDROP`, the paths in UTF-16.
pub fn dropped(paths: &[String]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER);
    for word in [HEADER as u32, 0, 0, 0, 1] {
        bytes.extend(word.to_le_bytes());
    }
    for path in paths {
        let units = path.encode_utf16().chain([0]);
        bytes.extend(units.flat_map(u16::to_le_bytes));
    }
    bytes.extend(0u16.to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn paths_written_as_explorer_writes_them_read_back_the_same() {
        let written = paths(&[r"C:\Users\me\отчёт.pdf", r"D:\a folder"]);
        let bytes = dropped(&written);
        assert_eq!(&bytes[..4], [20, 0, 0, 0]);
        assert_eq!(&bytes[4..16], [0; 12], "no point, in the client area");
        assert_eq!(&bytes[16..20], [1, 0, 0, 0], "wide");
        assert_eq!(
            &bytes[bytes.len() - 4..],
            [0, 0, 0, 0],
            "a NUL, then one more"
        );
        assert_eq!(files(&bytes), Some(written));
        assert_eq!(files(&dropped(&[])), Some(Vec::new()));
    }

    #[test]
    fn the_list_starts_where_the_header_says() {
        let mut bytes = dropped(&paths(&[r"C:\a"]));
        bytes.splice(20..20, [9, 9, 9, 9]);
        bytes[0] = 24;
        assert_eq!(files(&bytes), Some(paths(&[r"C:\a"])));
    }

    #[test]
    fn ansi_paths_and_what_does_not_read_are_nothing() {
        let mut ansi = dropped(&paths(&[r"C:\a"]));
        ansi[16] = 0;
        assert_eq!(files(&ansi), None);
        assert_eq!(files(&[0; 19]), None);
        let mut far = dropped(&paths(&[r"C:\a"]));
        far[0] = 200;
        assert_eq!(files(&far), None);
    }
}
