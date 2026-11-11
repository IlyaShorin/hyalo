//! Puts hyalo's icon into the executable, so Explorer, the Start menu and
//! the tray show it: `assets/hyalo.ico` (drawn by `scripts/icon.ps1`)
//! becomes a compiled resource file the linker takes as it is. No
//! resource compiler and no crate: the format is a header per resource,
//! one icon resource per image and a group listing them.

use std::path::PathBuf;
use std::{env, fs};

const ICON: &str = "assets/hyalo.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
const LICENSE: &str = "../../LICENSE";

/// The license's copyright line, for the version resource.
fn copyright() -> String {
    let license = fs::read_to_string(LICENSE).expect("the license");
    let line = license.lines().find(|line| line.starts_with("Copyright"));
    line.expect("a copyright line").to_string()
}

/// The version resource Explorer's properties, Task Manager and the
/// Startup apps list read hyalo's name and version from: the fixed part
/// with the workspace version, then English (US, Unicode) strings.
fn version_info(copyright: &str) -> Vec<u8> {
    let version = env::var("CARGO_PKG_VERSION").expect("the version");
    let part = |name: &str| -> u32 { env::var(name).expect("a part").parse().expect("a number") };
    let (major, minor, patch) = (
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH"),
    );
    let ms = (major << 16) | minor;
    let ls = patch << 16;
    // VS_FIXEDFILEINFO: signature, structure version, file and product
    // versions, flags mask and flags, NT Win32, an app, no subtype or date.
    let fixed: Vec<u8> = [
        0xFEEF_04BD,
        0x0001_0000,
        ms,
        ls,
        ms,
        ls,
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ]
    .iter()
    .flat_map(|word: &u32| word.to_le_bytes())
    .collect();
    let strings = [
        ("CompanyName", "Ilya Shorin"),
        ("FileDescription", "hyalo"),
        ("FileVersion", version.as_str()),
        ("InternalName", "hyalo"),
        ("LegalCopyright", copyright),
        ("OriginalFilename", "hyalo.exe"),
        ("ProductName", "hyalo"),
        ("ProductVersion", version.as_str()),
    ];
    let table = strings
        .iter()
        .map(|(key, value)| text_node(key, value))
        .collect();
    let string_info = node(
        "StringFileInfo",
        &[],
        0,
        1,
        vec![node("040904B0", &[], 0, 1, table)],
    );
    let translation = [0x0409_u16, 0x04B0]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<u8>>();
    let var_info = node(
        "VarFileInfo",
        &[],
        0,
        1,
        vec![node("Translation", &translation, 4, 0, vec![])],
    );
    node(
        "VS_VERSION_INFO",
        &fixed,
        52,
        0,
        vec![string_info, var_info],
    )
}

/// A string of the version resource: its value counted in UTF-16 units,
/// the closing zero included.
fn text_node(key: &str, value: &str) -> Vec<u8> {
    let units: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    node(key, &bytes, units.len() as u16, 1, vec![])
}

/// One block of the version resource: its length, its value's length, its
/// type (1 text, 0 binary), its key in UTF-16 with a closing zero, then
/// its value and its children, each starting on four bytes. The length
/// leaves out padding after the last part. Every block here has a value
/// or children, so the padding before an empty value is the padding the
/// first child needs anyway.
fn node(key: &str, value: &[u8], value_length: u16, kind: u16, children: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = vec![0, 0];
    out.extend(value_length.to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(
        key.encode_utf16()
            .chain([0])
            .flat_map(|unit| unit.to_le_bytes()),
    );
    out.resize(out.len().next_multiple_of(4), 0);
    out.extend(value);
    for child in children {
        out.resize(out.len().next_multiple_of(4), 0);
        out.extend(child);
    }
    let length = out.len() as u16;
    out[..2].copy_from_slice(&length.to_le_bytes());
    out
}

fn main() {
    let ico = fs::read(ICON).expect("the icon");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("hyalo.res");
    let mut res = resources(&ico);
    res.extend(resource(RT_VERSION, 1, &version_info(&copyright())));
    fs::write(&out, res).expect("write the resources");
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed={LICENSE}");
    // The executable and the tests alike, so tests can load the icon.
    println!("cargo:rustc-link-arg={}", out.display());
}

/// The resources for an `.ico` file: after the empty resource every file
/// starts with, each image as icon `1..`, then group icon 1 naming them.
/// A group entry is the file's directory entry with the icon's number in
/// place of the image's offset.
fn resources(ico: &[u8]) -> Vec<u8> {
    let count = u16::from_le_bytes([ico[4], ico[5]]);
    let mut out = resource(0, 0, &[]);
    let mut group = [0, 0, 1, 0].to_vec();
    group.extend(count.to_le_bytes());
    for id in 1..=count {
        let entry = &ico[6 + 16 * usize::from(id - 1)..][..16];
        let size = u32::from_le_bytes(entry[8..12].try_into().expect("4 bytes")) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().expect("4 bytes")) as usize;
        out.extend(resource(RT_ICON, id, &ico[offset..offset + size]));
        group.extend(&entry[..12]);
        group.extend(id.to_le_bytes());
    }
    out.extend(resource(RT_GROUP_ICON, 1, &group));
    out
}

/// One resource: its 32-byte header (sizes, type and name as numbers,
/// then versions, flags and language, all 0), its data, padding to four
/// bytes.
fn resource(kind: u16, name: u16, data: &[u8]) -> Vec<u8> {
    let mut out = (data.len() as u32).to_le_bytes().to_vec();
    out.extend(32_u32.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(name.to_le_bytes());
    out.extend([0; 16]);
    out.extend(data);
    out.resize(out.len().next_multiple_of(4), 0);
    out
}
