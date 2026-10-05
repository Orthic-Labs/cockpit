// Dependency-free tests for footprint-report.mjs. Run: node --test scripts/probes/footprint-report.test.mjs
// Fixtures are synthetic format samples, not measurements.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  RESERVOIR_SIZE, buildReport, buildStreamingReport, cpuPercent, createStreamStat, createSummary, createTracker,
  formatSummary, parseArgs, parsePowerShellOutput, parseProbeOutput, parsePsCpuTime, parsePsOutput, stats
} from "./footprint-report.mjs";

const dir = join(dirname(fileURLToPath(import.meta.url)), "fixtures");
const fx = (name) => readFileSync(join(dir, name), "utf8");
const PID = 4242;

test("parseArgs refuses without a pid and bounds duration", () => {
  assert.equal(parseArgs([]).ok, false);
  assert.match(parseArgs([]).error, /--pid is required/);
  assert.equal(parseArgs(["--pid", "0"]).ok, false);
  assert.equal(parseArgs(["--pid", "12x"]).ok, false);
  assert.equal(parseArgs(["--pid", "5", "--duration", "3601"]).ok, false);
  assert.equal(parseArgs(["--pid", "5", "--bogus", "1"]).ok, false);
  assert.equal(parseArgs(["--pid", "5", "--interval", "0.1"]).ok, false);
});

test("parseArgs defaults to a 10 sample 1s smoke window", () => {
  const r = parseArgs(["--pid", "77"]);
  assert.deepStrictEqual(r.options, { pid: 77, durationS: null, intervalS: 1, out: null, probeBin: null, soak: false, samples: 10, windowKind: "smoke" });
  const d = parseArgs(["--pid", "77", "--duration", "3600", "--interval", "2"]);
  assert.equal(d.options.samples, 1801);
  assert.equal(d.options.windowKind, "duration");
});

test("ps parsing", () => {
  const r = parsePsOutput(fx("ps-ok.txt"), PID);
  assert.deepStrictEqual(r, { ok: true, sample: { pid: PID, startTime: "Mon Jan 5 10:00:00 2026", rssBytes: 10240 * 1024, cpuSeconds: 1.5, unavailable: { footprint: "ps_has_no_physical_footprint" } } });
  assert.equal(parsePsOutput(fx("ps-ok-days.txt"), PID).sample.cpuSeconds, 86400 + 2 * 3600 + 3 * 60 + 4.25);
  assert.equal(parsePsCpuTime("12:34.5"), 754.5);
  assert.equal(parsePsCpuTime("nope"), null);
});

test("powershell parsing: ISO (7.x) and legacy 5.1 shapes", () => {
  const a = parsePowerShellOutput(fx("powershell-ok.json"), PID);
  assert.equal(a.sample.startTime, "2026-01-05T10:00:00.000Z");
  assert.equal(a.sample.rssBytes, 20971520);
  assert.equal(a.sample.cpuSeconds, 1.5);
  assert.equal(a.sample.privateBytes, null);
  assert.equal(a.sample.unavailable.private_bytes, "private_bytes_missing");
  const b = parsePowerShellOutput(fx("powershell-ps51.json"), PID);
  assert.equal(b.sample.startTime, a.sample.startTime);
  assert.equal(b.sample.cpuSeconds, 3);
});

test("powershell: missing cpu time is explicit, missing start time is unavailable", () => {
  assert.equal(parsePowerShellOutput(fx("powershell-null-cpu.json"), PID).sample.cpuSeconds, null);
  const r = parsePowerShellOutput(fx("powershell-no-start.json"), PID);
  assert.deepStrictEqual([r.ok, r.kind], [false, "unavailable"]);
});

test("malformed output", () => {
  assert.deepStrictEqual(parsePsOutput(fx("ps-malformed.txt"), PID).kind, "malformed");
  assert.equal(parsePsOutput(fx("ps-ok.txt"), 1).kind, "malformed");
  assert.equal(parsePsOutput(null, PID).kind, "malformed");
  assert.equal(parsePowerShellOutput(fx("powershell-malformed.json"), PID).kind, "malformed");
  assert.equal(parsePowerShellOutput("[1,2]", PID).kind, "malformed");
  assert.equal(parsePowerShellOutput(fx("powershell-ok.json"), 1).kind, "malformed");
});

test("empty output means vanished", () => {
  assert.equal(parsePsOutput(fx("ps-empty.txt"), PID).kind, "vanished");
  assert.equal(parsePowerShellOutput("  \n", PID).kind, "vanished");
});

