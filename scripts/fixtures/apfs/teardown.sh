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
RUN_TOKEN="$(state_get "$STATE" RUN_TOKEN)"
DEVICE="$(state_get "$STATE" DEVICE)"
IMAGE_DEVICE="$(state_get "$STATE" IMAGE_DEVICE)"

SNAPSHOT_UUID="$(state_get "$STATE" SNAPSHOT_UUID)"
SNAPSHOT_NAME="$(state_get "$STATE" SNAPSHOT_NAME)"

# Guard: the temp dir must be a harness dir that holds this very state file and the creation marker.
case "$(basename "$TMP")" in cockpit-apfs-fixture.??????) ;; *) echo "REFUSED: unexpected temp dir name" >&2; exit 3 ;; esac
[[ "$TMP" == /* && "$TMP" != "/" && "$STATE" == "$TMP/state" && -d "$TMP" && ! -L "$TMP" ]] || { echo "REFUSED: state file not inside temp dir" >&2; exit 3; }
# Ownership token: the marker must contain exactly the unique run token recorded in the state file.
[[ -n "$RUN_TOKEN" ]] || { echo "REFUSED: state file records no run token" >&2; exit 3; }
[[ "$(cat "$TMP/.cockpit-apfs-fixture" 2>/dev/null || true)" == "$RUN_TOKEN" ]] || { echo "REFUSED: harness creation marker does not match this run's token" >&2; exit 3; }
[[ "$MNT" == "$TMP/mnt" && "$IMG" == "$TMP/"* && "$IMG" != *..* ]] || { echo "REFUSED: paths not under harness temp dir" >&2; exit 3; }
HARNESS_TMP="$TMP"

# Ownership is established by image path alone (works when setup failed before recording devices).
OWNED=""
if [[ -e "$IMG" || -n "$IMAGE_DEVICE" ]]; then
  OWNED="$(owned_devices "$IMG")" || { echo "REFUSED: cannot read hdiutil info" >&2; exit 3; }
fi

if [[ -n "$OWNED" ]]; then
  # Any recorded device must be one of the image's own entries.
  for d in "$IMAGE_DEVICE" "$DEVICE"; do
    [[ -z "$d" ]] || printf '%s\n' "$OWNED" | grep -qx -- "$d" || { echo "REFUSED: $d not owned by $IMG" >&2; exit 3; }
  done
  # Whatever is mounted at our mountpoint must be a device owned by this image — never
  # detach/unmount a foreign volume merely because it happens to sit on our path.
  MOUNTED_DEV="$(bounded 30 mount | sed -n "s|^\(/dev/[^ ]*\) on $MNT (.*|\1|p" | head -n 1)"
  if [[ -n "$MOUNTED_DEV" ]]; then
    printf '%s\n' "$OWNED" | grep -qx -- "$MOUNTED_DEV" \
      || { echo "REFUSED: $MNT is mounted from unowned device $MOUNTED_DEV" >&2; exit 3; }
  fi
  # Volume device, if known, must be mounted exactly at our mountpoint (or not mounted).
  if [[ "$DEVICE" == /dev/disk* ]]; then
    ACTUAL="$(bounded 30 diskutil info "$DEVICE" 2>/dev/null | sed -n 's/^ *Mount Point: *//p' | head -n 1 || true)"
    if [[ -n "$ACTUAL" && "$ACTUAL" != "$MNT" ]]; then
      echo "REFUSED: $DEVICE mounted at an unexpected location" >&2
      exit 3
    fi
    # Delete only the snapshot captured at creation, by exact UUID, on the harness volume.
    if [[ -n "$SNAPSHOT_UUID" && -n "$SNAPSHOT_NAME" ]]; then
      if snapshot_records "$DEVICE" | grep -Fxq "$(printf '%s\t%s' "$SNAPSHOT_UUID" "$SNAPSHOT_NAME")"; then
        bounded 60 diskutil apfs deleteSnapshot "$DEVICE" -uuid "$SNAPSHOT_UUID" >/dev/null 2>&1 \
          || echo "warning: snapshot $SNAPSHOT_UUID delete failed; detach destroys it with the image" >&2
      fi
    fi
  fi
  # Whole-image disk: recorded device when valid, else the first entry owned by this image path.
  TARGET="$IMAGE_DEVICE"
  [[ -n "$TARGET" ]] || TARGET="$(printf '%s\n' "$OWNED" | sed -n 1p)"
  [[ "$TARGET" == /dev/disk* ]] || { echo "REFUSED: no valid device to detach" >&2; exit 3; }
  # macOS 27 runners briefly hold a fresh volume busy (indexing, fseventsd);
  # retry with a short wait rather than failing the gate on the first try.
  detached=""
  for attempt in 1 2 3 4 5 6; do
    if bounded 120 hdiutil detach "$TARGET" >/dev/null 2>&1 \
      || bounded 120 hdiutil detach -force "$TARGET" >/dev/null 2>&1; then
      detached=1
      break
    fi
    echo "detach attempt $attempt: $TARGET busy; retrying" >&2
    sleep 5
  done
  [[ -n "$detached" ]] || { echo "detach failed: $TARGET still busy" >&2; exit 4; }
else
  echo "image not attached (never attached or already detached); skipping detach"
fi

# Only remove the harness temp dir once nothing is mounted at the mountpoint or attached from the image.
if mount | grep -F " on $MNT " >/dev/null 2>&1; then
  echo "mountpoint still mounted; refusing to remove" >&2
  exit 4
fi
if [[ -e "$IMG" && -n "$(owned_devices "$IMG" || echo unknown)" ]]; then
  echo "image still attached; refusing to remove" >&2
  exit 4
fi
rm -rf "$TMP"
echo "teardown complete"
