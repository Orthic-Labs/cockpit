import { useEffect, useMemo, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { bytes } from "../api";

interface Finding {
  id: string;
  rule_id: string;
  category: string;
  name: string;
  path: string;
  bytes: number;
  partial: boolean;
  risk: "safe" | "review" | "info";
  eligible: boolean;
  preselected: boolean;
  reason: string;
  action: string;
  dev: string;
  ino: string;
}

interface Report {
  findings: Finding[];
  safe_bytes: number;
  review_bytes: number;
  scanned_at: number;
}

interface Skipped {
  path: string;
  reason: string;
}

interface ApplyResult {
  moved_items: number;
  moved_bytes: number;
  skipped: Skipped[];
  activity_id: string | null;
}

interface RestoreResult {
  restored_items: number;
  restored_bytes: number;
  skipped: Skipped[];
}

interface ActivityItem {
  path: string;
  trash_path: string | null;
  bytes: number;
  restored: boolean;
}

interface Activity {
  id: string;
  at: number;
  bytes: number;
  items: ActivityItem[];
}

const cleanup = {
  scan: () => invoke<Report>("cleanup_scan"),
  apply: (items: { rule_id: string; path: string; dev: string; ino: string }[]) =>
    invoke<ApplyResult>("cleanup_apply", { items }),
  history: () => invoke<Activity[]>("cleanup_history"),
  restore: (id: string) => invoke<RestoreResult>("cleanup_restore", { id }),
};

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");

export function Cleanup() {
  const [report, setReport] = useState<Report | null>(null);
  const [history, setHistory] = useState<Activity[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const run = async (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const load = () =>
    run(async () => {
      const next = await cleanup.scan();
      setReport(next);
      setSelected(new Set(next.findings.filter((f) => f.preselected).map((f) => f.id)));
      setConfirming(false);
      setHistory(await cleanup.history());
    });

  useEffect(() => {
    load();
  }, []);

  const eligible = (report?.findings ?? []).filter((f) => f.eligible);
  const safe = eligible.filter((f) => f.risk === "safe");
  const review = eligible.filter((f) => f.risk === "review");
  const held = (report?.findings ?? []).filter((f) => !f.eligible);
  const chosen = useMemo(() => eligible.filter((f) => selected.has(f.id)), [report, selected]);
  const chosenBytes = chosen.reduce((sum, f) => sum + f.bytes, 0);
  const canGo = (report?.safe_bytes ?? 0) + (report?.review_bytes ?? 0);

  const toggle = (id: string) => {
    setConfirming(false);
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const move = () =>
    run(async () => {
      const result = await cleanup.apply(
        chosen.map((f) => ({ rule_id: f.rule_id, path: f.path, dev: f.dev, ino: f.ino })),
      );
      const skipped = result.skipped.length ? `; ${result.skipped.length} skipped` : "";
      setNotice(`Moved to Trash: ${bytes(result.moved_bytes)} (${result.moved_items} items${skipped})`);
      if (result.skipped.length) setError(result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n"));
      setConfirming(false);
      const next = await cleanup.scan();
      setReport(next);
      setSelected(new Set(next.findings.filter((f) => f.preselected).map((f) => f.id)));
      setHistory(await cleanup.history());
    });

  const restore = (id: string) =>
    run(async () => {
      const result = await cleanup.restore(id);
      setNotice(`Restored: ${bytes(result.restored_bytes)} (${result.restored_items} items)`);
      if (result.skipped.length) setError(result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n"));
      setHistory(await cleanup.history());
      const next = await cleanup.scan();
      setReport(next);
    });

  const Group = ({ title, note, items }: { title: string; note: string; items: Finding[] }) =>
    items.length === 0 ? null : (
      <div>
        <div className="section">
          {title} · {bytes(items.reduce((s, f) => s + f.bytes, 0))} · {note}
        </div>
        {items.map((f) => (
          <label key={f.id} className="row" style={rowStyle} title={f.path}>
            <input type="checkbox" checked={selected.has(f.id)} onChange={() => toggle(f.id)} disabled={busy} />
            <span style={{ minWidth: 0 }}>
              <span className="name" style={{ display: "block" }}>
                {f.name}
                <span className="muted small"> · {f.category}</span>
              </span>
              <span className="muted small" style={{ display: "block", whiteSpace: "normal" }}>
                {f.reason}
              </span>
            </span>
            <span className="size">
              {f.partial ? "≥ " : ""}
              {bytes(f.bytes)}
            </span>
          </label>
        ))}
      </div>
    );

  return (
    <div className="view" style={{ overflow: "auto" }}>
      <div className="volume-head" style={{ marginBottom: 0 }}>
        <span className="strong" style={{ fontSize: 15 }}>
          {report ? `${bytes(canGo)} can go` : "Looking for what can go…"}
        </span>
        <button className="btn" onClick={load} disabled={busy}>
          {busy ? "Working…" : "Rescan"}
        </button>
      </div>
      <div className="muted small">
        Nothing is deleted. Items go to the Trash, where you can put them back or empty it yourself.
      </div>

      {notice && <div className="strong">{notice}</div>}
      {error && (
        <div className="error" style={{ whiteSpace: "pre-wrap" }}>
          {error}
        </div>
      )}

      {report && eligible.length === 0 && <div className="muted">Nothing to clean up right now.</div>}

      <Group title="Safe to regenerate" note="preselected" items={safe} />
      <Group title="Review" note="your call, never preselected" items={review} />

      {held.length > 0 && (
        <div>
          <div className="section">Not offered</div>
          {held.map((f) => (
            <div key={f.id} className="row" style={{ ...rowStyle, gridTemplateColumns: "minmax(0, 1fr) 72px" }} title={f.path}>
              <span style={{ minWidth: 0 }}>
                <span className="name" style={{ display: "block" }}>
                  {f.name}
                </span>
                <span className="muted small" style={{ display: "block", whiteSpace: "normal" }}>
                  {f.reason}
                </span>
              </span>
              <span className="size">{bytes(f.bytes)}</span>
            </div>
          ))}
        </div>
      )}

      {eligible.length > 0 && (
        <div className="toolbar" style={{ position: "sticky", bottom: 0, alignItems: "center" }}>
          {confirming ? (
            <>
              <span className="muted" style={{ flex: 1 }}>
                Move {chosen.length} items ({bytes(chosenBytes)}) to the Trash?
              </span>
              <button className="btn" onClick={() => setConfirming(false)} disabled={busy}>
                Cancel
              </button>
              <button className="btn" style={{ background: "var(--accent)", color: "#fff" }} onClick={move} disabled={busy}>
                Move to Trash
              </button>
            </>
          ) : (
            <>
              <span className="muted" style={{ flex: 1 }}>
                {chosen.length} selected · {bytes(chosenBytes)}
              </span>
              <button
                className="btn"
                style={{ background: "var(--accent)", color: "#fff" }}
                onClick={() => setConfirming(true)}
                disabled={busy || chosen.length === 0}
              >
                Move {chosen.length} items to Trash
              </button>
            </>
          )}
        </div>
      )}

      <div>
        <div className="section">History</div>
        {history.length === 0 && <div className="muted small">Nothing moved yet.</div>}
        {history.map((a) => {
          const left = a.items.filter((i) => !i.restored);
          const canRestore = left.some((i) => i.trash_path);
          return (
            <div key={a.id} className="row" style={{ ...rowStyle, gridTemplateColumns: "minmax(0, 1fr) 72px 70px" }}>
              <span style={{ minWidth: 0 }}>
                <span className="name" style={{ display: "block" }}>
                  {new Date(a.at * 1000).toLocaleString()}
                </span>
                <span className="muted small">
                  Moved to Trash: {bytes(a.bytes)} · {a.items.length} items
                  {left.length < a.items.length ? ` · ${a.items.length - left.length} restored` : ""}
                </span>
              </span>
              <span className="size">{bytes(a.bytes)}</span>
              <button className="btn" onClick={() => restore(a.id)} disabled={busy || !canRestore}>
                Restore
              </button>
            </div>
          );
        })}
      </div>
    </div>
  );
}

const rowStyle: CSSProperties = {
  gridTemplateColumns: "18px minmax(0, 1fr) 72px",
  alignItems: "start",
};
