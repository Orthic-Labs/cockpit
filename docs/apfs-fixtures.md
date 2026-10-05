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
- The image, mountpoint, attach plist and state file live in a new `mktemp -d` dir named
  `cockpit-apfs-fixture.XXXXXX`, which also holds a `.cockpit-apfs-fixture` marker containing a
  unique per-run token (`uuidgen`, else a pid/random fallback).
  The state file records `TMPDIR`, `MOUNTPOINT`, `IMAGE`, `RUN_TOKEN`, `ATTACH_PLIST`,
  `IMAGE_DEVICE`, `DEVICE`, `ROOT`, `BASELINE_USED_BYTES`, `SNAPSHOT`, `SNAPSHOT_KIND`,
  `SNAPSHOT_UUID`, `SNAPSHOT_NAME`. It is parsed, never sourced.
- Attach uses `hdiutil attach -plist`; devices come from `system-entities:N:dev-entry` / `mount-point`
  (PlistBuddy). Ownership is verified by matching the exact `image-path` in `hdiutil info -plist`; both
  devices must appear under that image.
- Teardown requires the temp dir name pattern, the state file inside it, mountpoint and image under
  it, and an exact match between the marker content and the `RUN_TOKEN` recorded in the state file.
  It then looks devices up **by image path** (so a partial attach with no recorded devices is still
  found and detached). A recorded device that is not owned by the image is refused, and any device
  mounted at the mountpoint must also be owned by the image — anything else refuses rather than
  detaches. It never removes anything while the mountpoint is mounted or the image is still
  attached, and `rm -rf` targets only the validated temp dir.
- Image, mount & volume commands are bounded (`timeout`, else `gtimeout`, else `perl -e 'alarm'`).
  Setup installs its EXIT trap *before* creating the first owned resource; on failure it runs
  teardown when a state file exists, or removes the bare temp dir when the marker proves this run
  created it (matched against the in-memory run token).
- Snapshots: **never attempted — unsupported in this harness** (Codex review). `diskutil apfs` has
  `listSnapshots` and `deleteSnapshot` but no create verb (diskutil(8)). `tmutil localsnapshot` takes
  no mount-point operand and is documented to snapshot all Time Machine volumes, so it cannot be
  confined to the fixture and could create snapshots on real volumes — it is not called. Setup always
  records `SNAPSHOT_KIND=unsupported` and a `SNAPSHOT=UNSUPPORTED: snapshot creation is unconfigured
  in this harness ...` line; the `snapshot_retained.bin` file stays as a normal fixture entry.
  Teardown retains its exact-UUID `diskutil apfs deleteSnapshot` path, which no-ops when nothing was
  created. No product cleanup commands run.

## What the test asserts
1. Volume identity is stable, id starts with `uuid:`, and is identical across entries.
2. Nested folders (`nested`, `nested/inner`, `nested/inner/deep`) equal the sum of their children's attributed bytes.
3. Three hard-link names attribute allocation once (not double-counted).
4. `sparse.bin` allocation < 256 MiB logical.
5. Clones and the report total claim zero reclaim lower bound, state Unknown.
6. `locked/` (mode 000) makes `accounting.incomplete` true, drops the reclaim upper bound, and yields an
   explicit `enumerate` inspection error on exactly that path with a non-empty message.
7. Volume used-delta is `<= attributed + 64 MiB` and `>= half of the unique data`. Skipped explicitly if unavailable.
8. Snapshot retention: always `SKIP snapshot retention: ...` today — no volume-confined creation
   path is configured in this harness, so `COCKPIT_APFS_FIXTURE_SNAPSHOT` is always `unsupported`.
   The assertion path is kept for a future confined mechanism (e.g. `fs_snapshot_create` in a
   signed helper).

## Gate wiring

`scripts/gate.sh` runs this harness in its macOS block, exports only validated
`COCKPIT_APFS_FIXTURE_*` keys without evaluating shell output & runs the real APFS test.
Explicit teardown plus an EXIT trap covers successful & failed gates; teardown failure fails CI.
The earlier workspace test runs skip this fixture until setup supplies its root.
Snapshot retention, when supported, is created by overwriting an existing file after snapshot creation.
Setup also prints `COCKPIT_APFS_FIXTURE_SNAPSHOT` (currently always `unsupported`), which matches the gate's existing key filter (no gate change needed).
