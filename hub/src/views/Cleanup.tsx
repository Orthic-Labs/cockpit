import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, ConfirmDialog, SegmentedControl } from "@rightkit/app-shell/react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronRight } from "lucide-react";
import { ago, bytes, isStale, type ChromeSnapshots } from "../api";
import { ChromeSnapshotsLine } from "./ChromeSnapshots";
import "./cleanup.css";

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
  chrome_snapshots?: ChromeSnapshots | null;
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
  cached: () => invoke<Report | null>("cleanup_cached"),
  apply: (items: { rule_id: string; path: string; dev: string; ino: string }[]) =>
    invoke<ApplyResult>("cleanup_apply", { items }),
  history: () => invoke<Activity[]>("cleanup_history"),
  restore: (id: string) => invoke<RestoreResult>("cleanup_restore", { id }),
};

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
const items = (n: number) => `${n} item${n === 1 ? "" : "s"}`;

// One predicate for what can be selected, totalled and submitted. The backend
// still decides again when the move runs, and unknown liveness never passes.
const movable = (f: Finding) =>
  f.eligible && f.action === "Move to Trash" && (f.risk === "safe" || f.risk === "review") && !!f.dev && !!f.ino;

const total = (xs: Finding[]) => {
  const sum = xs.reduce((s, f) => s + f.bytes, 0);
  return `${xs.some((f) => f.partial) ? "≥ " : ""}${bytes(sum)}`;
};

type Pane = "findings" | "history";
type Note = { tone: "ok" | "warn" | "error"; text: string; detail?: string };

function GroupHead({
  title,
  xs,
  selected,
  onToggle,
  disabled,
}: {
  title: string;
  xs: Finding[];
  selected: Set<string>;
  onToggle: (on: boolean) => void;
  disabled: boolean;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const count = xs.filter((f) => selected.has(f.id)).length;
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = count > 0 && count < xs.length;
  }, [count, xs.length]);
  return (
    <div className="ck-group-head">
      <label>
        <span className="ck-check">
          <input
            ref={ref}
            type="checkbox"
            aria-label={`Select all ${title.toLowerCase()} items`}
            checked={count === xs.length}
            onChange={(e) => onToggle(e.target.checked)}
            disabled={disabled}
          />
        </span>
        <strong>{title}</strong>
      </label>
      <span className="muted ck-num">{total(xs)}</span>
    </div>
  );
}

