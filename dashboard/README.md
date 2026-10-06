# Cockpit dashboard

Static, dependency-free storage viewer for local Cockpit scan JSON. Open `index.html` in a WebView or browser, then choose a JSON export with **Import scan JSON**. No network request is made, scan is never started from page, and no cleanup/process action is exposed.

Native host bridge can inject one bounded response after module load:

```js
window.CockpitDashboard.importScan(json);
// aliases: loadScan(json), getState()
```

`importScan` accepts either CLI output:

```json
{
  "schema_version": 1,
  "snapshot": {
    "id": "snapshot-id",
    "created_at": "2026-10-05T00:00:00Z",
    "report": {
      "roots": ["/Users/example"],
      "entries": [],
      "folders": [],
      "accounting": {},
      "volume_usage": [],
      "incomplete_reasons": []
    },
    "findings": []
  }
}
```

or worker output:

```json
{ "schema_version": 1, "report": { "...": "ScanReport" }, "entries_omitted": 0 }
```

An optional versioned envelope may add `modules` beside `snapshot` or `report`. Dashboard renders supplied `duplicates`, `apps`, `monitor`/`resources`, `activity`, and `compress`/`compression` rows. Missing modules are labelled **Capability pending**; no placeholder telemetry is generated.

Storage uses `report.folders` when present, falls back to bounded aggregation from entries, and preserves logical bytes, attributed allocation, reclaim bounds, volume readings, incomplete reasons, and omitted counts separately. Find filters loaded entry metadata by name/path, extension, kind, and byte bounds. Cleanup findings are a local review staging set only; staged does not mean applied.

Safety bounds are enforced in page code:

- imported JSON file: 10 MiB maximum;
- entries retained: 100,000 maximum;
- visible table/module rows: 500 maximum;
- storage map segments: 24 maximum.

`app.test.mjs` covers normalization for CLI/worker/envelope shapes, accounting, path-aware drilldown, filter predicates, formatting, and omission bounds. Root gate can run it with Node.
