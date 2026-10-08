import { useEffect, useMemo, useRef, useState } from "react";
import { Button, ConfirmDialog, Toggle } from "@rightkit/app-shell/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AppWindow } from "lucide-react";
import {
  ago,
  api,
  appsApi,
  bytes,
  type AppEntry,
  type AppUpdate,
  type BackgroundEntry,
  type LeftoverPart,
  type LeftoversEvent,
  type RelatedItem,
  type UninstallResult,
  type UpdateJob,
  type UpdateReport,
} from "../api";

const DAY = 86_400;
const UNUSED_DAYS = 90;

function lastUsedText(epoch: number | null): string {
  if (epoch == null) return "Unknown";
  const days = Math.floor((Date.now() / 1000 - epoch) / DAY);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 60) return `${days} days ago`;
  return `${Math.floor(days / 30)} months ago`;
}

/** One `apps-icon` event. A null data URL means the app has no usable icon. */
interface IconEvent {
  path: string;
  data_url: string | null;
}

function byPath(apps: AppUpdate[]): Record<string, AppUpdate> {
  return Object.fromEntries(apps.map((a): [string, AppUpdate] => [a.path, a]));
}

function upsertApp(list: AppEntry[], row: AppEntry): AppEntry[] {
  const index = list.findIndex((a) => a.path === row.path);
  if (index < 0) return [...list, row];
  const next = list.slice();
  next[index] = row;
  return next;
}

