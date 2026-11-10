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

fn main() {
    let ico = fs::read(ICON).expect("the icon");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("hyalo.res");
    fs::write(&out, resources(&ico)).expect("write the resources");
    println!("cargo:rerun-if-changed={ICON}");
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
