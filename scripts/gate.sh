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
trap emit_generated EXIT
cargo fmt --all
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo run --locked --quiet --bin cockpit -- status --json
if [[ "$RUNNER_OS" == "Windows" ]]; then
  cargo fmt --manifest-path windows/Cargo.toml
  cargo test --manifest-path windows/Cargo.toml
  cargo clippy --manifest-path windows/Cargo.toml --all-targets -- -D warnings
fi
if [[ "$RUNNER_OS" == "macOS" ]]; then
  swift build --package-path mac -c release
  swift test --package-path mac
fi
