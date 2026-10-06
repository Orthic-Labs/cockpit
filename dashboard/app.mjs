/*
 * Cockpit's static dashboard. It accepts explicit local JSON & delegates
 * bounded, confirmation-backed actions to native host when available.
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
  const logicalBytes = asBytes(source.logical_bytes ?? source.logicalBytes ?? source.logical_size ?? source.logicalSize ?? metadata.logical_size ?? metadata.logicalSize) ?? 0;
  const attributedBytes = asBytes(source.attributed_allocation_bytes ?? source.attributedAllocationBytes ?? source.attributed_allocation_size ?? source.attributedAllocationSize ?? metadata.allocation_size ?? metadata.allocationSize);
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
    logicalBytes: asBytes(source.logical_bytes ?? source.logicalBytes ?? source.logical_size ?? source.logicalSize) ?? 0,
    attributedBytes: asBytes(source.attributed_allocation_bytes ?? source.attributedAllocationBytes ?? source.attributed_allocation_size ?? source.attributedAllocationSize ?? source.allocation_size ?? source.allocationSize),
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

const MODULE_ALIASES = { storage: ["storage"], growth: ["folder_growth", "folderGrowth", "growth"], duplicates: ["duplicates", "duplicate_groups"], apps: ["apps", "app_footprint"], monitor: ["monitor", "resources"], activity: ["activity"], compress: ["compress", "compression"] };
function moduleFor(scan, view) { for (const key of MODULE_ALIASES[view] ?? [view]) if (scan.modules?.[key] !== undefined) return scan.modules[key]; return null; }
function modulePayload(module) { const source = objectOrEmpty(module); return Object.keys(objectOrEmpty(source.data)).length ? source.data : module; }
function moduleItems(module, keys = ["items", "rows", "groups", "events", "apps", "processes"]) { const value = modulePayload(module); if (Array.isArray(value)) return value; for (const key of keys) if (Array.isArray(value?.[key])) return value[key]; return []; }
function text(value, fallback = "Unknown") { return value === null || value === undefined || value === "" ? fallback : pathText(value); }
function numberText(value, fallback = "Unknown") { const parsed = asNumber(value); return parsed === null ? fallback : formatCount(parsed); }
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
function card(title, subtitle, children, className = "card") { return node("section", { className }, [node("div", { className: "card-header" }, [node("div", {}, [node("h3", { textContent: title }), subtitle ? node("p", { textContent: subtitle }) : ""])]), node("div", { className: "card-body" }, children)]); }
function metaRow(label, value) { return node("div", { className: "meta-row" }, [node("dt", { className: "meta-label", textContent: label }), node("dd", { className: "meta-value", textContent: text(value) })]); }
function statistic(label, value, note) { return node("div", { className: "card stat-card" }, [node("p", { className: "stat-label", textContent: label }), node("p", { className: "stat-value", textContent: value }), node("p", { className: "stat-note", textContent: note })]); }
function pathCrumbs(path) { const wrap = node("div", { className: "crumbs", "aria-label": "Storage path" }); wrap.append(node("button", { className: "crumb", type: "button", dataset: { navigate: "" }, textContent: "All roots" })); if (!path) return wrap; const parts = normalizedPath(path).split("/").filter(Boolean); let built = normalizedPath(path).startsWith("/") ? "/" : ""; parts.forEach((part, index) => { wrap.append(node("span", { className: "crumb-separator", ariaHidden: "true", textContent: "/" })); built = built === "/" ? `/${part}` : built ? `${built}/${part}` : part; wrap.append(node("button", { className: "crumb", type: "button", dataset: { navigate: built }, textContent: part, "aria-current": index === parts.length - 1 ? "location" : null })); }); return wrap; }
function notice(scan) { const accounting = deriveAccounting(scan); const reasons = [...scan.incompleteReasons]; if (scan.limits.entriesOmitted) reasons.push(`${formatCount(scan.limits.entriesOmitted)} entries omitted from loaded result`); if (!accounting.incomplete && !reasons.length) return null; return node("div", { className: "notice", role: "status" }, [node("div", { className: "notice-icon", ariaHidden: "true", textContent: "!" }), node("div", {}, [node("strong", { textContent: "Accounting is incomplete" }), node("p", { textContent: [...new Set(reasons)].join(" · ") || "Some metadata or volume readings are unavailable. Unknown values stay unknown." })])]); }
function table(headers, rows, emptyText, className = "data-table") { const tableNode = node("table", { className }); tableNode.append(node("thead", {}, [node("tr", {}, headers.map((header) => node("th", { scope: "col", textContent: header })))])); const body = node("tbody"); if (!rows.length) body.append(node("tr", {}, [node("td", { colSpan: headers.length, className: "muted", textContent: emptyText })])); else rows.forEach((row) => body.append(row)); tableNode.append(body); return tableNode; }
function entryRow(entry, action = "inspect") { const size = entry.attributedBytes ?? entry.logicalBytes; return node("tr", {}, [node("td", {}, [node("button", { className: "row-button", type: "button", dataset: { [action]: entry.path }, ariaLabel: `Inspect ${entry.path}` }, [node("div", { className: "path-name", textContent: entry.name }), node("div", { className: "path-secondary", textContent: entry.path })])]), node("td", { className: "number", textContent: formatBytes(size) }), node("td", { className: "muted", textContent: entry.kind }), node("td", { className: entry.complete ? "" : "faint", textContent: entry.complete ? "Complete" : "Partial" })]); }
function folderRow(folder) { const size = folder.attributedBytes ?? folder.logicalBytes; return node("tr", {}, [node("td", {}, [node("button", { className: "row-button", type: "button", dataset: { navigate: folder.path }, ariaLabel: `Open ${folder.path}` }, [node("div", { className: "path-name", textContent: folder.name }), node("div", { className: "path-secondary", textContent: folder.path })]), node("button", { className: "button button-quiet", type: "button", dataset: { inspect: folder.path }, textContent: "Inspect" })]), node("td", { className: "number", textContent: formatBytes(size) }), node("td", { className: folder.incomplete || folder.attributedBytes === null ? "faint" : "", textContent: folder.incomplete || folder.attributedBytes === null ? "Unknown bound" : "Attributed" })]); }
function legacyRenderInspector(scan, selectedPath) { const entry = scan.entries.find((item) => item.path === selectedPath); const folder = scan.folders.find((item) => item.path === selectedPath); if (!entry && !folder) return card("Inspector", "Select a row to inspect metadata", [node("p", { className: "inspector-empty", textContent: "Choose a folder or entry from Storage or Find. Dashboard selection stays local to this page." })], "card inspector"); const item = entry ?? folder; const isEntry = Boolean(entry); const rows = [metaRow("Path", item.path), metaRow("Kind", isEntry ? item.kind : "folder"), metaRow("Logical size", formatBytes(item.logicalBytes)), metaRow("Allocation", formatBytes(item.attributedBytes)), metaRow("Volume", item.volume)]; if (isEntry) rows.push(metaRow("Metadata", item.complete ? "Complete" : "Partial"), metaRow("Placeholder", item.placeholder ? "Yes" : "No"), metaRow("Reclaim bound", item.reclaim ? (item.reclaim.upperBytes === null ? `${formatBytes(item.reclaim.lowerBytes)}+ · unknown upper` : `${formatBytes(item.reclaim.lowerBytes)}–${formatBytes(item.reclaim.upperBytes)}`) : "Not supplied")); else rows.push(metaRow("Accounting", item.incomplete ? "Incomplete" : "Attributed")); return node("section", { className: "card inspector" }, [node("div", { className: "card-header" }, [node("h3", { textContent: "Inspector" })]), node("div", { className: "inspector-body" }, [node("div", { className: "inspector-title", textContent: item.path }), node("dl", { className: "meta-list" }, rows)])]); }
function storageModuleRows(scan, kind) { const module = moduleFor(scan, "storage"); const value = modulePayload(module); const rows = kind === "files" ? (value?.largest_files ?? []) : (value?.largest_folders ?? []); return rows.map((item) => kind === "files" ? normalizeEntry(item) : normalizeFolder(item)); }
function growthRow(item) { const source = objectOrEmpty(item); const delta = source.attributed_growth_bytes ?? source.attributedGrowthBytes ?? source.logical_growth_bytes ?? source.logicalGrowthBytes; return node("div", { className: "module-row" }, [node("div", {}, [node("div", { className: "module-name", textContent: pathBase(source.path) }), node("div", { className: "module-detail", textContent: text(source.path) })]), node("div", { className: `number ${asNumber(delta) < 0 ? "good-text" : "warning-text"}`, textContent: `${asNumber(delta) < 0 ? "−" : "+"}${formatBytes(Math.abs(asNumber(delta) ?? 0))}` })]); }
function growthCard(scan) { const growth = moduleFor(scan, "growth"); if (!growth) return null; const source = modulePayload(growth); const growthRows = (source.top_growth ?? []).slice(0, 12); const shrinkRows = (source.top_shrink ?? []).slice(0, 8); return card("Folder growth", source.comparable === false ? "Comparison unavailable" : "Largest attributed changes between supplied snapshots", [source.comparable === false ? node("p", { className: "muted", textContent: (source.reasons ?? []).join(" · ") || "Snapshots cannot be compared." }) : node("div", { className: "growth-grid" }, [node("div", {}, [node("p", { className: "table-head", textContent: "Growing" }), node("div", { className: "module-list" }, growthRows.length ? growthRows.map(growthRow) : [node("p", { className: "muted", textContent: "No folder growth supplied." })])]), node("div", {}, [node("p", { className: "table-head", textContent: "Shrinking" }), node("div", { className: "module-list" }, shrinkRows.length ? shrinkRows.map(growthRow) : [node("p", { className: "muted", textContent: "No folder shrinkage supplied." })])])])]); }
/*
function renderStorage(scan, localState) { const accounting = deriveAccounting(scan); const model = buildStorageModel(scan, localState.path); const mapBytes = sum(model.mapFolders.map((folder) => folder.attributedBytes ?? folder.logicalBytes)); const mapParts = model.mapFolders.map((folder) => { const size = folder.attributedBytes ?? folder.logicalBytes; const percentage = mapBytes > 0 ? Math.max(4, (size / mapBytes) * 100) : 100 / Math.max(1, model.mapFolders.length); return node("button", { className: "map-segment", type: "button", style: `flex-grow:${percentage}`, dataset: { navigate: folder.path }, ariaLabel: `Open ${folder.path}, ${formatBytes(size)}` }, [node("span", { className: "segment-label", textContent: folder.name })]); }); if (model.unknownCount) mapParts.push(node("div", { className: "map-unknown", textContent: `${model.unknownCount} unknown` })); const largestFiles = storageModuleRows(scan, "files"); const largestFolders = storageModuleRows(scan, "folders"); const files = largestFiles.length ? largestFiles : model.entries; const folders = largestFolders.length ? largestFolders : model.folders; const note = notice(scan); return [node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: "Storage" }), node("h2", { textContent: localState.path ? pathBase(localState.path) : "Where space lives" }), node("p", { textContent: "Browse scanner-attributed bytes by folder, largest files, & observed folder growth." })]), node("div", { className: "head-actions" }, [node("span", { className: "tag", textContent: `${formatCount(scan.entries.length)} entries loaded` })])]), note, node("div", { className: "stats-grid" }, [statistic("Logical scanned", formatBytes(accounting.logicalBytes), "metadata sum"), statistic("Attributed allocation", formatBytes(accounting.attributedBytes), "shared bytes counted once"), statistic("Volume used", formatBytes(accounting.usedBytes), accounting.usedBytes === null ? "volume reading unavailable" : "provider reading"), statistic("Reclaim lower bound", formatBytes(accounting.reclaim.lowerBytes), accounting.reclaim.upperBytes === null ? "upper bound unknown" : `up to ${formatBytes(accounting.reclaim.upperBytes)}`)]), node("div", { className: "storage-grid" }, [node("div", { className: "storage-main" }, [node("section", { className: "card" }, [node("div", { className: "card-header" }, [node("div", {}, [node("h3", { textContent: "Storage map" }), node("p", { textContent: localState.path ? `Direct children of ${localState.path}` : "Top-level folders across selected roots" })])]), node("div", { className: "map-wrap" }, [pathCrumbs(localState.path), node("div", { className: "storage-map", role: "list", "aria-label": "Folder size map" }, mapParts.length ? mapParts : [node("div", { className: "map-unknown", textContent: "No folder totals" })]), node("div", { className: "map-legend" }, [node("span", { textContent: `${formatBytes(model.knownBytes)} shown` }), node("span", { textContent: model.hasMore ? "Map capped at 24 folders" : "Bounded to supplied rows" })])])]), card("Largest folders", "Top folders from typed storage projection", [node("div", { className: "table-wrap" }, [table(["Folder", "Attributed", "State"], folders.slice(0, MAX_RENDER_ROWS).map(folderRow), "No folder accounting supplied for this level.")])]), card("Largest files", "Top files by observed allocation", [node("div", { className: "table-wrap" }, [table(["File", "Size", "Kind", "Metadata"], files.slice(0, MAX_RENDER_ROWS).map((entry) => entryRow(entry)), "No largest-file projection supplied.")])]), growthCard(scan), card("Entries in view", "Direct children only · ranked by observed allocation", [node("div", { className: "table-wrap" }, [table(["Entry", "Size", "Kind", "Metadata"], model.entries.slice(0, MAX_RENDER_ROWS).map((entry) => entryRow(entry)), "No direct entries supplied for this level.")])])].filter(Boolean)]), renderInspector(scan, localState.selected)]), ]; }
*/
function renderStorage(scan, localState) {
  const accounting = deriveAccounting(scan);
  const model = buildStorageModel(scan, localState.path);
  const mapBytes = sum(model.mapFolders.map((folder) => folder.attributedBytes ?? folder.logicalBytes));
  const mapParts = model.mapFolders.map((folder) => {
    const size = folder.attributedBytes ?? folder.logicalBytes;
    const percentage = mapBytes > 0 ? Math.max(4, (size / mapBytes) * 100) : 100 / Math.max(1, model.mapFolders.length);
    return node("button", { className: "map-segment", type: "button", style: `flex-grow:${percentage}`, dataset: { navigate: folder.path }, ariaLabel: `Open ${folder.path}, ${formatBytes(size)}` }, [node("span", { className: "segment-label", textContent: folder.name })]);
  });
  if (model.unknownCount) mapParts.push(node("div", { className: "map-unknown", textContent: `${model.unknownCount} unknown` }));
  const largestFiles = storageModuleRows(scan, "files");
  const largestFolders = storageModuleRows(scan, "folders");
  const files = largestFiles.length ? largestFiles : model.entries;
  const folders = largestFolders.length ? largestFolders : model.folders;
  const sections = [
    node("section", { className: "card" }, [node("div", { className: "card-header" }, [node("div", {}, [node("h3", { textContent: "Storage map" }), node("p", { textContent: localState.path ? `Direct children of ${localState.path}` : "Top-level folders across selected roots" })])]), node("div", { className: "map-wrap" }, [pathCrumbs(localState.path), node("div", { className: "storage-map", role: "list", "aria-label": "Folder size map" }, mapParts.length ? mapParts : [node("div", { className: "map-unknown", textContent: "No folder totals" })]), node("div", { className: "map-legend" }, [node("span", { textContent: `${formatBytes(model.knownBytes)} shown` }), node("span", { textContent: model.hasMore ? "Map capped at 24 folders" : "Bounded to supplied rows" })])])]),
    card("Largest folders", "Top folders from typed storage projection", [node("div", { className: "table-wrap" }, [table(["Folder", "Attributed", "State"], folders.slice(0, MAX_RENDER_ROWS).map(folderRow), "No folder accounting supplied for this level.")])]),
    card("Largest files", "Top files by observed allocation", [node("div", { className: "table-wrap" }, [table(["File", "Size", "Kind", "Metadata"], files.slice(0, MAX_RENDER_ROWS).map((entry) => entryRow(entry)), "No largest-file projection supplied.")])]),
    growthCard(scan),
    card("Entries in view", "Direct children only · ranked by observed allocation", [node("div", { className: "table-wrap" }, [table(["Entry", "Size", "Kind", "Metadata"], model.entries.slice(0, MAX_RENDER_ROWS).map((entry) => entryRow(entry)), "No direct entries supplied for this level.")])]),
  ].filter(Boolean);
  return [pageHead("Storage", localState.path ? pathBase(localState.path) : "Where space lives", node("span", { className: "tag", textContent: `${formatCount(scan.entries.length)} entries loaded` })), notice(scan), node("div", { className: "stats-grid" }, [statistic("Logical scanned", formatBytes(accounting.logicalBytes), "metadata sum"), statistic("Attributed allocation", formatBytes(accounting.attributedBytes), "shared bytes counted once"), statistic("Volume used", formatBytes(accounting.usedBytes), accounting.usedBytes === null ? "volume reading unavailable" : "provider reading"), statistic("Reclaim lower bound", formatBytes(accounting.reclaim.lowerBytes), accounting.reclaim.upperBytes === null ? "upper bound unknown" : `up to ${formatBytes(accounting.reclaim.upperBytes)}`)]), node("div", { className: "storage-grid" }, [node("div", { className: "storage-main" }, sections), renderInspector(scan, localState.selected)])].filter(Boolean);
}
function renderFind(scan, localState) { const filtered = filterEntries(scan.entries, localState.filters); const form = node("form", { className: "filters", id: "find-form" }, [node("div", { className: "field" }, [node("label", { for: "find-name", textContent: "Name or path" }), node("input", { id: "find-name", name: "name", type: "search", placeholder: "e.g. screenshots", value: localState.filters.name })]), node("div", { className: "field" }, [node("label", { for: "find-extension", textContent: "Extension" }), node("input", { id: "find-extension", name: "extension", type: "text", placeholder: "pdf", value: localState.filters.extension })]), node("div", { className: "field" }, [node("label", { for: "find-kind", textContent: "Kind" }), node("select", { id: "find-kind", name: "kind" }, [node("option", { value: "", textContent: "Any kind" }), ...["file", "directory", "symlink", "other"].map((kind) => node("option", { value: kind, selected: localState.filters.kind === kind, textContent: kind }))])]), node("div", { className: "field" }, [node("label", { for: "find-min", textContent: "Min bytes" }), node("input", { id: "find-min", name: "minBytes", type: "number", min: "0", inputMode: "numeric", value: localState.filters.minBytes })]), node("div", { className: "field" }, [node("label", { for: "find-max", textContent: "Max bytes" }), node("input", { id: "find-max", name: "maxBytes", type: "number", min: "0", inputMode: "numeric", value: localState.filters.maxBytes })]), node("div", { className: "filter-actions" }, [node("button", { className: "button button-primary", type: "submit", textContent: "Search" }), node("button", { className: "button button-quiet", type: "button", dataset: { clearFilters: "" }, textContent: "Clear" })])]); return [pageHead("Find", "Search supplied entries"), card("Filters", "All filters are local to this imported scan", [form]), node("div", { className: "results-bar" }, [node("span", { textContent: filtered.length > MAX_RENDER_ROWS ? `Showing first ${formatCount(MAX_RENDER_ROWS)} of ${formatCount(filtered.length)} matches` : `${formatCount(filtered.length)} matches` }), node("span", { className: "faint", textContent: scan.limits.entriesOmitted ? "Loaded result is bounded" : "" })]), node("section", { className: "card" }, [node("div", { className: "table-wrap" }, [table(["Entry", "Size", "Kind", "Metadata"], filtered.slice(0, MAX_RENDER_ROWS).map((entry) => entryRow(entry)), "No entries match these filters.")])])]; }
function capability(title, reason, action) { return node("div", { className: "capability" }, [node("div", { className: "capability-inner" }, [node("span", { className: "tag" , textContent: "Capability pending" }), node("h2", { textContent: title }), node("p", { textContent: reason }), action ? action : node("p", { className: "faint", textContent: "Native host can populate this panel with a bounded versioned response." })])]); }
function pageHead(title, description, action) { return node("div", { className: "page-head" }, [node("div", {}, [node("p", { className: "eyebrow", textContent: title }), node("h2", { textContent: description }), node("p", { textContent: "Rendered from explicit supplied evidence; unknown values stay unknown." })]), action ? node("div", { className: "head-actions" }, [action]) : ""]); }
function nativeAvailable() { return Boolean(globalThis?.webkit?.messageHandlers?.cockpitAction?.postMessage); }
function nativeButton(label, action, payload = {}, className = "button button-primary") { const disabled = Boolean(bridge.pending) || !nativeAvailable(); return node("button", { className, type: "button", disabled, dataset: { action, payload: JSON.stringify(payload) }, title: nativeAvailable() ? "" : "Native host unavailable", textContent: disabled && bridge.pending ? "Working…" : label }); }
function observation(value, unit = "") { const source = objectOrEmpty(value); const reading = source.value; if (reading === null || reading === undefined) return `${text(source.capability, "Unavailable")} · ${text(source.label, "No reading")}`; return `${unit ? `${reading} ${unit}` : text(reading)} · ${text(source.label, "OS reading")}`; }
function actionFeedback() { if (!bridge.feedback && !bridge.error) return null; const message = bridge.error ? `Action failed: ${bridge.error}` : bridge.feedback; return node("div", { className: `notice ${bridge.error ? "notice-danger" : "notice-good"}`, role: "status" }, [node("div", { className: "notice-icon", ariaHidden: "true", textContent: bridge.error ? "!" : "✓" }), node("p", { textContent: message })]); }
function moduleUnavailable(scan, title, action, reason) { const actionNode = action && nativeAvailable() ? nativeButton(action.label, action.name, action.payload ?? {}) : null; return [pageHead(title, `No ${title.toLowerCase()} evidence supplied`, actionNode), actionFeedback(), capability(`${title} needs native evidence`, reason || "This scan does not contain this typed module.", nativeAvailable() ? node("p", { className: "faint", textContent: "Use action above to request a bounded native refresh." }) : null)]; }
function renderDuplicates(scan) { const module = moduleFor(scan, "duplicates"); const request = { consent: "exact_content_read", snapshot_id: scan.snapshotId, min_size: 102400 }; if (!module) return moduleUnavailable(scan, "Duplicates", { label: "Inspect content for duplicates", name: "find_duplicates", payload: request }, "Duplicate detection is an explicit content-read operation. Nothing reads file content until you choose it."); const data = modulePayload(module); const groups = (data.groups ?? []).slice(0, MAX_RENDER_ROWS); const groupNodes = groups.map((group) => node("div", { className: "duplicate-group" }, [node("div", { className: "duplicate-head" }, [node("strong", { textContent: text(group.kept_path, "Kept item") }), node("span", { className: "number", textContent: formatBytes(group.size_bytes ?? group.sizeBytes) })]), node("p", { className: "muted", textContent: `${formatCount((group.extras ?? []).length)} duplicate${(group.extras ?? []).length === 1 ? "" : "s"} · ${pathText((group.extras ?? []).join(" · "))}` }), node("div", { className: "row-actions" }, (group.extras ?? []).map((path) => knownPathButton(scan, path, "Reveal", "reveal_item"))) ])); return [pageHead("Duplicates", "Exact duplicate groups", nativeButton("Re-run content inspection", "find_duplicates", request)), actionFeedback(), card("Duplicate groups", `${formatCount(groups.length)} groups shown · ${numberText(data.files_considered)} files considered · ${formatBytes(data.bytes_read)} read`, [node("div", { className: "duplicate-list" }, groupNodes.length ? groupNodes : [node("p", { className: "muted", textContent: "No duplicate groups were found." })])]), data.skipped?.length ? card("Skipped content", "Reads omitted by native safety bounds", [node("div", { className: "module-list" }, data.skipped.slice(0, MAX_RENDER_ROWS).map((row) => node("div", { className: "module-row" }, [node("span", { textContent: text(row.path) }), node("span", { className: "muted", textContent: text(row.reason) })])) )]) : null].filter(Boolean); }
function knownModulePath(value, seen = new Set(), depth = 0) { if (depth > 4 || value === null || value === undefined) return []; if (typeof value === "string") return value.startsWith("/") || /^[A-Za-z]:[\\/]/.test(value) ? [normalizedPath(value)] : []; if (Array.isArray(value)) return value.flatMap((item) => knownModulePath(item, seen, depth + 1)); if (typeof value !== "object" || seen.has(value)) return []; seen.add(value); return Object.entries(value).flatMap(([key, item]) => key.toLowerCase().includes("path") || key === "root" ? knownModulePath(item, seen, depth + 1) : knownModulePath(item, seen, depth + 1)); }
function scanHasPath(scan, path) { const value = normalizedPath(path); if (value === "/" || scan.entries.some((entry) => entry.path === value) || scan.folders.some((folder) => folder.path === value)) return true; return Object.values(scan.modules ?? {}).some((module) => knownModulePath(module).includes(value)); }
function knownPathButton(scan, path, label, action) { const value = normalizedPath(path); if (!scanHasPath(scan, value)) return ""; const reveal = node("button", { className: "button button-quiet", type: "button", dataset: { action, path: value, snapshotId: scan.snapshotId ?? "" }, textContent: label }); const preview = node("button", { className: "button button-quiet", type: "button", dataset: { action: "preview_item", path: value, snapshotId: scan.snapshotId ?? "" }, textContent: "Preview" }); return node("span", { className: "row-actions" }, [reveal, preview]); }
function normalizeFinding(finding) { const source = objectOrEmpty(finding); const reclaim = normalizeReclaim(source.reclaim); return { raw: finding, id: text(source.id, "Finding"), path: normalizedPath(source.path ?? source.item_path ?? source.itemPath), rule: text(source.rule_id ?? source.ruleId, "Rule not supplied"), eligible: Boolean(source.eligible), route: text(source.route, "Report only"), reason: text((Array.isArray(source.reasons) && source.reasons[0]) ?? source.reason, "Evidence requires review"), reclaim }; }
function legacyRenderCleanup() { return []; }
const PREF_KEY = "cockpit.dashboard.preferences.v1";
const VIEWS = new Set(["storage", "find", "duplicates", "cleanup", "apps", "monitor", "activity", "compress"]);
function savePreferences() { if (typeof localStorage === "undefined") return; try { localStorage.setItem(PREF_KEY, JSON.stringify({ view: state.view, filters: state.filters })); } catch { /* private mode or disabled storage */ } }
function loadPreferences() { if (typeof localStorage === "undefined") return; try { const saved = JSON.parse(localStorage.getItem(PREF_KEY) || "{}"); if (VIEWS.has(saved.view)) state.view = saved.view; if (saved.filters && typeof saved.filters === "object") state.filters = { ...state.filters, ...saved.filters }; } catch { /* malformed preference stays ignored */ } }
function unsafePostNativeAction(action, payload = {}) { if (bridge.pending) return false; const handler = globalThis?.webkit?.messageHandlers?.cockpitAction; if (!handler?.postMessage) { bridge.error = "Native host unavailable"; renderApp(); return false; } if ((action === "reveal_item" || action === "preview_item") && state.scan) payload = { ...payload, snapshot_id: state.scan.snapshotId }; const requestId = `cockpit-${Date.now().toString(36)}-${++bridge.sequence}`; bridge.pending = { request_id: requestId, action }; bridge.feedback = null; bridge.error = null; renderApp(); try { handler.postMessage({ version: 1, request_id: requestId, action, payload }); } catch (error) { bridge.pending = null; bridge.error = error instanceof Error ? error.message : "Native action could not be sent"; renderApp(); } return true; }
function legacyReceiveAction2() { return false; }
function legacyUpdateModule2() { return false; }
function legacyRenderCleanup2() { return false; }
function legacyBoot() { loadPreferences(); document.querySelector("#import-scan").addEventListener("click", openFilePicker); document.querySelector("#scan-file").addEventListener("change", (event) => { const file = event.target.files?.[0]; event.target.value = ""; importFile(file); }); document.querySelector("#section-nav").addEventListener("click", (event) => { const button = event.target.closest("[data-view]"); if (!button) return; state.view = button.dataset.view; state.path = null; state.selected = null; savePreferences(); syncNav(); renderApp(); }); document.querySelector("#app-view").addEventListener("click", (event) => { const open = event.target.closest("[data-open-file]"); if (open) { openFilePicker(); return; } const clear = event.target.closest("[data-clear-filters]"); if (clear) { state.filters = { name: "", extension: "", kind: "", minBytes: "", maxBytes: "" }; savePreferences(); renderApp(); return; } const stage = event.target.closest("[data-stage-finding]"); if (stage) { if (stage.checked) state.staged.add(stage.dataset.stageFinding); else state.staged.delete(stage.dataset.stageFinding); renderApp(); return; } const action = event.target.closest("[data-action]"); if (action) { let payload = {}; try { payload = JSON.parse(action.dataset.payload ?? "{}"); } catch { payload = {}; } if (action.dataset.path) payload.path = action.dataset.path; if (action.dataset.action === "reveal_item" || action.dataset.action === "preview_item") { if (!state.scan || !scanHasPath(state.scan, payload.path)) return; } postNativeAction(action.dataset.action, payload); return; } const navigate = event.target.closest("[data-navigate]"); if (navigate) { state.path = navigate.dataset.navigate || null; state.selected = null; renderApp(); return; } const inspect = event.target.closest("[data-inspect]"); if (inspect) { state.selected = inspect.dataset.inspect; renderApp(); return; } }); document.querySelector("#app-view").addEventListener("submit", (event) => { if (event.target.id !== "find-form") return; event.preventDefault(); state.filters = formFilters(event.target); savePreferences(); renderApp(); }); syncNav(); renderApp(); }
function postNativeAction(action, payload = {}) { if ((action === "reveal_item" || action === "preview_item") && state.scan && !scanHasPath(state.scan, payload.path)) return false; return unsafePostNativeAction(action, payload); }

