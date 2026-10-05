// Pure parsing, tracking and aggregation for the footprint probe. No I/O, no clocks, no process access.
// Measures one explicitly supplied PID. RSS / working set are reference metrics only; they are not the
// physical footprint (Mac) or private bytes (Windows) defined in docs/implementation-plan.md.

export const SCHEMA_VERSION = 1;
export const DEFAULT_SAMPLES = 10;
export const DEFAULT_INTERVAL_S = 1;
export const MIN_INTERVAL_S = 1;
export const MAX_INTERVAL_S = 60;
export const MAX_DURATION_S = 3600;
export const MAX_CONSECUTIVE_FAILURES = 3;

const fail = (kind, reason) => ({ ok: false, kind, reason });

// ---- argument handling -------------------------------------------------------------------------

function parseNumber(text) {
  return /^\d+(\.\d+)?$/.test(text ?? "") ? Number(text) : NaN;
}

/** Returns {ok:true, options} or {ok:false, error}. A PID is mandatory; there is no default target. */
export function parseArgs(argv) {
  const opts = { pid: null, durationS: null, intervalS: DEFAULT_INTERVAL_S, out: null };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (!["--pid", "--duration", "--interval", "--out"].includes(flag)) {
      return { ok: false, error: `unknown argument: ${flag}` };
    }
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) return { ok: false, error: `${flag} needs a value` };
    i += 1;
    if (flag === "--pid") {
      if (!/^[1-9]\d{0,9}$/.test(value)) return { ok: false, error: "--pid must be a positive integer" };
      opts.pid = Number(value);
    } else if (flag === "--duration") {
      const n = parseNumber(value);
      if (!(n >= 1 && n <= MAX_DURATION_S)) return { ok: false, error: `--duration must be 1..${MAX_DURATION_S} seconds` };
      opts.durationS = n;
    } else if (flag === "--interval") {
      const n = parseNumber(value);
      if (!(n >= MIN_INTERVAL_S && n <= MAX_INTERVAL_S)) return { ok: false, error: `--interval must be ${MIN_INTERVAL_S}..${MAX_INTERVAL_S} seconds` };
      opts.intervalS = n;
    } else {
      opts.out = value;
    }
  }
  if (opts.pid === null) return { ok: false, error: "--pid is required: pass the PID of the Cockpit process to measure" };
  const samples = opts.durationS === null ? DEFAULT_SAMPLES : Math.floor(opts.durationS / opts.intervalS) + 1;
  return { ok: true, options: { ...opts, samples, windowKind: opts.durationS === null ? "smoke" : "duration" } };
}

// ---- parsing -----------------------------------------------------------------------------------

/** ps cputime: [[dd-]hh:]mm:ss[.ff] -> seconds, or null. */
export function parsePsCpuTime(text) {
  const m = /^(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+(?:\.\d+)?)$/.exec(text ?? "");
  if (!m) return null;
  const [, d, h, min, s] = m;
  return Number(d ?? 0) * 86400 + Number(h ?? 0) * 3600 + Number(min) * 60 + Number(s);
}

const PS_LINE = /^\s*(\d+)\s+([A-Z][a-z]{2}\s+[A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}\s+\d{4})\s+(\d+)\s+(\S+)\s*$/;

/**
 * Parse `ps -o pid=,lstart=,rss=,time= -p PID` stdout. Empty output means the process is gone.
 * Success: {ok:true, sample:{pid,startTime,rssBytes,cpuSeconds}}. lstart is local time without zone.
 */
export function parsePsOutput(stdout, expectedPid) {
  if (typeof stdout !== "string") return fail("malformed", "ps_output_not_text");
  if (stdout.trim() === "") return fail("vanished", "ps_empty_output");
  const lines = stdout.split(/\r?\n/).filter((l) => l.trim() !== "");
  if (lines.length !== 1) return fail("malformed", "ps_unexpected_line_count");
  const m = PS_LINE.exec(lines[0]);
  if (!m) return fail("malformed", "ps_line_unrecognised");
  const pid = Number(m[1]);
  if (pid !== expectedPid) return fail("malformed", "ps_pid_mismatch");
  const rssKib = Number(m[3]);
  if (!Number.isSafeInteger(rssKib)) return fail("malformed", "ps_rss_invalid");
  return {
    ok: true,
    sample: { pid, startTime: m[2].replace(/\s+/g, " "), rssBytes: rssKib * 1024, cpuSeconds: parsePsCpuTime(m[4]) }
  };
}

