#!/usr/bin/env bash
# CI-only: create a fresh disposable APFS sparse image, mount it inside a new temp dir and
# populate it. Never touches existing volumes. Static-only locally: do NOT run on a dev machine.
set -euo pipefail
# Accept legacy user/CI overrides without replacing explicit Pulse values.
for pulse_legacy_key in ${!COCKPIT_@}; do
  pulse_current_key="PULSE_${pulse_legacy_key#COCKPIT_}"
  if [[ -z "${!pulse_current_key+x}" ]]; then
    export "$pulse_current_key=${!pulse_legacy_key}"
  fi
done
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

if [[ "${GITHUB_ACTIONS:-}" != "true" || "${PULSE_APFS_FIXTURE:-}" != "1" ]]; then
  echo "REFUSED: requires GITHUB_ACTIONS=true and PULSE_APFS_FIXTURE=1" >&2
  exit 2
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "REFUSED: macOS only" >&2
  exit 2
fi
if [[ "$(id -u)" == "0" ]]; then
  echo "REFUSED: running as root would defeat the chmod 000 inaccessible-directory fixture" >&2
  exit 2
fi

BASE="$(cd "${RUNNER_TEMP:-${TMPDIR:-/tmp}}" && pwd -P)"

# Unique token for this run. It is written into the creation marker and the state file so
# teardown can prove the temp dir/mount/image belong to THIS run before deleting anything.
RUN_TOKEN="$(uuidgen 2>/dev/null || printf 'tok-%s-%s-%s' "$$" "$RANDOM" "$RANDOM")"
TMP=""
STATE=""

# The cleanup trap is installed BEFORE the first owned resource (tempdir, mount dir, marker,
# state, image, attached device) is created. Every partial stage is handled:
#   - state file exists            -> teardown.sh detaches/removes only what it records
#   - temp dir only (no state yet) -> removed iff the marker holds this run's token
#   - nothing created              -> no-op
# On success the trap is disarmed and CI's always() teardown step handles cleanup.
on_exit() {
  local rc=$?
  trap - EXIT
  if [[ $rc -ne 0 ]]; then
    echo "setup failed (rc=$rc); tearing down what this run created" >&2
    if [[ -n "$STATE" && -f "$STATE" ]]; then
      "$HERE/teardown.sh" "$STATE" || true
    elif [[ -n "$TMP" && -d "$TMP" && ! -L "$TMP" \
            && "$(cat "$TMP/.pulse-apfs-fixture" 2>/dev/null || true)" == "$RUN_TOKEN" ]]; then
      # Only the bare temp dir can exist before the state file is written; the marker token
      # proves this run created it, so removing it cannot touch anything unowned.
      rm -rf "$TMP"
    fi
  fi
  exit $rc
}
trap on_exit EXIT
trap 'exit 130' INT TERM

TMP="$(mktemp -d "$BASE/pulse-apfs-fixture.XXXXXX")"
TMP="$(cd "$TMP" && pwd -P)"
STATE="$TMP/state"
MNT="$TMP/mnt"
IMG="$TMP/fixture.sparseimage"
HARNESS_TMP="$TMP"
# Marker proving this directory was created by this harness run, written before anything else so
# every later failure path can prove ownership; teardown refuses to rm without an exact match on
# this run's unique token (recorded in the state file as RUN_TOKEN).
printf '%s\n' "$RUN_TOKEN" >"$TMP/.pulse-apfs-fixture"
mkdir "$MNT"
{
  echo "TMPDIR=$TMP"
  echo "MOUNTPOINT=$MNT"
  echo "IMAGE=$IMG"
  echo "RUN_TOKEN=$RUN_TOKEN"
} >"$STATE"

bounded 120 hdiutil create -size 512m -fs APFS -type SPARSE -volname PulseFixture "$IMG" >/dev/null
[[ -f "$IMG" ]] || IMG="$IMG.sparseimage" # hdiutil appends the extension when absent
[[ -f "$IMG" ]]
sed -i '' "s|^IMAGE=.*|IMAGE=$IMG|" "$STATE"

ATTACH_PLIST="$TMP/attach.plist"
echo "ATTACH_PLIST=$ATTACH_PLIST" >>"$STATE"
# Image path is already in the state file, so teardown can find devices by image path even if this fails.
bounded 120 hdiutil attach -plist -nobrowse -mountpoint "$MNT" "$IMG" >"$ATTACH_PLIST"
# First system entity is the whole-image disk; the entity whose mount-point is our mountpoint is the volume.
IMAGE_DEVICE=""
DEVICE=""
while IFS=$'\t' read -r dev mnt; do
  [[ -n "$IMAGE_DEVICE" ]] || IMAGE_DEVICE="$dev"
  [[ "$mnt" == "$MNT" && -z "$DEVICE" ]] && DEVICE="$dev"