function captureCleanupHistory(data) {
  const source = modulePayload(data);
  const cleanup = objectOrEmpty(source.cleanup ?? source.cleanup_history ?? source.cleanupHistory);
  const plans = cleanup.plans ?? cleanup.history ?? source.plans ?? source.cleanup_plans ?? source.cleanupPlans;
  if (Array.isArray(plans)) bridge.cleanupPlans = plans;
  return bridge.cleanupPlans;
}

function cleanupEligible(finding) {
  const raw = objectOrEmpty(finding.raw);
  const route = pathText(finding.route).toLowerCase().replaceAll("_", "");
  return Boolean(finding.eligible) && raw.report_only !== true && route === "trash";
}

function cleanupPlanItems(plan) {
  const source = objectOrEmpty(plan);
  const items = source.items ?? source.paths ?? source.files ?? source.item_names ?? source.itemNames ?? [];
  if (!Array.isArray(items)) return [];
  return items.map((item) => {
    if (typeof item === "string") return item;
    const value = objectOrEmpty(item);
    return value.path ?? value.name ?? value.filename ?? value.file_name ?? "";
  }).map(pathText).filter(Boolean);
}

function cleanupPlanLabel(plan) {
  const items = cleanupPlanItems(plan);
  const count = asNumber(plan.item_count ?? plan.itemCount) ?? items.length;
  const names = items.slice(0, 3).map(pathBase);
  const shown = names.join(" · ");
  return `${formatCount(count)} item${count === 1 ? "" : "s"}${shown ? ` · ${shown}${items.length > 3 ? " · …" : ""}` : ""}`;
}

