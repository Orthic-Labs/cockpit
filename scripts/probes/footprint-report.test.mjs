// Dependency-free tests for footprint-report.mjs. Run: node --test scripts/probes/footprint-report.test.mjs
// Fixtures are synthetic format samples, not measurements.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  buildReport, cpuPercent, createTracker, formatSummary, parseArgs, parsePowerShellOutput,
  parsePsCpuTime, parsePsOutput, stats
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
  assert.deepStrictEqual(r.options, { pid: 77, durationS: null, intervalS: 1, out: null, samples: 10, windowKind: "smoke" });
  const d = parseArgs(["--pid", "77", "--duration", "3600", "--interval", "2"]);
  assert.equal(d.options.samples, 1801);
  assert.equal(d.options.windowKind, "duration");
});

test("ps parsing", () => {
  const r = parsePsOutput(fx("ps-ok.txt"), PID);
  assert.deepStrictEqual(r, { ok: true, sample: { pid: PID, startTime: "Mon Jan 5 10:00:00 2026", rssBytes: 10240 * 1024, cpuSeconds: 1.5 } });
  assert.equal(parsePsOutput(fx("ps-ok-days.txt"), PID).sample.cpuSeconds, 86400 + 2 * 3600 + 3 * 60 + 4.25);
  assert.equal(parsePsCpuTime("12:34.5"), 754.5);
  assert.equal(parsePsCpuTime("nope"), null);
});

test("powershell parsing: ISO (7.x) and legacy 5.1 shapes", () => {
  const a = parsePowerShellOutput(fx("powershell-ok.json"), PID);
  assert.deepStrictEqual(a.sample, { pid: PID, startTime: "2026-01-05T10:00:00.000Z", rssBytes: 20971520, cpuSeconds: 1.5 });
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
  assert.deepStrictEqual(report.samples[0].unavailable, ["cpu_percent"]);
  assert.equal(report.samples[1].cpu_percent, 200);
  assert.equal(report.summary.samples_ok, 2);
  assert.equal(report.summary.rss_bytes.max, 12288 * 1024);
  assert.equal(report.summary.cpu_percent.n, 1);
  assert.equal(report.summary.unavailable_counts.cpu_percent, 1);
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

test("windows tracker: null cpu time is recorded as unavailable", () => {
  const t = createTracker(PID);
  t.add({ at: "x", monoMs: 0, result: parsePowerShellOutput(fx("powershell-null-cpu.json"), PID) });
  assert.deepStrictEqual(t.records[0].unavailable, ["cpu_time"]);
  const report = buildReport({ platform: "win32", options, startedAt: "a", endedAt: "b", tracker: t });
  assert.match(report.metric_notes.rss_bytes, /WorkingSet64/);
});