export function Apps() {
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [updates, setUpdates] = useState<Record<string, AppUpdate>>({});
  const [updatesAt, setUpdatesAt] = useState<number | null>(null);
  const [checking, setChecking] = useState(false);
  const [icons, setIcons] = useState<Record<string, string | null>>({});
  const [jobs, setJobs] = useState<Record<string, UpdateJob>>({});
  const [query, setQuery] = useState("");
  const [unusedOnly, setUnusedOnly] = useState(false);
  const [updatesOnly, setUpdatesOnly] = useState(false);
  const [open, setOpen] = useState<AppEntry | null>(null);
  const [error, setError] = useState<string | null>(null);
  const iconsAsked = useRef(new Set<string>());

  const refreshList = () => {
    setRefreshing(true);
    setError(null);
    appsApi.refresh().catch((e) => {
      setRefreshing(false);
      setError(String(e));
    });
  };

  const checkUpdates = (force: boolean) => {
    setChecking(true);
    appsApi.updatesRefresh(force).catch((e) => {
      setChecking(false);
      setError(String(e));
    });
  };

  const setJob = (path: string, job: UpdateJob) => setJobs((prev) => ({ ...prev, [path]: job }));

  const startUpdate = (path: string) => {
    setJob(path, { path, state: "running", message: "Starting…" });
    appsApi
      .update(path)
      .then((outcome) => {
        if (outcome === "running") return;
        setJob(path, {
          path,
          state: "done",
          message: outcome === "store" ? "Opened in the App Store." : "Opened. Use the app's own updater.",
        });
      })
      .catch((e) => setJob(path, { path, state: "failed", message: String(e) }));
  };

  // Listeners first, then the saved data, then the background refreshes.
  useEffect(() => {
    const subs: Promise<() => void>[] = [
      listen<AppEntry>("apps-row", (e) => setApps((prev) => upsertApp(prev, e.payload))),
      listen<AppEntry[]>("apps-inventory", (e) => {
        setApps(e.payload);
        setRefreshing(false);
        setLoaded(true);
      }),
      listen<AppUpdate>("apps-update-row", (e) =>
        setUpdates((prev) => ({ ...prev, [e.payload.path]: e.payload })),
      ),
      listen<UpdateReport>("apps-updates-done", (e) => {
        setUpdates(byPath(e.payload.apps));
        setUpdatesAt(e.payload.checked_at);
        setChecking(false);
      }),
      listen<IconEvent>("apps-icon", (e) =>
        setIcons((prev) => ({ ...prev, [e.payload.path]: e.payload.data_url })),
      ),
      listen<UpdateJob>("apps-update-job", (e) => {
        setJob(e.payload.path, e.payload);
        // Homebrew changed an app: check again so its row reflects the new version.
        if (e.payload.state === "done") checkUpdates(false);
      }),
    ];
    let alive = true;
    Promise.all(subs).then(async () => {
      const saved = await appsApi.cached().catch(() => null);
      if (alive && saved && saved.apps.length > 0) setApps(saved.apps);
      if (alive) refreshList();
      if (alive) setLoaded(true);
      const savedUpdates = await appsApi.updatesCached().catch(() => null);
      if (alive && savedUpdates) {
        setUpdates(byPath(savedUpdates.apps));
        setUpdatesAt(savedUpdates.checked_at);
      }
      if (alive) checkUpdates(false);
      // `--app <path>` opens straight to that app's detail.
      const first = await invoke<string | null>("initial_app").catch(() => null);
      if (alive && first) {
        const entry = await appsApi.summary(first).catch((e) => {
          setError(String(e));
          return null;
        });
        if (alive && entry) setOpen(entry);
      }
    });
    return () => {
      alive = false;
      subs.forEach((p) => p.then((off) => off()));
    };
  }, []);

  // Icons: ask once per app; cached ones come back now, the rest as events.
  useEffect(() => {
    const wanted = apps.map((a) => a.path).filter((p) => !iconsAsked.current.has(p));
    if (wanted.length === 0) return;
    wanted.forEach((p) => iconsAsked.current.add(p));
    appsApi
      .icons(wanted)
      .then((ready) => setIcons((prev) => ({ ...prev, ...ready })))
      .catch(() => {});
  }, [apps]);

  const shown = useMemo(() => {
    const now = Date.now() / 1000;
    const q = query.trim().toLowerCase();
    return apps
      .filter(
        (a) =>
          (!q || a.name.toLowerCase().includes(q) || (a.bundle_id ?? "").toLowerCase().includes(q)) &&
          // Unknown last-used is never treated as unused.
          (!unusedOnly || (a.last_used != null && now - a.last_used >= UNUSED_DAYS * DAY)) &&
          (!updatesOnly || updates[a.path]?.state === "available"),
      )
      .sort((a, b) => b.size_bytes - a.size_bytes);
  }, [apps, query, unusedOnly, updatesOnly, updates]);

  const updateCount = useMemo(
    () => apps.filter((a) => updates[a.path]?.state === "available").length,
    [apps, updates],
  );

  if (open) {
    return (
      <Detail
        key={open.path}
        app={open}
        update={updates[open.path]}
        job={jobs[open.path]}
        onStartUpdate={() => startUpdate(open.path)}
        onBack={() => {
          setOpen(null);
          refreshList();
        }}
      />
    );
  }

  const largest = Math.max(1, ...shown.map((a) => a.size_bytes));

  return (
    <div className="view">
      <div className="toolbar">
        <input className="search" placeholder="Search apps" value={query} onChange={(e) => setQuery(e.target.value)} />
        <Toggle checked={unusedOnly} onChange={setUnusedOnly} label="Unused 90+ days" />
        <Toggle
          checked={updatesOnly}
          onChange={setUpdatesOnly}
          label={updateCount > 0 ? `Updates (${updateCount})` : "Updates"}
        />
        <Button size="sm" variant="secondary" disabled={refreshing} onClick={refreshList}>
          {refreshing ? "Refreshing…" : "Refresh"}
        </Button>
        <Button size="sm" variant="secondary" disabled={checking} onClick={() => checkUpdates(true)}>
          {checking ? "Checking…" : "Check updates"}
        </Button>
      </div>
      {error && <div className="error">{error}</div>}
      {apps.length === 0 && (refreshing || !loaded) && <div className="muted">Reading your applications…</div>}
      {apps.length === 0 && loaded && !refreshing && <div className="muted">No applications found.</div>}
      {apps.length > 0 && (
        <div className="muted small">
          {shown.length} of {apps.length} apps · {bytes(shown.reduce((n, a) => n + a.size_bytes, 0))}
          {refreshing ? " · sizes updating…" : ""}
          {checking
            ? " · checking for updates…"
            : updatesAt != null
              ? ` · updates checked ${ago(updatesAt)}`
              : ""}
        </div>
      )}
      <div className="list">
        {shown.map((a) => (
          <AppRow
            key={a.path}
            app={a}
            largest={largest}
            icon={icons[a.path]}
            update={updates[a.path]}
            job={jobs[a.path]}
            onOpen={() => setOpen(a)}
            onUpdate={() => startUpdate(a.path)}
          />
        ))}
      </div>
    </div>
  );
}