function cleanupPlanHistory(data) {
  const plans = captureCleanupHistory(data);
  if (!plans.length) return card("Cleanup history", "Native cleanup journal", [node("p", { className: "muted", textContent: "No cleanup history supplied." })]);
  const rows = plans.slice(0, MAX_RENDER_ROWS).map((plan) => {
    const source = objectOrEmpty(plan);
    const planId = source.plan_id ?? source.planId;
    const stateName = text(source.state ?? source.status, "Recorded");
    const expiry = source.expires_at ?? source.expiresAt;
    const canUndo = Boolean(planId) && /applied|moved|complete/i.test(stateName) && !/undone|expired/i.test(stateName);
    const outcome = source.freed_bytes !== null && source.freed_bytes !== undefined ? `Freed ${formatBytes(source.freed_bytes)}` : source.moved_bytes !== null && source.moved_bytes !== undefined ? `Moved ${formatBytes(source.moved_bytes)}` : source.state ? stateName : "Outcome pending";
    return node("div", { className: "module-row" }, [node("div", {}, [node("div", { className: "module-name", textContent: cleanupPlanLabel(source) }), node("div", { className: "module-detail", textContent: `${stateName} · ${outcome}${expiry ? ` · Expires ${text(expiry)}` : ""}` })]), canUndo ? nativeButton("Undo", "undo_cleanup", { plan_id: planId }, "button button-quiet") : ""]);
  });
  return card("Cleanup history", "Native cleanup journal", [node("div", { className: "module-list" }, rows)]);
}

