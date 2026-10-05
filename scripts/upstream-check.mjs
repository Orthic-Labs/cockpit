#!/usr/bin/env node
// Read-only upstream poller. Parsing and report building live in upstream-report.mjs.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildLockReadFailure, buildReport, exitCodeFor } from "./upstream-report.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const lockPath = resolve(root, "upstream.lock.json");
const checkedAt = new Date().toISOString();

function poll(donor) {
  const env = {
    ...process.env,
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_SYSTEM: "/dev/null",
    GIT_TERMINAL_PROMPT: "0",
    GCM_INTERACTIVE: "Never"
  };
  const args = [
    "-c", "credential.helper=",
    "-c", "core.askPass=",
    "ls-remote", "--refs", "--", donor.repository, donor.poll_ref
  ];
  return execFileSync("git", args, {
    cwd: root,
    env,
    encoding: "utf8",
    shell: false,
    stdio: ["ignore", "pipe", "ignore"],
    timeout: 15_000,
    killSignal: "SIGKILL",
    maxBuffer: 64 * 1024
  });
}

let report;
let lock;
try {
  lock = JSON.parse(readFileSync(lockPath, "utf8"));
} catch (error) {
  report = buildLockReadFailure({
    checkedAt,
    detail: error instanceof Error ? error.message : String(error)
  });
}
report ??= buildReport({ checkedAt, lock, poll });
console.log(JSON.stringify(report, null, 2));
process.exitCode = exitCodeFor(report);