function psDateToIso(value) {
  if (value && typeof value === "object") value = value.value ?? value.DateTime;
  if (typeof value === "string") {
    const legacy = /^\/Date\((-?\d+)\)\/$/.exec(value);
    const ms = legacy ? Number(legacy[1]) : Date.parse(value);
    return Number.isFinite(ms) ? new Date(ms).toISOString() : null;
  }
  return null;
}

function psTimeSpanSeconds(value) {
  if (value === null || value === undefined) return null;
  if (typeof value === "object" && Number.isFinite(value.TotalSeconds) && value.TotalSeconds >= 0) return value.TotalSeconds;
  return null;
}

/**
 * Parse PowerShell `Select Id,StartTime,WorkingSet64,TotalProcessorTime | ConvertTo-Json`.
 * Accepts PowerShell 7 (ISO string) and 5.1 (/Date(ms)/ or {value,DisplayHint,DateTime}) shapes.
 * Empty output means the process is gone. A missing StartTime means identity cannot be verified.
 */
export function parsePowerShellOutput(stdout, expectedPid) {
  if (typeof stdout !== "string") return fail("malformed", "powershell_output_not_text");
  if (stdout.trim() === "") return fail("vanished", "powershell_empty_output");
  let doc;
  try {
    doc = JSON.parse(stdout);
  } catch {
    return fail("malformed", "powershell_json_invalid");
  }
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) return fail("malformed", "powershell_json_not_object");
  if (doc.Id !== expectedPid) return fail("malformed", "powershell_pid_mismatch");
  if (!Number.isSafeInteger(doc.WorkingSet64) || doc.WorkingSet64 < 0) return fail("malformed", "powershell_working_set_invalid");
  const startTime = psDateToIso(doc.StartTime);
  if (startTime === null) return fail("unavailable", "start_time_unavailable");
  return {
    ok: true,
    sample: { pid: doc.Id, startTime, rssBytes: doc.WorkingSet64, cpuSeconds: psTimeSpanSeconds(doc.TotalProcessorTime) }
  };
}

// ---- tracking ----------------------------------------------------------------------------------

/** CPU percent of one core from two cumulative CPU-second readings and monotonic ms. Null if not computable. */
export function cpuPercent(prev, cur) {
  if (prev?.cpuSeconds == null || cur?.cpuSeconds == null) return null;
  const wallS = (cur.monoMs - prev.monoMs) / 1000;
  const cpuS = cur.cpuSeconds - prev.cpuSeconds;
  if (!(wallS > 0) || cpuS < 0) return null;
  return (cpuS / wallS) * 100;
}

/**
 * Stateful (in-memory only) sample tracker. add({at, monoMs, result}) takes a parser result and
 * returns {record, stop} where stop is a termination status string once the run must end.
 */
export function createTracker(pid) {
  let identity = null;
  let lastGood = null;
  let lastMono = null;
  let failures = 0;
  let termination = null;
  const records = [];

  function push(rec) {
    records.push({ index: records.length, ...rec });
    return records[records.length - 1];
  }

  return {
    records,
    get termination() { return termination; },
    get identity() { return identity; },
    add({ at, monoMs, result }) {
      const intervalMs = lastMono === null ? null : monoMs - lastMono;
      lastMono = monoMs;
      const base = { at, interval_ms: intervalMs };
      if (!result.ok) {
        failures += 1;
        const status = result.kind;
        const rec = push({ ...base, status, reason: result.reason, rss_bytes: null, cpu_seconds: null, cpu_percent: null, unavailable: ["rss", "cpu_time"] });
        if (status === "vanished") termination = "process_exited";
        else if (failures >= MAX_CONSECUTIVE_FAILURES) termination = "unreadable";
        return { record: rec, stop: termination };
      }
      const s = result.sample;
      if (identity !== null && s.startTime !== identity.start_time) {
        const rec = push({ ...base, status: "pid_reused", reason: "start_time_changed", rss_bytes: null, cpu_seconds: null, cpu_percent: null, unavailable: ["rss", "cpu_time"], observed_start_time: s.startTime });
        termination = "pid_reused";
        return { record: rec, stop: termination };
      }
      failures = 0;
      if (identity === null) identity = { pid, start_time: s.startTime };
      const cur = { cpuSeconds: s.cpuSeconds, monoMs };
      const pct = cpuPercent(lastGood, cur);
      const unavailable = [];
      if (s.cpuSeconds === null) unavailable.push("cpu_time");
      else if (pct === null) unavailable.push("cpu_percent");
      lastGood = cur;
      const rec = push({ ...base, status: "ok", reason: null, rss_bytes: s.rssBytes, cpu_seconds: s.cpuSeconds, cpu_percent: pct, unavailable });
      return { record: rec, stop: null };
    },
    /** Set a runner-decided termination ("completed", "interrupted") unless the tracker already stopped. */
    finish(status) {
      if (termination === null) termination = status;
      return termination;
    }
  };
}

