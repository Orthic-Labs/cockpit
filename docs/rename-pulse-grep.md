# Pulse rename grep audit

Requested command executed without staging files:

```sh
git grep -n -i cockpit -- . :!vendor :!docs/design-refresh
```

107 tracked hits across 27 files; every hit is intentional. Line numbers below identify each hit & its reason.

| File | Hit lines | Why retained |
| --- | --- | --- |
| `.github/workflows/ci.yml` | 3, 79 | Unchanged generated workflow name, cache identity & artifact root. |
| `.github/workflows/release-candidate.yml` | 132, 146, 158, 165, 175, 191 | Unchanged generated workflow name, cache identity & artifact root. |
| `.rightgit.json` | 34 | Unchanged generated workflow name, cache identity & artifact root. |
| `core/src/scan.rs` | 563 | Legacy environment read fallback; Pulse override takes precedence. |
| `core/src/store.rs` | 154, 156 | Legacy directory name needed to move existing user state. |
| `core/tests/apfs_fixture.rs` | 38, 44, 196 | Legacy environment read fallback; Pulse override takes precedence. |
| `docs/feasibility.md` | 3 | GitHub repo/remote remains Orthic-Labs/cockpit; existing run URLs stay valid. |
| `docs/helper.md` | 37 | Documents legacy helper unregister during migration. |
| `docs/implementation-plan.md` | 30 | Historical plan/status, delivered artifact identity, run link or frozen test evidence; old names remain truthful. |
| `docs/mac-app-delivery.md` | 14, 18, 20, 22, 28, 30, 31, 50, 52, 57, 65, 67 | Historical plan/status, delivered artifact identity, run link or frozen test evidence; old names remain truthful. |
| `docs/plan.md` | 51, 59, 62, 66 | Historical plan/status, delivered artifact identity, run link or frozen test evidence; old names remain truthful. |
| `docs/test-inventory.json` | 485, 497, 519, 531, 539, 547, 567, 582, 605, 638, 653, 1374, 1386, 1394, 1416, 1428, 1436, 1444, 1464, 1479, 1502, 1535, 1550, 1759 | Historical plan/status, delivered artifact identity, run link or frozen test evidence; old names remain truthful. |
| `docs/test-migration.md` | 25, 27, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 191, 193, 195, 197, 199, 201, 203, 205, 207, 209, 211 | Historical plan/status, delivered artifact identity, run link or frozen test evidence; old names remain truthful. |
| `hub/qa-e2e/tests/ui.rs` | 62, 87 | Legacy environment read fallback; Pulse override takes precedence. |
| `hub/src-tauri/src/lib.rs` | 322 | Legacy environment read fallback; Pulse override takes precedence. |
| `hub/src-tauri/src/scanner.rs` | 278 | Legacy environment read fallback; Pulse override takes precedence. |
| `mac/Notch/Sources/Conveniences/DiskImageInstaller.swift` | 4, 9 | Preserved third-party adaptation notice. |
| `mac/Notch/Sources/Conveniences/DiskImageInstallerSupport.swift` | 4 | Preserved third-party adaptation notice. |
| `mac/Notch/Sources/System/PrivilegedHelper.swift` | 16 | Legacy launch daemon plist used solely for best-effort unregister. |
| `right-release.config.mjs` | 5 | GitHub repo/remote remains Orthic-Labs/cockpit; existing run URLs stay valid. |
| `scripts/fixtures/apfs/setup.sh` | 6, 7 | Legacy environment read fallback; Pulse override takes precedence. |
| `scripts/fixtures/apfs/teardown.sh` | 6, 7 | Legacy environment read fallback; Pulse override takes precedence. |
| `scripts/gate.sh` | 4, 5 | Legacy environment read fallback; Pulse override takes precedence. |
| `scripts/probes/footprint-report.mjs` | 182 | Parser accepts old probe record kind for compatibility. |
| `scripts/release/candidate.mjs` | 9 | Legacy environment read fallback; Pulse override takes precedence. |
| `scripts/release/mac-payload.mjs` | 68, 69, 70 | Legacy environment read fallback; Pulse override takes precedence. |
| `windows/src/settings.rs` | 531 | Legacy directory name needed to move existing user state. |

Git grep covers tracked files. New unstaged state_migration.rs & ProductMigration.swift additionally contain legacy directory/domain identifiers & original login sentinel needed to preserve user data. rename-pulse.md names old identifiers as migration mappings; this audit quotes requested search term & preserved identities. New Pulse helper plist, renamed salvage sources/tests & all other new source paths were also inspected for accidental legacy product references.

Static verification: git diff --check, Node syntax checks, shell syntax checks & configuration parsing passed. Existing deliberately malformed JSON fixture remains malformed. Source test declaration inventory remains 390; no builds, test execution, commits, pushes or publication occurred. Concurrent docs/design-refresh/ & mac/.build/ stayed untouched.