function reviewItems(review) {
  const items = cleanupPlanItems(review);
  const count = asNumber(review.item_count ?? review.itemCount) ?? items.length;
  return { items, count };
}

function reviewNotice(review) {
  if (!review || !Object.keys(review).length) return node("div", { className: "notice", role: "status" }, [node("div", { className: "notice-icon", textContent: "i" }), node("div", {}, [node("strong", { textContent: "Staging is not applied" }), node("p", { textContent: "Select eligible files, then review them with native confirmation." })])]);
  const selected = reviewItems(review);
  const names = selected.items.slice(0, 3).map(pathBase).join(" · ");
  const expiry = review.expires_at ?? review.expiresAt;
  return node("div", { className: "notice", role: "status" }, [node("div", { className: "notice-icon", textContent: "i" }), node("div", {}, [node("strong", { textContent: "Review ready" }), node("p", { textContent: `${formatCount(selected.count)} selected file${selected.count === 1 ? "" : "s"}${names ? ` · ${names}${selected.items.length > 3 ? " · …" : ""}` : ""}${expiry ? ` · Expires ${text(expiry)}` : ""}` })])]);
}

function actionResultFeedback(action, data) {
  const source = objectOrEmpty(data);
  const items = cleanupPlanItems(source);
  const count = asNumber(source.item_count ?? source.itemCount) ?? items.length;
  if (action === "review_cleanup") return `Review ready · ${formatCount(count)} file${count === 1 ? "" : "s"}`;
  if (action === "apply_cleanup") return `Cleanup complete · ${formatCount(count)} file${count === 1 ? "" : "s"} moved to Trash`;
  if (action === "undo_cleanup") return `Undo complete · ${formatCount(count)} file${count === 1 ? "" : "s"} restored`;
  return `${action.replaceAll("_", " ")} complete`;
}

