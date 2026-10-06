import assert from "node:assert/strict";
import {
  buildStorageModel,
  deriveAccounting,
  filterEntries,
  formatBytes,
  normalizeScan,
  pathBase,
  pathContains,
  pathParent,
} from "./app.mjs";

const report = {
  roots: ["/Users/test"],
  folders: [
    { path: "/Users/test/Library", volume: { id: "disk-a" }, logical_bytes: 800, attributed_allocation_bytes: 700, incomplete: false },
    { path: "/Users/test/Projects", volume: { id: "disk-a" }, logical_bytes: 400, attributed_allocation_bytes: 390, incomplete: false },
    { path: "/Users/test/Unknown", volume: { id: "disk-a" }, logical_bytes: 200, attributed_allocation_bytes: 200, incomplete: true },
  ],
  entries: [
    { path: "/Users/test/Library/cache.db", metadata: { kind: "File", volume: { id: "disk-a" }, logical_size: 500, allocation_size: 450, metadata_complete: true }, logical_bytes: 500, attributed_allocation_bytes: 450 },
    { path: "/Users/test/Projects/readme.md", metadata: { kind: "File", volume: { id: "disk-a" }, logical_size: 20, allocation_size: 20, metadata_complete: true }, logical_bytes: 20, attributed_allocation_bytes: 20 },
    { path: "/Users/test/Projects/src", metadata: { kind: "Directory", volume: { id: "disk-a" }, metadata_complete: true }, logical_bytes: 380, attributed_allocation_bytes: 370 },
  ],
  accounting: {
    logical_bytes: 1200,
    attributed_allocation_bytes: 1090,
    reclaim: { lower_bytes: 90, upper_bytes: 120, state: "Bounded", reasons: [] },
    incomplete: true,
  },
  volume_usage: [{ volume: { id: "disk-a" }, total_bytes: 5000, used_bytes: 3000, available_bytes: 2000 }],
  incomplete_reasons: ["permission denied: /Users/test/Unknown"],
};

const scan = normalizeScan({ schema_version: 1, snapshot: { id: "snap-1", report, findings: [] } });

assert.equal(scan.schemaVersion, 1);
assert.equal(scan.entries.length, 3);
assert.equal(scan.folders.length, 3);
assert.equal(scan.limits.entriesOmitted, 0);
assert.equal(pathBase("/Users/test/Library/cache.db"), "cache.db");
assert.equal(pathParent("/Users/test/Library/cache.db"), "/Users/test/Library");
assert.equal(pathContains("/Users/test", "/Users/test/Library/cache.db"), true);
assert.equal(pathContains("/Users/test", "/Users/other/cache.db"), false);

const accounting = deriveAccounting(scan);
assert.deepEqual(accounting, {
  logicalBytes: 1200,
  attributedBytes: 1090,
  totalBytes: 5000,
  usedBytes: 3000,
  availableBytes: 2000,
  discrepancyBytes: null,
  incomplete: true,
  reclaim: { lowerBytes: 90, upperBytes: 120, state: "bounded", reasons: [] },
});

const rootModel = buildStorageModel(scan);
assert.deepEqual(rootModel.folders.map((folder) => folder.path), ["/Users/test/Library", "/Users/test/Projects", "/Users/test/Unknown"]);
assert.equal(rootModel.unknownCount, 1);
const projectsModel = buildStorageModel(scan, "/Users/test/Projects");
assert.equal(projectsModel.entries.length, 2);
assert.equal(projectsModel.entries[0].path, "/Users/test/Projects/src");

assert.equal(filterEntries(scan.entries, { extension: "md" }).length, 1);
assert.equal(filterEntries(scan.entries, { kind: "directory" }).length, 1);
assert.equal(filterEntries(scan.entries, { minBytes: 400 }).length, 1);
assert.equal(filterEntries(scan.entries, { name: "cache" })[0].path, "/Users/test/Library/cache.db");
assert.equal(formatBytes(1024 * 1024), "1.00 MB");

const workerEnvelope = normalizeScan({ report, entries_omitted: 4 });
assert.equal(workerEnvelope.entries.length, 3);
assert.equal(workerEnvelope.limits.entriesOmitted, 4);

const versionedEnvelope = normalizeScan({
  schema_version: 1,
  scan: { snapshot: { report, findings: [{ id: "finding-1", path: "/Users/test/Library/cache.db" }] }, modules: { apps: [{ name: "Example" }] } },
});
assert.equal(versionedEnvelope.findings.length, 1);
assert.equal(versionedEnvelope.modules.apps[0].name, "Example");

console.log("dashboard model tests passed");
