// Dependency-free tests for upstream-report.mjs. Run: node --test scripts/upstream-report.test.mjs
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  buildLockReadFailure, buildReport, classifyPollError, exitCodeFor, parseLsRemote, validateDonor
} from "./upstream-report.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "fixtures", "upstream");
const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

for (const name of readdirSync(fixtures, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => e.name).sort()) {
  test(`fixture: ${name}`, () => {
    const input = readJson(join(fixtures, name, "input.json"));
    const expected = readJson(join(fixtures, name, "expected.json"));
    const polled = [];
    const poll = (donor) => {
      polled.push(donor.id);
      const entry = input.polls[donor.id];
      assert.ok(entry, `unexpected poll for ${donor.id}`);
      if (entry.throw) throw Object.assign(new Error("fixture failure"), entry.throw);
      return entry.stdout;
    };
    const report = buildReport({ checkedAt: input.checked_at, lock: input.lock, poll });
    assert.deepStrictEqual(report, expected);
    assert.equal(exitCodeFor(report), input.expected_exit_code);
    assert.deepStrictEqual(polled.sort(), Object.keys(input.polls).sort());
  });
}

test("parseLsRemote rejects conflicting duplicate lines and ignores other refs", () => {
  const a = "a".repeat(40), b = "b".repeat(40);
  assert.equal(parseLsRemote(`${a}\trefs/heads/main\n${b}\trefs/heads/main\n`, "refs/heads/main").error.code, "ambiguous_ref");
  assert.equal(parseLsRemote(`${a}\trefs/heads/other\n`, "refs/heads/main").error.code, "ref_not_found");
  assert.equal(parseLsRemote(`${a}\trefs/heads/main\n`, "refs/heads/main").commit, a);
});

test("validateDonor flags non-https repositories and option-like refs", () => {
  const pin = "a".repeat(40);
  assert.deepEqual(validateDonor({ id: "x", repository: "--upload-pack=x", poll_ref: "refs/heads/main", pin_commit: pin }), ["invalid_repository"]);
  assert.deepEqual(validateDonor({ id: "x", repository: "file:///tmp/r", poll_ref: "refs/heads/main", pin_commit: pin }), ["invalid_repository"]);
  assert.deepEqual(validateDonor({ id: "x", repository: "https://h.invalid/r.git", poll_ref: "-x", pin_commit: pin }), ["invalid_poll_ref"]);
  assert.deepEqual(validateDonor({ id: "x", repository: "https://h.invalid/r.git", poll_ref: "refs/heads/main", pin_commit: "short" }), ["invalid_pin"]);
  assert.deepEqual(validateDonor(null), ["donor_not_object"]);
});

test("classifyPollError and lock read failure are structured and bounded", () => {
  assert.equal(classifyPollError({ killed: true, signal: "SIGKILL" }).code, "timeout");
  assert.equal(classifyPollError({ code: "ENOENT" }).code, "git_unavailable");
  assert.equal(classifyPollError(new Error("x".repeat(1000))).detail.length <= 203, true);
  const report = buildLockReadFailure({ checkedAt: "t", detail: "boom" });
  assert.equal(report.error, "lock_read_failed");
  assert.equal(exitCodeFor(report), 2);
});

test("upstream-check keeps git bounded and shell-free", () => {
  const src = readFileSync(join(here, "upstream-check.mjs"), "utf8");
  assert.doesNotMatch(src, /shell:\s*true|\bexec\(|execSync/);
  assert.match(src, /timeout:/);
  assert.match(src, /maxBuffer:/);
});
