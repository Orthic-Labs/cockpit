#!/usr/bin/env bash
# CI-only: create a fresh disposable APFS sparse image, mount it inside a new temp dir and
# populate it. Never touches existing volumes. Static-only locally: do NOT run on a dev machine.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

if [[ "${GITHUB_ACTIONS:-}" != "true" || "${COCKPIT_APFS_FIXTURE:-}" != "1" ]]; then
  echo "REFUSED: requires GITHUB_ACTIONS=true and COCKPIT_APFS_FIXTURE=1" >&2
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
TMP="$(mktemp -d "$BASE/cockpit-apfs-fixture.XXXXXX")"
TMP="$(cd "$TMP" && pwd -P)"
STATE="$TMP/state"
MNT="$TMP/mnt"
IMG="$TMP/fixture.sparseimage"
mkdir "$MNT"
{
  echo "TMPDIR=$TMP"
  echo "MOUNTPOINT=$MNT"
  echo "IMAGE=$IMG"
} >"$STATE"

# Reliable teardown on any failure after this point; on success the CI `always()` step tears down.
on_exit() {
  local rc=$?
  if [[ $rc -ne 0 ]]; then
    echo "setup failed (rc=$rc); tearing down what this run created" >&2
    "$HERE/teardown.sh" "$STATE" || true
  fi
  exit $rc
}
trap on_exit EXIT
trap 'exit 130' INT TERM

bounded 120 hdiutil create -size 512m -fs APFS -type SPARSE -volname CockpitFixture "$IMG" >/dev/null
[[ -f "$IMG" ]] || IMG="$IMG.sparseimage" # hdiutil appends the extension when absent
[[ -f "$IMG" ]]
sed -i '' "s|^IMAGE=.*|IMAGE=$IMG|" "$STATE"

ATTACH_OUT="$(bounded 120 hdiutil attach -nobrowse -mountpoint "$MNT" "$IMG")"
# First field of the first line is the whole-image disk; the line naming our mountpoint is the volume.
IMAGE_DEVICE="$(printf '%s\n' "$ATTACH_OUT" | awk 'NR==1 {print $1}')"
DEVICE="$(printf '%s\n' "$ATTACH_OUT" | awk -v m="$MNT" 'index($0, m) {print $1; exit}')"
[[ "$IMAGE_DEVICE" == /dev/disk* && "$DEVICE" == /dev/disk* ]] || {
  echo "could not determine devices from hdiutil attach" >&2
  exit 1
}
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

# Optional snapshot-retained allocation: only against the harness volume's own mountpoint.
# tmutil localsnapshot is deliberately NOT used: it snapshots user volumes.
dd if=/dev/urandom of="$ROOT/clone/snapshot_retained.bin" bs=$MIB count=2 2>/dev/null
sync
SNAPSHOT_STATUS="SKIP: snapshot not permitted on harness volume"
if bounded 60 diskutil apfs snapshot "$MNT" >/dev/null 2>&1; then
  dd if=/dev/urandom of="$ROOT/clone/snapshot_retained.bin" bs=$MIB count=2 conv=notrunc 2>/dev/null
  SNAPSHOT_STATUS="CREATED on harness volume"
fi
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
} >>"$STATE"

if [[ -n "${GITHUB_ENV:-}" ]]; then
  {
    echo "COCKPIT_APFS_FIXTURE_STATE=$STATE"
    echo "COCKPIT_APFS_FIXTURE_ROOT=$ROOT"
    echo "COCKPIT_APFS_FIXTURE_BASELINE_USED=$BASELINE"
  } >>"$GITHUB_ENV"
fi
echo "COCKPIT_APFS_FIXTURE_STATE=$STATE"
echo "COCKPIT_APFS_FIXTURE_ROOT=$ROOT"
echo "COCKPIT_APFS_FIXTURE_BASELINE_USED=$BASELINE"
trap - EXIT
