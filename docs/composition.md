# M0 composition design

This is a design decision record. It does not claim that any runtime gate, permission flow, footprint budget or donor extraction has passed. Existing ownership targets are in [`runtime.md`](runtime.md).

## Mac composition root

`CockpitAppDelegate` owns one `CockpitRuntime` per process. It creates one registry, injects platform dependencies, starts services in dependency order and stops them in reverse order. Donor app delegates are never called.

```swift
protocol CockpitService: AnyObject {
    var id: ServiceID { get }
    func start(_ context: ServiceContext) async throws
    func stop(_ reason: StopReason) async
}

@MainActor
final class CockpitRuntime {
    let permissions: MacPermissionBroker
    let settings: SettingsStore
    let events: EventTapHub
    let usage: UsagePoller
    let pill: PillFleet
    let launcher: LauncherPort       // lazy; no index until first request
    let worker: WorkerPort
    let dashboard: DashboardPort
    let registry: ServiceRegistry
}
```

`ServiceRegistry` rejects duplicate `ServiceID` and duplicate provider IDs. `start` is idempotent and records each service's state; `stop` is idempotent, cancels child tasks, waits for callbacks to drain, then releases resources. Registry order is:

1. `SettingsStore` and `MacPermissionBroker`.
2. `EventTapHub`.
3. `UsagePoller` with Claude and Codex adapters.
4. `PillFleet` and its display controllers.
5. `WorkerPort` client and dashboard channel.
6. `LauncherPort` only after its first hotkey request.

The registry owns updater, login-item and activation-policy decisions. Codenotch's `AppDelegate`, Vorssaint's `FeatureRuntime` startup and Tinycast's `AppCore.start()` are source references only.

## Service boundaries

### Settings, permissions and IPC

`SettingsStore` is the only settings writer. It validates schema versions, writes a temporary file, fsyncs where supported and renames atomically. Pill, dashboard and CLI use typed requests; dashboard and CLI never write preferences directly.

`MacPermissionBroker` is the only permission status reader and prompt owner:

| Capability | Owner | Policy |
| --- | --- | --- |
| Accessibility | Mac pill | AX window/fullscreen reads, Finder and convenience actions; missing access produces `Unavailable` and no hidden retry loop |
| Input Monitoring | Mac pill | One global event tap shared by remaps, Finder cut/paste, maximizer, Dock click and launcher hotkey |
| Automation | Mac pill | Finder Apple Events only; structured URL lists and per-item results |
| Full Disk Access | Worker / CLI process | Coverage is checked per signed executable; pill permission is not treated as worker or Terminal-launched CLI permission |

The broker reports capability state to Settings; donor permission prompts, launch-item registration and activation-policy changes are removed during extraction. Windows has no parallel permission broker in this design; native Win32 failures become explicit unavailable states.

`WorkerPort` is a client of the per-user Unix socket or named pipe described in [`runtime.md`](runtime.md). The worker owns scans, plans, journal and mutations. Its idle exit does not stop the pill or usage reader.

### One usage owner

```swift
protocol UsageReader: Sendable {
    var providerID: String { get }
    func read() async -> UsageSnapshot
}

actor UsagePoller {
    func register(_ reader: any UsageReader) throws       // providerID is unique
    func latest() -> [UsageSnapshot]
    func start() async
    func stop() async
}
```

`UsagePoller` is the sole owner of Claude/Codex reads, retry/backoff, stale timestamps and persistence. Pill renders its latest snapshot; dashboard and CLI request that snapshot through IPC. No donor provider starts its own timer, subprocess, credential refresh or network request. `register` fails on duplicate IDs, and adapters must not expose a second path to the same account.

### One event-tap owner

```swift
struct EventTapToken: Sendable { let rawValue: UUID }

@MainActor
final class EventTapHub {
    func register(_ handler: EventHandler, priority: Int) throws -> EventTapToken
    func unregister(_ token: EventTapToken)
    func stop() async
}
```

`EventTapHub` creates one tap and dispatches key-down, key-up, repeat and modifier-release events to ordered handlers. Handlers do bounded work and enqueue UI or file operations off the tap callback. Central ownership gives Fn remap, Finder cut/paste, maximizer, Dock click and launcher hotkey explicit priority and teardown. Donor `HotKeyManager`, `HyperKeyTap`, `GlobalShortcut`, Carbon monitors and separate Finder/Dock taps are not started.

