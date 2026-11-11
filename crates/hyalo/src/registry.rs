//! Reading the registry: values as Windows stores them, and the names of
//! a key's subkeys.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, KEY_READ, REG_ROUTINE_FLAGS, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::core::{HSTRING, PWSTR};

/// The value `name` of `key` under `root`, as `kind` reads it, in bytes;
/// `None` when there is none.
pub fn value(root: HKEY, key: &str, name: &str, kind: REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    let (key, name) = (HSTRING::from(key), HSTRING::from(name));
    let mut size = 0_u32;
    // SAFETY: the first call only measures; the second writes at most
    // `size` bytes into a buffer of that size.
    unsafe {
        let measured = RegGetValueW(root, &key, &name, kind, None, None, Some(&mut size));
        measured.ok().ok()?;
        let mut bytes = vec![0_u8; size as usize];
        let data = Some(bytes.as_mut_ptr().cast());
        let read = RegGetValueW(root, &key, &name, kind, None, data, Some(&mut size));
        read.ok().ok()?;
        bytes.truncate(size as usize);
        Some(bytes)
    }
}

/// Reads a string, plain or with `%variables%`, which come expanded.
pub const STRING: REG_ROUTINE_FLAGS = REG_ROUTINE_FLAGS(RRF_RT_REG_SZ.0 | RRF_RT_REG_EXPAND_SZ.0);

/// A registry string's bytes as text, up to its first NUL: an expanded
/// string comes in the room its unexpanded text took, with what was there
/// left after the NUL.
pub fn text(bytes: &[u8]) -> String {
    let pairs = bytes.as_chunks::<2>().0.iter();
    let units = pairs.map(|&pair| u16::from_le_bytes(pair));
    let units: Vec<u16> = units.take_while(|&unit| unit != 0).collect();
    String::from_utf16_lossy(&units)
}

/// The names of `key`'s subkeys under `root`, in the registry's order;
/// none when the key is not there.
pub fn subkeys(root: HKEY, key: &str) -> Vec<String> {
    let mut open = HKEY::default();
    // SAFETY: a plain call; the key it opens is closed below.
    let opened = unsafe { RegOpenKeyExW(root, &HSTRING::from(key), None, KEY_READ, &mut open) };
    if opened != ERROR_SUCCESS {
        return Vec::new();
    }
    let names = (0..).map_while(|index| subkey(open, index)).collect();
    // SAFETY: `open` is the key opened above, closed once.
    let _ = unsafe { RegCloseKey(open) };
    names
}

