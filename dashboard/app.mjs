/*
 * Cockpit's static dashboard. It accepts only an explicitly chosen local JSON
 * file; it never starts a scan, calls a network, or applies a cleanup plan.
 */

export const MAX_FILE_BYTES = 10 * 1024 * 1024;
export const MAX_ENTRIES = 100_000;
export const MAX_RENDER_ROWS = 500;

const EMPTY_RECLAIM = { lowerBytes: 0, upperBytes: null, state: "unknown", reasons: [] };

function objectOrEmpty(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : {};
}

function asNumber(value) {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && value.trim() !== "" && Number.isFinite(Number(value))) return Number(value);
  return null;
}

function asBytes(value) {
  const number = asNumber(value);
  return number !== null && number >= 0 ? number : null;
}

function pathText(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.join("/");
  return value === null || value === undefined ? "" : String(value);
}

function normalizedPath(value) {
  const raw = pathText(value).replaceAll("\\", "/");
  if (/^[A-Za-z]:\/+\s*$/.test(raw)) return raw[0] + ":/";
  return raw.replace(/\/+$/g, "").replace(/^\.(?=\/)/, "") || "/";
}

export function pathBase(value) {
  const path = normalizedPath(value);
  if (path === "/") return "/";
  const trimmed = path.replace(/\/$/, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || trimmed;
}

export function pathParent(value) {
  const path = normalizedPath(value);
  if (path === "/") return "/";
  if (/^[A-Za-z]:\/$/.test(path)) return path;
  const index = path.lastIndexOf("/");
  if (index <= 0) return path.startsWith("/") ? "/" : ".";
  if (index === 2 && /^[A-Za-z]:/.test(path)) return path.slice(0, 3);
  return path.slice(0, index);
}

export function pathContains(parent, child, includeSelf = true) {
  const parentPath = normalizedPath(parent);
  const left = parentPath === "/" ? "/" : parentPath.replace(/\/$/, "");
  const right = normalizedPath(child).replace(/\/$/, "");
  if (left === right) return includeSelf;
  if (left === "/") return right.startsWith("/");
  return right.startsWith(`${left}/`);
}

function directChild(parent, child) {
  if (!pathContains(parent, child, false)) return false;
  return pathParent(child) === normalizedPath(parent).replace(/\/$/, "") || (normalizedPath(parent) === "/" && pathParent(child) === "/");
}

function kindText(value) {
  const text = pathText(value).toLowerCase();
  if (text.includes("dir")) return "directory";
  if (text.includes("link")) return "symlink";
  if (text === "other") return "other";
  return "file";
}

function normalizeReclaim(value) {
  const source = objectOrEmpty(value);
  const lower = asBytes(source.lower_bytes ?? source.lowerBytes) ?? 0;
  const upperValue = source.upper_bytes ?? source.upperBytes;
  const upper = upperValue === null || upperValue === undefined ? null : asBytes(upperValue);
  const state = pathText(source.state).toLowerCase() || (upper === null ? "unknown" : "bounded");
  const reasons = Array.isArray(source.reasons) ? source.reasons.map(pathText).filter(Boolean) : [];
  return { lowerBytes: lower, upperBytes: upper, state, reasons };
}

function normalizeEntry(entry) {
  const source = objectOrEmpty(entry);
  const metadata = objectOrEmpty(source.metadata);
  const path = normalizedPath(source.path);
  const logicalBytes = asBytes(source.logical_bytes ?? source.logicalBytes ?? metadata.logical_size ?? metadata.logicalSize) ?? 0;
  const attributedBytes = asBytes(source.attributed_allocation_bytes ?? source.attributedAllocationBytes ?? metadata.allocation_size ?? metadata.allocationSize);
  const volume = objectOrEmpty(metadata.volume).id ?? source.volume_id ?? source.volumeId ?? "unknown";
  return {
    raw: entry,
    path,
    name: pathBase(path),
    kind: kindText(metadata.kind ?? source.kind),
    logicalBytes,
    attributedBytes,
    volume: pathText(volume) || "unknown",
    fileId: objectOrEmpty(metadata.file_id ?? metadata.fileId).id ?? null,
    cloneId: objectOrEmpty(metadata.clone_id ?? metadata.cloneId).id ?? null,
    placeholder: Boolean(metadata.is_placeholder ?? metadata.isPlaceholder),
    complete: metadata.metadata_complete !== false && source.metadata_complete !== false,
    owner: source.accounting_owner ?? source.accountingOwner ?? null,
    reclaim: source.reclaim ? normalizeReclaim(source.reclaim) : null,
  };
}

function normalizeFolder(folder) {
  const source = objectOrEmpty(folder);
  const volume = objectOrEmpty(source.volume).id ?? source.volume_id ?? source.volumeId ?? "unknown";
  return {
    raw: folder,
    path: normalizedPath(source.path),
    name: pathBase(source.path),
    volume: pathText(volume) || "unknown",
    logicalBytes: asBytes(source.logical_bytes ?? source.logicalBytes) ?? 0,
    attributedBytes: asBytes(source.attributed_allocation_bytes ?? source.attributedAllocationBytes),
    incomplete: Boolean(source.incomplete),
  };
}

function unwrapInput(input) {
  const top = objectOrEmpty(input);
  const scan = objectOrEmpty(top.scan);
  const source = Object.keys(scan).length ? scan : top;
  const snapshot = objectOrEmpty(source.snapshot);
  const report = objectOrEmpty(snapshot.report ?? source.report ?? top.report ?? objectOrEmpty(source.data).report);
  const findings = snapshot.findings ?? source.findings ?? top.findings ?? [];
  const modules = objectOrEmpty(top.modules ?? source.modules ?? snapshot.modules);
  return { top, source, snapshot, report, findings, modules };
}

export function normalizeScan(input, options = {}) {
  const { top, source, snapshot, report, findings, modules } = unwrapInput(input);
  if (!Object.keys(report).length) throw new Error("JSON has no scan report");
  const roots = Array.isArray(report.roots) ? report.roots.map(normalizedPath).filter(Boolean) : [];
  const entriesRaw = Array.isArray(report.entries) ? report.entries : [];
  const entries = entriesRaw.slice(0, MAX_ENTRIES).map(normalizeEntry);
  const folders = (Array.isArray(report.folders) ? report.folders : []).slice(0, MAX_ENTRIES).map(normalizeFolder);
  const accounting = objectOrEmpty(report.accounting);
  const usage = Array.isArray(report.volume_usage ?? report.volumeUsage) ? (report.volume_usage ?? report.volumeUsage).map(objectOrEmpty) : [];
  const omittedFromPayload = asNumber(top.entries_omitted ?? source.entries_omitted ?? source.entriesOmitted) ?? 0;
  const omittedByBound = Math.max(0, entriesRaw.length - entries.length);
  const reasons = Array.isArray(report.incomplete_reasons ?? report.incompleteReasons) ? (report.incomplete_reasons ?? report.incompleteReasons).map(pathText).filter(Boolean) : [];
  return {
    raw: input,
    schemaVersion: snapshot.schema_version ?? source.schema_version ?? top.schema_version ?? null,
    snapshotId: snapshot.id ?? source.snapshot_id ?? top.snapshot_id ?? null,
    createdAt: snapshot.created_at ?? snapshot.createdAt ?? source.created_at ?? null,
    roots,
    entries,
    folders,
    findings: Array.isArray(findings) ? findings.slice(0, MAX_ENTRIES).map(objectOrEmpty) : [],
    accounting: {
      ...accounting,
      logicalBytes: asBytes(accounting.logical_bytes ?? accounting.logicalBytes),
      attributedBytes: asBytes(accounting.attributed_allocation_bytes ?? accounting.attributedAllocationBytes),
      discrepancyBytes: asNumber(accounting.signed_discrepancy_bytes ?? accounting.signedDiscrepancyBytes),
      reclaim: normalizeReclaim(accounting.reclaim),
      incomplete: Boolean(accounting.incomplete),
    },
    usage,
    deltas: Array.isArray(report.volume_deltas ?? report.volumeDeltas) ? report.volume_deltas ?? report.volumeDeltas : [],
    inspectionErrors: Array.isArray(report.inspection_errors ?? report.inspectionErrors) ? report.inspection_errors ?? report.inspectionErrors : [],
    skippedLinks: Array.isArray(report.skipped_links ?? report.skippedLinks) ? report.skipped_links ?? report.skippedLinks : [],
    incompleteReasons: reasons,
    modules,
    limits: {
      loadedBytes: options.loadedBytes ?? null,
      entriesLoaded: entries.length,
      entriesReported: entriesRaw.length,
      entriesOmitted: omittedFromPayload + omittedByBound,
    },
  };
}

function sum(values) { return values.reduce((total, value) => total + (asNumber(value) ?? 0), 0); }

export function deriveAccounting(scan) {
  const report = scan?.accounting ?? {};
  const logicalBytes = report.logicalBytes ?? sum((scan?.entries ?? []).map((entry) => entry.logicalBytes));
  const attributedBytes = report.attributedBytes ?? sum((scan?.entries ?? []).map((entry) => entry.attributedBytes));
  const totalBytes = sum((scan?.usage ?? []).map((item) => item.total_bytes ?? item.totalBytes));
  const usedBytes = sum((scan?.usage ?? []).map((item) => item.used_bytes ?? item.usedBytes));
  const availableBytes = sum((scan?.usage ?? []).map((item) => item.available_bytes ?? item.availableBytes));
  const availableKnown = (scan?.usage ?? []).length > 0 && (scan.usage ?? []).every((item) => asBytes(item.available_bytes ?? item.availableBytes) !== null);
  return {
    logicalBytes,
    attributedBytes,
    totalBytes: totalBytes || null,
    usedBytes: usedBytes || null,
    availableBytes: availableKnown ? availableBytes : null,
    discrepancyBytes: report.discrepancyBytes ?? null,
    incomplete: Boolean(report.incomplete || scan?.incompleteReasons?.length || scan?.limits?.entriesOmitted),
    reclaim: report.reclaim ?? EMPTY_RECLAIM,
  };
}

export function filterEntries(entries, filters = {}) {
  const name = pathText(filters.name).trim().toLowerCase();
  const extension = pathText(filters.extension).trim().toLowerCase().replace(/^\./, "");
  const kind = pathText(filters.kind).trim().toLowerCase();
  const min = filters.minBytes === "" || filters.minBytes === undefined ? null : asBytes(filters.minBytes);
  const max = filters.maxBytes === "" || filters.maxBytes === undefined ? null : asBytes(filters.maxBytes);
  return (entries ?? []).filter((entry) => {
    if (name && !`${entry.name} ${entry.path}`.toLowerCase().includes(name)) return false;
    if (extension && (entry.kind !== "file" || !entry.name.toLowerCase().endsWith(`.${extension}`))) return false;
    if (kind && entry.kind !== kind) return false;
    const size = entry.attributedBytes ?? entry.logicalBytes;
    if (min !== null && (size === null || size < min)) return false;
    if (max !== null && (size === null || size > max)) return false;
    return true;
  });
}

function folderFromEntries(scan, basePath) {
  const grouped = new Map();
  for (const entry of scan.entries) {
    if (!pathContains(basePath, entry.path, false)) continue;
    if (directChild(basePath, entry.path) && entry.kind !== "directory") continue;
    let child = entry.path;
    while (!directChild(basePath, child)) child = pathParent(child);
    if (!child || child === ".") continue;
    const current = grouped.get(child) ?? { path: child, name: pathBase(child), logicalBytes: 0, attributedBytes: 0, incomplete: false, volume: entry.volume, derived: true };
    current.logicalBytes += entry.logicalBytes;
    current.attributedBytes += entry.attributedBytes ?? 0;
    current.incomplete ||= !entry.complete || entry.attributedBytes === null;
    grouped.set(child, current);
  }
  return [...grouped.values()];
}

export function buildStorageModel(scan, currentPath = null) {
  const basePath = currentPath ? normalizedPath(currentPath) : null;
  const selectedFolders = scan.folders.filter((folder) => !basePath ? scan.roots.some((root) => directChild(root, folder.path)) : directChild(basePath, folder.path));
  const derivedFolders = basePath ? folderFromEntries(scan, basePath) : scan.roots.flatMap((root) => folderFromEntries(scan, root));
  const sourceFolders = selectedFolders.length ? selectedFolders : derivedFolders;
  const folders = sourceFolders.filter((folder) => folder.path !== basePath).sort((a, b) => (b.attributedBytes ?? b.logicalBytes ?? 0) - (a.attributedBytes ?? a.logicalBytes ?? 0));
  const directEntries = scan.entries.filter((entry) => !basePath ? scan.roots.includes(entry.path) : directChild(basePath, entry.path));
  const visibleEntries = directEntries.sort((a, b) => (b.attributedBytes ?? b.logicalBytes ?? 0) - (a.attributedBytes ?? a.logicalBytes ?? 0));
  const knownBytes = sum(folders.map((folder) => folder.attributedBytes ?? folder.logicalBytes));
  const unknownCount = folders.filter((folder) => folder.attributedBytes === null || folder.incomplete).length;
  return {
    basePath,
    folders,
    entries: visibleEntries,
    mapFolders: folders.slice(0, 24),
    knownBytes,
    unknownCount,
    hasMore: folders.length > 24,
  };
}

export function formatBytes(value) {
  if (value === null || value === undefined || !Number.isFinite(Number(value))) return "Unknown";
  const bytes = Number(value);
  if (Math.abs(bytes) < 1024) return `${Math.round(bytes)} B`;
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let size = Math.abs(bytes);
  let index = -1;
  while (size >= 1024 && index < units.length - 1) { size /= 1024; index += 1; }
  const sign = bytes < 0 ? "−" : "";
  return `${sign}${size.toFixed(size >= 100 ? 0 : size >= 10 ? 1 : 2)} ${units[index]}`;
}

export function formatCount(value) {
  if (value === null || value === undefined) return "Unknown";
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 }).format(value);
}