## Launcher isolation

`LauncherPort` is a lazy adapter around an allowlisted subset of Tinycast. It owns no app delegate, menu-bar item, updater, login item, permission prompt, activation policy or independent hotkey. The first accepted launcher event asks `AppIndex` to load; closing launcher releases search-session state while retaining only user settings and ranking data. Launcher actions call Cockpit `WorkerPort`, `SettingsStore` or typed open/launch operations; they cannot call raw cleanup or uninstall primitives.

The hotkey is registered with `EventTapHub` at one priority. If Mac footprint evidence later requires a process boundary, `LauncherPort` becomes an XPC client without changing its interface; this is a design seam, not runtime evidence.

## Windows native ring

`WindowsPillRuntime` owns one native ring renderer, one sampler and one monitor-placement service. It receives `UsageSnapshot` values from shared core and cheap counters from `Sampler`; it does not embed Codenotch's WebView2 or start Tauri's provider loops. A native ring adapter may reuse Codenotch geometry constants as reference, while `ui/notch.html` remains excluded.

The fullscreen probe is a single `MonitorOccupancy` service. It enumerates topmost visible windows on each pill monitor, applies the borderless-style and full-coverage test, and publishes suppression to every ring. It does not use a foreground-only provider path. Dashboard and worker remain separate processes.

## Shutdown contract

`CockpitRuntime.stop()` executes once on application termination:

1. Mark registry `stopping`; reject new launcher, settings and worker requests.
2. Hide pill surfaces and stop new sampling/drawing.
3. Stop launcher session and usage poller; cancel bounded child processes and persist last snapshots as stale.
4. Disable `EventTapHub`, unregister all tokens and wait for in-flight callbacks.
5. Stop Finder, maximizer, Dock click and Auto Quit adapters; remove AX observers and restore borrowed input state.
6. Flush settings, close IPC clients and release instance locks.

Worker shutdown is independent: it finishes or journals current job, closes its endpoint and exits after idle. A failed stop records the service ID and state; it never starts a second service instance to recover.

## Minimum extraction sequence

1. **License gate.** Keep immutable pins and source notices. Preserve Tinycast AGPL-3.0-or-later & Vorssaint GPL-3.0-or-later obligations through extraction & combined distribution. Pearcleaner remains reference-only; do not copy its Commons Clause-covered source. Codenotch is MIT, Petal is MIT; Codenotch's SwiftNIO, Sparkle and vendored zstd notices remain separate obligations. See [`donors.md`](donors.md).
2. **Owned seams.** Add registry, permission broker, event hub, usage protocol and launcher port without donor source. Preserve `runtime.md` ownership boundaries.
3. **Petal algorithms.** Extract MIT filesystem algorithms behind Cockpit's provider interface; exclude GPUI `main.rs`, UI and `admin.rs`.
4. **Codenotch readers/ring references.** Adapt Mac provider readers and Swift ring into registry-owned services; disable Codenotch updater, phone link and independent composition root. Use Windows readers only behind shared core; redraw native Windows ring.
5. **Vorssaint conveniences.** Extract one module at a time through `EventTapHub` and `MacPermissionBroker`; start with Finder cut/paste, then maximizer, Dock click and Auto Quit. Preserve GPL notices and mark changed files.
6. **Tinycast launcher.** Only after step 1 permits it, extract allowlisted launcher model/UI pieces behind `LauncherPort`; do not copy `AppCore`, `AppDelegate`, `HotKeyManager`, updater or menu-bar composition. Keep AGPL notices and packaging decision with source receipts.
7. **Package review.** Reconcile every copied file, transitive package, notice and binary bundle against pinned commits before any build or scheduler is enabled.

## Evidence versus decisions

**[Evidence]** Exact donor roots and source paths are recorded in [`donors.md`](donors.md); HeardRight fullscreen paths are source-inspected. No donor runtime was launched here, and no footprint, TCC, Fn, Windows native-ring or shutdown behavior is proven.

**[Design decision]** One Mac AppDelegate/registry, one permission broker, one usage poller, one event-tap hub, lazy launcher index and independent worker are the required composition boundaries. These decisions remain subject to M0 runtime gates.
