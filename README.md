# Cockpit

Native system gauges & controls for macOS (Windows later): Codenotch-fork notch, Tauri hub & shared Rust core. Plan: [docs/plan.md](docs/plan.md).

Current state (2026-10-07): Rust core & read-only CLI are kept. The notch is a Codenotch fork in [`mac/Notch`](mac/Notch/FORK.md), built in CI on the `xcode-27` runner; `notch-preview` uploads an ad-hoc-signed build for trying. Mac release packaging still needs repointing to it before a signed installer. The Tauri hub (phase 2) comes next.

## CLI

```text
cockpit status --json
cockpit scan /explicit/path --max-entries 100000 --json
cockpit scan /explicit/path --save --json
cockpit findings --json
cockpit explain chrome-signing-copies --json
cockpit history --json
cockpit procs --sort ram --json
cockpit usage --json
```

Default scan does not persist anything. `--save` records private metadata snapshots in platform application-support storage; `--state-dir` chooses a fixture/history directory. History compares attribution only when scan scopes & provider volume identities agree. No file contents are read, links are not traversed & cloud placeholders are skipped. Missing accounting, sharing, usage & liveness evidence remains unknown. Reported allocation does not promise reclaimed bytes.

## Development

Public repository: compilation & tests run exclusively in generated RightKit GitHub Actions. Toolchains are pinned in `rust-toolchain.toml` & `package.json`. Rust dependency/compiler caches use `rust-cache` & `sccache`; Swift native prototype builds on Mac runner. Primary agent owns integration & pushes. Follow [AGENTS.md](AGENTS.md).

See [implementation plan](docs/implementation-plan.md), [Mac app delivery](docs/mac-app-delivery.md) & [Windows prototype](windows/README.md). Donor feature extraction, cleanup, uninstallation & update integration remain planned work. Hardware qualification covers footprint, fullscreen occupancy, keyboard events & permission identity.