test("cpuPercent from deltas", () => {
  assert.equal(cpuPercent({ cpuSeconds: 1.5, monoMs: 0 }, { cpuSeconds: 3.5, monoMs: 1000 }), 200);
  assert.equal(cpuPercent({ cpuSeconds: 3, monoMs: 0 }, { cpuSeconds: 2, monoMs: 1000 }), null);
  assert.equal(cpuPercent({ cpuSeconds: 1, monoMs: 5 }, { cpuSeconds: 2, monoMs: 5 }), null);
  assert.equal(cpuPercent(null, { cpuSeconds: 2, monoMs: 5 }), null);
  assert.equal(cpuPercent({ cpuSeconds: null, monoMs: 0 }, { cpuSeconds: 2, monoMs: 5 }), null);
});

test("stats: min/median/p95/max with empty case", () => {
  assert.deepStrictEqual(stats([], "x"), { unit: "x", n: 0, min: null, median: null, p95: null, max: null });
  const s = stats(Array.from({ length: 20 }, (_, i) => 20 - i), "u");
  assert.deepStrictEqual(s, { unit: "u", n: 20, min: 1, median: 10.5, p95: 19, max: 20 });
  assert.equal(stats([3, NaN, 1, 2], "u").n, 3);
});

const step = (t, name, mono) => ({ at: `t${mono}`, monoMs: mono, result: parsePsOutput(fx(name), PID) });
const options = { pid: PID, durationS: null, intervalS: 1, samples: 10, windowKind: "smoke" };

test("aggregation over a short run", () => {
  const t = createTracker(PID);
  t.add(step(0, "ps-ok.txt", 0));
  t.add(step(0, "ps-ok-later.txt", 1000));
  t.finish("completed");
  const report = buildReport({ platform: "darwin", options, startedAt: "a", endedAt: "b", tracker: t });
  assert.equal(report.termination, "completed");
  assert.deepStrictEqual(report.process, { pid: PID, start_time: "Mon Jan 5 10:00:00 2026" });
  assert.equal(report.samples[0].cpu_percent, null);
  assert.deepStrictEqual(report.samples[0].unavailable, ["footprint", "cpu_percent"]);
  assert.equal(report.samples[1].cpu_percent, 200);
  assert.equal(report.summary.samples_ok, 2);
  assert.equal(report.summary.rss_bytes.max, 12288 * 1024);
  assert.equal(report.summary.cpu_percent.n, 1);
  assert.equal(report.summary.unavailable_counts.cpu_percent, 1);
  assert.equal(report.summary.unavailable_counts.footprint, 2);
  assert.equal(report.summary.footprint_bytes.n, 0);
  assert.match(formatSummary(report), /rss: n=2 .* MiB/);
});

test("PID reuse stops the run and is not counted as a measurement", () => {
  const t = createTracker(PID);
  t.add(step(0, "ps-ok.txt", 0));
  const { stop, record } = t.add(step(0, "ps-reused.txt", 1000));
  assert.equal(stop, "pid_reused");
  assert.equal(record.status, "pid_reused");
  assert.equal(record.rss_bytes, null);
  t.finish("completed");
  const report = buildReport({ platform: "darwin", options, startedAt: "a", endedAt: "b", tracker: t });
  assert.equal(report.termination, "pid_reused");
  assert.equal(report.summary.samples_ok, 1);
});

test("vanished process terminates with process_exited", () => {
  const t = createTracker(PID);
  t.add(step(0, "ps-ok.txt", 0));
  assert.equal(t.add(step(0, "ps-empty.txt", 1000)).stop, "process_exited");
  assert.equal(t.finish("completed"), "process_exited");
});

test("malformed output tolerated twice, third consecutive stops; success resets", () => {
  const t = createTracker(PID);
  t.add(step(0, "ps-ok.txt", 0));
  assert.equal(t.add(step(0, "ps-malformed.txt", 1000)).stop, null);
  assert.equal(t.add(step(0, "ps-malformed.txt", 2000)).stop, null);
  t.add(step(0, "ps-ok-later.txt", 3000));
  assert.equal(t.add(step(0, "ps-malformed.txt", 4000)).stop, null);
  assert.equal(t.add(step(0, "ps-malformed.txt", 5000)).stop, null);
  assert.equal(t.add(step(0, "ps-malformed.txt", 6000)).stop, "unreadable");
  const report = buildReport({ platform: "darwin", options, startedAt: "a", endedAt: "b", tracker: t });
  assert.equal(report.summary.sample_status_counts.malformed, 5);
  assert.equal(report.summary.unavailable_counts.rss, 5);
});

