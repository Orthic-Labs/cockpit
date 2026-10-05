#!/usr/bin/env bash
set -euo pipefail
if [[ "${GITHUB_ACTIONS:-}" != "true" ]]; then
  echo "Cockpit compilation & tests run in GitHub Actions. Local work is static-only." >&2
  exit 2
fi
emit_generated() {
  if [[ -n "$(git diff -- '*.rs')" ]]; then
    echo 'COCKPIT_FORMAT_PATCH_BEGIN'
    git diff -- '*.rs'
    echo 'COCKPIT_FORMAT_PATCH_END'
  fi
  if [[ -f Cargo.lock ]] && ! git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
    echo 'COCKPIT_CARGO_LOCK_BEGIN'
    cat Cargo.lock
    echo 'COCKPIT_CARGO_LOCK_END'
  fi
  if [[ -f windows/Cargo.lock ]] && ! git ls-files --error-unmatch windows/Cargo.lock >/dev/null 2>&1; then
    echo 'COCKPIT_WINDOWS_LOCK_BEGIN'
    cat windows/Cargo.lock
    echo 'COCKPIT_WINDOWS_LOCK_END'
  fi
}
apfs_teardown() {
  if [[ "${COCKPIT_APFS_FIXTURE:-}" == "1" && -n "${COCKPIT_APFS_FIXTURE_STATE:-}" ]]; then
    scripts/fixtures/apfs/teardown.sh "$COCKPIT_APFS_FIXTURE_STATE"
  fi
}
gate_exit() {
  local result=$?
  apfs_teardown || { [[ $result -ne 0 ]] || result=1; }
  emit_generated
  exit "$result"
}
trap gate_exit EXIT
node --test scripts/upstream-report.test.mjs scripts/probes/footprint-report.test.mjs
cargo fmt --all
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run --locked --quiet --bin cockpit -- status --json
if [[ "$RUNNER_OS" == "Windows" ]]; then
  cargo fmt --manifest-path windows/Cargo.toml
  cargo test --locked --manifest-path windows/Cargo.toml
  cargo clippy --locked --manifest-path windows/Cargo.toml --all-targets -- -D warnings
fi
if [[ "$RUNNER_OS" == "macOS" ]]; then
  export COCKPIT_APFS_FIXTURE=1
  apfs_out="$(scripts/fixtures/apfs/setup.sh)"
  echo "$apfs_out"
  while IFS= read -r line; do
    [[ "$line" =~ ^(COCKPIT_APFS_FIXTURE_[A-Z_]+)=(.*)$ ]] && export "${BASH_REMATCH[1]}=${BASH_REMATCH[2]}"
  done <<< "$apfs_out"
  cargo test --locked -p cockpit-core --test apfs_fixture -- --nocapture
  apfs_teardown
  unset COCKPIT_APFS_FIXTURE_STATE
  swift build --package-path mac -c release
  swift test --package-path mac
fi