// ---- aggregation -------------------------------------------------------------------------------

/** min/median/p95 (nearest rank)/max over finite numbers. Empty input gives n:0 and nulls. */
export function stats(values, unit) {
  const v = values.filter((x) => Number.isFinite(x)).sort((a, b) => a - b);
  const n = v.length;
  if (n === 0) return { unit, n: 0, min: null, median: null, p95: null, max: null };
  const median = n % 2 ? v[(n - 1) / 2] : (v[n / 2 - 1] + v[n / 2]) / 2;
  return { unit, n, min: v[0], median, p95: v[Math.ceil(0.95 * n) - 1], max: v[n - 1] };
}

export function buildReport({ platform, options, startedAt, endedAt, tracker }) {
  const recs = tracker.records;
  const ok = recs.filter((r) => r.status === "ok");
  const unavailable = {};
  for (const r of recs) for (const k of r.unavailable) unavailable[k] = (unavailable[k] ?? 0) + 1;
  const statuses = {};
  for (const r of recs) statuses[r.status] = (statuses[r.status] ?? 0) + 1;
  return {
    schema_version: SCHEMA_VERSION,
    kind: "cockpit.footprint",
    platform,
    process: tracker.identity,
    target_pid: options.pid,
    window: {
      kind: options.windowKind,
      requested_samples: options.samples,
      requested_interval_s: options.intervalS,
      requested_duration_s: options.durationS
    },
    started_at: startedAt,
    ended_at: endedAt,
    termination: tracker.termination ?? "completed",
    metric_notes: {
      rss_bytes: platform === "win32" ? "WorkingSet64; not private bytes" : "ps rss (KiB converted to bytes); not physical footprint",
      cpu_percent: "100 = one core; from cumulative CPU time over measured monotonic interval; first sample has none"
    },
    summary: {
      samples_total: recs.length,
      samples_ok: ok.length,
      sample_status_counts: statuses,
      unavailable_counts: unavailable,
      rss_bytes: stats(ok.map((r) => r.rss_bytes), "bytes"),
      cpu_percent: stats(ok.map((r) => r.cpu_percent), "percent_of_one_core"),
      interval_ms: stats(recs.map((r) => r.interval_ms), "ms")
    },
    samples: recs
  };
}

const fmt = (x, digits = 1) => (x === null ? "n/a" : x.toFixed(digits));

/** Human-readable summary with units and sample counts. */
export function formatSummary(report) {
  const s = report.summary;
  const line = (label, st, div, unit, digits) =>
    `${label}: n=${st.n} min=${fmt(st.min === null ? null : st.min / div, digits)} median=${fmt(st.median === null ? null : st.median / div, digits)} p95=${fmt(st.p95 === null ? null : st.p95 / div, digits)} max=${fmt(st.max === null ? null : st.max / div, digits)} ${unit}`;
  return [
    `termination: ${report.termination}; samples ok ${s.samples_ok} of ${s.samples_total}`,
    line("rss", s.rss_bytes, 1048576, "MiB", 1),
    line("cpu", s.cpu_percent, 1, "% of one core", 2),
    line("interval", s.interval_ms, 1, "ms", 0),
    `unavailable: ${Object.keys(s.unavailable_counts).length ? JSON.stringify(s.unavailable_counts) : "none"}`
  ].join("\n");
}
