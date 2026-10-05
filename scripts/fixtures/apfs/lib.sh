#!/usr/bin/env bash
# Shared helpers for the CI-only disposable APFS fixture harness. Sourced, not executed.

# bounded SECONDS cmd... : run with a hard time limit (coreutils timeout, else perl alarm).
bounded() {
  local secs="$1"
  shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$secs" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$secs" "$@"
  else
    perl -e 'alarm shift; exec @ARGV or die "exec failed: $!\n"' "$secs" "$@"
  fi
}

# state_get FILE KEY : print the value of KEY=VALUE (never sourced/evaluated).
state_get() {
  sed -n "s/^$2=//p" "$1" | tail -n 1
}

# used_bytes PATH : used bytes of the filesystem containing PATH (df -k, 1024-byte blocks).
used_bytes() {
  local kb
  kb="$(bounded 30 df -k -P "$1" | awk 'NR==2 {print $3}')"
  echo $((kb * 1024))
}

# one_line TEXT : collapse whitespace/newlines and bound length so values stay single-line state entries.
one_line() {
  printf '%s' "$1" | tr '\r\n\t' '   ' | tr -s ' ' | cut -c1-300
}

# pb_get FILE :key:path : print a plist value, empty (rc 1) when absent. Never evaluates the value.
pb_get() {
  /usr/libexec/PlistBuddy -c "Print $2" "$1" 2>/dev/null
}

# attach_entries ATTACH_PLIST : print "dev-entry<TAB>mount-point" per system entity of `hdiutil attach -plist`.
attach_entries() {
  local file="$1" i=0 dev mnt
  while dev="$(pb_get "$file" ":system-entities:$i:dev-entry")"; do
    mnt="$(pb_get "$file" ":system-entities:$i:mount-point" || true)"
    printf '%s\t%s\n' "$dev" "$mnt"
    i=$((i + 1))
  done
}

# owned_devices IMAGE_PATH : print every dev-entry of the image whose `hdiutil info -plist` image-path
# equals IMAGE_PATH exactly (whole image disk first). Empty when the image is not attached.
owned_devices() {
  local img="$1" info i=0 j p dev
  info="$(mktemp "${HARNESS_TMP:?}/cockpit-apfs-info.XXXXXX")"
  if ! bounded 30 hdiutil info -plist >"$info" 2>/dev/null; then
    rm -f "$info"
    return 1
  fi
  while p="$(pb_get "$info" ":images:$i:image-path")"; do
    if [[ "$p" == "$img" ]]; then
      j=0
      while dev="$(pb_get "$info" ":images:$i:system-entities:$j:dev-entry")"; do
        printf '%s\n' "$dev"
        j=$((j + 1))
      done
    fi
    i=$((i + 1))
  done
  rm -f "$info"
}

# snapshot_records DEVICE : print "uuid<TAB>name" per APFS snapshot of the volume device.
snapshot_records() {
  local dev="$1" out i=0 u n
  out="$(mktemp "${HARNESS_TMP:?}/cockpit-apfs-snap.XXXXXX")"
  if ! bounded 60 diskutil apfs listSnapshots -plist "$dev" >"$out" 2>/dev/null; then
    rm -f "$out"
    return 0
  fi
  while u="$(pb_get "$out" ":Snapshots:$i:SnapshotUUID")"; do
    n="$(pb_get "$out" ":Snapshots:$i:SnapshotName" || true)"
    printf '%s\t%s\n' "$u" "$n"
    i=$((i + 1))
  done
  rm -f "$out"
}