function renderInspector(scan, selectedPath) {
  const entry = scan.entries.find((item) => item.path === selectedPath);
  const folder = scan.folders.find((item) => item.path === selectedPath);
  if (!entry && !folder) return card("Inspector", "Select a row to inspect metadata", [node("p", { className: "inspector-empty", textContent: "Choose a folder or entry from Storage or Find. Dashboard selection stays local to this page." })], "card inspector");
  const item = entry ?? folder;
  const isEntry = Boolean(entry);
  const rows = [metaRow("Path", item.path), metaRow("Kind", isEntry ? item.kind : "folder"), metaRow("Logical size", formatBytes(item.logicalBytes)), metaRow("Allocation", formatBytes(item.attributedBytes)), metaRow("Volume", item.volume)];
  if (isEntry) rows.push(metaRow("Metadata", item.complete ? "Complete" : "Partial"), metaRow("Placeholder", item.placeholder ? "Yes" : "No"), metaRow("Reclaim bound", item.reclaim ? (item.reclaim.upperBytes === null ? `${formatBytes(item.reclaim.lowerBytes)}+ · unknown upper` : `${formatBytes(item.reclaim.lowerBytes)}–${formatBytes(item.reclaim.upperBytes)}`) : "Not supplied"));
  else rows.push(metaRow("Accounting", item.incomplete ? "Incomplete" : "Attributed"));
  const manual = isEntry && item.kind === "file" && item.complete && !item.placeholder && Boolean(scan.snapshotId)
    ? nativeButton("Review Move to Trash", "review_cleanup", { paths: [item.path], snapshot_id: scan.snapshotId, selection_mode: "manual" }, "button button-quiet")
    : null;
  return node("section", { className: "card inspector" }, [node("div", { className: "card-header" }, [node("h3", { textContent: "Inspector" })]), node("div", { className: "inspector-body" }, [node("div", { className: "inspector-title", textContent: item.path }), node("dl", { className: "meta-list" }, rows), manual ? node("div", { className: "row-actions" }, [manual]) : null])]);
}

