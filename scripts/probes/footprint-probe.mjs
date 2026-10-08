#!/usr/bin/env node
// Thin runner for footprint-report.mjs. Read-only: samples ONE explicitly supplied PID with the OS
// process listing. Never starts/stops apps, installs anything, uses a shell, or needs admin rights.
// Usage: node scripts/probes/footprint-probe.mjs --pid PID [--duration S] [--interval S] [--out FILE]
//        [--probe-bin PATH]  (macOS: explicit pulse-probe executable; otherwise ps, RSS only)
//        [--soak --duration S --out FILE]  (long run, S up to 86400; NDJSON stream, bounded memory)
import { execFile } from "node:child_process";
import { accessSync, closeSync, constants, openSync, statSync, writeFileSync, writeSync } from "node:fs";
import { resolve } from "node:path";
import { performance } from "node:perf_hooks";
import {
  SCHEMA_VERSION, buildReport, buildStreamingReport, createSummary, createTracker, formatSummary, parseArgs,
  parsePowerShellOutput, parseProbeOutput, parsePsOutput
} from "./footprint-report.mjs";

const EXEC_TIMEOUT_MS = 10_000;
const MAX_BUFFER = 64 * 1024;

const parsed = parseArgs(process.argv.slice(2));
if (!parsed.ok) {
  console.error(`footprint-probe: ${parsed.error}`);
  console.error("usage: footprint-probe.mjs --pid PID [--duration SECONDS (max 3600)] [--interval SECONDS] [--out FILE] [--probe-bin PATH] [--soak]");
  process.exit(2);
}
const options = parsed.options;
const platform = process.platform;
if (platform !== "darwin" && platform !== "win32") {
  console.error(`footprint-probe: unsupported platform ${platform} (macOS and Windows only)`);
  process.exit(2);
}

function command() {
  if (platform === "darwin") {
    if (options.probeBin) return { file: probePath, args: ["--pid", String(options.pid)], parse: parseProbeOutput, parseOnError: true };
    return { file: "ps", args: ["-o", "pid=,lstart=,rss=,time=", "-p", String(options.pid)], parse: parsePsOutput };
  }
  // pid is a validated integer, so interpolation cannot inject. SilentlyContinue makes a missing process print nothing.
  // StartTime is read first and again (fresh process object) after the other properties; the parser compares them.
  // GDI objects need GetGuiResources via Add-Type, which this probe does not compile; the parser reports them unavailable.
  const n = options.pid;
  const script = [
    "$ErrorActionPreference='SilentlyContinue'",
    `$p=Get-Process -Id ${n} -ErrorAction SilentlyContinue`,
    "if($p){$b=$p.StartTime;$ws=$p.WorkingSet64;$pm=$p.PrivateMemorySize64;$hc=$p.HandleCount;$cpu=$p.TotalProcessorTime;" +
      `$a=(Get-Process -Id ${n} -ErrorAction SilentlyContinue).StartTime;` +
      "[pscustomobject]@{Id=$p.Id;StartTime=$b;WorkingSet64=$ws;PrivateMemorySize64=$pm;HandleCount=$hc;TotalProcessorTime=$cpu;StartTimeAfter=$a}|ConvertTo-Json -Compress}"
  ].join(";");
  return { file: "powershell", args: ["-NoProfile", "-Command", script], parse: parsePowerShellOutput };
}

function sampleOnce(cmd) {
  return new Promise((resolve) => {
    execFile(cmd.file, cmd.args, { timeout: EXEC_TIMEOUT_MS, maxBuffer: MAX_BUFFER, windowsHide: true, encoding: "utf8", env: { ...process.env, LC_ALL: "C", LANG: "C" } }, (err, stdout) => {
      if (err && cmd.parseOnError && String(stdout ?? "").trim() !== "") return resolve(cmd.parse(stdout, options.pid));
      if (err) {
        // ps exits 1 with no output when the PID does not exist; treat as an empty (vanished) listing.
        if (err.code === 1 && String(stdout ?? "").trim() === "") return resolve(cmd.parse("", options.pid));
        const reason = err.killed ? "probe_timeout" : `probe_error_${typeof err.code === "string" || typeof err.code === "number" ? err.code : "unknown"}`;
        return resolve({ ok: false, kind: "unavailable", reason });
      }
      resolve(cmd.parse(stdout, options.pid));
    });
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let interrupted = false;
process.on("SIGINT", () => { interrupted = true; });

let probePath = null;
if (options.probeBin) {
  if (platform !== "darwin") { console.error("footprint-probe: --probe-bin is only supported on macOS"); process.exit(2); }
  probePath = resolve(options.probeBin);
  try {
    if (!statSync(probePath).isFile()) throw new Error("not a regular file");
    accessSync(probePath, constants.X_OK);
  } catch {
    console.error("footprint-probe: --probe-bin must be an executable regular file");
    process.exit(2);
  }
}
const soak = options.soak;
const cmd = command();
const tracker = createTracker(options.pid, { retain: !soak });
const summary = soak ? createSummary() : null;
let fd = null;
let outputFailed = false;
const emit = (obj) => {
  try { writeSync(fd, `${JSON.stringify(obj)}\n`); } catch { outputFailed = true; }
};
const startedAt = new Date().toISOString();
if (soak) {
  try { fd = openSync(options.out, "wx", 0o600); } catch { console.error("footprint-probe: cannot create --out (it must not already exist)"); process.exit(2); }
  emit({
    kind: "pulse.footprint.header", schema_version: SCHEMA_VERSION, platform, target_pid: options.pid, started_at: startedAt,
    probe: options.probeBin ? "pulse-probe" : platform === "win32" ? "powershell" : "ps",
    requested_duration_s: options.durationS, requested_interval_s: options.intervalS
  });
}
const t0 = performance.now();
for (let i = 0; i < options.samples && !interrupted; i += 1) {
  const wait = t0 + i * options.intervalS * 1000 - performance.now();
  if (wait > 0) await sleep(wait);
  if (interrupted) break;
  const result = await sampleOnce(cmd);
  const { record, stop } = tracker.add({ at: new Date().toISOString(), monoMs: performance.now(), result });
  if (soak) {
    summary.add(record);
    emit({ kind: "pulse.footprint.sample", ...record });
    if (outputFailed) { tracker.finish("output_error"); break; }
  }
  if (stop) break;
}
tracker.finish(interrupted ? "interrupted" : "completed");
const endedAt = new Date().toISOString();
if (soak) {
  const report = buildStreamingReport({ platform, options, startedAt, endedAt, tracker, summary });
  emit({ kind: "pulse.footprint.summary", ...report });
  closeSync(fd);
  console.error(formatSummary(report));
  process.exit(report.termination === "completed" && !outputFailed ? 0 : 1);
}
const report = buildReport({ platform, options, startedAt, endedAt, tracker });
const json = `${JSON.stringify(report, null, 2)}\n`;
if (options.out) writeFileSync(options.out, json, { flag: "wx", mode: 0o600 });
else process.stdout.write(json);
console.error(formatSummary(report));
process.exit(report.termination === "completed" ? 0 : 1);
