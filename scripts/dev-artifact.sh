#!/usr/bin/env bash
# Builds and stages the dev app for one platform, without the QA gate: the complete unsigned
# Pulse.app at dist/dev/Pulse.app on macOS, dist/dev/windows on Windows. RightKit's
# dev-artifact job runs this beside the gate job so the app is ready while the tests run.
set -euo pipefail
if [[ "${GITHUB_ACTIONS:-}" != "true" ]]; then
  echo "Pulse compilation runs in GitHub Actions. Local work is static-only." >&2
  exit 2
fi
pnpm --dir hub install --frozen-lockfile
if [[ "$RUNNER_OS" == "Windows" ]]; then
  pnpm --dir hub run build
  rm -rf dist/dev/windows
  node scripts/release/windows-payload.mjs dev
fi
if [[ "$RUNNER_OS" == "macOS" ]]; then
  # The release hub and CLI compile in the background while the notch builds.
  build_log="$RUNNER_TEMP/pulse-dev-build.log"
  (pnpm --dir hub exec tauri build --bundles app --no-sign &&
    cargo build --locked --release --bin pulse) > "$build_log" 2>&1 &
  build_pid=$!
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
  build_rc=0
  wait "$build_pid" || build_rc=$?
  cat "$build_log"
  [[ $build_rc -eq 0 ]] || { echo "Dev app release build failed ($build_rc)" >&2; exit "$build_rc"; }
  rm -rf dist/dev/Pulse.app
  node scripts/release/mac-payload.mjs dev
  test -x dist/dev/Pulse.app/Contents/MacOS/Pulse
fi
