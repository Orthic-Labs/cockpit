# Cockpit

Native system gauges & controls for macOS & Windows. Native always-on pills, shared Rust core & on-demand Tauri dashboard.

Initial implementation: bounded read-only storage CLI, report-only incident rules & native pill feasibility prototypes. Mutation commands refuse execution. No apps are installed or existing utilities replaced by this source checkout.

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

See [implementation plan](docs/implementation-plan.md), [Mac prototype](mac/README.md) & [Windows prototype](windows/README.md). M0 machine gates decide subsequent integration: footprint, fullscreen occupancy, keyboard events & permission identity. Dashboard, cleanup, uninstallation, donor extraction & update integration follow those gates.
