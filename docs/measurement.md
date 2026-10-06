# Footprint measurement

Bounded probes for the "Mac/Windows footprint" gate in `feasibility.md`. This page records method only; **no measurements are recorded here** and none may be invented. Results stay local, since they contain PIDs, start times and timestamps; never commit probe output.

## What the probe does

`scripts/probes/footprint-probe.mjs` samples **one process you name**, read-only, with `execFile` (no shell), a 10 s timeout and a 64 KiB output cap. It never starts, stops or signals apps, installs anything, or needs admin rights. `--pid` is required; there is no auto-discovery. Parsing, tracking and aggregation live in `footprint-report.mjs` (no I/O).

| Platform | Source | Metrics |
| --- | --- | --- |
| macOS, `--probe-bin PATH` | `cockpit-probe --pid N` (Swift, `mac/Sources/CockpitProbe`) | physical footprint, resident size, CPU time, start time, Mach ports (own PID only) |
| macOS, default | `ps -o pid=,lstart=,rss=,time= -p PID` | RSS and CPU time only; footprint reported unavailable |
| Windows | PowerShell `Get-Process` query | private bytes, working set, handle count, CPU time, start time; GDI objects unavailable |

`--probe-bin` is an explicit path to the built `cockpit-probe` executable. It must be an executable regular file, is run without a shell, and is macOS only. Nothing is searched for or built by the runner.

### Metric semantics

- **Physical footprint** (`footprint_bytes`): `ri_phys_footprint` from `proc_pid_rusage(RUSAGE_INFO_V4)`; the figure the Mac budget refers to. Not RSS.
- **Resident size** (`rss_bytes`, Mac): `ri_resident_size`. With the `ps` fallback, `rss` converted from KiB. Reference only.
- **Working set** (`rss_bytes`, Windows): `WorkingSet64`; reference only, not private bytes.
- **Private bytes** (`private_bytes`, Windows): `PrivateMemorySize64`; the figure the Windows budget refers to.
- **Handle count** (`handle_count`, Windows): `HandleCount`.
- **GDI objects** (`gdi_objects`): always unavailable, reason `gdi_requires_add_type_compile`. `GetGuiResources` needs `Add-Type` P/Invoke compilation, which the probe avoids.
- **Mach ports** (`mach_port_count`): `PROC_PIDLISTFDS` covers file descriptors, not ports, and `mach_port_names` on another task needs `task_for_pid`, which is not used. The count is reported only when the probe samples its own PID (never the case from the Node runner), otherwise unavailable with reason `task_for_pid_not_allowed`.
- **CPU time**: Mac `ri_user_time + ri_system_time` (kernel values already expressed in nanoseconds); Windows `TotalProcessorTime`. **CPU%** = delta cumulative CPU time / delta monotonic wall time (100 = one core). The first sample, and any sample with a missing or decreasing counter, has none and is listed unavailable.
- **Start time / PID reuse**: the kernel start time (`pbi_start_tvsec`/`pbi_start_tvusec`; Windows `StartTime`) is read before and after each sample. A difference is `pid_reused` and the sample carries no metrics. The runner also compares start time across samples. The `ps` fallback has only the across-samples comparison; `lstart` is local time without a zone and is compared only for equality.

Unreadable values are never filled in; each is listed in the sample's `unavailable` with a reason in `unavailable_reasons`.

### Probe JSON

`cockpit-probe` prints one line: `schema_version` 1, `kind` `cockpit.probe.sample`, `status` (`ok`, `vanished`, `pid_reused`, `unavailable`), `pid`, `start_time {sec, usec}`, and for `ok` `physical_footprint_bytes`, `resident_bytes`, `user_cpu_ns`, `system_cpu_ns`, `mach_ports {available, count | reason}`. Exit codes: 0 ok, 1 vanished/unavailable, 2 usage, 3 pid_reused. It uses only same-user, entitlement-free Darwin calls (`proc_pidinfo` `PROC_PIDTBSDINFO`, `proc_pid_rusage`); another user's process may report `unavailable`.

## Usage

```
node scripts/probes/footprint-probe.mjs --pid PID                    # smoke: 10 samples at 1 s
node scripts/probes/footprint-probe.mjs --pid PID --duration 600 --interval 2 --out local/run.json
node scripts/probes/footprint-probe.mjs --pid PID --probe-bin /abs/path/cockpit-probe --duration 600
node scripts/probes/footprint-probe.mjs --pid PID --soak --duration 86400 --interval 10 --out local/soak.ndjson
```

- `--duration SECONDS`: 1..3600 (hard cap) normally; with `--soak` 1..86400. Samples = floor(duration / interval) + 1.
- `--interval SECONDS`: 1..60; default 1, or 10 with `--soak`.
- `--out FILE`: created exclusively, mode 0600; refuses to overwrite. Without `--soak`, JSON goes to stdout if omitted.
- `--soak`: long run. Requires an explicit `--duration` and `--out`. Samples stream to `--out` as NDJSON (one `header` line, one `sample` line each, a final `summary` line) and are never kept in memory.
- A text summary always goes to stderr. Exit 0 only when termination is `completed`. Ctrl-C ends the run as `interrupted` and still writes the report/summary.

Pass the PID of an already-running Cockpit build; find it with Activity Monitor or Task Manager.

## Output

Smoke/duration JSON (`schema_version` 1) holds process identity (`pid` + `start_time`), per-sample records, `termination` and a `summary` with `n`, min, median, p95 (nearest rank), max for each metric with explicit units (bytes, count, percent of one core, ms). Soak summaries add `mean` and drop the samples array (`samples_streamed` counts them).

Soak aggregation is bounded: count, min, max and mean are exact running values; median and p95 come from a fixed 1024-value uniform reservoir per metric. They are exact while n <= 1024 and a sampling estimate afterwards (`quantiles` says which). Reconstruct exact quantiles from the NDJSON if needed.

Termination values: `completed`, `interrupted`, `process_exited`, `pid_reused` (excluded from metrics), `unreadable` (3 consecutive failed or malformed samples), `output_error` (soak stream write failed).

Prototype RAM rings show physical-memory occupancy, not macOS memory pressure or Windows commit pressure. Pressure providers remain pending.

## Limits against the budget

The budget (`implementation-plan.md`) is physical footprint on Mac and private bytes on Windows, plus RSS for reference, with child processes reported separately. Footprint needs `--probe-bin`; without it Mac reports RSS only. The probe covers a single PID, so children (e.g. WebView2) must be probed by their own explicit PIDs and reported separately.

## Capturing release-build baselines (planned)

1. Use a signed release build produced by the generated RightKit workflows, on a real machine (not a hosted runner or VM), one platform at a time.
2. Launch the notch normally, leave it idle with the notch visible and no dashboard, and wait for the 10-minute steady state.
3. Build `cockpit-probe` in CI and copy it to the test machine (explicit path). Run the probe for the plan's windows (for example `--duration 600 --interval 2` for idle CPU), then repeat around hover/ring updates and after closing the dashboard (return to baseline within 60 s).
4. Keep raw JSON local. Only reviewed, aggregated figures with machine class, OS version, build identity and probe schema version may be added to `feasibility.md`, replacing "Pending". Footprint/private bytes come from platform tools in the same session.

## Checks

Static only locally: `node --check`, `bash -n`, `git diff --check`. CI commands: `swift build --package-path mac --product cockpit-probe` (macOS runner) and `node --test scripts/probes/footprint-report.test.mjs`. The probes are never run in CI or by agents; fixtures are synthetic format samples, not measurements.
