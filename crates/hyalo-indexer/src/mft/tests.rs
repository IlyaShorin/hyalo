use super::build::record;
use super::*;

/// The size of a record on most volumes.
const SIZE: usize = 1024;

/// 13.11.2026 10:30 UTC, as Windows counts.
const WRITTEN: i64 = 134_390_394_000_000_000;
const WRITTEN_SECONDS: i64 = 1_794_565_800;

const WIN32: u8 = 1;
const POSIX: u8 = 0;
const WIN32_AND_DOS: u8 = 3;

fn name(parent: u32, text: &str) -> Name {
    Name {
        parent,
        text: text.to_string(),
    }
}

#[test]
fn windows_time_reads_in_seconds_since_1970() {
    assert_eq!(seconds(116_444_736_000_000_000), 0);
    assert_eq!(seconds(WRITTEN + 9_999_999), WRITTEN_SECONDS);
    assert_eq!(seconds(-1), -11_644_473_601, "rounded down");
}

#[test]
fn a_file_s_record_gives_its_name_folder_size_and_last_write() {
    let made = record()
        .sequence(4)
        .written(WRITTEN)
        .name(77, "report.pdf", WIN32_AND_DOS)
        .data(0, 123_456, &[0x11, 0x20, 0x30, 0x00]);
    let parsed = parse(&made.bytes(SIZE)).expect("in use");
    assert_eq!(
        parsed,
        Record {
            sequence: 4,
            base: None,
            folder: false,
            names: vec![name(77, "report.pdf")],
            written: Some(WRITTEN_SECONDS),
            size: Some(123_456),
            elsewhere: vec![],
        }
    );
}

#[test]
fn a_folder_s_record_says_it_is_one_and_holds_no_data() {
    let made = record()
        .folder()
        .written(WRITTEN)
        .name(5, "Documents", WIN32);
    let parsed = parse(&made.bytes(SIZE)).expect("in use");
    assert!(parsed.folder);
    assert_eq!(parsed.names, [name(5, "Documents")]);
    assert_eq!(parsed.size, None);
}

#[test]
fn a_small_file_s_data_lies_in_its_record() {
    let made = record().name(5, "a.txt", WIN32).small_data(37, "");
    assert_eq!(parse(&made.bytes(SIZE)).expect("in use").size, Some(37));
}

#[test]
fn another_stream_of_the_file_is_not_its_size() {
    let made = record()
        .name(5, "a.txt", WIN32)
        .small_data(9, "Zone.Identifier")
        .small_data(3, "");
    assert_eq!(parse(&made.bytes(SIZE)).expect("in use").size, Some(3));
    let only_another = record()
        .name(5, "b.txt", WIN32)
        .small_data(9, "Zone.Identifier");
    let parsed = parse(&only_another.bytes(SIZE)).expect("in use");
    assert_eq!(parsed.size, None);
}

#[test]
fn a_short_8_3_name_is_left_out_and_every_hard_link_kept() {
    let made = record()
        .name(40, "LONGFI~1.TXT", DOS)
        .name(40, "long file name.txt", WIN32)
        .name(41, "Case Sensitive", POSIX)
        .name(42, "LINK.TXT", WIN32_AND_DOS);
    let parsed = parse(&made.bytes(SIZE)).expect("in use");
    assert_eq!(
        parsed.names,
        [
            name(40, "long file name.txt"),
            name(41, "Case Sensitive"),
            name(42, "LINK.TXT")
        ]
    );
}

#[test]
fn a_name_that_is_not_valid_unicode_is_left_out() {
    let made = record()
        .name_units(5, &[0x61, 0xD800, 0x62], WIN32)
        .name(5, "ёлка", WIN32);
    let parsed = parse(&made.bytes(SIZE)).expect("in use");
    assert_eq!(parsed.names, [name(5, "ёлка")]);
}

#[test]
fn a_record_no_longer_in_use_holds_no_file() {
    let made = record().free().name(5, "gone.txt", WIN32);
    assert_eq!(parse(&made.bytes(SIZE)), None);
}

