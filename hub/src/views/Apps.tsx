import { useEffect, useMemo, useState } from "react";
import { Button, ConfirmDialog, Toggle } from "@rightkit/app-shell/react";
import { api, bytes, type AppDetail, type AppEntry, type UninstallResult } from "../api";

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

export function Apps() {
  const [apps, setApps] = useState<AppEntry[] | null>(null);
  const [query, setQuery] = useState("");
  const [unusedOnly, setUnusedOnly] = useState(false);
  const [detail, setDetail] = useState<AppDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setError(null);
    api.apps().then(setApps).catch((e) => setError(String(e)));
  };
  useEffect(load, []);

  const open = (app: AppEntry) => {
    setError(null);
    api.appDetail(app.path).then(setDetail).catch((e) => setError(String(e)));
  };

  const shown = useMemo(() => {
    const now = Date.now() / 1000;
    const q = query.trim().toLowerCase();
    return (apps ?? []).filter(
      (a) =>
        (!q || a.name.toLowerCase().includes(q) || (a.bundle_id ?? "").toLowerCase().includes(q)) &&
        // Unknown last-used is never treated as unused.
        (!unusedOnly || (a.last_used != null && now - a.last_used >= UNUSED_DAYS * DAY)),
    );
  }, [apps, query, unusedOnly]);

  if (detail) {
    return (
      <Detail
        initial={detail}
        onBack={() => {
          setDetail(null);
          load();
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
        <Button size="sm" variant="secondary" onClick={load}>
          Refresh
        </Button>
      </div>
      {error && <div className="error">{error}</div>}
      {!apps && !error && <div className="muted">Reading your applications…</div>}
      {apps && (
        <div className="muted small">
          {shown.length} of {apps.length} apps · {bytes(shown.reduce((n, a) => n + a.size_bytes, 0))}
        </div>
      )}
      <div className="list">
        {shown.map((a) => (
          <div key={a.path} className="row apps clickable" onClick={() => open(a)} title={a.path}>
            <span className="name">
              {a.running && <span className="dot" title="Running" />}
              {a.name}
              <span className="muted small"> {a.version ?? ""} · {lastUsedText(a.last_used)}</span>
            </span>
            <span className="bar" style={{ height: 4 }}>
              <i style={{ width: `${(a.size_bytes / largest) * 100}%`, background: "var(--accent)" }} />
            </span>
            <span className="size">{bytes(a.size_bytes)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function Detail({ initial, onBack }: { initial: AppDetail; onBack: () => void }) {
  const { app } = initial;
  const [items, setItems] = useState(initial.items);
  const [picked, setPicked] = useState(() => new Set(initial.items.filter((i) => i.preselected).map((i) => i.path)));
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<UninstallResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const exact = items.filter((i) => i.exact);
  const review = items.filter((i) => !i.exact);
  const total = items.filter((i) => picked.has(i.path)).reduce((n, i) => n + i.size_bytes, 0);

  const toggle = (path: string, on: boolean) =>
    setPicked((prev) => {
      const next = new Set(prev);
      if (on) next.add(path);
      else next.delete(path);
      return next;
    });

  const run = async () => {
    setConfirm(false);
    setBusy(true);
    setError(null);
    try {
      const r = await api.uninstall(app.path, app.bundle_id, [...picked]);
      setResult(r);
      const gone = new Set(r.moved.map((m) => m.path));
      setItems((prev) => prev.filter((i) => !gone.has(i.path)));
      setPicked(new Set());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const group = (title: string, note: string, rows: typeof items) =>
    rows.length > 0 && (
      <>
        <div className="section">
          {title} <span className="muted">· {note}</span>
        </div>
        {rows.map((i) => (
          <label key={i.path} className="row items" title={i.path}>
            <input
              type="checkbox"
              checked={picked.has(i.path)}
              disabled={busy}
              onChange={(e) => toggle(i.path, e.target.checked)}
            />
            <span className="name">
              {i.path.replace(/^\/Users\/[^/]+/, "~")}
              <span className="muted small"> {i.label}</span>
            </span>
            <span className="size">{bytes(i.size_bytes)}</span>
          </label>
        ))}
      </>
    );

  return (
    <div className="view">
      <div className="section-row">
        <span>
          <span className="strong">{app.name}</span>{" "}
          <span className="muted small">
            {app.version ?? ""} {app.bundle_id ?? ""}
          </span>
        </span>
        <Button size="sm" variant="secondary" onClick={onBack}>
          Back
        </Button>
      </div>
      {app.protected && <div className="note">{app.protected} Uninstall is not offered.</div>}
      {app.running && !app.protected && (
        <div className="note">Running. Cockpit will ask it to quit first, and will not remove anything if it stays open.</div>
      )}
      {error && <div className="error">{error}</div>}
      {result && (
        <div className="ok-note">
          Moved to Trash: {bytes(result.moved_bytes)}
          {result.failed.length > 0 && (
            <span className="error"> · {result.failed.length} left in place: {result.failed[0].error}</span>
          )}
        </div>
      )}
      <div className="list">
        {group("Found by bundle id", "selected", exact)}
        {group("Name matches", "review, not selected", review)}
      </div>
      {!app.protected && (
        <div className="section-row">
          <span className="muted small">
            {picked.size} selected · {bytes(total)}
          </span>
          <Button size="sm" variant="danger" disabled={busy || picked.size === 0} onClick={() => setConfirm(true)}>
            {busy ? "Working…" : "Move to Trash"}
          </Button>
        </div>
      )}
      {confirm && (
        <ConfirmDialog
          danger
          title={`Move ${picked.size} item${picked.size === 1 ? "" : "s"} to Trash?`}
          description={`${app.name} and the selected files, ${bytes(total)} in all. Everything goes to the Trash, so it can be put back.${
            app.running ? " The app will be asked to quit first." : ""
          }`}
          confirmLabel="Move to Trash"
          onConfirm={run}
          onCancel={() => setConfirm(false)}
        />
      )}
    </div>
  );
}