function renderActivity(scan) {
  const module = moduleFor(scan, "activity");
  if (!module) return moduleUnavailable(scan, "Activity", { label: "Refresh activity", name: "refresh_activity" }, "Activity totals, scan history, & cleanup history require native evidence.");
  const data = modulePayload(module);
  captureCleanupHistory(data);
  const totals = objectOrEmpty(data.totals ?? data.weekly ?? {});
  const historyData = data.history;
  const historyEvents = Array.isArray(historyData) ? historyData : (objectOrEmpty(historyData).events ?? objectOrEmpty(historyData).items ?? []);
  const events = [...(Array.isArray(data.events) ? data.events : Array.isArray(data.items) ? data.items : []), ...historyEvents].slice(0, MAX_RENDER_ROWS).sort((a, b) => (asNumber(b.occurred_at ?? b.occurredAt ?? b.created_at) ?? 0) - (asNumber(a.occurred_at ?? a.occurredAt ?? a.created_at) ?? 0));
  const scansData = data.scans ?? data.scan_history ?? data.scanHistory ?? [];
  const scans = Array.isArray(scansData) ? scansData : (objectOrEmpty(scansData).scans ?? objectOrEmpty(scansData).events ?? []);
  const rows = events.map((event) => { const kind = objectOrEmpty(event.kind); return node("div", { className: "timeline-item" }, [node("div", { className: "timeline-dot", ariaHidden: "true" }), node("div", {}, [node("div", { className: "module-name", textContent: typeof event.kind === "string" ? event.kind : Object.keys(kind)[0] || "Event" }), node("div", { className: "module-detail", textContent: `${text(event.id, "Activity")} · ${text(event.occurred_at ?? event.occurredAt ?? event.created_at, "Time unknown")}` })])]); });
  const scanRows = Array.isArray(scans) ? scans.slice(0, MAX_RENDER_ROWS).map((item) => node("div", { className: "module-row" }, [node("span", { className: "module-name", textContent: text(item.snapshot_id ?? item.snapshotId ?? item.id, "Scan") }), node("span", { className: "muted", textContent: text(item.created_at ?? item.createdAt ?? item.occurred_at, "Time unknown") })])) : [];
  return [pageHead("Activity", "Cleanup, scan, compression, & volume history", nativeButton("Refresh activity", "refresh_activity")), actionFeedback(), node("div", { className: "stats-grid" }, [statistic("Events", numberText(totals.events ?? events.length), "selected window"), statistic("Moved", formatBytes(totals.moved_bytes ?? totals.movedBytes), "moved to Trash or owner tool"), statistic("Observed reclaim", formatBytes(totals.observed_reclaimed_bytes ?? totals.observedReclaimedBytes), "volume observation"), statistic("Compression", formatBytes(totals.compression_logical_delta_bytes ?? totals.compressionLogicalDeltaBytes), "logical delta")]), card("Timeline", `${formatCount(events.length)} events shown`, [node("div", { className: "timeline" }, rows.length ? rows : [node("p", { className: "muted", textContent: "No activity events supplied." })])]), card("Scan history", "Native scan records", [node("div", { className: "module-list" }, scanRows.length ? scanRows : [node("p", { className: "muted", textContent: "No scan history supplied." })])]), cleanupPlanHistory(data)];
}

