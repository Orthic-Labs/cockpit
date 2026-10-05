// Pure parsing and report construction for upstream-check.mjs.
// No I/O, no child_process, no clock: callers inject checkedAt and a poll function.

export const REPORT_SCHEMA = 1;
const COMMIT_RE = /^[0-9a-f]{40}$/;
const ID_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const REF_RE = /^refs\/(heads|tags)\/[A-Za-z0-9._\/+-]+$/;
const REPO_RE = /^https:\/\/[A-Za-z0-9.-]+(:\d+)?\/[A-Za-z0-9._~\/-]+$/;
const MAX_DETAIL = 200;

export function isCommit(value) {
  return typeof value === "string" && COMMIT_RE.test(value);
}

export function isSafeRef(value) {
  return (
    typeof value === "string" &&
    value.length <= 255 &&
    REF_RE.test(value) &&
    !value.includes("..") &&
    !value.includes("//") &&
    !value.endsWith("/") &&
    !value.endsWith(".") &&
    !value.endsWith(".lock") &&
    !value.split("/").some((part) => part.startsWith("."))
  );
}

export function isSafeRepository(value) {
  return typeof value === "string" && value.length <= 300 && REPO_RE.test(value) && !value.includes("..");
}

function bounded(text) {
  const clean = String(text ?? "").replace(/[\u0000-\u001f\u007f]+/g, " ").trim();
  return clean.length > MAX_DETAIL ? `${clean.slice(0, MAX_DETAIL)}...` : clean;
}

// Returns array of problem codes for one donor entry (empty when usable).
export function validateDonor(donor) {
  if (donor === null || typeof donor !== "object" || Array.isArray(donor)) return ["donor_not_object"];
  const problems = [];
  if (typeof donor.id !== "string" || !ID_RE.test(donor.id)) problems.push("invalid_id");
  if (!isSafeRepository(donor.repository)) problems.push("invalid_repository");
  if (!isSafeRef(donor.poll_ref)) problems.push("invalid_poll_ref");
  if (donor.pin_commit === undefined || donor.pin_commit === null || donor.pin_commit === "") {
    problems.push("missing_pin");
  } else if (!isCommit(donor.pin_commit)) {
    problems.push("invalid_pin");
  }
  return problems;
}

// Parse `git ls-remote --refs <repo> <ref>` stdout. Returns { commit } or { error: {code, detail} }.
export function parseLsRemote(raw, pollRef) {
  if (typeof raw !== "string") return { error: { code: "malformed_output", detail: "output was not text" } };
  const lines = raw.split(/\r?\n/).filter((line) => line.trim() !== "");
  if (lines.length === 0) return { error: { code: "ref_not_found", detail: "upstream returned no matching ref" } };
  const matches = [];
  for (const line of lines) {
    const parts = line.split("\t");
    if (parts.length !== 2 || !isCommit(parts[0]) || !parts[1].startsWith("refs/")) {
      return { error: { code: "malformed_output", detail: "unparseable ls-remote line" } };
    }
    if (parts[1] === pollRef) matches.push(parts[0]);
  }
  if (matches.length === 0) return { error: { code: "ref_not_found", detail: "upstream returned no matching ref" } };
  if (new Set(matches).size > 1) return { error: { code: "ambiguous_ref", detail: "conflicting commits for ref" } };
  return { commit: matches[0] };
}

// Map a thrown subprocess error (from execFile/execFileSync) to a structured failure.
export function classifyPollError(error) {
  const e = error && typeof error === "object" ? error : {};
  if (e.code === "ETIMEDOUT" || (e.killed === true && typeof e.signal === "string")) {
    return { code: "timeout", detail: "git ls-remote exceeded time limit" };
  }
  if (e.code === "ERR_CHILD_PROCESS_STDIO_MAXBUFFER" || e.code === "ENOBUFS") {
    return { code: "output_too_large", detail: "git ls-remote output exceeded buffer limit" };
  }
  if (e.code === "ENOENT") return { code: "git_unavailable", detail: "git executable not found" };
  if (typeof e.status === "number") return { code: "git_failed", detail: `git exited with status ${e.status}` };
  if (typeof e.code === "string") return { code: "spawn_failed", detail: bounded(e.code) };
  return { code: "poll_failed", detail: bounded(e.message ?? error) };
}

function base(donor) {
  const pick = (v) => (typeof v === "string" ? bounded(v) : null);
  return {
    id: pick(donor?.id),
    repository: pick(donor?.repository),
    poll_ref: pick(donor?.poll_ref),
    pinned_commit: isCommit(donor?.pin_commit) ? donor.pin_commit : null
  };
}

function failure(donor, status, code, detail) {
  return { ...base(donor), observed_head: null, changed: null, status, error_code: code, error: detail };
}

export function buildDonorResult(donor, outcome) {
  if (outcome.error) return failure(donor, "error", outcome.error.code, outcome.error.detail);
  const changed = outcome.commit !== donor.pin_commit;
  return { ...base(donor), observed_head: outcome.commit, changed, status: changed ? "changed" : "pinned" };
}

// poll(donor) must return ls-remote stdout text or throw. Called only for valid, unique donors.
export function buildReport({ checkedAt, lock, poll }) {
  const head = { schema: REPORT_SCHEMA, checked_at: checkedAt, read_only: true };
  if (lock === null || typeof lock !== "object" || Array.isArray(lock) || !Array.isArray(lock.donors)) {
    return { ...head, ok: false, error: "lock_invalid", detail: "lock must be an object with a donors array" };
  }
  const counts = new Map();
  for (const d of lock.donors) {
    if (d && typeof d.id === "string") counts.set(d.id, (counts.get(d.id) ?? 0) + 1);
  }
  const results = lock.donors.map((donor) => {
    const problems = validateDonor(donor);
    if (donor && counts.get(donor.id) > 1) problems.push("duplicate_id");
    if (problems.length > 0) {
      return failure(donor, "invalid", problems[0], problems.join(","));
    }
    let outcome;
    try {
      outcome = parseLsRemote(poll(donor), donor.poll_ref);
    } catch (error) {
      outcome = { error: classifyPollError(error) };
    }
    return buildDonorResult(donor, outcome);
  });
  const count = (s) => results.filter((r) => r.status === s).length;
  const failed = count("error") + count("invalid");
  return {
    ...head,
    lock_path: "upstream.lock.json",
    ok: failed === 0 && results.length > 0,
    partial: failed > 0 && failed < results.length,
    changed: results.some((r) => r.changed === true),
    summary: { total: results.length, pinned: count("pinned"), changed: count("changed"), error: count("error"), invalid: count("invalid") },
    donors: results
  };
}

export function buildLockReadFailure({ checkedAt, detail }) {
  return {
    schema: REPORT_SCHEMA,
    checked_at: checkedAt,
    read_only: true,
    ok: false,
    error: "lock_read_failed",
    detail: bounded(detail)
  };
}

// 0 clean; 1 some donor failed/invalid or empty; 2 lock unreadable/invalid.
export function exitCodeFor(report) {
  if (report.error) return 2;
  return report.ok ? 0 : 1;
}