#[test]
fn bytes_that_are_no_record_hold_no_file() {
    let mut bytes = record().name(5, "a.txt", WIN32).bytes(SIZE);
    bytes[0..4].copy_from_slice(b"BAAD");
    assert_eq!(parse(&bytes), None);
    assert_eq!(parse(&[]), None);
    assert_eq!(parse(b"FILE"), None);
}

#[test]
fn an_extension_record_names_its_base_and_holds_what_it_could_not() {
    let made = record()
        .extending(300)
        .name(9, "fourth link.txt", WIN32)
        .data(0, 5_000, &[0x11, 0x02, 0x40, 0x00]);
    let parsed = parse(&made.bytes(SIZE)).expect("in use");
    assert_eq!(parsed.base, Some(300));
    assert_eq!(parsed.written, None);
    let mut base = parse(&record().written(WRITTEN).bytes(SIZE)).expect("in use");
    base.merge(parsed);
    assert_eq!(base.names, [name(9, "fourth link.txt")]);
    assert_eq!(base.size, Some(5_000));
    assert_eq!(base.written, Some(WRITTEN_SECONDS));
}

#[test]
fn data_mapped_from_a_later_cluster_on_does_not_say_the_size() {
    let made = record()
        .extending(300)
        .data(16, 999, &[0x11, 0x02, 0x40, 0x00]);
    assert_eq!(parse(&made.bytes(SIZE)).expect("in use").size, None);
    let mut base = parse(&record().data(0, 5_000, &[]).bytes(SIZE)).expect("in use");
    base.merge(parse(&made.bytes(SIZE)).expect("in use"));
    assert_eq!(base.size, Some(5_000), "the start's size stays");
}

#[test]
fn an_attribute_list_names_the_records_that_hold_names_or_data() {
    let made = record().written(WRITTEN).list(&[
        (0x10, 0, 300),
        (0x30, 0, 300),
        (0x30, 0, 301),
        (0x80, 0, 302),
        (0x80, 16, 301),
        (0x90, 0, 303),
    ]);
    let parsed = parse(&made.bytes(SIZE)).expect("in use");
    assert_eq!(parsed.elsewhere, [300, 301, 302]);
}

#[test]
fn a_record_read_from_the_disk_gets_its_sectors_ends_back() {
    // A name long enough to cross the first sector's end.
    let long = "x".repeat(200);
    let made = record().name(5, &long, WIN32);
    let mut on_disk = made.on_disk(SIZE);
    assert_ne!(on_disk, made.bytes(SIZE));
    assert!(fix(&mut on_disk));
    assert_eq!(on_disk, made.bytes(SIZE));
    assert_eq!(parse(&on_disk).expect("in use").names, [name(5, &long)]);
}

#[test]
fn a_record_whose_sector_was_not_written_whole_is_refused() {
    let made = record().name(5, "torn.txt", WIN32);
    let mut torn = made.on_disk(SIZE);
    torn[SIZE - 2] ^= 0xFF;
    assert!(!fix(&mut torn));
    let mut first_torn = made.on_disk(SIZE);
    first_torn[510] ^= 0xFF;
    assert!(!fix(&mut first_torn));
}

#[test]
fn a_record_of_four_kilobytes_has_eight_sectors_to_put_back() {
    let made = record().name(5, &"y".repeat(250), WIN32);
    let mut on_disk = made.on_disk(4096);
    assert!(fix(&mut on_disk));
    assert_eq!(on_disk, made.bytes(4096));
}

#[test]
fn an_update_sequence_that_does_not_fit_the_record_is_refused() {
    let made = record().name(5, "a.txt", WIN32);
    assert!(!fix(&mut made.on_disk(SIZE)[..512].to_vec()), "too short");
    let mut wrong_count = made.on_disk(SIZE);
    wrong_count[6] = 9;
    assert!(!fix(&mut wrong_count));
    let mut no_count = made.on_disk(SIZE);
    no_count[6..8].copy_from_slice(&[0, 0]);
    assert!(!fix(&mut no_count));
    let mut out_of_it = made.on_disk(SIZE);
    out_of_it[4..6].copy_from_slice(&1020_u16.to_le_bytes());
    assert!(!fix(&mut out_of_it));
    assert!(!fix(&mut []));
}

