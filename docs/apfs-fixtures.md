# Disposable APFS fixtures (CI only)

Exercises the real `cockpit_core::scan_paths` against a genuine APFS volume: sparse files, hard links,
clones, an unreadable directory, and an optional snapshot. Local work stays static-only; never run
these scripts on a developer machine.

## Files
- `scripts/fixtures/apfs/setup.sh` - create, mount and populate a fresh 512 MB sparse APFS image.
- `scripts/fixtures/apfs/teardown.sh` - verify, detach and remove exactly what the state file names.
- `scripts/fixtures/apfs/lib.sh` - `bounded` (timeout / gtimeout / perl alarm), state parsing, `df` helper.
- `core/tests/apfs_fixture.rs` - `#[cfg(target_os = "macos")]`; prints `SKIP` and returns unless
  `COCKPIT_APFS_FIXTURE_ROOT` is set.

## Guards
- Both scripts refuse unless `GITHUB_ACTIONS=true`, `COCKPIT_APFS_FIXTURE=1`, the OS is Darwin; setup
  also refuses root (it would defeat the `chmod 000` fixture).
- The image, mountpoint and state file live in a new `mktemp -d` dir named `cockpit-apfs-fixture.XXXXXX`.
  The state file records `TMPDIR`, `MOUNTPOINT`, `IMAGE`, `IMAGE_DEVICE`, `DEVICE`, `ROOT`,
  `BASELINE_USED_BYTES`, `SNAPSHOT`. It is parsed, never sourced.
- Teardown requires the temp dir name pattern, state file inside it, mountpoint and image under it,
  `diskutil info` reporting the device mounted at exactly that mountpoint, and `hdiutil info` showing the
  device belongs to that image. Only then does it `hdiutil detach` that device and `rm -rf` the temp dir.
  It refuses to remove anything while still mounted.
- Image, mount & volume commands are bounded (`timeout`, else `gtimeout`, else `perl -e 'alarm'`). Setup traps
  failure and runs teardown for what it created.
- Snapshot: only `diskutil apfs snapshot <harness mountpoint>`. `tmutil localsnapshot` is intentionally not
  used (it snapshots user volumes). If refused, setup prints `SNAPSHOT: SKIP: ...`. No snapshot is ever
  deleted explicitly; detaching destroys the image and its snapshots. No product cleanup commands run.

## What the test asserts
1. Three hard-link names attribute allocation once (not double-counted).
2. `sparse.bin` allocation < 256 MiB logical.
3. Clones (`orig/copy1/copy2.bin`) and the report total claim zero reclaim lower bound, state Unknown.
4. `locked/` (mode 000) makes `accounting.incomplete` true, drops the reclaim upper bound, and is named
   in an inspection error or incomplete reason.
5. Volume used-delta (`df` now minus setup baseline, passed via `ScanOptions::volume_deltas`) is
   `<= attributed + 64 MiB` and `>= half of the ~13 MiB unique data`. Skipped explicitly if unavailable.

## Gate wiring

`scripts/gate.sh` runs this harness in its macOS block, exports only validated
`COCKPIT_APFS_FIXTURE_*` keys without evaluating shell output & runs the real APFS test.
Explicit teardown plus an EXIT trap covers successful & failed gates; teardown failure fails CI.
The earlier workspace test runs skip this fixture until setup supplies its root.
Snapshot retention, when supported, is created by overwriting an existing file after snapshot creation.
