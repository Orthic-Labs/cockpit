#!/usr/bin/env bash
set -euo pipefail
# Accept legacy user/CI overrides without replacing explicit Pulse values.
for pulse_legacy_key in ${!COCKPIT_@}; do
  pulse_current_key="PULSE_${pulse_legacy_key#COCKPIT_}"
  if [[ -z "${!pulse_current_key+x}" ]]; then
    export "$pulse_current_key=${!pulse_legacy_key}"
  fi
done
if [[ "${GITHUB_ACTIONS:-}" != "true" ]]; then
  echo "Pulse compilation & tests run in GitHub Actions. Local work is static-only." >&2
  exit 2
fi
emit_generated() {
  if [[ -n "$(git diff -- '*.rs')" ]]; then
    echo 'PULSE_FORMAT_PATCH_BEGIN'
    git diff -- '*.rs'
    echo 'PULSE_FORMAT_PATCH_END'
  fi
  if [[ -f Cargo.lock ]] && ! git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
    echo 'PULSE_CARGO_LOCK_BEGIN'
    cat Cargo.lock
    echo 'PULSE_CARGO_LOCK_END'
  fi
  if [[ -f windows/Cargo.lock ]] && ! git ls-files --error-unmatch windows/Cargo.lock >/dev/null 2>&1; then
    echo 'PULSE_WINDOWS_LOCK_BEGIN'
    cat windows/Cargo.lock
    echo 'PULSE_WINDOWS_LOCK_END'
  fi
}
apfs_teardown() {
  if [[ "${PULSE_APFS_FIXTURE:-}" == "1" && -n "${PULSE_APFS_FIXTURE_STATE:-}" ]]; then
    scripts/fixtures/apfs/teardown.sh "$PULSE_APFS_FIXTURE_STATE"
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
cargo run --locked --quiet --bin pulse -- status --json
if [[ "$RUNNER_OS" == "Windows" ]]; then
  cargo fmt --manifest-path windows/Cargo.toml
  cargo test --locked --manifest-path windows/Cargo.toml
  cargo clippy --locked --manifest-path windows/Cargo.toml --all-targets --keep-going -- -D warnings
fi
if [[ "$RUNNER_OS" == "macOS" ]]; then
  export PULSE_APFS_FIXTURE=1
  apfs_out="$(scripts/fixtures/apfs/setup.sh)"
  echo "$apfs_out"
  while IFS= read -r line; do
    [[ "$line" =~ ^(PULSE_APFS_FIXTURE_[A-Z_]+)=(.*)$ ]] && export "${BASH_REMATCH[1]}=${BASH_REMATCH[2]}"
  done <<< "$apfs_out"
  cargo test --locked -p pulse-core --test apfs_fixture -- --nocapture
  apfs_teardown
  unset PULSE_APFS_FIXTURE_STATE
  PULSE_TEST_HELPER="$(cargo build --locked --bin pulse --message-format=json | python3 -c 'import json,sys; paths=[r["executable"] for line in sys.stdin if (r:=json.loads(line)).get("reason")=="compiler-artifact" and r.get("target",{}).get("name")=="pulse" and r.get("executable")]; assert paths, "Missing pulse compiler artifact"; print(paths[-1])')"
  export PULSE_TEST_HELPER
  # mac/Sources (salvage services) is reference-only until reviewed; see mac/README.md.
  # Pulse hub (Tauri): type-check and bundle the page, then compile the
  # Rust backend (its own workspace, pinned to the RightKit toolchain).
  pnpm --dir hub install --frozen-lockfile
  pnpm --dir hub exec tsc --noEmit
  pnpm --dir hub run build
  (cd hub/src-tauri && cargo check)
  # Pulse notch: Codenotch fork, built unsigned (release signing is RightKit's).
  xcodebuild -version
  command -v xcodegen >/dev/null || brew install xcodegen
  xcodegen generate --spec mac/Notch/project.yml --project mac/Notch
  notch_log="$RUNNER_TEMP/pulse-notch-build.log"
  if ! xcodebuild -project mac/Notch/Pulse.xcodeproj -scheme Pulse -configuration Release \
    -destination 'generic/platform=macOS' -derivedDataPath "$RUNNER_TEMP/pulse-notch" \
    CODE_SIGNING_ALLOWED=NO build > "$notch_log" 2>&1; then
    grep -E "(error|warning: unreachable):" "$notch_log" | sort -u | head -n 150 || true
    tail -n 40 "$notch_log"
    exit 1
  fi
  test -x "$RUNNER_TEMP/pulse-notch/Build/Products/Release/Pulse.app/Contents/MacOS/Pulse"
  # Headless dogfood of the hub on rightkit-qa: the debug-only qa-native build
  # (compile-time barred from release) is launched hidden and driven through its
  # in-app control server (rightkit-control). The test makes its own fixture HOME so
  # Storage scans a tiny folder. UI tests must run from the user's login session
  # (macOS `open` needs a GUI session), the same reason tools/rightkit/scripts/run-ui-tests.sh
  # builds through the broker and then runs the binary itself. That script needs the
  # `rightkit` CLI, which this runner does not have, so we do the equivalent directly:
  # plain `cargo build` of the qa-native bin, then `cargo test` of hub/qa-e2e here (not in a wrapper).
  # hub/qa-e2e is its own cargo workspace: rightkit-qa's exact pins clash with pulse-core's.
  # Evidence lands in $RUNNER_TEMP/pulse-hub-qa (screenshots/ + evidence/); the
  # generated ci workflow has no artifact upload.
  if [[ -z "${PULSE_SKIP_HUB_QA:-}" ]]; then
    qa_out="$RUNNER_TEMP/pulse-hub-qa"
    rm -rf "$qa_out"; mkdir -p "$qa_out/screenshots" "$qa_out/evidence"
    qa_rc=0
    (cd hub/src-tauri && cargo build --features qa-native,custom-protocol) || qa_rc=$?
    if [[ $qa_rc -eq 0 ]]; then
      (cd hub/qa-e2e && PULSE_QA_SHOTS="$qa_out/screenshots" RIGHTKIT_QA_EVIDENCE="$qa_out/evidence" \
        cargo test --test ui -- --nocapture) || qa_rc=$?
    fi
    echo "Hub QA evidence in $qa_out:"; ls -lR "$qa_out" || true
    [[ $qa_rc -eq 0 ]] || { echo "Hub native QA failed ($qa_rc)" >&2; exit "$qa_rc"; }
    # A silent no-op (skipped scenario) must not pass: demand the receipt and one screenshot per section.
    grep -rq '"passed"' "$qa_out/evidence" --include=evidence.json || { echo "Hub native QA left no passing evidence" >&2; exit 1; }
    [[ "$(find "$qa_out/screenshots" -name '*.png' | wc -l | tr -d ' ')" -ge 8 ]] || { echo "Hub native QA saved fewer than 8 screenshots" >&2; exit 1; }
  else
    echo "Hub QA skipped (PULSE_SKIP_HUB_QA set: signed-build gate)"
  fi
fi