test("failed samples retain parser reason for unavailable metrics", () => {
  const t = createTracker(PID);
  const result = parseProbeOutput(fx("probe-unavailable.json"), PID);
  const { record } = t.add({ at: "x", monoMs: 0, result });
  assert.deepStrictEqual(record.unavailable_reasons, {
    rss: "proc_pidinfo_errno_1",
    cpu_time: "proc_pidinfo_errno_1"
  });
});

test("windows tracker: null cpu time is recorded as unavailable", () => {
  const t = createTracker(PID);
  t.add({ at: "x", monoMs: 0, result: parsePowerShellOutput(fx("powershell-null-cpu.json"), PID) });
  assert.ok(t.records[0].unavailable.includes("cpu_time"));
  const report = buildReport({ platform: "win32", options, startedAt: "a", endedAt: "b", tracker: t });
  assert.match(report.metric_notes.rss_bytes, /WorkingSet64/);
});

// ---- Swift probe JSON ---------------------------------------------------------------------------

test("probe: ok sample from another PID reports Mach ports unavailable with a reason", () => {
  const r = parseProbeOutput(fx("probe-ok.json"), PID);
  assert.deepStrictEqual(r, {
    ok: true,
    sample: {
      pid: PID, startTime: "unix:1767607200.000042", rssBytes: 41943040, footprintBytes: 31457280, cpuSeconds: 1.5,
      machPortCount: null, unavailable: { mach_ports: "task_for_pid_not_allowed" }
    }
  });
});

test("probe: self sample carries the Mach port count", () => {
  const r = parseProbeOutput(fx("probe-self.json"), PID);
  assert.equal(r.sample.machPortCount, 57);
  assert.deepStrictEqual(r.sample.unavailable, {});
  assert.equal(r.sample.cpuSeconds, 3.5);
});

test("probe: reused, vanished, unavailable and malformed documents", () => {
  assert.deepStrictEqual(parseProbeOutput(fx("probe-reused.json"), PID), { ok: false, kind: "pid_reused", reason: "start_time_changed_during_sample" });
  assert.equal(parseProbeOutput(fx("probe-vanished.json"), PID).kind, "vanished");
  const u = parseProbeOutput(fx("probe-unavailable.json"), PID);
  assert.deepStrictEqual([u.kind, u.reason], ["unavailable", "proc_pidinfo_errno_1"]);
  assert.equal(parseProbeOutput(fx("probe-malformed.json"), PID).kind, "malformed");
  assert.equal(parseProbeOutput(fx("probe-ok.json"), 1).reason, "probe_pid_mismatch");
  assert.equal(parseProbeOutput("", PID).kind, "malformed");
  assert.equal(parseProbeOutput("{not json", PID).kind, "malformed");
  assert.equal(parseProbeOutput(null, PID).kind, "malformed");
});

test("probe samples flow into a report with footprint and no private bytes", () => {
  const t = createTracker(PID);
  t.add({ at: "a", monoMs: 0, result: parseProbeOutput(fx("probe-ok.json"), PID) });
  t.finish("completed");
  const report = buildReport({ platform: "darwin", options: { ...options, probeBin: "x" }, startedAt: "a", endedAt: "b", tracker: t });
  assert.equal(report.summary.footprint_bytes.max, 31457280);
  assert.equal(report.summary.private_bytes.n, 0);
  assert.equal(report.summary.unavailable_counts.mach_ports, 1);
  assert.match(report.metric_notes.rss_bytes, /ri_resident_size/);
  assert.match(formatSummary(report), /footprint: n=1/);
});

// ---- Windows private bytes, handles, start time before vs after ---------------------------------

test("powershell: private bytes, handles; GDI unavailable with a reason", () => {
  const r = parsePowerShellOutput(fx("powershell-full.json"), PID);
  assert.equal(r.sample.privateBytes, 15728640);
  assert.equal(r.sample.handleCount, 312);
  assert.equal(r.sample.gdiObjects, null);
  assert.deepStrictEqual(r.sample.unavailable, { gdi_objects: "gdi_requires_add_type_compile" });
  const t = createTracker(PID);
  t.add({ at: "x", monoMs: 0, result: r });
  assert.equal(t.records[0].private_bytes, 15728640);
  assert.equal(t.records[0].handle_count, 312);
  assert.deepStrictEqual(t.records[0].unavailable_reasons, r.sample.unavailable);
});

