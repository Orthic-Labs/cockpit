#!/usr/bin/env bash
set -euo pipefail
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
  if [[ -n "$(git diff -- Cargo.lock)" ]]; then
    echo 'PULSE_CARGO_LOCK_PATCH_BEGIN'
    git diff -- Cargo.lock
    echo 'PULSE_CARGO_LOCK_PATCH_END'
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
# Dev artifact (RightKit devArtifact lane): on push to main, or PULSE_DEV_ARTIFACT=1.
# PULSE_DEV_ARTIFACT=0 forces it off (the release candidate build runs this gate too).
dev_artifact() {
  [[ "${PULSE_DEV_ARTIFACT:-}" == "1" ]] && return 0
  [[ -z "${PULSE_DEV_ARTIFACT:-}" && "${GITHUB_EVENT_NAME:-}" == "push" && "${GITHUB_REF:-}" == "refs/heads/main" ]]
}
node --test scripts/upstream-report.test.mjs scripts/probes/footprint-report.test.mjs
# A path in the form a native (non-MSYS) process wants: C:\... under the Windows runner's Git Bash, unchanged elsewhere.
native_path() {
  if [[ "${RUNNER_OS:-}" == "Windows" ]] && command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s\n' "$1"
  fi
}
# Headless dogfood of the hub on rightkit-qa, on the macOS and the Windows leg: the debug-only
# qa-native build (compile-time barred from release) is launched hidden and driven through its
# in-app control server (rightkit-control). The test makes its own fixture HOME so Storage scans a
# tiny folder and writes the notch's published state where the hub reads it (~/Library/Application
# Support/Pulse on the Mac, %LOCALAPPDATA%\Pulse on Windows, which rightkit-qa points at its own
# data dir). macOS UI tests must run from the user's login session (`open` needs a GUI session),
# the same reason tools/rightkit/scripts/run-ui-tests.sh builds through the broker and then runs the
# binary itself. That script needs the `rightkit` CLI, which this runner does not have, so we do the
# equivalent directly: plain `cargo build` of the qa-native bin, then `cargo test` of hub/qa-e2e
# here (not in a wrapper). hub/qa-e2e is its own cargo workspace: rightkit-qa's exact pins clash
# with pulse-core's. On Windows the same test then starts the real notch (windows/ crate, debug
# build, PULSE_QA_NOTCH_BIN) beside a second hub to prove the hub's Edge control reaches the notch.
# Evidence lands in $RUNNER_TEMP/pulse-hub-qa (screenshots/ + managed/.../evidence); .rightgit.json's
# qaEvidencePath uploads that folder as qa-evidence-<os>-<attempt> (ci.yml has the artifact step).
run_hub_qa() {
  if [[ -n "${PULSE_SKIP_HUB_QA:-}" ]]; then
    echo "Hub QA skipped (PULSE_SKIP_HUB_QA set: signed-build gate)"
    return 0
  fi
  local qa_out="$RUNNER_TEMP/pulse-hub-qa" qa_rc=0 notch_bin=""
  rm -rf "$qa_out/screenshots" "$qa_out/evidence" "$qa_out/managed"; mkdir -p "$qa_out/screenshots" "$qa_out/evidence"
  (cd hub/src-tauri && cargo build --features qa-native,custom-protocol) || qa_rc=$?
  if [[ $qa_rc -eq 0 && "${RUNNER_OS:-}" == "Windows" ]]; then
    (cargo build --locked --manifest-path windows/Cargo.toml) || qa_rc=$?
    notch_bin="$PWD/windows/target/debug/pulse-windows-prototype.exe"
    if [[ $qa_rc -eq 0 && ! -f "$notch_bin" ]]; then
      echo "Notch binary missing after build: $notch_bin" >&2
      qa_rc=1
    fi
  fi
  if [[ $qa_rc -eq 0 ]]; then
    # rightkit-qa >= 0.2.10 keeps managed runs under RIGHTKIT_MANAGED_ROOT
    # (default is a workstation path the runner cannot create).
    (cd hub/qa-e2e && RIGHTKIT_MANAGED_ROOT="$(native_path "$qa_out/managed")" \
      PULSE_QA_SHOTS="$(native_path "$qa_out/screenshots")" RIGHTKIT_QA_EVIDENCE="$(native_path "$qa_out/evidence")" \
      PULSE_QA_NOTCH_BIN="${notch_bin:+$(native_path "$notch_bin")}" \
      cargo test --test ui -- --nocapture) || qa_rc=$?
  fi
  echo "Hub QA evidence in $qa_out:"; ls -lR "$qa_out" || true
  [[ $qa_rc -eq 0 ]] || { echo "Hub native QA failed ($qa_rc)" >&2; exit "$qa_rc"; }
  # A silent no-op (skipped scenario) must not pass: demand the receipt and one screenshot per section.
  # 0.2.10+ writes evidence inside the managed run (managed/runs/<app>/<id>/evidence).
  grep -rq '"passed"' "$qa_out" --include=evidence.json || { echo "Hub native QA left no passing evidence" >&2; exit 1; }
  [[ "$(find "$qa_out/screenshots" -name '*.png' | wc -l | tr -d ' ')" -ge 8 ]] || { echo "Hub native QA saved fewer than 8 screenshots" >&2; exit 1; }
  if [[ "${RUNNER_OS:-}" == "Windows" ]]; then
    # The notch step skips (and still writes a receipt) without PULSE_QA_NOTCH_BIN; its last screenshot proves it ran.
    [[ -s "$qa_out/screenshots/12-notch-edge-after.png" ]] || { echo "Hub native QA did not run the notch edge step" >&2; exit 1; }
  fi
}
cargo fmt --all
# A manifest change without a matching lock: resolve it here (CI is the only
# place Pulse may resolve), print the lock patch for a verbatim commit, fail.
if ! metadata_err="$(cargo metadata --locked --format-version 1 2>&1 >/dev/null)"; then
  echo "cargo metadata --locked failed:" >&2
  printf '%s\n' "$metadata_err" >&2
  cargo update --workspace
  if git diff --quiet -- Cargo.lock; then
    echo "Cargo.lock is unchanged by cargo update; the failure above is not a stale lock." >&2
  else
    echo "Cargo.lock is out of date; commit this patch verbatim:" >&2
    echo PULSE_CARGO_LOCK_PATCH_BEGIN; git diff -- Cargo.lock; echo PULSE_CARGO_LOCK_PATCH_END
  fi
  exit 1
