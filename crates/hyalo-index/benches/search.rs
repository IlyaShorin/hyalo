//! How long a search takes on a made-up drive of a million names (or as
//! many as `HYALO_BENCH_NAMES` says): `cargo bench -p hyalo-index`.

use std::time::{Duration, Instant};

use hyalo_core::filter::{Clock, Query};
use hyalo_core::places::Places;
use hyalo_index::{Index, synthetic};

/// Single letters, words, an acronym, the wrong layout, ё typed as е, a
/// full name, letters no name has, then filters, alone and with words.
const QUERIES: [&str; 22] = [
    "a",
    "e",
    "no",
    "rep",
    "report",
    "rpt",
    "notes.txt",
    "vsc",
    "мысщву",
    "отчет",
    "readme",
    "zqxj",
    "ext:pdf",
    "ext:pdf,docx report",
    "in:Documents",
    "notes in:Desktop",
    "size:>10mb",
    "size:<1kb e",
    "mod:7d",
    "mod:2026-11 rep",
    "mod:>2026-01-01 ext:txt in:node_modules",
    "e ext:rs size:1kb..1mb mod:30d",
];

/// Runs of each query; the median and the worst are reported.
const RUNS: usize = 21;

/// Rows asked for: the eight the list shows, and room for picked files.
const LIMIT: usize = 16;

fn main() {
    let count = std::env::var("HYALO_BENCH_NAMES")
        .map_or(1_000_000, |names| names.parse().unwrap_or(1_000_000));
    let started = Instant::now();
    let index = synthetic::drive(count, 42);
    let built = started.elapsed();
    let memory = index.memory() as f64 / 1_048_576.0;
    println!(
        "{} entries, built in {built:.2?}, {memory:.0} MiB",
        index.len()
    );
    let index = reloaded(&index);
    let memory = index.memory() as f64 / 1_048_576.0;
    println!("loaded back: {memory:.0} MiB");
    let places = Places::new(&[r"C:\Desktop", r"C:\Documents"]);
    for query in QUERIES {
        report(&index, &places, query);
    }
}

/// The index saved to memory and loaded back, as a restart loads it.
fn reloaded(index: &Index) -> Index {
    let started = Instant::now();
    let mut bytes = Vec::new();
    index.save(&mut bytes).expect("save to memory");
    let saved = started.elapsed();
    let started = Instant::now();
    let back = Index::load(&mut bytes.as_slice()).expect("load");
    let size = bytes.len() as f64 / 1_048_576.0;
    println!(
        "snapshot {size:.0} MiB, saved in {saved:.2?}, loaded in {:.2?}",
        started.elapsed()
    );
    back
}

fn report(index: &Index, places: &Places, text: &str) {
    let clock = Clock {
        now: synthetic::NOW,
        offset: 3 * 60 * 60,
    };
    let query = Query::parse(text, clock);
    let mut times: Vec<Duration> = (0..RUNS).map(|_| timed(index, places, &query)).collect();
    times.sort();
    let found = index.search_with(&query.words, &query.filters, places, LIMIT);
    let query = text;
    let best = found.rows.first().map_or("", |row| row.item.title.as_str());
    println!(
        "{query:>10}: median {:6.2?}, worst {:6.2?}, {} scored, best row {best:?}",
        times[RUNS / 2],
        times[RUNS - 1],
        found.scored
    );
}

fn timed(index: &Index, places: &Places, query: &Query) -> Duration {
    let started = Instant::now();
    let found = index.search_with(&query.words, &query.filters, places, LIMIT);
    std::hint::black_box(found);
    started.elapsed()
}
