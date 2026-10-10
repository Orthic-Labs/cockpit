# Whole-disk file name search: what exists, and what to choose

Written 2026-10-11 for the redesign of RightKit's file index (`rightkit-fsindex`), which Pulse's hub uses for Storage search and folder sizes. Adrian's rule: do not implement before knowing what exists and which option is best. This is the Mac and cross-platform part; the Windows part is the Dell chat's and is appended when it arrives.

Every external claim has a link. "Derived" means arithmetic from the cited source, not a figure the project states. "Unverified" means no primary source was found.

## What went wrong with the current index

Measured on Adrian's Mac on 2026-10-11 (`top`, `sample`, Pulse's `scan.log`), `rightkit-fsindex` 0.1.0 inside the hub:

- 12.58 million entries for `/` and 8.10 million for `/Volumes/D`: 5.4 GB resident, about 220 bytes per entry, for as long as the app runs.
- Change handling: FSEvents batches every 50 ms, and each batch re-lists every changed folder in full (`getattrlistbulk`). On a volume with compiler output that was continuous listing, 12 to 15 thousand small disk operations per second, and it starved the builds on the same drive.
- First crawl of 8 million entries without a snapshot: 183 s.

## Can the operating system's own index do the job?

No, as these two computers are set up.

- Mac, measured with `mdutil`, `mdfind` and `mdls` on 2026-10-11:
  - Off for the second drive: `mdutil -s /Volumes/D` says "Indexing and searching disabled", and `mdfind -name deliver_claude.rs` returns nothing although the file is there.
  - Hidden folders are not covered: `~/.claude` holds 303 `.json` files two levels down and `mdfind -onlyin ~/.claude` finds none.
  - System folders are not covered: `/usr/bin` has 932 items and `mdfind -onlyin /usr/bin` finds none.
  - No folder sizes: `kMDItemFSSize` is null for a folder and present for a file, so the size of a folder would still have to be summed by us. Pulse takes folder sizes from the same index as search.
  - Where it does cover (the home folder's visible files, `~/Library/Application Support`), a name query answers in 0.03 to 0.14 s.
  - A query under `~/Documents` returned nothing from this process; whether that is privacy filtering was not established.
- Dell: Windows Search runs but its default scope leaves out D: and the repositories: a query for `Cargo.toml` returned no rows (Dell chat, 2026-10-11).

## How the established tools do it

| Tool | Memory per entry | Picks up changes by | Under a storm of changes | Default excludes | Crawl and restart |
|---|---|---|---|---|---|
| [FSearch](https://github.com/cboxdoerfer/fsearch) (Linux, C) | About 60 to 80 bytes, derived: a 16-byte header, the name inline, 8 bytes each for size and time ([entry](https://github.com/cboxdoerfer/fsearch/blob/master/src/fsearch_database_entry.c)); sort orders are separate pointer arrays | inotify or fanotify, one watch per folder; or a scheduled rescan (both off by default, [config](https://raw.githubusercontent.com/cboxdoerfer/fsearch/master/src/fsearch_config.c)) | Events queue up and are applied once per second as one batch; events under a deleted folder are dropped; each event updates the one named entry, no folder is re-listed ([index store](https://github.com/cboxdoerfer/fsearch/blob/master/src/fsearch_database_index_store.c), [index](https://github.com/cboxdoerfer/fsearch/blob/master/src/fsearch_database_index.c)) | `/.snapshots`, `/proc`, `/sys` | `readdir` plus `fstatat`; saved database with names prefix-compressed against the previous entry ([file format](https://github.com/cboxdoerfer/fsearch/blob/master/src/fsearch_database_file.c)) |
| [plocate](https://plocate.sesse.net/) (Linux, C++) | Nothing resident: the index stays on disk, 466 MB for 27 million entries, about 17 bytes per entry (derived) | No live tracking; a daily `updatedb` ([timer](https://git.sesse.net/?p=plocate;a=blob_plain;f=plocate-updatedb.timer;hb=HEAD)) | Not applicable. Its rescan skips every folder whose change time is unchanged since the last run ([updatedb](https://git.sesse.net/?p=plocate;a=blob_plain;f=updatedb.cpp;hb=HEAD)) | None built in; Debian adds `/tmp`, network and virtual file systems, and offers `.git .hg .svn` commented out ([conf](https://sources.debian.org/data/main/p/plocate/1.1.23-1/debian/updatedb.conf)) | Trigram posting lists, names zstd-compressed in blocks of 32 ([README](https://git.sesse.net/?p=plocate;a=blob_plain;f=README;hb=HEAD)) |
| [Everything](https://www.voidtools.com/faq/) (Windows, closed source) | About 100 bytes: "1,000,000 files will use about 100 MB of ram"; 21 million files, about 2 GB ([forum](https://www.voidtools.com/forum/viewtopic.php?p=42178)) | The drive's own change journal (NTFS USN), so nothing is missed while it is not running | The journal is kept by the file system, not by the tool; for other volumes a 64 KB change buffer and a rescan when it overflows ([folder indexing](https://www.voidtools.com/support/everything/folder_indexing/)) | None; the user excludes by folder, wildcard or attribute ([indexes](https://www.voidtools.com/support/everything/indexes/)) | Reads the drive's file table in bulk; about 1 minute for 1,000,000 files; database reloaded at start, then the journal is replayed |
| [Cardinal](https://github.com/cardisoft/cardinal) (macOS, Rust, Tauri) | Not stated (unverified). Nodes in a slab with 32-bit indices, names stored once in a pool ([design](https://github.com/cardisoft/cardinal/blob/master/doc/inner/search-cache.md)) | FSEvents with file-level events, 0.1 s latency | A batch is reduced to the smallest set of covering paths and each is rescanned; a change at the root is reported, not rebuilt | `/Volumes`, `~/Library` caches, logs and cloud storage, `/private/var`, `/private/tmp` ([list](https://github.com/cardisoft/cardinal/blob/master/cardinal/src/hooks/useIgnorePaths.ts)) | Parallel walk without metadata, metadata fetched later; compressed snapshot that stores the last event id so FSEvents replays from there |
| [Watchman](https://facebook.github.io/watchman/docs/config) (Meta) | Not an index of the disk | FSEvents, 0.01 s latency, 20 ms settle | After dropped events it recrawls; resync from the FSEvents journal is off by default for correctness doubts | `.git .hg .svn` watched shallowly; advises moving busy build folders out of the tree; only the first 8 ignores are filtered by macOS itself | Not persistent |

What Apple says about FSEvents ([header and guide](https://developer.apple.com/library/archive/documentation/Darwin/Conceptual/FSEvents_ProgGuide/UsingtheFSEventsFramework/UsingtheFSEventsFramework.html)): a longer latency reduces the volume of events, and the guide's example uses 3 seconds; file-level events name the changed item, so it can be handled without listing its folder; events are advisory and a periodic full sweep is expected; "must scan subdirectories" and dropped-event flags mean rescan that subtree; a stored event id replays history after a restart.

No source was found on how Spotlight itself consumes FSEvents (unverified). The licences of FSearch and Cardinal were not checked in this pass; that has to be done before any code is taken from them.

## What the evidence supports

1. **Memory.** The tools that keep the whole list in memory use 60 to 100 bytes per entry; one keeps it on disk at about 17. At 220 the current index is two to three times the worst of them. A fixed record of about 16 bytes with the name inline and 32-bit parent links (FSearch, Cardinal) reaches well under 100; keeping names compressed on disk and only the structure in memory (plocate) goes lower.
2. **Changes.** None of them lists a whole folder for each change. They take the named item from the event and update that one entry, in batches no shorter than 0.1 s and typically 1 s. Re-listing is kept for the cases the system says need it (dropped events, "must scan subdirectories"), and then only for that subtree.
3. **Excludes.** Caches, logs, temporary folders and version-control folders are excluded by default in the Mac tool and offered in the Linux ones. No tool surveyed excludes build output such as `target` or `node_modules` by default; that would be our own choice, and Watchman's advice (keep busy build folders out of what is watched) supports it.
4. **Restart.** Save the index with the last event id and replay from it (Cardinal, Everything); skip unchanged folders by their change time when a rescan is needed (plocate).
5. **Windows.** The reference design is the drive's file table plus its change journal (Everything). First facts from the Dell: both drives are NTFS, the journal can be queried without elevation, and it is only 32 MB, so a design on it must handle the journal wrapping.

## Options for the decision

| Option | For | Against |
|---|---|---|
| Use the operating system's index | No memory or crawl of our own | Off for the second drive on the Mac and for D: and the repositories on the Dell; no folder sizes |
| Adopt an existing engine | Proven designs | Everything is closed source and Windows-only; FSearch is Linux-only; Cardinal is a Mac app, not a library, and its licence is unchecked |
| Keep RightKit's index and rebuild its storage and change handling on the points above | One owner on both systems; Pulse already calls it | It is the design that failed; it has to be measured against these figures before it goes back into the hub |

A fourth choice sits beside these: whether Pulse needs a whole-disk index at all, or only the folders the Storage page shows. The Dell's hub answers Storage with a plain scan (2.77 million entries in about 8.5 minutes, 1.0 to 1.3 GB peak) and no index.

## Windows

To be added from the Dell chat's survey.