function renderCleanup(scan, localState) {
  const findings = scan.findings.map(normalizeFinding);
  const review = bridge.review;
  const selectedCount = findings.filter((finding) => localState.staged.has(finding.id) && cleanupEligible(finding)).length;
  const rows = findings.slice(0, MAX_RENDER_ROWS).map((finding) => {
    const eligible = cleanupEligible(finding);
    const staged = localState.staged.has(finding.id) && eligible;
    return node("div", { className: "finding" }, [node("input", { type: "checkbox", checked: staged, disabled: !eligible, dataset: { stageFinding: finding.id }, ariaLabel: eligible ? `Stage ${finding.path}` : `Report only ${finding.path}` }), node("div", {}, [node("div", { className: "finding-path", textContent: finding.path }), node("p", { className: "finding-note", textContent: `${finding.rule} · ${finding.reason}` })]), node("div", { className: "finding-right" }, [node("span", { className: eligible ? "tag warning" : "tag", textContent: eligible ? "Review" : "Report only" }), node("span", { className: "number muted", textContent: finding.reclaim.upperBytes === null ? `${formatBytes(finding.reclaim.lowerBytes)}+` : formatBytes(finding.reclaim.upperBytes) })])]);
  });
  const selectedPaths = findings.filter((finding) => localState.staged.has(finding.id) && cleanupEligible(finding)).map((finding) => finding.path);
  const action = review && (review.plan_id ?? review.planId) ? nativeButton("Apply cleanup", "apply_cleanup", { plan_id: review.plan_id ?? review.planId }) : selectedPaths.length ? nativeButton("Review selected files", "review_cleanup", { paths: selectedPaths, snapshot_id: scan.snapshotId }) : null;
  const history = cleanupPlanHistory({ cleanup: { plans: bridge.cleanupPlans } });
  return [pageHead("Cleanup", review ? "Review selected files" : "Stage & review cleanup", node("span", { className: "tag", textContent: `${formatCount(selectedCount)} selected` })), actionFeedback(), action ? node("div", { className: "row-actions" }, [action]) : null, reviewNotice(review), node("section", { className: "card" }, [node("div", { className: "card-header" }, [node("h3", { textContent: "Findings" }), node("p", { textContent: findings.length ? `${formatCount(findings.length)} supplied · ${formatCount(selectedCount)} eligible selected` : "No findings supplied" })]), node("div", { className: "card-body" }, [node("div", { className: "finding-list" }, rows.length ? rows : [node("p", { className: "muted", textContent: "Native scan supplied no cleanup findings." })])])]), history].filter(Boolean);
}

function receiveAction(response) {
  const source = objectOrEmpty(response);
  if (!bridge.pending || source.request_id !== bridge.pending.request_id || source.action !== bridge.pending.action) return false;
  const action = bridge.pending.action;
  bridge.pending = null;
  const data = objectOrEmpty(source.data);
  const payload = objectOrEmpty(data.payload);
  const moduleName = data.module ?? actionModule(action);
  const moduleData = Object.keys(payload).length ? payload : source.data;
  if (!source.ok) { bridge.error = text(source.error, "Native action failed"); bridge.feedback = null; renderApp(); return false; }
  bridge.error = null;
  bridge.feedback = actionResultFeedback(action, moduleData);
  if (action === "review_cleanup") bridge.review = objectOrEmpty(moduleData);
  if (action === "apply_cleanup") { bridge.review = null; bridge.undoPlanId = moduleData.plan_id ?? moduleData.planId ?? null; captureCleanupHistory({ cleanup: { plans: [moduleData] } }); }
  if (action === "undo_cleanup") { bridge.undoPlanId = null; captureCleanupHistory({ cleanup: { plans: [moduleData] } }); }
  if (action === "refresh_activity") captureCleanupHistory(moduleData);
  if (moduleName && source.data !== undefined && !["review_cleanup", "apply_cleanup", "undo_cleanup"].includes(action)) applyModule(moduleName, moduleData);
  renderApp();
  return true;
}

function updateModule(name, data) {
  const source = objectOrEmpty(data);
  const moduleName = source.module ?? name;
  const moduleData = Object.keys(objectOrEmpty(source.payload)).length ? source.payload : data;
  applyModule(moduleName, moduleData);
  if (moduleName === "activity" || moduleName === "history") captureCleanupHistory(moduleData);
  bridge.feedback = `${moduleName} updated`;
  bridge.error = null;
  renderApp();
  return moduleData;
}

export function importScanJson(input, options = {}) {
  loadPreferences();
  state.scan = normalizeScan(input, options);
  state.path = null;
  state.selected = null;
  state.staged = new Set();
  bridge.review = null;
  bridge.undoPlanId = null;
  captureCleanupHistory(state.scan.modules.activity ?? state.scan.modules.history ?? {});
  if (typeof document !== "undefined") { setStatus(`Loaded ${formatCount(state.scan.entries.length)} entries`, "good"); syncNav(); renderApp(); }
  return state.scan;
}

function renderMonitor(scan) {
  const module = moduleFor(scan, "monitor");
  if (!module) return moduleUnavailable(scan, "Monitor", { label: "Refresh monitor", name: "refresh_monitor" }, "Resource, network, battery, & listening-port readings require native sampling.");
  const data = modulePayload(module);
  const network = objectOrEmpty(data.network);
  const battery = objectOrEmpty(data.battery);
  const processes = (data.processes ?? data.process_groups ?? data.processGroups ?? []).slice(0, MAX_RENDER_ROWS);
  const ports = objectOrEmpty(data.listening_ports ?? data.listeningPorts);
  const processRows = processes.map((process) => node("div", { className: "module-row" }, [
    node("div", {}, [node("div", { className: "module-name", textContent: text(process.name, "Process") }), node("div", { className: "module-detail", textContent: `PID ${text(process.identity?.pid, "Unknown")} · Started ${text(process.identity?.start_time ?? process.identity?.startTime, "Unknown")}` })]),
    node("div", { className: "app-metrics" }, [node("span", { className: "number", textContent: process.cpu_usage_percent === undefined ? "Unknown CPU" : `${Number(process.cpu_usage_percent).toFixed(1)}% CPU` }), node("span", { className: "number", textContent: formatBytes(process.memory?.value) })]),
  ]));
  const portRows = Array.isArray(ports.value) ? ports.value.slice(0, MAX_RENDER_ROWS).map((port) => node("div", { className: "module-row" }, [node("span", { textContent: `${text(port.protocol)} ${text(port.local_address ?? port.localAddress)}:${text(port.port)}` }), node("span", { className: "muted", textContent: port.owner ? `PID ${text(port.owner.pid)}` : text(port.reason) })])) : [];
  return [pageHead("Monitor", "Resources, network, battery, & listening ports", nativeButton("Refresh readings", "refresh_monitor")), actionFeedback(), node("div", { className: "stats-grid" }, [statistic("Network in", observation(network.counters, "B/s"), "native counter"), statistic("Network out", observation(network.rates, "B/s"), "rate requires two samples"), statistic("Battery", observation(battery.charge_percent, "%"), text(battery.charging?.value, "Charging state unknown")), statistic("Listening ports", portRows.length ? formatCount(portRows.length) : text(ports.capability, "Unavailable"), "owner identity is bounded")]), card("Resource processes", "CPU & memory from supplied process observations", [node("div", { className: "module-list" }, processRows.length ? processRows : [node("p", { className: "muted", textContent: "No process readings supplied." })])]), portRows.length ? card("Listening ports", "Only native owner matches are shown as identified", [node("div", { className: "module-list" }, portRows)]) : null].filter(Boolean);
}