fi
cargo test --locked --workspace --no-fail-fast
cargo clippy --locked --workspace --all-targets --keep-going -- -D warnings
cargo run --locked --quiet --bin pulse -- status --json
if [[ "$RUNNER_OS" == "Windows" ]]; then
  cargo fmt --manifest-path windows/Cargo.toml
  cargo test --locked --manifest-path windows/Cargo.toml
  cargo clippy --locked --manifest-path windows/Cargo.toml --all-targets --keep-going -- -D warnings
  # Every notch view the Windows notch has an equivalent for, drawn off-screen by the notch
  # binary itself (software rasteriser, hand-written PNG; no window, hook or desktop access)
  # from qa/notch-views.json for the Mac-vs-Windows side-by-side. The rest get placeholder
  # PNGs and are listed in windows-gaps.txt. Lands beside the Mac set; a missing id fails.
  win_views="$RUNNER_TEMP/pulse-hub-qa/views/windows"
  rm -rf "$win_views"; mkdir -p "$win_views"
  cargo run --locked --manifest-path windows/Cargo.toml -- --render-views "$win_views"
  win_expected=0
  win_missing=""
  while IFS= read -r view_id; do
    win_expected=$((win_expected + 1))
    [[ -s "$win_views/$view_id.png" ]] || win_missing="$win_missing $view_id"
  done < <(grep -oE '^    "id": "[^"]+"' qa/notch-views.json | sed -E 's/^    "id": "([^"]+)"$/\1/')
  echo "Windows view shots: $(find "$win_views" -name '*.png' | wc -l | tr -d ' ') PNGs for $win_expected view ids"
  if [[ $win_expected -eq 0 || -n "$win_missing" ]]; then
    echo "Missing Windows view shots:$win_missing" >&2
    exit 1
  fi
  # Pulse hub (Tauri): the real frontend is bundled here too (the qa-native custom-protocol
  # build embeds it, and tauri::generate_context! needs the dist folder); the page is
  # type-checked on the macOS leg. Then the Rust backend compile check. The hub is its own workspace.
  pnpm --dir hub install --frozen-lockfile
  pnpm --dir hub run build
  (cd hub/src-tauri && cargo check --all-targets)
  # Headless dogfood of the hub and the real notch on Windows (see run_hub_qa).
  run_hub_qa
  # Dev artifact: release builds with the real hub frontend, staged unsigned at
  # dist/dev/windows (Pulse.exe, pulse-hub.exe, Helpers/pulse.exe, ThirdParty); an incomplete payload fails.
  if dev_artifact; then
    rm -rf dist/dev/windows
    node scripts/release/windows-payload.mjs dev
  fi
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
  # Every notch view rendered off-screen (ImageRenderer, scale 2) from qa/notch-views.json,
  # for the Mac-vs-Windows side-by-side. Lands beside the hub QA evidence so the artifact
  # upload carries it; fewer PNGs than view ids fails the gate.
  views_out="$RUNNER_TEMP/pulse-hub-qa/views/mac"
  rm -rf "$views_out"; mkdir -p "$views_out"
  views_rc=0
  PULSE_RENDER_VIEWS=1 PULSE_VIEWS_JSON="$PWD/qa/notch-views.json" PULSE_VIEW_SHOTS="$views_out" \
    python3 -c 'import subprocess,sys; sys.exit(subprocess.run(sys.argv[1:], timeout=300).returncode)' \
    "$RUNNER_TEMP/pulse-notch/Build/Products/Release/Pulse.app/Contents/MacOS/Pulse" || views_rc=$?
  python3 - "$PWD/qa/notch-views.json" "$views_out" <<'PY' || views_rc=$?
