# hyalo and Everything, measured

[Everything](https://www.voidtools.com/) is the yardstick for finding files on Windows, so hyalo was measured against it on the same machine, the same drives, at the same time.

## The machine

- Intel Core Ultra 7 270K Plus, 32 GB of RAM
- Two NVMe SSDs in RAID 0, NTFS, both indexed (C: and D:)
- Windows 11 Pro, build 26300
- hyalo 0.2.7 (release build), Everything 1.4.1.1032 (x64) with its service
- Measured on 13 November 2026, with warm caches. hyalo's search times were measured again later that day, after the search changes that followed 0.2.8, on a fresh walk of the same drives (2,220,790 names).
- hyalo's indexer service was measured that evening, from the build that brought it (after 0.2.9), installed as hyalo installs it. Loading and searching the index it lists were measured later still, from 0.3.0, in turn with the walked snapshot above.

## Results

| | hyalo, walking | hyalo, with its indexer service | Everything |
|---|---|---|---|
| Names in the index | 2,217,570 | 2,296,194 | 2,384,839 |
| Building the index from nothing | 104 s | 2.7 s | 10.6 s |
| Loading the saved index at start | 0.36 s | 0.49 s | 0.55 s |
| Memory the index takes | 212 MB | 236 MB | 220 MB |
| Search, median of one query | 2–6 ms | 2–6 ms | 2–11 ms |

Search times per query, in milliseconds, the median of many runs:

| Query | hyalo, walked | hyalo, from its indexer service | Everything |
|---|---:|---:|---:|
| `a` | 5.5 | 5.6 | 5.9 |
| `e` | 5.7 | 5.2 | 7.0 |
| `no` | 5.2 | 4.9 | 10.6 |
| `rep` | 5.1 | 5.4 | 10.5 |
| `report` | 4.8 | 5.3 | 9.9 |
| `rpt` | 5.9 | 6.1 | 10.2 |
| `notes.txt` | 4.6 | 4.8 | 9.3 |
| `vsc` | 4.8 | 5.2 | 9.8 |
| `readme` | 5.9 | 5.5 | 9.4 |
| `zqxj` (no such name) | 4.3 | 4.7 | 9.4 |
| `ext:pdf` | 1.8 | 2.0 | 2.4 |

## What the numbers mean

- **Searching is as quick, or quicker.** Over more than two million names hyalo answered every query here in 2 to 6 ms, Everything in 2 to 11 ms, both faster than a screen refreshes. hyalo also does more with each name than Everything does: besides finding the letters anywhere, it scores word starts and acronyms (`vsc` finds Visual Studio Code) and the other keyboard layout, then ranks the results.
- **Building the index from nothing takes hyalo 2.7 seconds with its indexer service**, four times quicker than Everything, forty times quicker than its own walk. The service, which you install once from the settings window with an administrator's yes, reads each NTFS drive's master file table as Everything does: 1.5 s to have the system drive's table read and the first files sent, then the launcher takes in 2.29 million names in 0.8 s; the data drive, with 4,685, takes 0.05 s. The launcher itself never runs as administrator. Without the service hyalo walks the folders with your own rights, as before: 104 s, and only on its first start. Either way it then loads its saved index at start, in a third of a second walked and half a second from the service, whose index has 85,000 more names and each entry's record number, and follows every change as it happens: with the service through each drive's journal, told within a tenth of a second.
- **Searching is the same either way**: the index is the launcher's own, wherever its names came from. Measured on both, in turn, every query here took the same within half a millisecond.
- **Memory is the same**, about 100 bytes a name. With the service each entry also keeps the number of its file's record (4 bytes), so a change from the journal finds it.
- The indexes are not the same size. Everything lists every file record on the volumes. hyalo leaves out `$Recycle.Bin`, `System Volume Information` and the other `$` folders; walking, it never enters junctions or symbolic links and cannot see into folders your account may not open, which the service's master file table lists like any other (other users' profiles, `WindowsApps`), as Everything does. A real hyalo config also leaves out `C:\Windows\WinSxS` by default, which brings it to about 1.9 million names here.

## How it was measured

**hyalo.** The index came from hyalo's own walk of both drives with nothing excluded, saved as a snapshot. `cargo bench -p hyalo-index` with `HYALO_BENCH_SNAPSHOT` naming that snapshot loads it as the launcher does at start and runs each query 21 times in the same process, asking for the 16 best rows, as the launcher does. Its memory is the index's own count of the bytes it holds.

**hyalo's indexer service.** Installed from the build that brought it, as the settings window installs it. `service_lists_the_fixed_drives`, an ignored test (`cargo test --release -p hyalo -- --ignored --nocapture service_lists`), asks the service for each fixed drive from nothing, as the launcher does at a first start, and takes the listing into an index as the launcher does; the time runs from the request to the index whole, and includes the service flushing what Windows holds of the drive to the disk before it reads the table. The median of five runs: C: 2.61 s (1.46 s to the first files, 0.82 s taking them in), D: 0.05 s; the launcher asks for both drives at once. Its memory is the index's own count of its bytes. `service_tells_of_changes_on_the_system_drive` makes, renames and removes a file in `%TEMP%`: the service told of each within 0.1 to 0.3 s. With `HYALO_BENCH_SNAPSHOT` naming a file, `service_lists_the_fixed_drives` also saves the index of both drives there as the launcher saves it (2,306,240 names when measured); the bench above then loaded and searched it, five runs taken in turn with five on the walked snapshot.

**Everything.** Started with an empty database, the time until it said, through its IPC, that its database was loaded; then closed and started again on the saved database. Searches went through Everything's IPC (`WM_COPYDATA`, as its SDK sends them) from another process, so they include the hop between processes. Each query ran 51 times, asking for the first 16 results in Everything's default order, by name. Its memory is the working set of its process after indexing, without its service.

The medians of repeated runs differ by a millisecond or two from one run to the next, on both sides; hyalo's here are the median of five bench runs, both of its columns from the same session.

`readme` used to take 31 ms. Twelve thousand names in `C:\Windows\WinSxS\Manifests`, a hundred letters in fifteen words each, have an `r` that starts a word with `eadme` after it in order, so each looked like a possible acronym, and hyalo checked them one by one with the matcher's full search, all on the one thread whose share of the index held that folder. Now a possible acronym is checked word by word without that search, and the threads are dealt the index a few thousand entries at a time in turn, so such a folder is shared among them.
