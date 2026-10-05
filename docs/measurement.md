# Footprint measurement

Bounded probes for the "Mac/Windows footprint" gate in `feasibility.md`. This page records method only; **no measurements are recorded here** and none may be invented. Results stay local, since they contain PIDs, start times and timestamps; never commit probe output.

## What the probe does

`scripts/probes/footprint-probe.mjs` samples **one process you name** with the OS process listing, read-only:

- macOS: `ps -o pid=,lstart=,rss=,time= -p PID`
- Windows: `powershell -NoProfile -Command "Get-Process -Id PID -ErrorAction SilentlyContinue | Select Id,StartTime,WorkingSet64,TotalProcessorTime | ConvertTo-Json"`

It uses `execFile` (no shell) with a 10 s timeout and 64 KiB output cap. It never starts, stops or signals apps, installs anything, or needs admin rights. `--pid` is required; there is no auto-discovery. Parsing and aggregation live in `footprint-report.mjs` (no I/O).

## Usage

```
node scripts/probes/footprint-probe.mjs --pid PID                    # smoke: 10 samples at 1 s
node scripts/probes/footprint-probe.mjs --pid PID --duration 600 --interval 2 --out local/run.json
```

- `--duration SECONDS`: 1..3600 (hard cap). Samples = floor(duration / interval) + 1.
- `--interval SECONDS`: 1..60, default 1.
- `--out FILE`: write JSON (created exclusively, mode 0600; refuses to overwrite). Otherwise JSON goes to stdout.
- A text summary always goes to stderr. Exit 0 only when termination is `completed`.

Pass the PID of an already-running Cockpit build; find it yourself with Activity Monitor or Task Manager. Ctrl-C ends the run as `interrupted` and still writes the report.

## Output

JSON (`schema_version` 1) holds process identity (`pid` + `start_time`), per-sample timestamp, `rss_bytes`, `cpu_seconds`, `cpu_percent`, `interval_ms`, per-sample `unavailable` metrics, `termination`, and a `summary` with `n`, min, median, p95 (nearest rank), max and explicit units for RSS (bytes), CPU (percent of one core) and measured interval (ms).

CPU% = delta cumulative CPU time / delta monotonic wall time; the first sample, and any sample with a missing or decreasing CPU counter, has none and is listed as unavailable. Unreadable values are never filled in.

Termination values: `completed`, `interrupted`, `process_exited` (listing empty), `pid_reused` (start time changed; that sample is excluded), `unreadable` (3 consecutive failed or malformed samples).

macOS `lstart` is local time without a zone; it is only compared for equality within a run.

Prototype RAM rings show physical-memory occupancy, not macOS memory pressure or Windows commit pressure. Pressure providers remain pending.

## Limits against the budget

The budget (`implementation-plan.md`) is physical footprint on Mac and private bytes on Windows, plus RSS for reference, with child processes reported separately. This probe reports **RSS (Mac) and working set (Windows) only**, for the single PID, so it is a smoke and reference tool. It does not replace footprint/private-bytes capture, and children (e.g. WebView2) must be probed by their own explicit PIDs and reported separately.

## Capturing release-build baselines (planned)

1. Use a signed release build produced by the generated RightKit workflows, on a real machine (not a hosted runner or VM), one platform at a time.
2. Launch the pill normally, leave it idle with the pill visible and no dashboard, and wait for the 10-minute steady state.
3. Run the probe for the plan's windows (for example `--duration 600 --interval 2` for idle CPU), then repeat around hover/ring updates and after closing the dashboard (return to baseline within 60 s).
4. Keep raw JSON local. Only reviewed, aggregated figures with machine class, OS version, build identity and probe schema version may be added to `feasibility.md`, replacing "Pending". Footprint/private bytes come from platform tools in the same session.

## Checks

Static only locally: `node --check`, `bash -n`, `git diff --check`. CI command: `node --test scripts/probes/footprint-report.test.mjs`. It runs alongside upstream parser tests in `scripts/gate.sh`. The probe itself is never run in CI or by agents.
