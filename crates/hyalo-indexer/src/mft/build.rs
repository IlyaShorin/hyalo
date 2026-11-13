//! Records made in tests, laid out byte for byte as NTFS lays them out.

use super::{
    ATTRIBUTE_LIST, DATA, DIRECTORY, FILE_NAME, IN_USE, Name, STANDARD_INFORMATION, STRIDE,
};

/// Where the update sequence starts, as NTFS 3.1 puts it.
const SEQUENCE_AT: usize = 0x30;

/// What every sector's end holds on disk while its own bytes wait in the
/// update sequence.
const CHECK: [u8; 2] = [0x2A, 0x00];

/// A record to make: its header and its attributes.
#[derive(Debug, Clone)]
pub struct Made {
    sequence: u16,
    flags: u16,
    base: u64,
    attributes: Vec<Vec<u8>>,
}

/// A file's name in the folder `parent`.
pub fn name(parent: u32, text: &str) -> Name {
    Name {
        parent,
        text: text.to_string(),
    }
}

/// A record in use for a file, with nothing in it yet.
pub fn record() -> Made {
    Made {
        sequence: 1,
        flags: IN_USE,
        base: 0,
        attributes: Vec::new(),
    }
}

impl Made {
    pub fn folder(mut self) -> Made {
        self.flags |= DIRECTORY;
        self
    }

    /// No longer in use: its file was removed.
    pub fn free(mut self) -> Made {
        self.flags &= !IN_USE;
        self
    }

    pub fn sequence(mut self, sequence: u16) -> Made {
        self.sequence = sequence;
        self
    }

    /// An extension record of the file whose own record is `base`.
    pub fn extending(mut self, base: u32) -> Made {
        self.base = u64::from(base) | 7 << 48;
        self
    }

    /// `$STANDARD_INFORMATION`, last written at `filetime`.
    pub fn written(self, filetime: i64) -> Made {
        let mut value = vec![0; 0x48];
        value[0x08..0x10].copy_from_slice(&filetime.to_le_bytes());
        // The other three times, which must not be taken for it.
        for at in [0x00, 0x10, 0x18] {
            value[at..at + 8].copy_from_slice(&(filetime + 1).to_le_bytes());
        }
        self.resident(STANDARD_INFORMATION, &[], &value)
    }

    /// A `$FILE_NAME` in the folder `parent`, in `namespace`.
    pub fn name(self, parent: u32, text: &str, namespace: u8) -> Made {
        let units: Vec<u16> = text.encode_utf16().collect();
        self.name_units(parent, &units, namespace)
    }

    /// A `$FILE_NAME` given as UTF-16 units, which may not be valid.
    pub fn name_units(self, parent: u32, units: &[u16], namespace: u8) -> Made {
        let mut value = vec![0; 0x42];
        let reference = u64::from(parent) | 3 << 48;
        value[0..8].copy_from_slice(&reference.to_le_bytes());
        // The sizes a name keeps go stale: never the file's size.
        value[0x28..0x38].copy_from_slice(&[0x11; 16]);
        value[0x40] = units.len() as u8;
        value[0x41] = namespace;
        value.extend(units.iter().flat_map(|unit| unit.to_le_bytes()));
        self.resident(FILE_NAME, &[], &value)
    }

    /// A data stream of `length` bytes kept in the record; named, another
    /// stream than the file's own.
    pub fn small_data(self, length: usize, name: &str) -> Made {
        let name: Vec<u16> = name.encode_utf16().collect();
        self.resident(DATA, &name, &vec![0x55; length])
    }

    /// The file's data stream, `length` bytes, of which this record maps
    /// the clusters from `first` on, lying where `runs` says.
    pub fn data(self, first: u64, length: u64, runs: &[u8]) -> Made {
        let mut attribute = vec![0; 0x40];
        attribute[0..4].copy_from_slice(&DATA.to_le_bytes());
        attribute[8] = 1;
        attribute[0x10..0x18].copy_from_slice(&first.to_le_bytes());
        attribute[0x20..0x22].copy_from_slice(&0x40_u16.to_le_bytes());
        for at in [0x28, 0x30, 0x38] {
            attribute[at..at + 8].copy_from_slice(&length.to_le_bytes());
        }
        attribute.extend_from_slice(runs);
        self.attribute(attribute)
    }