function moduleFor(scan, view) {
  const aliases = { duplicates: ["duplicates", "duplicate_groups"], apps: ["apps"], monitor: ["monitor", "resources"], activity: ["activity"], compress: ["compress", "compression"] };
  for (const key of aliases[view] ?? [view]) {
    if (scan.modules?.[key] !== undefined) return scan.modules[key];
  }
  return null;
}

function moduleItems(module) {
  if (Array.isArray(module)) return module;
  if (module && Array.isArray(module.items)) return module.items;
  if (module && Array.isArray(module.rows)) return module.rows;
  if (module && Array.isArray(module.groups)) return module.groups;
  return [];
}

function text(value, fallback = "Unknown") { return value === null || value === undefined || value === "" ? fallback : pathText(value); }

function node(tag, options = {}, children = []) {
  const element = document.createElement(tag);
  for (const [key, value] of Object.entries(options)) {
    if (value === null || value === undefined || value === false) continue;
    if (key === "className") element.className = value;
    else if (key === "textContent") element.textContent = value;
    else if (key === "dataset") Object.entries(value).forEach(([name, item]) => { element.dataset[name] = item; });
    else if (key === "checked") element.checked = Boolean(value);
    else if (key === "value") element.value = value;
    else if (key === "disabled") element.disabled = Boolean(value);
    else if (key.startsWith("aria")) element.setAttribute(key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`), value);
    else element.setAttribute(key, value);
  }
  for (const child of children) element.append(child instanceof Node ? child : document.createTextNode(String(child)));
  return element;
}

function card(title, subtitle, children, className = "card") {
  return node("section", { className }, [
    node("div", { className: "card-header" }, [node("div", {}, [node("h3", { textContent: title }), subtitle ? node("p", { textContent: subtitle }) : ""])]),
    node("div", { className: "card-body" }, children),
  ]);
}

function metaRow(label, value) {
  return node("div", { className: "meta-row" }, [node("dt", { className: "meta-label", textContent: label }), node("dd", { className: "meta-value", textContent: text(value) })]);
}

function statistic(label, value, note) {
  return node("div", { className: "card stat-card" }, [node("p", { className: "stat-label", textContent: label }), node("p", { className: "stat-value", textContent: value }), node("p", { className: "stat-note", textContent: note })]);
}

function pathCrumbs(path, onRoot) {
  const wrap = node("div", { className: "crumbs", "aria-label": "Storage path" });
  wrap.append(node("button", { className: "crumb", type: "button", dataset: { navigate: "" }, textContent: "All roots" }));
  if (!path) return wrap;
  const parts = normalizedPath(path).split("/").filter(Boolean);
  let built = normalizedPath(path).startsWith("/") ? "/" : "";
  parts.forEach((part, index) => {
    const separator = node("span", { className: "crumb-separator", ariaHidden: "true", textContent: "/" });
    wrap.append(separator);
    built = built === "/" ? `/${part}` : built ? `${built}/${part}` : part;
    wrap.append(node("button", { className: "crumb", type: "button", dataset: { navigate: built }, textContent: part, "aria-current": index === parts.length - 1 ? "location" : null }));
  });
  return wrap;
}

function notice(scan) {
  const accounting = deriveAccounting(scan);
  const reasons = [...scan.incompleteReasons];
  if (scan.limits.entriesOmitted) reasons.push(`${formatCount(scan.limits.entriesOmitted)} entries omitted from loaded result`);
  if (!accounting.incomplete && !reasons.length) return null;
  const uniqueReasons = [...new Set(reasons)];
  return node("div", { className: "notice", role: "status" }, [
    node("div", { className: "notice-icon", ariaHidden: "true", textContent: "!" }),
    node("div", {}, [node("strong", { textContent: "Accounting is incomplete" }), node("p", { textContent: uniqueReasons.length ? uniqueReasons.join(" · ") : "Some metadata or volume readings are unavailable. Unknown values stay unknown." })]),
  ]);
}

function entryRow(entry, action = "inspect") {
  const size = entry.attributedBytes ?? entry.logicalBytes;
  return node("tr", {}, [
    node("td", {}, [node("button", { className: "row-button", type: "button", dataset: { [action]: entry.path }, ariaLabel: `Inspect ${entry.path}` }, [node("div", { className: "path-name", textContent: entry.name }), node("div", { className: "path-secondary", textContent: entry.path })])]),
    node("td", { className: "number", textContent: formatBytes(size) }),
    node("td", { className: "muted", textContent: entry.kind }),
    node("td", { className: entry.complete ? "" : "faint", textContent: entry.complete ? "Complete" : "Partial" }),
  ]);
}

function folderRow(folder) {
  const size = folder.attributedBytes ?? folder.logicalBytes;
  return node("tr", {}, [
    node("td", {}, [node("button", { className: "row-button", type: "button", dataset: { navigate: folder.path }, ariaLabel: `Open ${folder.path}` }, [node("div", { className: "path-name", textContent: folder.name }), node("div", { className: "path-secondary", textContent: folder.path })]), node("button", { className: "button button-quiet", type: "button", dataset: { inspect: folder.path }, textContent: "Inspect" })]),
    node("td", { className: "number", textContent: formatBytes(size) }),
    node("td", { className: folder.incomplete || folder.attributedBytes === null ? "faint" : "", textContent: folder.incomplete || folder.attributedBytes === null ? "Unknown bound" : "Attributed" }),
  ]);
}

function table(headers, rows, emptyText) {
  const tableNode = node("table", { className: "data-table" });
  tableNode.append(node("thead", {}, [node("tr", {}, headers.map((header) => node("th", { scope: "col", textContent: header })))]));
  const body = node("tbody");
  if (!rows.length) body.append(node("tr", {}, [node("td", { colSpan: headers.length, className: "muted", textContent: emptyText })]));
  else rows.forEach((row) => body.append(row));
  tableNode.append(body);
  return tableNode;
}

function renderInspector(scan, selectedPath) {
  const entry = scan.entries.find((item) => item.path === selectedPath);
  const folder = scan.folders.find((item) => item.path === selectedPath);
  if (!entry && !folder) return card("Inspector", "Select a row to inspect metadata", [node("p", { className: "inspector-empty", textContent: "Choose a folder or entry from Storage or Find. Dashboard selection stays local to this page." })], "card inspector");
  const item = entry ?? folder;
  const isEntry = Boolean(entry);
  const size = item.attributedBytes ?? item.logicalBytes;
  const rows = [metaRow("Path", item.path), metaRow("Kind", isEntry ? item.kind : "folder"), metaRow("Logical size", formatBytes(item.logicalBytes)), metaRow("Allocation", formatBytes(item.attributedBytes)), metaRow("Volume", item.volume)];
  if (isEntry) rows.push(metaRow("Metadata", item.complete ? "Complete" : "Partial"), metaRow("Placeholder", item.placeholder ? "Yes" : "No"), metaRow("Reclaim bound", item.reclaim ? (item.reclaim.upperBytes === null ? `${formatBytes(item.reclaim.lowerBytes)}+ · unknown upper` : `${formatBytes(item.reclaim.lowerBytes)}–${formatBytes(item.reclaim.upperBytes)}`) : "Not supplied"));
  else rows.push(metaRow("Accounting", item.incomplete ? "Incomplete" : "Attributed"));
  return node("section", { className: "card inspector" }, [node("div", { className: "card-header" }, [node("h3", { textContent: "Inspector" })]), node("div", { className: "inspector-body" }, [node("div", { className: "inspector-title", textContent: item.path }), node("dl", { className: "meta-list" }, rows)])]);
}

function renderStorage(scan, state) {
  const accounting = deriveAccounting(scan);
  const model = buildStorageModel(scan, state.path);
  const mapBytes = sum(model.mapFolders.map((folder) => folder.attributedBytes ?? folder.logicalBytes));
  const mapParts = model.mapFolders.map((folder) => {
    const size = folder.attributedBytes ?? folder.logicalBytes;
    const percentage = mapBytes > 0 ? Math.max(4, (size / mapBytes) * 100) : 100 / Math.max(1, model.mapFolders.length);
    return node("button", { className: "map-segment", type: "button", style: `flex-grow:${percentage}`, dataset: { navigate: folder.path }, ariaLabel: `Open ${folder.path}, ${formatBytes(size)}` }, [node("span", { className: "segment-label", textContent: folder.name })]);
  });
  if (model.unknownCount) mapParts.push(node("div", { className: "map-unknown", title: "Some folder bounds are unknown", textContent: `${model.unknownCount} unknown` }));
  const note = notice(scan);
  const rows = [statistic("Logical scanned", formatBytes(accounting.logicalBytes), "metadata sum"), statistic("Attributed allocation", formatBytes(accounting.attributedBytes), "shared bytes counted once"), statistic("Volume used", formatBytes(accounting.usedBytes), accounting.usedBytes === null ? "volume reading unavailable" : "provider reading"), statistic("Reclaim lower bound", formatBytes(accounting.reclaim.lowerBytes), accounting.reclaim.upperBytes === null ? "upper bound unknown" : `up to ${formatBytes(accounting.reclaim.upperBytes)}`)];
  return [
    node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: "Storage" }), node("h2", { textContent: state.path ? pathBase(state.path) : "Where space lives" }), node("p", { textContent: "Browse scanner-attributed bytes by folder, then inspect individual metadata rows." })]), node("div", { className: "head-actions" }, [node("span", { className: "tag", textContent: `${formatCount(scan.entries.length)} entries loaded` })])]),
    note,
    node("div", { className: "stats-grid" }, rows),
    node("div", { className: "storage-grid" }, [
      node("div", { className: "storage-main" }, [
        node("section", { className: "card" }, [node("div", { className: "card-header" }, [node("div", {}, [node("h3", { textContent: "Storage map" }), node("p", { textContent: state.path ? `Direct children of ${state.path}` : "Top-level folders across selected roots" })])]), node("div", { className: "map-wrap" }, [pathCrumbs(state.path), node("div", { className: "storage-map", role: "list", "aria-label": "Folder size map" }, mapParts.length ? mapParts : [node("div", { className: "map-unknown", textContent: "No folder totals" })]), node("div", { className: "map-legend" }, [node("span", { textContent: `${formatBytes(model.knownBytes)} shown` }), node("span", { textContent: model.hasMore ? "Map capped at 24 folders" : "Bounded to supplied rows" })])])]),
        card("Largest folders", "Select a folder to drill down", [node("div", { className: "table-wrap" }, [table(["Folder", "Attributed", "State"], model.folders.slice(0, MAX_RENDER_ROWS).map(folderRow), "No folder accounting supplied for this level.")])]),
        card("Entries in view", "Direct children only · ranked by observed allocation", [node("div", { className: "table-wrap" }, [table(["Entry", "Size", "Kind", "Metadata"], model.entries.slice(0, MAX_RENDER_ROWS).map((entry) => entryRow(entry)), "No direct entries supplied for this level.")])]),
      ]),
      renderInspector(scan, state.selected),
    ]),
  ].filter(Boolean);
}

function renderFind(scan, state) {
  const filtered = filterEntries(scan.entries, state.filters);
  const visible = filtered.slice(0, MAX_RENDER_ROWS);
  const form = node("form", { className: "filters", id: "find-form" }, [
    node("div", { className: "field" }, [node("label", { for: "find-name", textContent: "Name or path" }), node("input", { id: "find-name", name: "name", type: "search", placeholder: "e.g. screenshots", value: state.filters.name })]),
    node("div", { className: "field" }, [node("label", { for: "find-extension", textContent: "Extension" }), node("input", { id: "find-extension", name: "extension", type: "text", placeholder: "pdf", value: state.filters.extension })]),
    node("div", { className: "field" }, [node("label", { for: "find-kind", textContent: "Kind" }), node("select", { id: "find-kind", name: "kind" }, [node("option", { value: "", textContent: "Any kind" }), ...["file", "directory", "symlink", "other"].map((kind) => node("option", { value: kind, selected: state.filters.kind === kind, textContent: kind }))])]),
    node("div", { className: "field" }, [node("label", { for: "find-min", textContent: "Min bytes" }), node("input", { id: "find-min", name: "minBytes", type: "number", min: "0", inputMode: "numeric", value: state.filters.minBytes })]),
    node("div", { className: "field" }, [node("label", { for: "find-max", textContent: "Max bytes" }), node("input", { id: "find-max", name: "maxBytes", type: "number", min: "0", inputMode: "numeric", value: state.filters.maxBytes })]),
    node("button", { className: "button button-quiet", type: "button", dataset: { clearFilters: "" }, textContent: "Clear" }),
  ]);
  const countText = filtered.length > MAX_RENDER_ROWS ? `Showing first ${formatCount(MAX_RENDER_ROWS)} of ${formatCount(filtered.length)} matches` : `${formatCount(filtered.length)} matches`;
  return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: "Find" }), node("h2", { textContent: "Search supplied entries" }), node("p", { textContent: "Filter loaded metadata without touching filesystem contents." })])]), card("Filters", "All filters are local to this imported scan", [form]), node("div", { className: "results-bar" }, [node("span", { textContent: countText }), node("span", { className: "faint", textContent: scan.limits.entriesOmitted ? "Loaded result is bounded" : "" })]), node("section", { className: "card" }, [node("div", { className: "table-wrap" }, [table(["Entry", "Size", "Kind", "Metadata"], visible.map((entry) => entryRow(entry)), "No entries match these filters.")])])];
}

function capability(title, reason) {
  return node("div", { className: "capability" }, [node("div", { className: "capability-inner" }, [node("span", { className: "tag", textContent: "Capability pending" }), node("h2", { textContent: title }), node("p", { textContent: reason }), node("p", { className: "faint", textContent: "This panel will render supplied versioned module data when core exports it. No sample telemetry is shown." })])]);
}

function moduleView(scan, view, title, description) {
  const module = moduleFor(scan, view);
  if (!module) return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: title }), node("h2", { textContent: description })])]), capability(`${title} data is not in this scan`, `Imported envelope contains storage report data only. ${title} remains read-only until an explicit module is supplied.`)];
  const items = moduleItems(module).slice(0, MAX_RENDER_ROWS);
  if (module.capability === "unavailable") return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: title }), node("h2", { textContent: description })])]), capability(`${title} capability unavailable`, text(module.reason, "Source did not provide this module."))];
  const rows = items.map((item) => {
    const source = objectOrEmpty(item);
    const name = source.name ?? source.title ?? source.path ?? source.id ?? "Record";
    const detail = [source.path, source.size_bytes ?? source.sizeBytes, source.status, source.state].filter((value) => value !== undefined && value !== null).map((value) => typeof value === "number" ? formatBytes(value) : pathText(value)).join(" · ");
    return node("div", { className: "module-row" }, [node("div", { className: "module-name", textContent: name }), node("div", { className: "module-detail", textContent: detail || "Supplied record" })]);
  });
  return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: title }), node("h2", { textContent: description }), node("p", { textContent: "Rendered from explicit imported module data." })])]), card(`${title} records`, `${formatCount(items.length)} rows shown · capped at ${formatCount(MAX_RENDER_ROWS)}`, [node("div", { className: "module-list" }, rows.length ? rows : [node("p", { className: "muted", textContent: "Module supplied no rows." })])])];
}

function normalizeFinding(finding) {
  const source = objectOrEmpty(finding);
  const reclaim = normalizeReclaim(source.reclaim);
  return { raw: finding, id: text(source.id, "Finding"), path: normalizedPath(source.path ?? source.item_path ?? source.itemPath), rule: text(source.rule_id ?? source.ruleId, "Rule not supplied"), eligible: Boolean(source.eligible), route: text(source.route, "Report only"), reason: text((Array.isArray(source.reasons) && source.reasons[0]) ?? source.reason, "Evidence requires review"), reclaim };
}

function renderCleanup(scan, state) {
  const findings = scan.findings.map(normalizeFinding);
  const stagedCount = state.staged.size;
  const findingNodes = findings.slice(0, MAX_RENDER_ROWS).map((finding) => node("div", { className: "finding" }, [node("input", { type: "checkbox", checked: state.staged.has(finding.id), dataset: { stageFinding: finding.id }, ariaLabel: `Stage ${finding.path}` }), node("div", {}, [node("div", { className: "finding-path", textContent: finding.path }), node("p", { className: "finding-note", textContent: `${finding.rule} · ${finding.reason}` })]), node("div", { className: "finding-right" }, [node("span", { className: finding.eligible ? "tag warning" : "tag", textContent: finding.eligible ? "Review" : "Not eligible" }), node("span", { className: "number muted", textContent: finding.reclaim.upperBytes === null ? `${formatBytes(finding.reclaim.lowerBytes)}+` : formatBytes(finding.reclaim.upperBytes) })]) ]));
  if (!findings.length) return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: "Cleanup" }), node("h2", { textContent: "Review queue" }), node("p", { textContent: "Findings are staged locally for inspection." })])]), capability("No findings supplied", "Core report-only findings are absent from imported scan. Nothing is eligible or staged.")];
  return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: "Cleanup" }), node("h2", { textContent: "Review queue" }), node("p", { textContent: "Select findings for a local review set. Staging never applies cleanup." })]), node("div", { className: "head-actions" }, [node("span", { className: "tag warning", textContent: `${formatCount(stagedCount)} staged locally` })])]), node("div", { className: "notice", role: "status" }, [node("div", { className: "notice-icon", ariaHidden: "true", textContent: "i" }), node("div", {}, [node("strong", { textContent: "Staging is not applied" }), node("p", { textContent: "This dashboard has no delete, trash, uninstall, or process controls. Review remains report-only." })])]), node("section", { className: "card" }, [node("div", { className: "card-header" }, [node("div", {}, [node("h3", { textContent: "Findings" }), node("p", { textContent: `${formatCount(findings.length)} supplied · report-only mode` })])]), node("div", { className: "card-body" }, [node("div", { className: "finding-list" }, findingNodes)])])];
}

function renderApp() {
  if (typeof document === "undefined") return;
  const view = document.querySelector("#app-view");
  view.replaceChildren();
  if (!state.scan) {
    view.append(node("div", { className: "empty-state" }, [node("div", { className: "empty-state-inner" }, [node("div", { className: "empty-glyph", ariaHidden: "true", textContent: "▦" }), node("p", { className: "eyebrow", textContent: "Local scan viewer" }), node("h2", { textContent: "Import a Cockpit scan to begin" }), node("p", { textContent: "Choose a bounded JSON export from Cockpit core. Dashboard reads metadata only, keeps unknown accounting visible, and never reaches the network." }), node("button", { className: "button button-primary", type: "button", dataset: { openFile: "" }, textContent: "Choose scan JSON" })]) ]));
    return;
  }
  const renderers = { storage: () => renderStorage(state.scan, state), find: () => renderFind(state.scan, state), cleanup: () => renderCleanup(state.scan, state), duplicates: () => moduleView(state.scan, "duplicates", "Duplicates", "Compare duplicate candidates"), apps: () => moduleView(state.scan, "apps", "Apps", "Review installed app data"), monitor: () => moduleView(state.scan, "monitor", "Monitor", "Inspect resource readings"), activity: () => moduleView(state.scan, "activity", "Activity", "Inspect recorded activity"), compress: () => moduleView(state.scan, "compress", "Compress", "Review compression candidates") };
  view.append(...(renderers[state.view] ? renderers[state.view]() : renderers.storage()));
}

const state = { scan: null, view: "storage", path: null, selected: null, filters: { name: "", extension: "", kind: "", minBytes: "", maxBytes: "" }, staged: new Set(), error: null };

function setStatus(message, tone = "neutral") {
  const status = document.querySelector("#scan-status");
  status.textContent = message;
  status.dataset.tone = tone;
}

function openFilePicker() { document.querySelector("#scan-file").click(); }

async function importFile(file) {
  if (!file) return;
  if (file.size > MAX_FILE_BYTES) { setStatus("File exceeds 10 MB limit", "danger"); return; }
  setStatus("Reading scan…", "warning");
  try {
    const parsed = JSON.parse(await file.text());
    state.scan = normalizeScan(parsed, { loadedBytes: file.size });
    state.view = "storage";
    state.path = null;
    state.selected = null;
    state.staged = new Set();
    setStatus(`Loaded ${formatCount(state.scan.entries.length)} entries`, "good");
    renderApp();
  } catch (error) {
    setStatus("Could not load scan JSON", "danger");
    const view = document.querySelector("#app-view");
    view.replaceChildren(node("div", { className: "empty-state" }, [node("div", { className: "empty-state-inner" }, [node("div", { className: "empty-glyph", ariaHidden: "true", textContent: "!" }), node("p", { className: "eyebrow", textContent: "Import error" }), node("h2", { textContent: "Scan JSON was not accepted" }), node("p", { className: "error-text", textContent: error instanceof Error ? error.message : "Invalid JSON" }), node("button", { className: "button button-primary", type: "button", dataset: { openFile: "" }, textContent: "Choose another file" })]) ]));
  }
}

function formFilters(form) {
  const data = new FormData(form);
  return { name: data.get("name") ?? "", extension: data.get("extension") ?? "", kind: data.get("kind") ?? "", minBytes: data.get("minBytes") ?? "", maxBytes: data.get("maxBytes") ?? "" };
}

function syncNav() {
  document.querySelectorAll(".nav-item").forEach((item) => {
    const active = item.dataset.view === state.view;
    item.classList.toggle("is-active", active);
    if (active) item.setAttribute("aria-current", "page"); else item.removeAttribute("aria-current");
  });
}

function boot() {
  document.querySelector("#import-scan").addEventListener("click", openFilePicker);
  document.querySelector("#scan-file").addEventListener("change", (event) => importFile(event.target.files?.[0]));
  document.querySelector("#section-nav").addEventListener("click", (event) => {
    const button = event.target.closest("[data-view]");
    if (!button) return;
    state.view = button.dataset.view;
    state.path = null;
    state.selected = null;
    syncNav();
    renderApp();
  });
  document.querySelector("#app-view").addEventListener("click", (event) => {
    const open = event.target.closest("[data-open-file]");
    if (open) { openFilePicker(); return; }
    const clear = event.target.closest("[data-clear-filters]");
    if (clear) { state.filters = { name: "", extension: "", kind: "", minBytes: "", maxBytes: "" }; renderApp(); return; }
    const stage = event.target.closest("[data-stage-finding]");
    if (stage) { if (stage.checked) state.staged.add(stage.dataset.stageFinding); else state.staged.delete(stage.dataset.stageFinding); renderApp(); return; }
    const navigate = event.target.closest("[data-navigate]");
    if (navigate) { state.path = navigate.dataset.navigate || null; state.selected = null; renderApp(); return; }
    const inspect = event.target.closest("[data-inspect]");
    if (inspect) { state.selected = inspect.dataset.inspect; renderApp(); return; }
  });
  document.querySelector("#app-view").addEventListener("submit", (event) => {
    if (event.target.id !== "find-form") return;
    event.preventDefault();
    state.filters = formFilters(event.target);
    renderApp();
  });
  syncNav();
  renderApp();
}

export function importScanJson(input, options = {}) {
  state.scan = normalizeScan(input, options);
  state.view = "storage";
  state.path = null;
  state.selected = null;
  state.staged = new Set();
  if (typeof document !== "undefined") {
    setStatus("Loaded " + formatCount(state.scan.entries.length) + " entries", "good");
    syncNav();
    renderApp();
  }
  return state.scan;
}

export function getDashboardState() {
  return {
    view: state.view,
    path: state.path,
    selected: state.selected,
    stagedFindingIds: [...state.staged],
    scan: state.scan,
  };
}

if (typeof globalThis !== "undefined") {
  globalThis.CockpitDashboard = { importScan: importScanJson, importScanJson, loadScan: importScanJson, getState: getDashboardState };
  globalThis.loadCockpitScan = importScanJson;
}

if (typeof document !== "undefined") boot();
