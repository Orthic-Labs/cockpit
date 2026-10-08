// Pure parsing, tracking and aggregation for the footprint probe. No I/O, no clocks, no process access.
// Measures one explicitly supplied PID. RSS / working set are reference metrics only; they are not the
// physical footprint (Mac) or private bytes (Windows) defined in docs/implementation-plan.md.

export const SCHEMA_VERSION = 1;
export const DEFAULT_SAMPLES = 10;
export const DEFAULT_INTERVAL_S = 1;
export const MIN_INTERVAL_S = 1;
export const MAX_INTERVAL_S = 60;
export const MAX_DURATION_S = 3600;
export const MAX_SOAK_DURATION_S = 86400;
export const DEFAULT_SOAK_INTERVAL_S = 10;
export const RESERVOIR_SIZE = 1024;
export const MAX_CONSECUTIVE_FAILURES = 3;

const fail = (kind, reason) => ({ ok: false, kind, reason });

// ---- argument handling -------------------------------------------------------------------------

function parseNumber(text) {
  return /^\d+(\.\d+)?$/.test(text ?? "") ? Number(text) : NaN;
}

/**
 * Returns {ok:true, options} or {ok:false, error}. A PID is mandatory; there is no default target.
 * --soak is a boolean flag: it needs an explicit --duration (max 86400) and --out (NDJSON stream).
 */
export function parseArgs(argv) {
  const opts = { pid: null, durationS: null, intervalS: null, out: null, probeBin: null, soak: false };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === "--soak") { opts.soak = true; continue; }
    if (!["--pid", "--duration", "--interval", "--out", "--probe-bin"].includes(flag)) {
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
      if (!(n >= 1 && n <= MAX_SOAK_DURATION_S)) return { ok: false, error: `--duration must be 1..${MAX_SOAK_DURATION_S} seconds` };
      opts.durationS = n;
    } else if (flag === "--interval") {
      const n = parseNumber(value);
      if (!(n >= MIN_INTERVAL_S && n <= MAX_INTERVAL_S)) return { ok: false, error: `--interval must be ${MIN_INTERVAL_S}..${MAX_INTERVAL_S} seconds` };
      opts.intervalS = n;
    } else if (flag === "--probe-bin") {
      opts.probeBin = value;
    } else {
      opts.out = value;
    }
  }
  if (opts.pid === null) return { ok: false, error: "--pid is required: pass the PID of the Pulse process to measure" };
  if (opts.soak) {
    if (opts.durationS === null) return { ok: false, error: `--soak needs an explicit --duration (1..${MAX_SOAK_DURATION_S} seconds)` };
    if (opts.out === null) return { ok: false, error: "--soak needs --out FILE (samples stream there as NDJSON; they are never kept in memory)" };
  } else if (opts.durationS !== null && opts.durationS > MAX_DURATION_S) {
    return { ok: false, error: `--duration must be 1..${MAX_DURATION_S} seconds without --soak` };
  }
  const intervalS = opts.intervalS ?? (opts.soak ? DEFAULT_SOAK_INTERVAL_S : DEFAULT_INTERVAL_S);
  const samples = opts.durationS === null ? DEFAULT_SAMPLES : Math.floor(opts.durationS / intervalS) + 1;
  const windowKind = opts.soak ? "soak" : opts.durationS === null ? "smoke" : "duration";
  return { ok: true, options: { ...opts, intervalS, samples, windowKind } };
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
    sample: {
      pid, startTime: m[2].replace(/\s+/g, " "), rssBytes: rssKib * 1024, cpuSeconds: parsePsCpuTime(m[4]),
      unavailable: { footprint: "ps_has_no_physical_footprint" }
    }
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
  // StartTimeAfter is read by the query after the other properties. Absent key: legacy shape, unchecked.
  if ("StartTimeAfter" in doc) {
    const after = psDateToIso(doc.StartTimeAfter);
    if (after === null) return fail("vanished", "process_exited_during_sample");
    if (after !== startTime) return fail("pid_reused", "start_time_changed_during_sample");
  }
  const unavailable = {};
  const count = (v) => Number.isSafeInteger(v) && v >= 0;
  if (!count(doc.PrivateMemorySize64)) unavailable.private_bytes = "private_bytes_missing";
  if (!count(doc.HandleCount)) unavailable.handle_count = "handle_count_missing";
  unavailable.gdi_objects = "gdi_requires_add_type_compile";
  return {
    ok: true,
    sample: {
      pid: doc.Id, startTime, rssBytes: doc.WorkingSet64, cpuSeconds: psTimeSpanSeconds(doc.TotalProcessorTime),
      privateBytes: count(doc.PrivateMemorySize64) ? doc.PrivateMemorySize64 : null,
      handleCount: count(doc.HandleCount) ? doc.HandleCount : null,
      gdiObjects: null,
      unavailable
    }
  };
}

