//! Little-endian numbers in what a volume gives, read only where they are.

/// A file reference: the record's number in its low 48 bits, the
/// record's sequence number above them.
const NUMBER: u64 = 0xFFFF_FFFF_FFFF;

/// The record number a file reference holds, if it fits the index's.
pub(crate) fn number(reference: u64) -> Option<u32> {
    u32::try_from(reference & NUMBER).ok()
}

pub(crate) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

pub(crate) fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

pub(crate) fn i64_at(bytes: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}