const bridge = { pending: null, sequence: 0, feedback: null, error: null, review: null, cleanupPlans: [], undoPlanId: null };
const state = { scan: null, view: "storage", path: null, selected: null, filters: { name: "", extension: "", kind: "", minBytes: "", maxBytes: "" }, staged: new Set(), error: null };

function setStatus(message, tone = "neutral") { const status = document.querySelector("#scan-status"); if (!status) return; status.textContent = message; status.dataset.tone = tone; }
function openFilePicker() { document.querySelector("#scan-file")?.click(); }
async function importFile(file) { if (!file) return; if (file.size > MAX_FILE_BYTES) { setStatus("File exceeds 10 MB limit", "danger"); return; } setStatus("Reading scan…", "warning"); try { importScanJson(JSON.parse(await file.text()), { loadedBytes: file.size }); } catch (error) { setStatus("Could not load scan JSON", "danger"); const view = document.querySelector("#app-view"); if (view) view.replaceChildren(node("div", { className: "empty-state" }, [node("div", { className: "empty-state-inner" }, [node("p", { className: "eyebrow", textContent: "Import error" }), node("h2", { textContent: "Scan JSON was not accepted" }), node("p", { className: "error-text", textContent: error instanceof Error ? error.message : "Invalid JSON" }), node("button", { className: "button button-primary", type: "button", dataset: { openFile: "" }, textContent: "Choose another file" })])])); } }
function formFilters(form) { const data = new FormData(form); return { name: data.get("name") ?? "", extension: data.get("extension") ?? "", kind: data.get("kind") ?? "", minBytes: data.get("minBytes") ?? "", maxBytes: data.get("maxBytes") ?? "" }; }
function syncNav() { document.querySelectorAll(".nav-item").forEach((item) => { const active = item.dataset.view === state.view; item.classList.toggle("is-active", active); if (active) item.setAttribute("aria-current", "page"); else item.removeAttribute("aria-current"); }); }
function actionModule(action) { return { refresh_monitor: "monitor", refresh_apps: "apps", refresh_activity: "activity", find_duplicates: "duplicates", compress_media: "compression" }[action]; }
function applyModule(name, data) { if (!state.scan) return; state.scan.modules = { ...state.scan.modules, [name]: data }; }
function renderApp() { if (typeof document === "undefined") return; const view = document.querySelector("#app-view"); if (!view) return; view.replaceChildren(); if (!state.scan) { view.append(node("div", { className: "empty-state" }, [node("div", { className: "empty-state-inner" }, [node("p", { className: "eyebrow", textContent: "Local scan viewer" }), node("h2", { textContent: "Import a Cockpit scan to begin" }), node("p", { textContent: "Choose a bounded JSON export from Cockpit core. Dashboard reads supplied evidence only." }), node("button", { className: "button button-primary", type: "button", dataset: { openFile: "" }, textContent: "Choose scan JSON" })])])); return; } const renderers = { storage: () => renderStorage(state.scan, state), find: () => renderFind(state.scan, state), cleanup: () => renderCleanup(state.scan, state), duplicates: () => renderDuplicates(state.scan), apps: () => renderApps(state.scan), monitor: () => renderMonitor(state.scan), activity: () => renderActivity(state.scan), compress: () => renderCompress(state.scan) }; view.append(...(renderers[state.view] ? renderers[state.view]() : renderers.storage())); }

function boot() { loadPreferences(); document.querySelector("#import-scan")?.addEventListener("click", openFilePicker); document.querySelector("#scan-file")?.addEventListener("change", (event) => { const file = event.target.files?.[0]; event.target.value = ""; importFile(file); }); document.querySelector("#section-nav")?.addEventListener("click", (event) => { const button = event.target.closest("[data-view]"); if (!button) return; state.view = button.dataset.view; state.path = null; state.selected = null; savePreferences(); syncNav(); renderApp(); }); document.querySelector("#app-view")?.addEventListener("click", (event) => { const open = event.target.closest("[data-open-file]"); if (open) { openFilePicker(); return; } const clear = event.target.closest("[data-clear-filters]"); if (clear) { state.filters = { name: "", extension: "", kind: "", minBytes: "", maxBytes: "" }; savePreferences(); renderApp(); return; } const stage = event.target.closest("[data-stage-finding]"); if (stage) { if (stage.checked) state.staged.add(stage.dataset.stageFinding); else state.staged.delete(stage.dataset.stageFinding); renderApp(); return; } const action = event.target.closest("[data-action]"); if (action) { let payload = {}; try { payload = JSON.parse(action.dataset.payload ?? "{}"); } catch { payload = {}; } if (action.dataset.path) payload.path = action.dataset.path; if ((action.dataset.action === "reveal_item" || action.dataset.action === "preview_item") && (!state.scan || !scanHasPath(state.scan, payload.path))) return; postNativeAction(action.dataset.action, payload); return; } const navigate = event.target.closest("[data-navigate]"); if (navigate) { state.path = navigate.dataset.navigate || null; state.selected = null; renderApp(); return; } const inspect = event.target.closest("[data-inspect]"); if (inspect) { state.selected = inspect.dataset.inspect; renderApp(); return; } }); document.querySelector("#app-view")?.addEventListener("submit", (event) => { if (event.target.id !== "find-form") return; event.preventDefault(); state.filters = formFilters(event.target); savePreferences(); renderApp(); }); syncNav(); renderApp(); }

export function getDashboardState() { return { view: state.view, path: state.path, selected: state.selected, stagedFindingIds: [...state.staged], scan: state.scan, pendingAction: bridge.pending }; }
if (typeof globalThis !== "undefined") { globalThis.CockpitDashboard = { importScan: importScanJson, importScanJson, loadScan: importScanJson, getState: getDashboardState, receiveAction, updateModule }; globalThis.loadCockpitScan = importScanJson; }
if (typeof document !== "undefined") boot();