#[test]
fn an_attribute_running_past_the_record_ends_the_list() {
    let made = record()
        .name(5, "kept.txt", WIN32)
        .name(5, "cut.txt", WIN32);
    let mut bytes = made.bytes(SIZE);
    // The second attribute claims to run on past the end.
    let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
    let length = u32::from_le_bytes(bytes[first + 4..first + 8].try_into().unwrap()) as usize;
    let second = first + length;
    bytes[second + 4..second + 8].copy_from_slice(&5000_u32.to_le_bytes());
    let parsed = parse(&bytes).expect("in use");
    assert_eq!(parsed.names, [name(5, "kept.txt")]);
}

#[test]
fn an_attribute_too_short_to_be_one_ends_the_list() {
    let made = record()
        .name(5, "kept.txt", WIN32)
        .name(5, "never.txt", WIN32);
    let mut bytes = made.bytes(SIZE);
    let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
    bytes[first + 4..first + 8].copy_from_slice(&0_u32.to_le_bytes());
    // A length of nothing would read the same attribute for ever.
    assert_eq!(parse(&bytes).expect("in use").names, []);
}

#[test]
fn a_name_longer_than_its_attribute_is_left_out() {
    let made = record().name(5, "short", WIN32);
    let mut bytes = made.bytes(SIZE);
    let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
    let value = first + 0x18;
    bytes[value + 0x40] = 200;
    assert_eq!(parse(&bytes).expect("in use").names, []);
}

#[test]
fn a_reference_beyond_the_index_s_numbers_is_no_parent() {
    let made = record().name(5, "a.txt", WIN32);
    let mut bytes = made.bytes(SIZE);
    let first = u16::from_le_bytes([bytes[0x14], bytes[0x15]]) as usize;
    let value = first + 0x18;
    let huge = (1_u64 << 40) | 5;
    bytes[value..value + 8].copy_from_slice(&huge.to_le_bytes());
    assert_eq!(parse(&bytes).expect("in use").names, []);
}

fn at(lcn: u64, clusters: u64) -> Extent {
    Extent {
        lcn: Some(lcn),
        clusters,
    }
}

#[test]
fn runs_say_where_each_piece_of_a_value_lies() {
    assert_eq!(runs(&[0x11, 0x20, 0x30, 0x00]), Some(vec![at(0x30, 0x20)]));
    // The second starts 16 clusters before the first: offsets are signed
    // and each counts from the one before.
    let two = [0x21, 0x10, 0x00, 0x01, 0x11, 0x08, 0xF0, 0x00];
    assert_eq!(runs(&two), Some(vec![at(256, 16), at(240, 8)]));
    let long = [0x33, 0x00, 0x00, 0x01, 0x00, 0x00, 0x10, 0x00];
    assert_eq!(runs(&long), Some(vec![at(0x10_0000, 0x1_0000)]));
}

#[test]
fn a_run_with_no_place_is_sparse() {
    let sparse = [0x11, 0x04, 0x10, 0x01, 0x06, 0x11, 0x02, 0x08, 0x00];
    let expected = vec![
        at(16, 4),
        Extent {
            lcn: None,
            clusters: 6,
        },
        at(24, 2),
    ];
    assert_eq!(runs(&sparse), Some(expected));
}

#[test]
fn runs_that_do_not_read_whole_are_refused() {
    assert_eq!(runs(&[]), None, "no end");
    assert_eq!(runs(&[0x11, 0x20]), None, "cut short");
    assert_eq!(runs(&[0x10, 0x20, 0x00]), None, "no length");
    assert_eq!(runs(&[0x11, 0x00, 0x30, 0x00]), None, "a length of nothing");
    assert_eq!(runs(&[0x11, 0x01, 0xFF, 0x00]), None, "before the volume");
    assert_eq!(
        runs(&[0x91, 0x01, 0x00]),
        None,
        "a place longer than 8 bytes"
    );
    assert_eq!(runs(&[0x00]), Some(vec![]), "nothing");
}