const REASON = /^[a-z0-9_]{1,64}$/;
const isCount = (v) => Number.isSafeInteger(v) && v >= 0;

/**
 * Parse one JSON line from `pulse-probe --pid N` (mac/Sources/PulseProbe). Statuses: ok, vanished,
 * pid_reused (start time differed before vs after the sample), unavailable. Success sample has
 * startTime "unix:SEC.UUUUUU" (kernel process start), rss, footprint, cpu seconds and optional Mach ports.
 */
export function parseProbeOutput(stdout, expectedPid) {
  if (typeof stdout !== "string") return fail("malformed", "probe_output_not_text");
  const text = stdout.trim();
  if (text === "") return fail("malformed", "probe_empty_output");
  let doc;
  try {
    doc = JSON.parse(text);
  } catch {
    return fail("malformed", "probe_json_invalid");
  }
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) return fail("malformed", "probe_json_not_object");
  if (doc.schema_version !== 1 || !["pulse.probe.sample"].includes(doc.kind)) return fail("malformed", "probe_schema_unrecognised");
  if (doc.pid !== expectedPid) return fail("malformed", "probe_pid_mismatch");
  if (doc.status === "vanished") return fail("vanished", "probe_process_vanished");
  if (doc.status === "pid_reused") return fail("pid_reused", "start_time_changed_during_sample");
  if (doc.status === "unavailable") return fail("unavailable", REASON.test(doc.reason ?? "") ? doc.reason : "probe_reason_invalid");
  if (doc.status !== "ok") return fail("malformed", "probe_status_unrecognised");
  const st = doc.start_time;
  if (!st || !isCount(st.sec) || !Number.isSafeInteger(st.usec) || st.usec < 0 || st.usec > 999999) return fail("malformed", "probe_start_time_invalid");
  if (!isCount(doc.physical_footprint_bytes)) return fail("malformed", "probe_footprint_invalid");
  if (!isCount(doc.resident_bytes)) return fail("malformed", "probe_resident_invalid");
  if (!isCount(doc.user_cpu_ns) || !isCount(doc.system_cpu_ns)) return fail("malformed", "probe_cpu_invalid");
  const unavailable = {};
  let machPortCount = null;
  const mp = doc.mach_ports;
  if (mp && mp.available === true && isCount(mp.count)) machPortCount = mp.count;
  else unavailable.mach_ports = REASON.test(mp?.reason ?? "") ? mp.reason : "mach_ports_unavailable";
  return {
    ok: true,
    sample: {
      pid: doc.pid,
      startTime: `unix:${st.sec}.${String(st.usec).padStart(6, "0")}`,
      rssBytes: doc.resident_bytes,
      footprintBytes: doc.physical_footprint_bytes,
      cpuSeconds: (doc.user_cpu_ns + doc.system_cpu_ns) / 1e9,
      machPortCount,
      unavailable
    }
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

const NULL_METRICS = { rss_bytes: null, footprint_bytes: null, private_bytes: null, handle_count: null, gdi_objects: null, mach_port_count: null, cpu_seconds: null, cpu_percent: null };

/**
 * Stateful sample tracker. add({at, monoMs, result}) takes a parser result and returns {record, stop}
 * where stop is a termination status string once the run must end. With {retain:false} (soak mode)
 * records are returned but never stored, so memory stays constant; feed them to createSummary().
 */
export function createTracker(pid, { retain = true } = {}) {
  let identity = null;
  let lastGood = null;
  let lastMono = null;
  let failures = 0;
  let termination = null;
  let count = 0;
  const records = [];

  function push(rec) {
    const full = { index: count, ...rec };
    count += 1;
    if (retain) records.push(full);
    return full;
  }

  return {
    records,
    get count() { return count; },
    get termination() { return termination; },
    get identity() { return identity; },
    add({ at, monoMs, result }) {
      const intervalMs = lastMono === null ? null : monoMs - lastMono;
      lastMono = monoMs;
      const base = { at, interval_ms: intervalMs };
      if (!result.ok) {
        failures += 1;
        const status = result.kind;
        const unavailable = ["rss", "cpu_time"];
        const reason = result.reason ?? "sample_failed";
        const unavailableReasons = Object.fromEntries(unavailable.map((key) => [key, reason]));
        const rec = push({ ...base, status, reason, ...NULL_METRICS, unavailable, unavailable_reasons: unavailableReasons });
        if (status === "vanished") termination = "process_exited";
        else if (status === "pid_reused") termination = "pid_reused";
        else if (failures >= MAX_CONSECUTIVE_FAILURES) termination = "unreadable";
        return { record: rec, stop: termination };
      }
      const s = result.sample;
      if (identity !== null && s.startTime !== identity.start_time) {
        const rec = push({ ...base, status: "pid_reused", reason: "start_time_changed", ...NULL_METRICS, unavailable: ["rss", "cpu_time"], unavailable_reasons: {}, observed_start_time: s.startTime });
        termination = "pid_reused";
        return { record: rec, stop: termination };
      }
      failures = 0;
      if (identity === null) identity = { pid, start_time: s.startTime };
      const cur = { cpuSeconds: s.cpuSeconds, monoMs };
      const pct = cpuPercent(lastGood, cur);
      const reasons = s.unavailable ?? {};
      const unavailable = Object.keys(reasons);
      if (s.cpuSeconds === null) unavailable.push("cpu_time");
      else if (pct === null) unavailable.push("cpu_percent");
      lastGood = cur;
      const rec = push({
        ...base, status: "ok", reason: null,
        rss_bytes: s.rssBytes ?? null, footprint_bytes: s.footprintBytes ?? null, private_bytes: s.privateBytes ?? null,
        handle_count: s.handleCount ?? null, gdi_objects: s.gdiObjects ?? null, mach_port_count: s.machPortCount ?? null,
        cpu_seconds: s.cpuSeconds, cpu_percent: pct, unavailable, unavailable_reasons: reasons
      });
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

/** [record key, unit] for every per-sample numeric metric summarised. */
export const METRICS = [
  ["rss_bytes", "bytes"], ["footprint_bytes", "bytes"], ["private_bytes", "bytes"],
  ["handle_count", "count"], ["gdi_objects", "count"], ["mach_port_count", "count"],
  ["cpu_percent", "percent_of_one_core"]
];

/** min/median/p95 (nearest rank)/max over finite numbers. Empty input gives n:0 and nulls. */
export function stats(values, unit) {
  const v = values.filter((x) => Number.isFinite(x)).sort((a, b) => a - b);
  const n = v.length;
  if (n === 0) return { unit, n: 0, min: null, median: null, p95: null, max: null };
  const median = n % 2 ? v[(n - 1) / 2] : (v[n / 2 - 1] + v[n / 2]) / 2;
  return { unit, n, min: v[0], median, p95: v[Math.ceil(0.95 * n) - 1], max: v[n - 1] };
}

/** Deterministic PRNG (mulberry32) so reservoir selection is reproducible in tests. */
export function seededRandom(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6D2B79F5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * Bounded-memory running statistic: exact count/min/max/mean (Welford-style running mean) plus a
 * fixed-size uniform reservoir (Algorithm R) for median and p95. Quantiles are exact while
 * n <= reservoirSize and a uniform-sample estimate afterwards; this is flagged in the snapshot.
 */
export function createStreamStat(unit, { reservoirSize = RESERVOIR_SIZE, random = seededRandom(0x5eed) } = {}) {
  let n = 0;
  let min = null;
  let max = null;
  let mean = 0;
  const reservoir = [];
  return {
    get retained() { return reservoir.length; },
    add(x) {
      if (!Number.isFinite(x)) return;
      n += 1;
      min = min === null || x < min ? x : min;
      max = max === null || x > max ? x : max;
      mean += (x - mean) / n;
      if (reservoir.length < reservoirSize) reservoir.push(x);
      else {
        const j = Math.floor(random() * n);
        if (j < reservoirSize) reservoir[j] = x;
      }
    },
    snapshot() {
      if (n === 0) return { unit, n: 0, min: null, mean: null, median: null, p95: null, max: null, quantiles: "exact", reservoir_size: reservoirSize };
      const q = stats(reservoir, unit);
      return { unit, n, min, mean, median: q.median, p95: q.p95, max, quantiles: n <= reservoirSize ? "exact" : "reservoir_estimate", reservoir_size: reservoirSize };
    }
  };
}

/** Streaming equivalent of the summary in buildReport: feed it each record, never keep records. */
export function createSummary({ reservoirSize = RESERVOIR_SIZE } = {}) {
  const opts = (i) => ({ reservoirSize, random: seededRandom(0x5eed + i) });
  const metrics = METRICS.map(([key, unit], i) => [key, createStreamStat(unit, opts(i))]);
  const interval = createStreamStat("ms", opts(metrics.length));
  const statuses = {};
  const unavailable = {};
  let total = 0;
  let okCount = 0;
  return {
    get retained() { return metrics.reduce((a, [, m]) => a + m.retained, interval.retained); },
    add(record) {
      total += 1;
      statuses[record.status] = (statuses[record.status] ?? 0) + 1;
      for (const k of record.unavailable) unavailable[k] = (unavailable[k] ?? 0) + 1;
      interval.add(record.interval_ms);
      if (record.status !== "ok") return;
      okCount += 1;
      for (const [key, m] of metrics) m.add(record[key]);
    },
    snapshot() {
      const out = { samples_total: total, samples_ok: okCount, sample_status_counts: { ...statuses }, unavailable_counts: { ...unavailable } };
      for (const [key, m] of metrics) out[key] = m.snapshot();
      out.interval_ms = interval.snapshot();
      return out;
    }
  };
}

function metricNotes(platform, options) {
  const rss = platform === "win32" ? "WorkingSet64; not private bytes"
    : options.probeBin ? "pulse-probe ri_resident_size (bytes); not physical footprint"
      : "ps rss (KiB converted to bytes); not physical footprint";
  return {
    rss_bytes: rss,
    footprint_bytes: "pulse-probe ri_phys_footprint (RUSAGE_INFO_V4); macOS with --probe-bin only",
    private_bytes: "PowerShell PrivateMemorySize64; Windows only",
    handle_count: "PowerShell HandleCount; Windows only",
    gdi_objects: "GetGuiResources would need Add-Type; reported unavailable",
    mach_port_count: "only for the probe's own PID; other PIDs need task_for_pid, which is not used",
    cpu_percent: "100 = one core; from cumulative CPU time over measured monotonic interval; first sample has none"
  };
}

function reportHeader({ platform, options, startedAt, endedAt, tracker }) {
  return {
    schema_version: SCHEMA_VERSION,
    kind: "pulse.footprint",
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
    metric_notes: metricNotes(platform, options)
  };
}

export function buildReport({ platform, options, startedAt, endedAt, tracker }) {
  const recs = tracker.records;
  const ok = recs.filter((r) => r.status === "ok");
  const unavailable = {};
  for (const r of recs) for (const k of r.unavailable) unavailable[k] = (unavailable[k] ?? 0) + 1;
  const statuses = {};
  for (const r of recs) statuses[r.status] = (statuses[r.status] ?? 0) + 1;
  const summary = { samples_total: recs.length, samples_ok: ok.length, sample_status_counts: statuses, unavailable_counts: unavailable };
  for (const [key, unit] of METRICS) summary[key] = stats(ok.map((r) => r[key]), unit);
  summary.interval_ms = stats(recs.map((r) => r.interval_ms), "ms");
  return { ...reportHeader({ platform, options, startedAt, endedAt, tracker }), summary, samples: recs };
}

/** Soak report: header + streaming summary, no samples array (they went to the NDJSON stream). */
export function buildStreamingReport({ platform, options, startedAt, endedAt, tracker, summary }) {
  return { ...reportHeader({ platform, options, startedAt, endedAt, tracker }), summary: summary.snapshot(), samples_streamed: tracker.count };
}

const fmt = (x, digits = 1) => (x === null ? "n/a" : x.toFixed(digits));

/** Human-readable summary with units and sample counts. */
export function formatSummary(report) {
  const s = report.summary;
  const line = (label, st, div, unit, digits) =>
    `${label}: n=${st.n} min=${fmt(st.min === null ? null : st.min / div, digits)} median=${fmt(st.median === null ? null : st.median / div, digits)} p95=${fmt(st.p95 === null ? null : st.p95 / div, digits)} max=${fmt(st.max === null ? null : st.max / div, digits)} ${unit}`;
  const lines = [
    `termination: ${report.termination}; samples ok ${s.samples_ok} of ${s.samples_total}`,
    line("rss", s.rss_bytes, 1048576, "MiB", 1)
  ];
  if (s.footprint_bytes.n) lines.push(line("footprint", s.footprint_bytes, 1048576, "MiB", 1));
  if (s.private_bytes.n) lines.push(line("private", s.private_bytes, 1048576, "MiB", 1));
  if (s.handle_count.n) lines.push(line("handles", s.handle_count, 1, "handles", 0));
  if (s.mach_port_count.n) lines.push(line("mach ports", s.mach_port_count, 1, "ports", 0));
  lines.push(line("cpu", s.cpu_percent, 1, "% of one core", 2), line("interval", s.interval_ms, 1, "ms", 0));
  lines.push(`unavailable: ${Object.keys(s.unavailable_counts).length ? JSON.stringify(s.unavailable_counts) : "none"}`);
  return lines.join("\n");
}
