#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

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
    "ls-remote", "--refs", donor.repository, donor.poll_ref
  ];
  const raw = execFileSync("git", args, {
    cwd: root,
    env,
    encoding: "utf8",
    timeout: 15_000,
    maxBuffer: 64 * 1024
  });
  const line = raw.trim().split("\n").find(Boolean);
  const [commit, ref] = line ? line.trim().split(/\s+/, 2) : [];
  if (!/^[0-9a-f]{40}$/.test(commit ?? "") || ref !== donor.poll_ref) {
    throw new Error("upstream returned no matching ref");
  }
  return commit;
}

let lock;
try {
  lock = JSON.parse(readFileSync(lockPath, "utf8"));
} catch (error) {
  console.log(JSON.stringify({
    schema: 1,
    checked_at: checkedAt,
    read_only: true,
    error: "lock_read_failed",
    detail: error instanceof Error ? error.message : String(error)
  }, null, 2));
  process.exitCode = 2;
}

if (lock) {
  const donors = Array.isArray(lock.donors) ? lock.donors : [];
  const results = donors.map((donor) => {
    try {
      const head = poll(donor);
      return {
        id: donor.id,
        repository: donor.repository,
        poll_ref: donor.poll_ref,
        pinned_commit: donor.pin_commit,
        observed_head: head,
        changed: head !== donor.pin_commit,
        status: head === donor.pin_commit ? "pinned" : "changed"
      };
    } catch (error) {
      return {
        id: donor.id,
        repository: donor.repository,
        poll_ref: donor.poll_ref,
        pinned_commit: donor.pin_commit,
        observed_head: null,
        changed: null,
        status: "error",
        error: error instanceof Error ? error.message : String(error)
      };
    }
  });
  console.log(JSON.stringify({
    schema: 1,
    checked_at: checkedAt,
    read_only: true,
    lock_path: "upstream.lock.json",
    changed: results.some((result) => result.changed === true),
    donors: results
  }, null, 2));
}