    /// An attribute list naming, for each entry, an attribute's type, the
    /// first cluster it maps and the record that holds it.
    pub fn list(self, entries: &[(u32, u64, u32)]) -> Made {
        let mut value = Vec::new();
        for &(kind, first, record) in entries {
            let mut entry = vec![0; 0x20];
            entry[0..4].copy_from_slice(&kind.to_le_bytes());
            entry[4..6].copy_from_slice(&0x20_u16.to_le_bytes());
            entry[8..0x10].copy_from_slice(&first.to_le_bytes());
            let reference = u64::from(record) | 9 << 48;
            entry[0x10..0x18].copy_from_slice(&reference.to_le_bytes());
            value.extend(entry);
        }
        self.resident(ATTRIBUTE_LIST, &[], &value)
    }

    /// An attribute whose value lies in the record.
    fn resident(self, kind: u32, name: &[u16], value: &[u8]) -> Made {
        let name_at = 0x18;
        let value_at = (name_at + 2 * name.len()).next_multiple_of(8);
        let mut attribute = vec![0; value_at];
        attribute[0..4].copy_from_slice(&kind.to_le_bytes());
        attribute[9] = name.len() as u8;
        attribute[0x0A..0x0C].copy_from_slice(&(name_at as u16).to_le_bytes());
        attribute[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        attribute[0x14..0x16].copy_from_slice(&(value_at as u16).to_le_bytes());
        let name = name.iter().flat_map(|unit| unit.to_le_bytes());
        for (at, byte) in name.enumerate() {
            attribute[name_at + at] = byte;
        }
        attribute.extend_from_slice(value);
        self.attribute(attribute)
    }

    /// Adds an attribute, its length set and padded to 8 bytes.
    fn attribute(mut self, mut attribute: Vec<u8>) -> Made {
        attribute.resize(attribute.len().next_multiple_of(8), 0);
        let length = attribute.len() as u32;
        attribute[4..8].copy_from_slice(&length.to_le_bytes());
        self.attributes.push(attribute);
        self
    }

    /// The record of `size` bytes as NTFS hands it over through its own
    /// calls, every sector's end holding its own bytes.
    pub fn bytes(&self, size: usize) -> Vec<u8> {
        let sectors = size / STRIDE;
        let first = (SEQUENCE_AT + 2 * (sectors + 1)).next_multiple_of(8);
        let mut record = vec![0; size];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&(SEQUENCE_AT as u16).to_le_bytes());
        record[6..8].copy_from_slice(&((sectors + 1) as u16).to_le_bytes());
        record[0x10..0x12].copy_from_slice(&self.sequence.to_le_bytes());
        record[0x14..0x16].copy_from_slice(&(first as u16).to_le_bytes());
        record[0x16..0x18].copy_from_slice(&self.flags.to_le_bytes());
        record[0x1C..0x20].copy_from_slice(&(size as u32).to_le_bytes());
        record[0x20..0x28].copy_from_slice(&self.base.to_le_bytes());
        let mut at = first;
        for attribute in &self.attributes {
            record[at..at + attribute.len()].copy_from_slice(attribute);
            at += attribute.len();
        }
        record[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let used = at + 8;
        record[0x18..0x1C].copy_from_slice(&(used as u32).to_le_bytes());
        // The update sequence: the check, then each sector's own end.
        record[SEQUENCE_AT..SEQUENCE_AT + 2].copy_from_slice(&CHECK);
        for sector in 1..=sectors {
            let end = sector * STRIDE - 2;
            let kept = SEQUENCE_AT + 2 * sector;
            let own = [record[end], record[end + 1]];
            record[kept..kept + 2].copy_from_slice(&own);
        }
        record
    }

    /// The record as it lies on disk: every sector's end holds the check,
    /// its own bytes kept in the update sequence.
    pub fn on_disk(&self, size: usize) -> Vec<u8> {
        let mut record = self.bytes(size);
        for sector in 1..=size / STRIDE {
            let end = sector * STRIDE - 2;
            record[end..end + 2].copy_from_slice(&CHECK);
        }
        record
    }
}
