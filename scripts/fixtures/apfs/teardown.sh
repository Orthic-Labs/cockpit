#!/usr/bin/env bash
# CI-only: detach and remove exactly what the state file names, after verification.
# Usage: teardown.sh [STATE_FILE]   (defaults to $COCKPIT_APFS_FIXTURE_STATE)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

if [[ "${GITHUB_ACTIONS:-}" != "true" || "${COCKPIT_APFS_FIXTURE:-}" != "1" ]]; then
  echo "REFUSED: requires GITHUB_ACTIONS=true and COCKPIT_APFS_FIXTURE=1" >&2
  exit 2
fi
[[ "$(uname -s)" == "Darwin" ]] || { echo "REFUSED: macOS only" >&2; exit 2; }

STATE="${1:-${COCKPIT_APFS_FIXTURE_STATE:-}}"
if [[ -z "$STATE" || ! -f "$STATE" ]]; then
  echo "no state file; nothing to tear down"
  exit 0
fi
STATE="$(cd "$(dirname "$STATE")" && pwd -P)/$(basename "$STATE")"

TMP="$(state_get "$STATE" TMPDIR)"
MNT="$(state_get "$STATE" MOUNTPOINT)"
IMG="$(state_get "$STATE" IMAGE)"
DEVICE="$(state_get "$STATE" DEVICE)"
IMAGE_DEVICE="$(state_get "$STATE" IMAGE_DEVICE)"

# Guard: the temp dir must be a harness dir that holds this very state file.
case "$(basename "$TMP")" in cockpit-apfs-fixture.??????) ;; *) echo "REFUSED: unexpected temp dir name" >&2; exit 3 ;; esac
[[ "$STATE" == "$TMP/state" && -d "$TMP" && ! -L "$TMP" ]] || { echo "REFUSED: state file not inside temp dir" >&2; exit 3; }
[[ "$MNT" == "$TMP/mnt" && "$IMG" == "$TMP/"* ]] || { echo "REFUSED: paths not under harness temp dir" >&2; exit 3; }

if [[ -n "$IMAGE_DEVICE" ]]; then
  [[ "$IMAGE_DEVICE" == /dev/disk* && "$DEVICE" == /dev/disk* ]] || { echo "REFUSED: bad device" >&2; exit 3; }
  # Verify the volume device is mounted exactly at our mountpoint (under the harness temp dir).
  ACTUAL="$(bounded 30 diskutil info "$DEVICE" 2>/dev/null | sed -n 's/^ *Mount Point: *//p' | head -n 1 || true)"
  if [[ -n "$ACTUAL" ]]; then
    [[ "$ACTUAL" == "$MNT" ]] || { echo "REFUSED: $DEVICE mounted at an unexpected location" >&2; exit 3; }
  fi
  # Verify the whole-image device belongs to our image file.
  OWNED="$(bounded 30 hdiutil info | awk -v img="$IMG" -v dev="$IMAGE_DEVICE" '
    /^=+$/ {cur=0}
    /^image-path/ {cur = (index($0, img) > 0)}
    cur && $1 == dev {found=1}
    END {print found ? "yes" : "no"}')"
  if [[ "$OWNED" == "yes" ]]; then
    bounded 120 hdiutil detach "$IMAGE_DEVICE" >/dev/null 2>&1 \
      || bounded 120 hdiutil detach -force "$IMAGE_DEVICE" >/dev/null
  else
    echo "image device not attached (already detached); skipping detach"
  fi
fi

# Only remove the temp dir once nothing is mounted at the mountpoint.
if mount | grep -F " on $MNT " >/dev/null 2>&1; then
  echo "mountpoint still mounted; refusing to remove" >&2
  exit 4
fi
rm -rf "$TMP"
echo "teardown complete"
