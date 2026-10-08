import { invoke } from "@tauri-apps/api/core";

export interface Metric<T> {
  value: T | null;
  capability: string;
  label: string;
}

export interface Disk {
  mount_point: string;
  total_bytes: number | null;
  available_bytes: number | null;
  removable: boolean;
}

export interface Status {
  cpu_usage_percent: Metric<number>;
  memory_used_bytes: Metric<number>;
  memory_total_bytes: Metric<number>;
  memory_pressure: Metric<string>;
  swap_used_bytes: Metric<number>;
  swap_total_bytes: Metric<number>;
  disks: Disk[];
}

export interface Process {
  identity: { pid: number; start_time: number };
  name: string;
  cpu_usage_percent: number;
  memory: Metric<number>;
}

export interface Row {
  path: string;
  name: string;
  is_dir: boolean;
  bytes: number;
  /** A "smaller files" total, not a real path. */
  summary?: boolean;
}

/** An item's identity when the person asked for an action on it (device and inode). */
export interface FileIdentity {
  dev: number;
  ino: number;
  is_dir: boolean;
}

export interface MovePlan {
  target: string;
  same_volume: boolean;
}

export interface MoveResult {
  target: string;
  copied: boolean;
}

export interface Folder {
  path: string;
  root: string;
  rows: Row[];
  total_children: number;
  incomplete: boolean;
  needs_access: boolean;
  limited: boolean;
  root_label: string;
  /** Unix seconds when this data was scanned. */
  scanned_at: number;
  /** Shown from the last saved scan rather than a scan made this session. */
  from_snapshot: boolean;
}

export interface ScanStatus {
  running: boolean;
  running_root: string | null;
  has_index: boolean;
  root: string | null;
  scanned_at: number | null;
  from_snapshot: boolean;
}

export interface Volume {
  name: string;
  mount_point: string;
  total_bytes: number;
  available_bytes: number;
  removable: boolean;
  internal: boolean;
  disk_image: boolean;
}

export interface CleanupFinding {
  id: string;
  rule_id: string;
  rule_name?: string;
  category: string;
  name: string;
  path: string;
  bytes: number;
  apparent_bytes?: number;
  partial: boolean;
  risk: "safe" | "review" | "info";
  eligible: boolean;
  preselected: boolean;
  reason: string;
  action: string;
  dev: string;
  ino: string;
}

export interface ChromeSnapshots {
  count: number;
  apparent_bytes: number;
  reclaimable_bytes: number;
  reclaimable_known: boolean;
  running: boolean;
  since_at: number | null;
  since_count: number | null;
}

export interface CleanupReport {
  chrome_snapshots?: ChromeSnapshots | null;
  findings: CleanupFinding[];
  safe_bytes: number;
  review_bytes: number;
  scanned_at: number;
}

export interface CleanupApplyResult {
  moved_items: number;
  moved_bytes: number;
  skipped: { path: string; reason: string }[];
  activity_id: string | null;
}

export interface AppEntry {
  name: string;
  path: string;
  bundle_id: string | null;
  version: string | null;
  size_bytes: number;
  last_used: number | null;
  running: boolean;
  protected: string | null;
}

export interface RelatedItem {
  path: string;
  /** Library folder, or "Application". */
  label: string;
  /** "Application", "User Library", "System Library" or "Installer receipt". */
  location: string;
  exact: boolean;
  /** exact, helper, group, prefix, team, name or receipt. */
  confidence: string;
  reason: string;
  /** Root-owned: Finder asks for an administrator password. */
  admin: boolean;
  size_bytes: number;
  preselected: boolean;
}

export interface BackgroundEntry {
  kind: string;
  label: string;
  path: string | null;
}

export interface AppDetail {
  app: AppEntry;
  items: RelatedItem[];
  background: BackgroundEntry[];
  receipts: string[];
}

export interface UninstallResult {
  moved: { path: string; bytes: number }[];
  failed: { path: string; error: string }[];
  moved_bytes: number;
}