import json, os, sys
ids = [v["id"] for v in json.load(open(sys.argv[1]))]
missing = [i for i in ids if not os.path.isfile(os.path.join(sys.argv[2], i + ".png")) or os.path.getsize(os.path.join(sys.argv[2], i + ".png")) < 500]
pngs = [f for f in os.listdir(sys.argv[2]) if f.endswith(".png")]
print(f"Mac view shots: {len(pngs)} PNGs for {len(ids)} view ids")
if missing or len(pngs) < len(ids):
    print("Missing or empty view shots: " + ", ".join(missing), file=sys.stderr)
    sys.exit(1)
PY
  [[ $views_rc -eq 0 ]] || { echo "Notch view rendering failed ($views_rc)" >&2; exit "$views_rc"; }
  # Headless dogfood of the hub on rightkit-qa (see run_hub_qa).
  run_hub_qa
  # Dev artifact: the complete unsigned Pulse.app at dist/dev/Pulse.app (the same bundle the release
  # candidate assembles, via mac-payload.mjs). Reuses the notch built above; builds only the release CLI
  # and the release hub (real frontend). RightKit's dev-mac-sign job signs and uploads it.
  if dev_artifact; then
    rm -rf dist/dev/Pulse.app
    cargo build --locked --release --bin pulse
    pnpm --dir hub exec tauri build --bundles app --no-sign
    node scripts/release/mac-payload.mjs dev
    test -x dist/dev/Pulse.app/Contents/MacOS/Pulse
  fi
fi