function AppIcon({ src }: { src: string | null | undefined }) {
  if (src) return <img className="apps-icon" src={src} alt="" />;
  return (
    <span className="apps-icon apps-icon--fallback" aria-hidden="true">
      <AppWindow size={15} strokeWidth={1.8} />
    </span>
  );
}

/** The update badge and action for one app. Clicks here never open the app. */
function UpdateControl({
  update,
  job,
  onUpdate,
}: {
  update: AppUpdate | undefined;
  job: UpdateJob | undefined;
  onUpdate: () => void;
}) {
  const stop = (e: { stopPropagation: () => void }) => e.stopPropagation();
  if (job?.state === "running") {
    return (
      <span className="apps-update" onClick={stop}>
        <span className="apps-status">{job.message}</span>
      </span>
    );
  }
  if (job?.state === "failed") {
    return (
      <span className="apps-update" onClick={stop}>
        <span className="apps-status apps-status--bad" title={job.message}>
          Update failed
        </span>
      </span>
    );
  }
  if (job?.state === "done") {
    return (
      <span className="apps-update" onClick={stop}>
        <span className="apps-status apps-status--ok" title={job.message}>
          {job.message}
        </span>
      </span>
    );
  }
  if (update?.state === "available") {
    return (
      <span className="apps-update" onClick={stop}>
        <span className="apps-badge">
          Update available{update.latest_version ? ` ${update.latest_version}` : ""}
        </span>
        <Button size="sm" variant="secondary" onClick={onUpdate}>
          Update
        </Button>
      </span>
    );
  }
  if (update?.state === "app_store") {
    return (
      <span className="apps-update" onClick={stop}>
        <Button size="sm" variant="secondary" onClick={onUpdate}>
          Open in App Store
        </Button>
      </span>
    );
  }
  return null;
}

function AppRow({
  app,
  largest,
  icon,
  update,
  job,
  onOpen,
  onUpdate,
}: {
  app: AppEntry;
  largest: number;
  icon: string | null | undefined;
  update: AppUpdate | undefined;
  job: UpdateJob | undefined;
  onOpen: () => void;
  onUpdate: () => void;
}) {
  return (
    <div className="row apps clickable" onClick={onOpen} title={app.path}>
      <span className="name apps-name">
        <AppIcon src={icon} />
        <span className="apps-title">
          {app.running && <span className="dot" title="Running" />}
          {app.name}
          <span className="muted small">
            {" "}
            {app.version ?? ""} · {lastUsedText(app.last_used)}
          </span>
        </span>
        <UpdateControl update={update} job={job} onUpdate={onUpdate} />
      </span>
      <span className="bar" style={{ height: 4 }}>
        <i style={{ width: `${(app.size_bytes / largest) * 100}%`, background: "var(--rk-accent)" }} />
      </span>
      <span className="size">{bytes(app.size_bytes)}</span>
    </div>
  );
}

const LOCATIONS = ["Application", "User Library", "System Library", "Installer receipt"];
const CONFIDENCE: Record<string, string> = {
  exact: "Exact id",
  helper: "Helper id",
  group: "App group",
  prefix: "Id prefix",
  team: "Team id",
  name: "Name",
  receipt: "Receipt",
};

/** The bundle row from the list's facts, shown before the bundle part arrives. */
function bundleRow(app: AppEntry): RelatedItem {
  return {
    path: app.path,
    label: "Application",
    location: "Application",
    exact: true,
    confidence: "exact",
    reason: "The application bundle",
    admin: false,
    size_bytes: app.size_bytes,
    preselected: app.protected == null,
  };
}

