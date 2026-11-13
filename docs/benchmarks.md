# hyalo and Everything, measured

[Everything](https://www.voidtools.com/) is the yardstick for finding files on Windows, so hyalo was measured against it on the same machine, the same drives, at the same time.

## The machine

- Intel Core Ultra 7 270K Plus, 32 GB of RAM
- Two NVMe SSDs in RAID 0, NTFS, both indexed (C: and D:)
- Windows 11 Pro, build 26300
- hyalo 0.2.7 (release build), Everything 1.4.1.1032 (x64) with its service
- Measured on 13 November 2026, with warm caches

## Results

| | hyalo | Everything |
|---|---|---|
| Names in the index | 2,217,570 | 2,384,839 |
| Building the index from nothing | 104 s | 10.6 s |
| Loading the saved index at start | 0.36 s | 0.55 s |
| Memory the index takes | 212 MB | 220 MB |
| Search, median of one query | 2–11 ms (31 ms for `readme`) | 2–11 ms |

Search times per query, in milliseconds, the median of many runs:

| Query | hyalo | Everything |
|---|---:|---:|
| `a` | 6.2 | 5.9 |
| `e` | 8.5 | 7.0 |
| `no` | 7.6 | 10.6 |
| `rep` | 6.2 | 10.5 |
| `report` | 5.6 | 9.9 |
| `rpt` | 10.7 | 10.2 |
| `notes.txt` | 5.5 | 9.3 |
| `vsc` | 7.5 | 9.8 |
| `readme` | 30.9 | 9.4 |
| `zqxj` (no such name) | 5.7 | 9.4 |
| `ext:pdf` | 2.0 | 2.4 |

## What the numbers mean

- **Searching is on a par.** Both answer most queries in under 11 ms over more than two million names, faster than a screen refreshes. hyalo was quicker on seven of the eleven queries and slower on four; `readme`, a name that appears tens of thousands of times in source trees, is its slow spot. hyalo also does more with each name than Everything does: besides finding the letters anywhere, it scores word starts and acronyms (`vsc` finds Visual Studio Code) and the other keyboard layout, then ranks the results.
- **Building the index from nothing is where Everything wins**, by ten times. Everything reads the NTFS master file table through a service that runs as administrator. hyalo never asks for admin rights: it walks the folders with your own rights, and only on its first start. After that it loads its saved index in a third of a second and follows every change to the drives as it happens, so the walk is not something you wait for again.
- **Memory is the same**, about 100 bytes a name.
- The two indexes are not the same size. Everything lists every file record on the volumes. hyalo leaves out `$Recycle.Bin`, `System Volume Information` and the other `$` folders, never enters junctions or symbolic links, and cannot see into folders your account may not open. A real hyalo config also leaves out `C:\Windows\WinSxS` by default, which brings it to about 1.9 million names here.

## How it was measured

**hyalo.** The index came from hyalo's own walk of both drives with nothing excluded, saved as a snapshot. `cargo bench -p hyalo-index` with `HYALO_BENCH_SNAPSHOT` naming that snapshot loads it as the launcher does at start and runs each query 21 times in the same process, asking for the 16 best rows, as the launcher does. Its memory is the index's own count of the bytes it holds.

**Everything.** Started with an empty database, the time until it said, through its IPC, that its database was loaded; then closed and started again on the saved database. Searches went through Everything's IPC (`WM_COPYDATA`, as its SDK sends them) from another process, so they include the hop between processes. Each query ran 51 times, asking for the first 16 results in Everything's default order, by name. Its memory is the working set of its process after indexing, without its service.

The medians of repeated runs differ by a millisecond or two from one run to the next, on both sides.
