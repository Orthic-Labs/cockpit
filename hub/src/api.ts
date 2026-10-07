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
}

export interface Folder {
  path: string;
  root: string;
  rows: Row[];
  total_children: number;
  incomplete: boolean;
  reasons: string[];
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
  label: string;
  exact: boolean;
  size_bytes: number;
  preselected: boolean;
}

export interface AppDetail {
  app: AppEntry;
  items: RelatedItem[];
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

export type QuitOutcome = "quit" | "still_running";

export const api = {
  status: () => invoke<Status>("status"),
  processes: () => invoke<Process[]>("processes"),
  scan: (path?: string) => invoke<Folder>("scan", { path: path ?? null }),
  children: (path: string) => invoke<Folder>("children", { path }),
  search: (query: string) => invoke<Row[]>("search", { query }),
  apps: () => invoke<AppEntry[]>("apps_list"),
  appDetail: (path: string) => invoke<AppDetail>("app_detail", { path }),
  uninstall: (path: string, bundleId: string | null, items: string[]) =>
    invoke<UninstallResult>("app_uninstall", { path, bundleId, items }),
  processRows: () => invoke<ProcessRow[]>("process_rows"),
  quit: (r: ProcessRow) =>
    invoke<QuitOutcome>("process_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
  forceQuit: (r: ProcessRow) =>
    invoke<QuitOutcome>("process_force_quit", { key: r.key, pid: r.lead.pid, startTime: r.lead.start_time }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
};

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

/** Colour for a 0–1 share, matching the notch rings. */
export function tone(fraction: number): string {
  if (fraction >= 0.85) return "var(--bad)";
  if (fraction >= 0.6) return "var(--warn)";
  return "var(--ok)";
}
