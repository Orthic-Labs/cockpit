# Cockpit

Native system gauges & controls for macOS & Windows. CodeNOTCH-inspired native notch, shared Rust core & on-demand storage dashboard.

Current implementation: read-only storage scanner, filename search, folder map, file inspector, report-only findings & native notch. Mac app bundles dashboard & scanner; generated hosted workflow produces installer candidates.

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
