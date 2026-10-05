#!/usr/bin/env node
// Thin runner for footprint-report.mjs. Read-only: samples ONE explicitly supplied PID with the OS
// process listing. Never starts/stops apps, installs anything, uses a shell, or needs admin rights.
// Usage: node scripts/probes/footprint-probe.mjs --pid PID [--duration S] [--interval S] [--out FILE]
import { execFile } from "node:child_process";
import { writeFileSync } from "node:fs";
import { performance } from "node:perf_hooks";
import {
  buildReport, createTracker, formatSummary, parseArgs, parsePowerShellOutput, parsePsOutput
} from "./footprint-report.mjs";

const EXEC_TIMEOUT_MS = 10_000;
const MAX_BUFFER = 64 * 1024;

const parsed = parseArgs(process.argv.slice(2));
if (!parsed.ok) {
  console.error(`footprint-probe: ${parsed.error}`);
  console.error("usage: footprint-probe.mjs --pid PID [--duration SECONDS (max 3600)] [--interval SECONDS] [--out FILE]");
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
    return { file: "ps", args: ["-o", "pid=,lstart=,rss=,time=", "-p", String(options.pid)], parse: parsePsOutput };
  }
  // pid is a validated integer, so interpolation cannot inject. SilentlyContinue makes a missing process print nothing.
  const script = `Get-Process -Id ${options.pid} -ErrorAction SilentlyContinue | Select Id,StartTime,WorkingSet64,TotalProcessorTime | ConvertTo-Json`;
  return { file: "powershell", args: ["-NoProfile", "-Command", script], parse: parsePowerShellOutput };
}

function sampleOnce(cmd) {
  return new Promise((resolve) => {
    execFile(cmd.file, cmd.args, { timeout: EXEC_TIMEOUT_MS, maxBuffer: MAX_BUFFER, windowsHide: true, encoding: "utf8", env: { ...process.env, LC_ALL: "C", LANG: "C" } }, (err, stdout) => {
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

const cmd = command();
const tracker = createTracker(options.pid);
const startedAt = new Date().toISOString();
const t0 = performance.now();
for (let i = 0; i < options.samples && !interrupted; i += 1) {
  const wait = t0 + i * options.intervalS * 1000 - performance.now();
  if (wait > 0) await sleep(wait);
  if (interrupted) break;
  const result = await sampleOnce(cmd);
  const { stop } = tracker.add({ at: new Date().toISOString(), monoMs: performance.now(), result });
  if (stop) break;
}
tracker.finish(interrupted ? "interrupted" : "completed");
const report = buildReport({ platform, options, startedAt, endedAt: new Date().toISOString(), tracker });
const json = `${JSON.stringify(report, null, 2)}\n`;
if (options.out) writeFileSync(options.out, json, { flag: "wx", mode: 0o600 });
else process.stdout.write(json);
console.error(formatSummary(report));
process.exit(report.termination === "completed" ? 0 : 1);