function mergeBackground(prev: BackgroundEntry[], more: BackgroundEntry[]): BackgroundEntry[] {
  const seen = new Set(prev.map((b) => `${b.kind}:${b.label}`));
  const out = [...prev];
  for (const b of more) {
    const key = `${b.kind}:${b.label}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push(b);
    }
  }
  return out;
}

function Detail({
  app,
  update,
  job,
  onStartUpdate,
  onBack,
}: {
  app: AppEntry;
  update: AppUpdate | undefined;
  job: UpdateJob | undefined;
  onStartUpdate: () => void;
  onBack: () => void;
}) {
  const [items, setItems] = useState<Map<string, RelatedItem>>(() => new Map([[app.path, bundleRow(app)]]));
  const [background, setBackground] = useState<BackgroundEntry[]>([]);
  const [receipts, setReceipts] = useState<string[]>([]);
  const [picked, setPicked] = useState<Set<string>>(() => new Set(app.protected == null ? [app.path] : []));
  // Paths the user has changed: leftovers arriving later never overrule them.
  const touched = useRef(new Set<string>());
  const [status, setStatus] = useState<"loading" | "done" | "error">("loading");
  const [leftoverError, setLeftoverError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<UninstallResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mergePart = (part: LeftoverPart) => {
    setItems((prev) => {
      const next = new Map(prev);
      for (const item of part.items) {
        const old = next.get(item.path);
        if (!old || part.source === "bundle" || (item.preselected && !old.preselected)) {
          next.set(item.path, item);
        }
      }
      return next;
    });
    const auto = part.items
      .filter((i) => i.preselected && !touched.current.has(i.path))
      .map((i) => i.path);
    if (auto.length > 0) setPicked((prev) => new Set([...prev, ...auto]));
    if (part.background.length > 0) setBackground((prev) => mergeBackground(prev, part.background));
    if (part.receipts.length > 0) setReceipts((prev) => [...new Set([...prev, ...part.receipts])]);
  };

  useEffect(() => {
    const sub = listen<LeftoversEvent>("apps-leftovers", (e) => {
      const event = e.payload;
      if (event.path !== app.path) return;
      if (event.kind === "part") {
        mergePart(event.part);
        return;
      }
      setLeftoverError(event.error);
      setStatus(event.error ? "error" : "done");
    });
    sub
      .then(() => appsApi.leftovers(app.path))
      .catch((err) => {
        setLeftoverError(String(err));
        setStatus("error");
      });
    return () => {
      sub.then((off) => off());
    };
  }, [app.path]);

  const all = [...items.values()];
  // Installer files under the app or a Library item are already listed there.
  const taken = [
    app.path,
    ...all.filter((i) => i.location === "User Library" || i.location === "System Library").map((i) => i.path),
  ];
  const visible = all.filter(
    (i) => i.location !== "Installer receipt" || !taken.some((t) => i.path === t || i.path.startsWith(`${t}/`)),
  );
  const pickedVisible = visible.filter((i) => picked.has(i.path));
  const total = pickedVisible.reduce((n, i) => n + i.size_bytes, 0);
  const adminPicked = pickedVisible.filter((i) => i.admin).length;

  const set = (paths: string[], on: boolean) => {
    for (const p of paths) touched.current.add(p);
    setPicked((prev) => {
      const next = new Set(prev);
      for (const p of paths) {
        if (on) next.add(p);
        else next.delete(p);
      }
      return next;
    });
  };

  const run = async () => {
    setConfirm(false);
    setBusy(true);
    setError(null);
    try {
      const r = await api.uninstall(app.path, app.bundle_id, pickedVisible.map((i) => i.path));
      setResult(r);
      const gone = new Set(r.moved.map((m) => m.path));
      setItems((prev) => {
        const next = new Map(prev);
        for (const p of gone) next.delete(p);
        return next;
      });
      setPicked(new Set());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const group = (location: string) => {
    const rows = visible.filter((i) => i.location === location);
    if (rows.length === 0) return null;
    const paths = rows.map((i) => i.path);
    const allOn = paths.every((p) => picked.has(p));
    return (
      <div key={location}>
        <div className="section-row">
          <span className="section">
            {location} <span className="muted">· {rows.length} · {bytes(rows.reduce((n, i) => n + i.size_bytes, 0))}</span>
          </span>
          {rows.length > 1 && (
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => set(paths, !allOn)}>
              {allOn ? "Clear" : "Select all"}
            </Button>
          )}
        </div>
        {rows.map((i) => (
          <label key={i.path} className="row items" title={`${i.path}\n${i.reason}`}>
            <input
              type="checkbox"
              checked={picked.has(i.path)}
              disabled={busy}
              onChange={(e) => set([i.path], e.target.checked)}
            />
            <span className="name">
              {i.path.replace(/^\/Users\/[^/]+/, "~")}
              <span className="muted small">
                {" "}
                {i.label} · {CONFIDENCE[i.confidence] ?? i.confidence}
                {i.admin ? " · admin" : ""}
                {i.preselected ? "" : " · review"}
              </span>
            </span>
            <span className="size">{bytes(i.size_bytes)}</span>
          </label>
        ))}
      </div>
    );
  };

  return (
    <div className="view">
      <div className="apps-header">
        <span className="apps-header-title">
          <span className="strong">{app.name}</span>{" "}
          <span className="muted small">
            {app.version ?? ""} {app.bundle_id ?? ""}
          </span>
        </span>
        <span className="apps-header-side">
          <UpdateControl update={update} job={job} onUpdate={onStartUpdate} />
          <Button size="sm" variant="secondary" onClick={onBack}>
            Back
          </Button>
        </span>
      </div>
      {app.protected && <div className="note">{app.protected} Uninstall is not offered.</div>}
      {app.running && !app.protected && (
        <div className="note">Running. Pulse will ask it to quit first, and will not remove anything if it stays open.</div>
      )}
      {status === "loading" && <div className="apps-progress">Finding leftovers…</div>}
      {leftoverError && <div className="error">{leftoverError}</div>}
      {error && <div className="error">{error}</div>}
      {result && (
        <div className="result">
          <div className="ok-note">
            Moved {result.moved.length} to Trash · freed {bytes(result.moved_bytes)}
            {result.failed.length > 0 && <span className="error"> · {result.failed.length} failed</span>}
          </div>
          {result.failed.map((f) => (
            <div key={f.path} className="error small" title={f.path}>
              {f.path.replace(/^\/Users\/[^/]+/, "~")}: {f.error}
            </div>
          ))}
        </div>
      )}
      <div className="list">{LOCATIONS.map(group)}</div>
      {background.length > 0 && (
        <div className="note">
          <div className="section">Login and background items</div>
          {background.map((b) => (
            <div key={`${b.kind}:${b.label}`} className="small muted" title={b.path ?? ""}>
              {b.kind} · {b.label}
            </div>
          ))}
        </div>
      )}
      {receipts.length > 0 && <div className="small muted">Installer packages: {receipts.join(", ")}</div>}
      {!app.protected && (
        <div className="section-row">
          <span className="muted small">
            {pickedVisible.length} selected · {bytes(total)}
            {adminPicked > 0 && ` · ${adminPicked} need an administrator password`}
          </span>
          <Button
            size="sm"
            variant="danger"
            disabled={busy || status === "loading" || pickedVisible.length === 0}
            onClick={() => setConfirm(true)}
          >
            {busy ? "Working…" : "Move to Trash"}
          </Button>
        </div>
      )}
      {confirm && (
        <ConfirmDialog
          danger
          title={`Move ${pickedVisible.length} item${pickedVisible.length === 1 ? "" : "s"} to Trash?`}
          description={`${app.name} and the selected files, ${bytes(total)} in all. Everything goes to the Trash, so it can be put back.${
            app.running ? " The app will be asked to quit first." : ""
          }${adminPicked > 0 ? " Root-owned items make Finder ask for your password." : ""}`}
          confirmLabel="Move to Trash"
          onConfirm={run}
          onCancel={() => setConfirm(false)}
        />
      )}
    </div>
  );
}
