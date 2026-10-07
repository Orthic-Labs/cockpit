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
  # mac/Sources (salvage services) is reference-only until reviewed; see mac/README.md.
  # Cockpit hub (Tauri): type-check and bundle the page, then compile the
  # Rust backend (its own workspace, pinned to the RightKit toolchain).
  pnpm --dir hub install --no-frozen-lockfile --ignore-scripts
  pnpm --dir hub exec tsc --noEmit
  pnpm --dir hub run build
  (cd hub/src-tauri && cargo check)
  # Headless dogfood of the hub: debug-only build with the WebDriver plugin
  # (qa-native, compile-time barred from release), driven by right-qa over a
  # tiny HOME so Storage scans a fixture. Evidence lands in $RUNNER_TEMP/cockpit-hub-qa
  # (screenshots/ + evidence.json); the generated ci workflow has no artifact upload.
  if [[ -z "${COCKPIT_SKIP_HUB_QA:-}" ]]; then
    pnpm --dir hub run qa:build
    qa_home="$(mktemp -d "$RUNNER_TEMP/cockpit-hub-qa-home.XXXXXX")"
    mkdir -p "$qa_home/alpha-folder"
    head -c 65536 /dev/zero > "$qa_home/alpha-folder/fixture.bin"
    qa_out="$RUNNER_TEMP/cockpit-hub-qa"
    rm -rf "$qa_out"; mkdir -p "$qa_out/screenshots"
    # Diagnostic probe: does the embedded WebDriver answer at all?
    HOME="$qa_home" RIGHTKIT_QA_NATIVE=1 TAURI_WEBDRIVER_PORT=4445 hub/src-tauri/target/debug/cockpit-hub > "$qa_out/probe.log" 2>&1 &
    probe_pid=$!
    sleep 10
    echo "probe alive: $(kill -0 $probe_pid 2>&1 && echo yes || echo no)"
    curl -sS -m 5 http://127.0.0.1:4445/status || true; echo
    curl -sS -m 5 -X POST -H 'content-type: application/json' -d '{"capabilities":{"alwaysMatch":{"browserName":"tauri"}}}' http://127.0.0.1:4445/session | head -c 600 || true; echo
    lsof -nP -iTCP -sTCP:LISTEN 2>/dev/null | grep -i cockpit || true
    tail -n 30 "$qa_out/probe.log" || true
    kill $probe_pid 2>/dev/null || true; sleep 1
    qa_rc=0
    HOME="$qa_home" COCKPIT_QA_SHOTS="$qa_out/screenshots" COCKPIT_QA_FIXTURE_NAME=alpha-folder \
      node "$(cd hub && node -p "fs.realpathSync('node_modules/@rightkit/qa/dist/cli.js')")" native --config hub/right-qa.config.mjs || qa_rc=$?
    find hub/.cache/rightkit-qa -name evidence.json -exec cp {} "$qa_out/" \; 2>/dev/null || true
    [[ -f "$qa_out/evidence.json" ]] && cat "$qa_out/evidence.json"
    echo "Hub QA evidence in $qa_out:"; ls -l "$qa_out" "$qa_out/screenshots" || true
    rm -rf "$qa_home"
    [[ $qa_rc -eq 0 ]] || { echo "Hub native QA failed ($qa_rc)" >&2; exit "$qa_rc"; }
    # A silent no-op must not pass: demand the receipt and one screenshot per section.
    [[ -f "$qa_out/evidence.json" ]] && grep -q '"passed"' "$qa_out/evidence.json" || { echo "Hub native QA left no passing evidence" >&2; exit 1; }
    [[ "$(find "$qa_out/screenshots" -name '*.png' | wc -l | tr -d ' ')" -ge 8 ]] || { echo "Hub native QA saved fewer than 8 screenshots" >&2; exit 1; }
  else
    echo "Hub QA skipped (COCKPIT_SKIP_HUB_QA set: signed-build gate)"
  fi
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
