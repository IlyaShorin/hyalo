//! Journal records made in tests, laid out as NTFS writes them.

/// The reason a made record gives.
const FILE_CREATE: u32 = 0x100;

/// A version 2 record of a change to the file `record` in `parent`.
pub fn change(record: u32, parent: u32, name: &str) -> Vec<u8> {
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let length = (60 + units.len()).next_multiple_of(8);
    let mut bytes = vec![0; length];
    bytes[0..4].copy_from_slice(&(length as u32).to_le_bytes());
    bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
    let reference = |record: u32| (u64::from(record) | 5 << 48).to_le_bytes();
    bytes[8..16].copy_from_slice(&reference(record));
    bytes[16..24].copy_from_slice(&reference(parent));
    bytes[40..44].copy_from_slice(&FILE_CREATE.to_le_bytes());
    bytes[56..58].copy_from_slice(&(units.len() as u16).to_le_bytes());
    bytes[58..60].copy_from_slice(&60_u16.to_le_bytes());
    bytes[60..60 + units.len()].copy_from_slice(&units);
    bytes
}

/// What a read answers: the place to read on from, then the records.
pub fn answer(next: i64, records: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = next.to_le_bytes().to_vec();
    records.iter().for_each(|record| bytes.extend(record));
    bytes
}