#[test]
fn the_table_s_own_record_says_where_the_table_lies() {
    let made =
        record()
            .name(5, "$MFT", WIN32_AND_DOS)
            .data(0, 64 * 4096, &[0x11, 0x40, 0x10, 0x00]);
    assert_eq!(
        data_runs(&made.bytes(SIZE)),
        Some((0, vec![at(0x10, 0x40)]))
    );
    let more = record().extending(0).data(64, 0, &[0x11, 0x08, 0x50, 0x00]);
    assert_eq!(data_runs(&more.bytes(SIZE)), Some((64, vec![at(0x50, 8)])));
    assert_eq!(data_runs(&record().small_data(4, "").bytes(SIZE)), None);
}

/// A table of 1 KB records in 4 KB clusters: 16 clusters at 100, then 8 at
/// 300.
fn table() -> Table {
    Table::new(
        4096,
        1024,
        vec![(16, vec![at(300, 8)]), (0, vec![at(100, 16)])],
    )
    .expect("whole")
}

#[test]
fn the_table_maps_its_bytes_to_the_volume_s_across_its_pieces() {
    let table = table();
    assert_eq!(table.length(), 24 * 4096);
    let span = |at, length| Span { at, length };
    assert_eq!(table.spans(0, 4096), Some(vec![span(100 * 4096, 4096)]));
    // Record 70 is in the second piece: 70 KB is 17.5 clusters in.
    assert_eq!(
        table.spans(70 * 1024, 1024),
        Some(vec![span(301 * 4096 + 2048, 1024)])
    );
    assert_eq!(
        table.spans(15 * 4096, 2 * 4096),
        Some(vec![span(115 * 4096, 4096), span(300 * 4096, 4096)]),
        "across the two"
    );
    assert_eq!(table.spans(23 * 4096, 2 * 4096), None, "beyond its end");
}

#[test]
fn a_table_with_a_gap_or_a_sparse_piece_is_refused() {
    assert!(Table::new(4096, 1024, vec![(16, vec![at(300, 8)])]).is_none());
    assert!(Table::new(4096, 1024, vec![(0, vec![at(1, 4)]), (5, vec![at(9, 4)])]).is_none());
    let sparse = Extent {
        lcn: None,
        clusters: 4,
    };
    assert!(Table::new(4096, 1024, vec![(0, vec![at(1, 4), sparse])]).is_none());
    assert!(
        Table::new(4096, 1000, vec![(0, vec![at(1, 4)])]).is_none(),
        "odd records"
    );
    assert!(Table::new(0, 1024, vec![(0, vec![at(1, 4)])]).is_none());
}

#[test]
fn the_table_is_read_in_pieces_of_whole_records_and_clusters() {
    let table = table();
    // 70 records in use: 17.5 clusters, read as 18.
    let pieces = table.pieces(70 * 1024, 8 * 4096);
    let piece = |start, length| Piece { start, length };
    assert_eq!(
        pieces,
        [
            piece(0, 8 * 4096),
            piece(8 * 4096, 8 * 4096),
            piece(16 * 4096, 2 * 4096)
        ]
    );
    assert_eq!(pieces[1].first(1024), 32);
    assert_eq!(
        table.pieces(1 << 40, 1 << 30),
        [piece(0, 24 * 4096)],
        "no further than it maps"
    );
    assert_eq!(table.pieces(0, 4096), []);
}

#[test]
fn records_bigger_than_a_cluster_are_read_whole() {
    let table = Table::new(512, 1024, vec![(0, vec![at(10, 3), at(20, 5)])]).expect("whole");
    let pieces = table.pieces(4 * 1024, 1500);
    let piece = |start, length| Piece { start, length };
    assert_eq!(
        pieces,
        [
            piece(0, 1024),
            piece(1024, 1024),
            piece(2048, 1024),
            piece(3072, 1024)
        ]
    );
    let span = |at, length| Span { at, length };
    assert_eq!(
        table.spans(1024, 1024),
        Some(vec![span(12 * 512, 512), span(20 * 512, 512)])
    );
}