export function Cleanup() {
  const [report, setReport] = useState<Report | null>(null);
  const [history, setHistory] = useState<Activity[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState<Finding[] | null>(null);
  const [pane, setPane] = useState<Pane>("findings");
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<Note | null>(null);
  const [restoreIssue, setRestoreIssue] = useState<Record<string, string>>({});
  const seeded = useRef(false);

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

  // Preselected items are chosen once; later scans keep only what is still movable.
  const show = (next: Report) => {
    const first = !seeded.current;
    seeded.current = true;
    setReport(next);
    setSelected((prev) => {
      const now = next.findings.filter(movable);
      return first
        ? new Set(now.filter((f) => f.preselected).map((f) => f.id))
        : new Set(now.filter((f) => prev.has(f.id)).map((f) => f.id));
    });
    setPending(null);
  };

  // A rescan runs behind the page, which stays usable while it runs.
  const refresh = async () => {
    setRefreshing(true);
    setError(null);
    try {
      show(await cleanup.scan());
    } catch (e) {
      setError(String(e));
    } finally {
      setRefreshing(false);
    }
  };

  // Saved findings show at once. A rescan follows only when there are none or they are old.
  useEffect(() => {
    cleanup.history().then(setHistory).catch(() => {});
    cleanup
      .cached()
      .then((saved) => {
        if (saved) show(saved);
        if (!saved || isStale(saved.scanned_at)) void refresh();
      })
      .catch(() => void refresh());
  }, []);

  const all = report?.findings ?? [];
  const eligible = all.filter(movable);
  const safe = eligible.filter((f) => f.risk === "safe");
  const review = eligible.filter((f) => f.risk === "review");
  const held = all.filter((f) => !movable(f));
  const chosen = useMemo(() => eligible.filter((f) => selected.has(f.id)), [report, selected]);
  const locked = busy || refreshing;

  const toggle = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const toggleGroup = (xs: Finding[], on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      xs.forEach((f) => (on ? next.add(f.id) : next.delete(f.id)));
      return next;
    });

  const move = (picked: Finding[]) => {
    setPending(null);
    return run(async () => {
      const result = await cleanup.apply(
        picked.filter(movable).map((f) => ({ rule_id: f.rule_id, path: f.path, dev: f.dev, ino: f.ino })),
      );
      const detail = result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n");
      setNote(
        result.skipped.length
          ? {
              tone: "warn",
              text: `Partly moved: ${bytes(result.moved_bytes)} (${items(result.moved_items)}) went to the Trash, ${result.skipped.length} skipped.`,
              detail,
            }
          : { tone: "ok", text: `Moved to the Trash: ${bytes(result.moved_bytes)} (${items(result.moved_items)}).` },
      );
      show(await cleanup.scan());
      setHistory(await cleanup.history());
    });
  };

  const restore = (id: string) =>
    run(async () => {
      setRestoreIssue((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      const result = await cleanup.restore(id);
      const detail = result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n");
      if (result.skipped.length) setRestoreIssue((prev) => ({ ...prev, [id]: detail }));
      setNote(
        result.skipped.length
          ? { tone: "warn", text: `Partly restored: ${bytes(result.restored_bytes)} (${items(result.restored_items)}), ${result.skipped.length} stayed in the Trash.` }
          : { tone: "ok", text: `Restored: ${bytes(result.restored_bytes)} (${items(result.restored_items)}).` },
      );
      setHistory(await cleanup.history());
      show(await cleanup.scan());
    });

  const Group = ({ title, xs }: { title: string; xs: Finding[] }) =>
    xs.length === 0 ? null : (
      <section className="ck-group">
        <GroupHead title={title} xs={xs} selected={selected} onToggle={(on) => toggleGroup(xs, on)} disabled={locked} />
        {xs.map((f) => (
          <div key={f.id} className="ck-row">
            <span className="ck-check">
              <input
                type="checkbox"
                aria-label={`Select ${f.name}`}
                checked={selected.has(f.id)}
                onChange={() => toggle(f.id)}
                disabled={locked}
              />
            </span>
            <details className="ck-detail">
              <summary>
                <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                <span>
                  <span className="ck-title">
                    <strong>{f.name}</strong>
                    <Badge tone={f.risk === "safe" ? "ok" : "warn"}>{f.risk === "safe" ? "Safe" : "Review"}</Badge>
                  </span>
                  <span className="muted ck-reason">{f.reason}</span>
                </span>
              </summary>
              <p className="muted ck-path">{shortPath(f.path)}</p>
              <p className="muted ck-path">{f.category}</p>
            </details>
            <span className="ck-num ck-size">
              {f.partial ? "≥ " : ""}
              {bytes(f.bytes)}
            </span>
          </div>
        ))}
      </section>
    );

  const historyPane = (
    <div className="ck-card">
      <h2>Moves and restore</h2>
      {history.length === 0 && <p className="muted">Nothing moved yet. Confirmed moves appear here.</p>}
      {history.map((a) => {
        const left = a.items.filter((i) => !i.restored);
        const canRestore = left.some((i) => i.trash_path);
        const issue = restoreIssue[a.id];
        return (
          <div key={a.id} className="ck-history">
            <div className="ck-history-main">
              <strong>{new Date(a.at * 1000).toLocaleString()}</strong>
              <span className="muted">
                {items(a.items.length)} moved to the Trash · {bytes(a.bytes)}
                {left.length < a.items.length ? ` · ${a.items.length - left.length} restored` : ""}
              </span>
              <details className="ck-detail ck-paths">
                <summary>
                  <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                  <span>Paths</span>
                </summary>
                {a.items.map((i) => (
                  <p key={i.path} className="muted ck-path">
                    {shortPath(i.path)} · {i.restored ? "Restored" : i.trash_path ? "In the Trash" : "Not restorable"}
                  </p>
                ))}
              </details>
              {issue && (
                <p className="ck-issue" role="alert">
                  Some items stayed in the Trash.
                  <span className="ck-lines">{issue}</span>
                </p>
              )}
            </div>
            <Button size="sm" variant="secondary" onClick={() => restore(a.id)} disabled={locked || !canRestore}>
              {issue ? "Retry restore" : left.length === 0 ? "Restored" : "Restore"}
            </Button>
          </div>
        );
      })}
    </div>
  );

  const heading = !report
    ? "Looking for what can go…"
    : eligible.length === 0
      ? "Nothing to clean up"
      : `${total(eligible)} can go`;

  return (
    <div className="view ck-cleanup">
      <div className="ck-head">
        <div>
          <h2>{heading}</h2>
          {report && (
            <p className="muted">
              {eligible.length > 0 ? `${items(eligible.length)} · ` : ""}Updated {ago(report.scanned_at)}
              {refreshing ? " · Refreshing…" : ""}
            </p>
          )}
        </div>
        <Button size="sm" variant="secondary" onClick={refresh} disabled={locked}>
          {refreshing ? "Refreshing…" : "Rescan"}
        </Button>
      </div>

      <SegmentedControl
        label="Cleanup workspace"
        value={pane}
        options={[
          { value: "findings", label: "Findings" },
          { value: "history", label: `History · ${history.length}` },
        ]}
        onChange={(v) => setPane(v as Pane)}
      />

      {note && (
        <div className={`ck-notice ${note.tone}`} role="status">
          {note.text}
          {note.detail && <span className="ck-lines">{note.detail}</span>}
          {pane === "findings" && history.length > 0 && (
            <span>
              <Button size="sm" variant="secondary" onClick={() => setPane("history")}>
                History and restore
              </Button>
            </span>
          )}
        </div>
      )}
      {error && (
        <div className="ck-notice error" role="alert">
          <span className="ck-lines">{error}</span>
        </div>
      )}

      <div className="ck-scroll" tabIndex={0} aria-label="Cleanup content">
        {pane === "history" ? (
          historyPane
        ) : (
          <>
            {report && report.findings.some((f) => f.partial) && (
              <div className="ck-notice warn">Some sizes are partial; ≥ marks a lower bound.</div>
            )}
            <ChromeSnapshotsLine info={report?.chrome_snapshots} />
            {report && eligible.length === 0 && <p className="muted ck-empty">Nothing to clean up right now. Rescan after apps close.</p>}
            {eligible.length > 0 && (
              <div className="ck-card">
                <Group title="Safe to regenerate" xs={safe} />
                <Group title="Review" xs={review} />
              </div>
            )}
            {held.length > 0 && (
              <details className="ck-held">
                <summary>
                  <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                  <span>
                    <strong>{held.length} not offered</strong>
                    <span className="muted"> · informational, running or unverified</span>
                  </span>
                </summary>
                {held.map((f) => (
                  <div key={f.id} className="ck-held-row">
                    <span>
                      <strong>{f.name}</strong>
                      <span className="muted ck-reason">{f.reason}</span>
                    </span>
                    <span className="ck-num ck-size">{bytes(f.bytes)}</span>
                  </div>
                ))}
              </details>
            )}
            <details className="ck-about">
              <summary>
                <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                <span>About these numbers</span>
              </summary>
              <p className="muted">
                Nothing is deleted. Items go to the Trash, where you can put them back or empty it yourself. Sizes are what the
                scan measured; ≥ marks a lower bound when part of a folder could not be read. Items whose owner could not be
                verified, or whose app is running, are never offered.
              </p>
            </details>
          </>
        )}
      </div>

      {pane === "findings" && eligible.length > 0 && (
        <div className="ck-selection">
          <div>
            <strong className="ck-num">
              {chosen.length} selected · {total(chosen)}
            </strong>
            <span className="muted"> to move to the Trash</span>
          </div>
          <Button onClick={() => setPending(chosen)} disabled={locked || chosen.length === 0}>
            Move {chosen.length} to Trash
          </Button>
        </div>
      )}

      {pending && (
        <ConfirmDialog
          title={`Move ${items(pending.length)} to the Trash?`}
          description={`${items(pending.length)}, ${total(pending)}, will move to the Trash. Nothing is deleted: you can put them back from History here or from the Trash.`}
          confirmLabel="Move to Trash"
          onConfirm={() => move(pending)}
          onCancel={() => setPending(null)}
        />
      )}
    </div>
  );
}
