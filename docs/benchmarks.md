# hyalo and Everything, measured

[Everything](https://www.voidtools.com/) is the yardstick for finding files on Windows, so hyalo was measured against it on the same machine, the same drives, at the same time.

## The machine

- Intel Core Ultra 7 270K Plus, 32 GB of RAM
- Two NVMe SSDs in RAID 0, NTFS, both indexed (C: and D:)
- Windows 11 Pro, build 26300
- hyalo 0.2.7 (release build), Everything 1.4.1.1032 (x64) with its service
- Measured on 13 November 2026, with warm caches. hyalo's search times were measured again later that day, after the search changes that followed 0.2.8, on a fresh walk of the same drives (2,220,790 names).

## Results

| | hyalo | Everything |
|---|---|---|
| Names in the index | 2,217,570 | 2,384,839 |
| Building the index from nothing | 104 s | 10.6 s |
| Loading the saved index at start | 0.36 s | 0.55 s |
| Memory the index takes | 212 MB | 220 MB |
| Search, median of one query | 2–6 ms | 2–11 ms |

Search times per query, in milliseconds, the median of many runs:

| Query | hyalo | Everything |
|---|---:|---:|
| `a` | 5.3 | 5.9 |
| `e` | 5.8 | 7.0 |
| `no` | 4.9 | 10.6 |
| `rep` | 5.4 | 10.5 |
| `report` | 4.9 | 9.9 |
| `rpt` | 6.0 | 10.2 |
| `notes.txt` | 4.6 | 9.3 |
| `vsc` | 5.1 | 9.8 |
| `readme` | 6.2 | 9.4 |
| `zqxj` (no such name) | 4.4 | 9.4 |
| `ext:pdf` | 1.8 | 2.4 |

## What the numbers mean

- **Searching is as quick, or quicker.** Over more than two million names hyalo answered every query here in 2 to 6 ms, Everything in 2 to 11 ms, both faster than a screen refreshes. hyalo also does more with each name than Everything does: besides finding the letters anywhere, it scores word starts and acronyms (`vsc` finds Visual Studio Code) and the other keyboard layout, then ranks the results.
- **Building the index from nothing is where Everything wins**, by ten times. Everything reads the NTFS master file table through a service that runs as administrator. hyalo never asks for admin rights: it walks the folders with your own rights, and only on its first start. After that it loads its saved index in a third of a second and follows every change to the drives as it happens, so the walk is not something you wait for again.
- **Memory is the same**, about 100 bytes a name.
- The two indexes are not the same size. Everything lists every file record on the volumes. hyalo leaves out `$Recycle.Bin`, `System Volume Information` and the other `$` folders, never enters junctions or symbolic links, and cannot see into folders your account may not open. A real hyalo config also leaves out `C:\Windows\WinSxS` by default, which brings it to about 1.9 million names here.

## How it was measured

**hyalo.** The index came from hyalo's own walk of both drives with nothing excluded, saved as a snapshot. `cargo bench -p hyalo-index` with `HYALO_BENCH_SNAPSHOT` naming that snapshot loads it as the launcher does at start and runs each query 21 times in the same process, asking for the 16 best rows, as the launcher does. Its memory is the index's own count of the bytes it holds.

**Everything.** Started with an empty database, the time until it said, through its IPC, that its database was loaded; then closed and started again on the saved database. Searches went through Everything's IPC (`WM_COPYDATA`, as its SDK sends them) from another process, so they include the hop between processes. Each query ran 51 times, asking for the first 16 results in Everything's default order, by name. Its memory is the working set of its process after indexing, without its service.

The medians of repeated runs differ by a millisecond or two from one run to the next, on both sides; hyalo's here are the median of five bench runs.

`readme` used to take 31 ms. Twelve thousand names in `C:\Windows\WinSxS\Manifests`, a hundred letters in fifteen words each, have an `r` that starts a word with `eadme` after it in order, so each looked like a possible acronym, and hyalo checked them one by one with the matcher's full search, all on the one thread whose share of the index held that folder. Now a possible acronym is checked word by word without that search, and the threads are dealt the index a few thousand entries at a time in turn, so such a folder is shared among them.
