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