/// The name of subkey `index` of the open `key`; `None` past the last.
fn subkey(key: HKEY, index: u32) -> Option<String> {
    // A key's name is at most 255 characters.
    let mut name = [0_u16; 256];
    let mut length = name.len() as u32;
    // SAFETY: `name` holds `length` units, which the call may fill.
    let found = unsafe {
        let name = PWSTR(name.as_mut_ptr());
        RegEnumKeyExW(key, index, Some(name), &mut length, None, None, None, None)
    };
    (found == ERROR_SUCCESS).then(|| String::from_utf16_lossy(&name[..length as usize]))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_DWORD, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_DWORD,
        RegDeleteKeyW, RegDeleteTreeW, RegSetKeyValueW,
    };

    /// A key of its own for one test, `Software\hyalo-test-<pid>-<test>`
    /// in the current user's key, deleted with all in it afterwards. Never
    /// Windows' own.
    pub struct TestKey(pub String);

    impl TestKey {
        pub fn new(test: &str) -> TestKey {
            TestKey(format!(
                r"Software\hyalo-test-{}-{test}",
                std::process::id()
            ))
        }

        /// `key` under this one.
        pub fn under(&self, key: &str) -> String {
            format!(r"{}\{key}", self.0)
        }

        /// Writes the string `value` as `name` of `key` under this one,
        /// making the key if it is not there.
        pub fn text(&self, key: &str, name: &str, value: &str) {
            self.string(key, name, value, REG_SZ);
        }

        /// The same, as a string with `%variables%` Windows expands.
        pub fn expandable(&self, key: &str, name: &str, value: &str) {
            self.string(key, name, value, REG_EXPAND_SZ);
        }

        fn string(&self, key: &str, name: &str, value: &str, kind: REG_VALUE_TYPE) {
            let units: Vec<u16> = value.encode_utf16().chain([0]).collect();
            let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
            self.write(key, name, kind, &bytes);
        }

        /// Writes the number `value` as `name` of `key` under this one.
        pub fn number(&self, key: &str, name: &str, value: u32) {
            self.write(key, name, REG_DWORD, &value.to_le_bytes());
        }

        /// Writes `bytes` of `kind` as `name` of `key` under this one.
        pub fn write(&self, key: &str, name: &str, kind: REG_VALUE_TYPE, bytes: &[u8]) {
            // SAFETY: the data is `bytes.len()` bytes long and outlives the
            // call.
            let written = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(self.under(key)),
                    &HSTRING::from(name),
                    kind.0,
                    Some(bytes.as_ptr().cast()),
                    bytes.len() as u32,
                )
            };
            written.ok().expect("write a test value");
        }
    }

    impl Drop for TestKey {
        fn drop(&mut self) {
            let key = HSTRING::from(&self.0);
            // SAFETY: plain calls on the test's own key.
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &key) };
            let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &key) };
        }
    }

    #[test]
    fn a_value_is_read_as_its_kind_and_one_not_there_is_none() {
        let test = TestKey::new("registry-value");
        test.text("a", "name", "Everything");
        test.number("a", "count", 7);
        let read = |name, kind| value(HKEY_CURRENT_USER, &test.under("a"), name, kind);
        let name = read("name", STRING).expect("the name");
        assert_eq!(text(&name), "Everything");
        assert_eq!(
            read("count", RRF_RT_REG_DWORD),
            Some(7_u32.to_le_bytes().to_vec())
        );
        assert_eq!(read("count", STRING), None, "not a string");
        assert_eq!(read("missing", STRING), None);
        let elsewhere = value(HKEY_CURRENT_USER, &test.under("b"), "name", STRING);
        assert_eq!(elsewhere, None, "no such key");
    }

    #[test]
    fn an_expandable_string_reads_with_its_variables_expanded() {
        let test = TestKey::new("registry-expand");
        test.expandable("a", "path", r"%SystemRoot%\explorer.exe");
        let path = value(HKEY_CURRENT_USER, &test.under("a"), "path", STRING);
        let root = std::env::var("SystemRoot").expect("SystemRoot is set");
        assert_eq!(
            path.map(|bytes| text(&bytes)),
            Some(format!(r"{root}\explorer.exe"))
        );
    }

    #[test]
    fn a_strings_text_ends_before_its_nul() {
        assert_eq!(text(&[b'h', 0, b'i', 0, 0, 0]), "hi");
        assert_eq!(text(&[b'h', 0, 0, 0, b'x', 0]), "h", "left after the NUL");
        assert_eq!(text(&[b'h', 0, b'i']), "h", "half a unit");
        assert_eq!(text(&[0x30, 0x04, 0, 0]), "а", "UTF-16, little end first");
        assert_eq!(text(&[]), "");
    }

    #[test]
    fn subkeys_are_listed_by_name_and_a_missing_key_has_none() {
        let test = TestKey::new("registry-subkeys");
        for name in [
            "Everything",
            "{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}",
            "Steam App 1",
        ] {
            test.text(&format!(r"list\{name}"), "x", "y");
        }
        test.text("list", "value", "not a subkey");
        let mut names = subkeys(HKEY_CURRENT_USER, &test.under("list"));
        names.sort();
        assert_eq!(
            names,
            [
                "Everything",
                "Steam App 1",
                "{B5F500BB-4625-445D-A2E7-E7FE5E1E7E85}"
            ]
        );
        assert!(subkeys(HKEY_CURRENT_USER, &test.under("missing")).is_empty());
    }
}