done < <(attach_entries "$ATTACH_PLIST")
[[ "$IMAGE_DEVICE" == /dev/disk* && "$DEVICE" == /dev/disk* ]] || {
  echo "could not determine devices from hdiutil attach plist" >&2
  exit 1
}
# Ownership: both devices must appear under this image path in `hdiutil info -plist`.
OWNED_NOW="$(owned_devices "$IMG")"
for d in "$IMAGE_DEVICE" "$DEVICE"; do
  printf '%s\n' "$OWNED_NOW" | grep -qx -- "$d" || { echo "device $d not owned by $IMG" >&2; exit 1; }
done
{
  echo "IMAGE_DEVICE=$IMAGE_DEVICE"
  echo "DEVICE=$DEVICE"
} >>"$STATE"

ROOT="$MNT/fixture"
mkdir "$ROOT"
BASELINE="$(used_bytes "$MNT")"

MIB=$((1024 * 1024))
# Sparse file: 256 MiB logical, 1 MiB of real data at the start.
mkdir "$ROOT/sparse"
if command -v mkfile >/dev/null 2>&1; then mkfile -n 256m "$ROOT/sparse/sparse.bin"; else truncate -s 256m "$ROOT/sparse/sparse.bin"; fi
dd if=/dev/urandom of="$ROOT/sparse/sparse.bin" bs=$MIB count=1 conv=notrunc 2>/dev/null

# Hard links: one 4 MiB inode, three names.
mkdir "$ROOT/hard"
dd if=/dev/urandom of="$ROOT/hard/a.bin" bs=$MIB count=4 2>/dev/null
ln "$ROOT/hard/a.bin" "$ROOT/hard/b.bin"
ln "$ROOT/hard/a.bin" "$ROOT/hard/c.bin"

# Clones: one 8 MiB original plus two APFS clones (cp -c).
mkdir "$ROOT/clone"
dd if=/dev/urandom of="$ROOT/clone/orig.bin" bs=$MIB count=8 2>/dev/null
cp -c "$ROOT/clone/orig.bin" "$ROOT/clone/copy1.bin"
cp -c "$ROOT/clone/orig.bin" "$ROOT/clone/copy2.bin"

# Nested folders: folder totals must equal the sum of their children's attributed bytes.
mkdir -p "$ROOT/nested/inner/deep"
dd if=/dev/urandom of="$ROOT/nested/top.bin" bs=$MIB count=1 2>/dev/null
dd if=/dev/urandom of="$ROOT/nested/inner/mid.bin" bs=$MIB count=1 2>/dev/null
dd if=/dev/urandom of="$ROOT/nested/inner/deep/leaf.bin" bs=$MIB count=1 2>/dev/null

# Snapshot-retained allocation is intentionally SKIPPED: this harness does not configure a
# volume-confined snapshot mechanism. `diskutil apfs` exposes listSnapshots/deleteSnapshot but no
# create verb, and `tmutil localsnapshot` is documented to snapshot all Time Machine volumes with
# no documented mount-point operand — it is not confined to the fixture volume and could create
# snapshots on real user volumes, so it is not used here. The file below stays: it is a normal
# fixture entry; the test records an explicit SKIP for the snapshot-retention assertions.
dd if=/dev/urandom of="$ROOT/clone/snapshot_retained.bin" bs=$MIB count=4 2>/dev/null
sync
SNAPSHOT_UUID=""
SNAPSHOT_NAME=""
SNAPSHOT_KIND="unsupported"
SNAPSHOT_STATUS="UNSUPPORTED: snapshot creation is unconfigured in this harness; no volume-confined mechanism is wired up"
echo "SNAPSHOT: $SNAPSHOT_STATUS"

# Inaccessible metadata: unreadable directory with content inside.
mkdir "$ROOT/locked"
echo hidden >"$ROOT/locked/inside.txt"
chmod 000 "$ROOT/locked"

sync
{
  echo "ROOT=$ROOT"
  echo "BASELINE_USED_BYTES=$BASELINE"
  echo "SNAPSHOT=$SNAPSHOT_STATUS"
  echo "SNAPSHOT_KIND=$SNAPSHOT_KIND"
  echo "SNAPSHOT_UUID=$SNAPSHOT_UUID"
  echo "SNAPSHOT_NAME=$SNAPSHOT_NAME"
} >>"$STATE"

if [[ -n "${GITHUB_ENV:-}" ]]; then
  {
    echo "PULSE_APFS_FIXTURE_STATE=$STATE"
    echo "PULSE_APFS_FIXTURE_ROOT=$ROOT"
    echo "PULSE_APFS_FIXTURE_BASELINE_USED=$BASELINE"
    echo "PULSE_APFS_FIXTURE_SNAPSHOT=$SNAPSHOT_KIND"
  } >>"$GITHUB_ENV"
fi
echo "PULSE_APFS_FIXTURE_STATE=$STATE"
echo "PULSE_APFS_FIXTURE_ROOT=$ROOT"
echo "PULSE_APFS_FIXTURE_BASELINE_USED=$BASELINE"
echo "PULSE_APFS_FIXTURE_SNAPSHOT=$SNAPSHOT_KIND"
trap - EXIT