test("powershell: start time changed or missing after the sample", () => {
  const re = parsePowerShellOutput(fx("powershell-reused-after.json"), PID);
  assert.deepStrictEqual([re.ok, re.kind], [false, "pid_reused"]);
  const gone = parsePowerShellOutput(fx("powershell-exited-after.json"), PID);
  assert.deepStrictEqual([gone.ok, gone.kind], [false, "vanished"]);
  const t = createTracker(PID);
  t.add({ at: "x", monoMs: 0, result: parsePowerShellOutput(fx("powershell-full.json"), PID) });
  assert.equal(t.add({ at: "y", monoMs: 1000, result: re }).stop, "pid_reused");
});

// ---- reuse across samples (before vs after) -----------------------------------------------------

test("probe start time differing between samples stops the run as pid_reused", () => {
  const t = createTracker(PID);
  t.add({ at: "a", monoMs: 0, result: parseProbeOutput(fx("probe-ok.json"), PID) });
  const other = parseProbeOutput(fx("probe-ok.json").replace('"usec":42', '"usec":43'), PID);
  const { stop, record } = t.add({ at: "b", monoMs: 1000, result: other });
  assert.equal(stop, "pid_reused");
  assert.equal(record.footprint_bytes, null);
});

// ---- soak: arguments and streaming aggregation --------------------------------------------------

test("parseArgs soak rules", () => {
  assert.match(parseArgs(["--pid", "5", "--soak"]).error, /explicit --duration/);
  assert.match(parseArgs(["--pid", "5", "--soak", "--duration", "60"]).error, /--out/);
  assert.equal(parseArgs(["--pid", "5", "--soak", "--duration", "86401", "--out", "f"]).ok, false);
  assert.equal(parseArgs(["--pid", "5", "--duration", "3601"]).ok, false);
  const r = parseArgs(["--pid", "5", "--soak", "--duration", "86400", "--out", "f", "--probe-bin", "/p/cockpit-probe"]);
  assert.deepStrictEqual(r.options, {
    pid: 5, durationS: 86400, intervalS: 10, out: "f", probeBin: "/p/cockpit-probe", soak: true, samples: 8641, windowKind: "soak"
  });
});

test("stream stat: exact while small, bounded and sane when large", () => {
  const small = createStreamStat("u", { reservoirSize: 100 });
  for (let i = 1; i <= 20; i += 1) small.add(i);
  small.add(NaN);
  assert.deepStrictEqual(small.snapshot(), { unit: "u", n: 20, min: 1, mean: 10.5, median: 10.5, p95: 19, max: 20, quantiles: "exact", reservoir_size: 100 });
  const big = createStreamStat("u", { reservoirSize: 64 });
  const N = 100000;
  for (let i = 1; i <= N; i += 1) big.add(i);
  const snap = big.snapshot();
  assert.equal(big.retained, 64);
  assert.equal(snap.n, N);
  assert.equal(snap.min, 1);
  assert.equal(snap.max, N);
  assert.ok(Math.abs(snap.mean - (N + 1) / 2) < 1e-6);
  assert.equal(snap.quantiles, "reservoir_estimate");
  assert.ok(snap.median > N * 0.25 && snap.median < N * 0.75);
  assert.ok(snap.p95 > N * 0.75);
});

test("soak summary keeps memory bounded and tracker retains no records", () => {
  const t = createTracker(PID, { retain: false });
  const summary = createSummary({ reservoirSize: 16 });
  const total = 5000;
  for (let i = 0; i < total; i += 1) {
    const { record } = t.add({ at: `t${i}`, monoMs: i * 1000, result: parseProbeOutput(fx("probe-ok.json"), PID) });
    summary.add(record);
  }
  t.finish("completed");
  assert.equal(t.records.length, 0);
  assert.equal(t.count, total);
  assert.ok(summary.retained <= 16 * 8);
  const report = buildStreamingReport({ platform: "darwin", options: { ...options, soak: true, windowKind: "soak" }, startedAt: "a", endedAt: "b", tracker: t, summary });
  assert.equal(report.samples_streamed, total);
  assert.equal(report.samples, undefined);
  assert.equal(report.summary.samples_ok, total);
  assert.equal(report.summary.footprint_bytes.n, total);
  assert.equal(report.summary.footprint_bytes.mean, 31457280);
  assert.equal(report.summary.cpu_percent.n, total - 1);
  assert.equal(report.summary.interval_ms.n, total - 1);
  assert.equal(RESERVOIR_SIZE, 1024);
});