export interface ProcessMember {
  identity: { pid: number; start_time: number };
  name: string;
  cpu_usage_percent: number;
  memory_bytes: number;
}

export interface ProcessRow {
  key: string;
  name: string;
  bundle_id: string | null;
  app_path: string | null;
  lead: { pid: number; start_time: number };
  cpu_usage_percent: number;
  memory_bytes: number;
  members: ProcessMember[];
  can_act: boolean;
  refusal: string | null;
}

export interface Change {
  path: string;
  /** Signed change in bytes. */
  bytes: number;
}

export interface Growth {
  available: boolean;
  /** Unix seconds of the scan compared against. */
  since: number | null;
  grown: Change[];
  shrunk: Change[];
  reason: string | null;
}

export type QuitOutcome = "quit" | "still_running";

export const api = {
  status: () => invoke<Status>("status"),
  processes: () => invoke<Process[]>("processes"),
  scan: (path?: string) => invoke<Folder>("scan", { path: path ?? null }),
  scanStatus: () => invoke<ScanStatus>("scan_status"),
  lastScan: () => invoke<Folder | null>("last_scan"),
  growth: () => invoke<Growth>("growth"),
  children: (path: string) => invoke<Folder>("children", { path }),
  search: (query: string, limit?: number, extensions?: string[]) =>
    invoke<Row[]>("search", { query, limit: limit ?? null, extensions: extensions ?? null }),
  apps: () => invoke<AppEntry[]>("apps_list"),
  appDetail: (path: string) => invoke<AppDetail>("app_detail", { path }),
  uninstall: (path: string, bundleId: string | null, items: string[]) =>
    invoke<UninstallResult>("app_uninstall", { path, bundleId, items }),
  processRows: () => invoke<ProcessRow[]>("process_rows"),
  quit: (r: ProcessRow) =>
    invoke<QuitOutcome>("process_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
  forceQuit: (r: ProcessRow) =>
    invoke<QuitOutcome>("process_force_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
  volumes: () => invoke<Volume[]>("volumes"),
  eject: (mount: string) => invoke<void>("eject", { mount }),
  /** Opens the Full Disk Access pane in System Settings. */
  openFullDiskAccess: () => invoke<void>("fda_request"),
  cleanupScan: () => invoke<CleanupReport>("cleanup_scan"),
  /** The last saved findings, or null when none were saved. Never scans. */
  cleanupCached: () => invoke<CleanupReport | null>("cleanup_cached"),
  cleanupApply: (items: CleanupFinding[]) =>
    invoke<CleanupApplyResult>("cleanup_apply", {
      items: items.map((f) => ({ rule_id: f.rule_id, path: f.path, dev: f.dev, ino: f.ino })),
    }),
  cleanupRestore: (id: string) => invoke<unknown>("cleanup_restore", { id }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  /** Device and inode of an item now; the file actions check them again before acting. */
  fileIdentity: (path: string) => invoke<FileIdentity>("file_identity", { path }),
  /** Opens a folder, or selects a file, in Finder. Read-only. */
  finderOpen: (path: string) => invoke<void>("finder_open", { path }),
  /** The native folder picker; null when cancelled. */
  fileChooseFolder: () => invoke<string | null>("file_choose_folder"),
  fileMovePlan: (path: string, id: FileIdentity, destination: string) =>
    invoke<MovePlan>("file_move_plan", { path, dev: id.dev, ino: id.ino, destination }),
  /** Renames within one drive; across drives, copies then trashes the original when copyAcrossVolumes is set. */
  fileMove: (path: string, id: FileIdentity, destination: string, copyAcrossVolumes: boolean) =>
    invoke<MoveResult>("file_move", { path, dev: id.dev, ino: id.ino, destination, copyAcrossVolumes }),
  fileTrash: (path: string, id: FileIdentity) => invoke<void>("file_trash", { path, dev: id.dev, ino: id.ino }),
  /** Drive health for these mounts; samples smartctl only when ten minutes have passed. */
  driveHealth: (mounts: string[]) => invoke<HealthReport>("drive_health", { mounts }),
  homePath: () => invoke<string>("home_path"),
  duplicatesScan: (path: string) => invoke<DuplicateReport>("duplicates_scan", { path }),
  /** Moves each extra copy to the Trash after re-checking it; the kept copy is never touched. */
  duplicatesTrash: (items: { kept: string; path: string }[]) =>
    invoke<DuplicatesTrashResult>("duplicates_trash", { items }),
};

// ---- Drive health (smartctl, kept by core) ----

export interface SelfTest {
  kind: string;
  result: string;
  passed: boolean | null;
  power_on_hours: number | null;
}

/** One successful SMART reading. `at` is Unix seconds. */
export interface HealthReading {
  at: number;
  passed: boolean | null;
  temperature_c: number | null;
  wear_percent: number | null;
  written_bytes: number | null;
  power_on_hours: number | null;
  critical_warning: number | null;
  media_errors: number | null;
  self_tests: SelfTest[];
}

export interface HealthAlert {
  id: string;
  at: number;
  disk: string;
  kind: "warning" | "wear" | "media_errors";
  message: string;
}

export interface DriveCard {
  mount: string;
  disk: string | null;
  model: string | null;
  status: "ok" | "warning" | "unavailable" | "unknown";
  reachable: boolean;
  /** The latest good reading; when the connection hides SMART, the last one kept, with its date. */
  latest: HealthReading | null;
  history: HealthReading[];
}

export interface HealthReport {
  tool_available: boolean;
  sampled_at: number | null;
  drives: DriveCard[];
  /** Newest first. */
  alerts: HealthAlert[];
}

// ---- Duplicates (exact content, bounded by core) ----

export interface DuplicateGroup {
  kept_path: string;
  extras: string[];
  size_bytes: number;
}

export interface DuplicateReport {
  groups: DuplicateGroup[];
  skipped: { path: string; reason: string }[];
  diagnostics: string[];
  truncated: boolean;
  files_considered: number;
  bytes_read: number;
}

export interface DuplicatesTrashResult {
  moved: { path: string; bytes: number }[];
  skipped: { path: string; reason: string }[];
  moved_bytes: number;
}

export function bytes(n: number | null | undefined): string {
  if (n == null) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000;
    i++;
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** Saved results older than this refresh in the background when their view opens. */
const STALE_SECS = 10 * 60;

/** True when a saved result (Unix seconds) is old enough to refresh. */
export function isStale(secs: number): boolean {
  return Date.now() / 1000 - secs > STALE_SECS;
}

/** "just now", "5 min ago", "2 h ago", or the date for anything older. */
export function ago(secs: number): string {
  const elapsed = Math.max(0, Date.now() / 1000 - secs);
  if (elapsed < 90) return "just now";
  if (elapsed < 3600) return `${Math.round(elapsed / 60)} min ago`;
  if (elapsed < 86400) return `${Math.round(elapsed / 3600)} h ago`;
  return new Date(secs * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Colour for a 0–1 share, matching the notch rings. */
export function tone(fraction: number): string {
  if (fraction >= 0.85) return "var(--rk-bad)";
  if (fraction >= 0.6) return "var(--rk-warn)";
  return "var(--rk-ok)";
}

export function signedBytes(n: number): string {
  return `${n < 0 ? "−" : "+"}${bytes(Math.abs(n))}`;
}

// ---- Apps: saved inventory, streamed leftovers, icons and updates ----

export interface CachedApps {
  /** Unix seconds when the saved list was written; null when there is none. */
  saved_at: number | null;
  apps: AppEntry[];
}

/** One source's share of an app's leftovers (`apps-leftovers` events). */
export interface LeftoverPart {
  source: "bundle" | "vendor" | "background" | "library" | "receipts";
  items: RelatedItem[];
  background: BackgroundEntry[];
  receipts: string[];
}

export type LeftoversEvent =
  | { kind: "part"; path: string; part: LeftoverPart }
  | { kind: "done"; path: string; error: string | null };

/** Update state of one app. Only Sparkle, Homebrew and App Store apps can have one. */
export interface AppUpdate {
  path: string;
  name: string;
  bundle_id: string | null;
  installed_version: string | null;
  source: "app_store" | "homebrew" | "sparkle" | "none";
  state: "available" | "current" | "app_store" | "unknown" | "unavailable";
  latest_version: string | null;
  cask: string | null;
  store_url: string | null;
  reason: string | null;
  /** Unix seconds when this row was checked. */
  checked_at: number;
}

export interface UpdateReport {
  /** Unix seconds of the newest check; null before any check. */
  checked_at: number | null;
  apps: AppUpdate[];
}

export interface UpdateJob {
  path: string;
  state: "running" | "done" | "failed";
  message: string;
}

export const appsApi = {
  cached: () => invoke<CachedApps>("apps_cached"),
  refresh: () => invoke<void>("apps_refresh"),
  summary: (path: string) => invoke<AppEntry>("app_summary", { path }),
  leftovers: (path: string) => invoke<void>("app_leftovers", { path }),
  /** Cached icons by path; the rest arrive as `apps-icon` events. */
  icons: (paths: string[]) => invoke<Record<string, string>>("app_icons", { paths }),
  updatesCached: () => invoke<UpdateReport>("apps_updates_cached"),
  updatesRefresh: (force: boolean) => invoke<void>("apps_updates_refresh", { force }),
  /** Returns "running", "opened" or "store". */
  update: (path: string) => invoke<string>("app_update", { path }),
};

// ---- Nearby sharing (LocalSend protocol, run by the hub: src-tauri/src/share.rs) ----

export interface ShareDevice {
  fingerprint: string;
  alias: string;
  deviceModel?: string | null;
  deviceType?: string | null;
  ip: string;
  port: number;
  protocol: string;
}

export interface ShareIncoming {
  id: string;
  from: string;
  fingerprint: string;
  fileCount: number;
  totalBytes: number;
  isMessage: boolean;
  preview?: string | null;
  files: { name: string; size: number }[];
  known: boolean;
}

export interface ShareTransfer {
  id: string;
  direction: "send" | "receive";
  peer: string;
  state: "waiting" | "active" | "done" | "failed" | "cancelled" | "declined";
  totalBytes: number;
  doneBytes: number;
  filesTotal: number;
  filesDone: number;
  current?: string | null;
  savedTo?: string | null;
  savedFiles: string[];
  error?: string | null;
}

export interface ShareSnapshot {
  running: boolean;
  error: string | null;
  alias?: string;
  saveDir?: string;
  devices: ShareDevice[];
  incoming: ShareIncoming[];
  transfers: ShareTransfer[];
  warnings: string[];
  localNetwork: "unknown" | "granted" | "blocked";
}

export const shareApi = {
  state: () => invoke<ShareSnapshot>("share_state"),
  devices: () => invoke<ShareDevice[]>("share_devices"),
  /** Send files and/or text to a device (by fingerprint). Resolves with the transfer id. */
  send: (to: string, paths: string[], text?: string) => invoke<string>("share_send", { to, paths, text }),
  accept: (id: string) => invoke<boolean>("share_accept", { id }),
  decline: (id: string) => invoke<boolean>("share_decline", { id }),
  cancel: (id: string) => invoke<boolean>("share_cancel", { id }),
  dismiss: (id: string) => invoke<void>("share_dismiss", { id }),
  /**
   * Events from the service: `share-devices` (ShareDevice[]), `share-incoming` (ShareIncoming),
   * `share-incoming-resolved` (id), `share-progress` (ShareTransfer).
   */
};
