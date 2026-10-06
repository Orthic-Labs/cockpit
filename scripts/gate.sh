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
cargo test --locked --workspace --no-fail-fast
cargo clippy --locked --workspace --all-targets --keep-going -- -D warnings
cargo run --locked --quiet --bin cockpit -- status --json
if [[ "$RUNNER_OS" == "Windows" ]]; then
  cargo fmt --manifest-path windows/Cargo.toml
  cargo test --locked --manifest-path windows/Cargo.toml
  cargo clippy --locked --manifest-path windows/Cargo.toml --all-targets --keep-going -- -D warnings
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
  COCKPIT_TEST_HELPER="$(cargo build --locked --bin cockpit --message-format=json | python3 -c 'import json,sys; paths=[r["executable"] for line in sys.stdin if (r:=json.loads(line)).get("reason")=="compiler-artifact" and r.get("target",{}).get("name")=="cockpit" and r.get("executable")]; assert paths, "Missing cockpit compiler artifact"; print(paths[-1])')"
  export COCKPIT_TEST_HELPER
  swift build --package-path mac -c release
  swift test --package-path mac
  # Cockpit notch: Codenotch fork, built unsigned (release signing is RightKit's).
  xcodebuild -version
  command -v xcodegen >/dev/null || brew install xcodegen
  xcodegen generate --spec mac/Notch/project.yml --project mac/Notch
  notch_log="$RUNNER_TEMP/cockpit-notch-build.log"
  if ! xcodebuild -project mac/Notch/Cockpit.xcodeproj -scheme Cockpit -configuration Release \
    -destination 'generic/platform=macOS' -derivedDataPath "$RUNNER_TEMP/cockpit-notch" \
    CODE_SIGNING_ALLOWED=NO build > "$notch_log" 2>&1; then
    grep -E "(error|warning: unreachable):" "$notch_log" | sort -u | head -n 150 || true
    tail -n 40 "$notch_log"
    exit 1
  fi
  test -x "$RUNNER_TEMP/cockpit-notch/Build/Products/Release/Cockpit.app/Contents/MacOS/Cockpit"
fi
