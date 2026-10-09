import { useEffect, useState } from "react";
import { Badge, Button, ConfirmDialog, EmptyState } from "@rightkit/app-shell/react";
import { Copy } from "lucide-react";
import { api, bytes, isWindows, type DuplicateReport } from "../api";
import "./health.css";

// Windows moves extra copies to the Recycle Bin; the Mac moves them to the Trash.
const TRASH = isWindows ? "Recycle Bin" : "Trash";

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
const copies = (n: number) => `${n} cop${n === 1 ? "y" : "ies"}`;

interface Item {
  kept: string;
  path: string;
  bytes: number;
}

/** The last result and folder, kept while the user is on another tab. */
let cached: { folder: string; report: DuplicateReport; removed: string[] } | null = null;

/** Exact-content duplicates under a folder. Extra copies move to the Trash; the kept copy stays. */
export function Duplicates() {
  const [folder, setFolder] = useState(cached?.folder ?? "");
  const [draft, setDraft] = useState(cached?.folder ?? "");
  const [report, setReport] = useState<DuplicateReport | null>(cached?.report ?? null);
  const [removed, setRemoved] = useState<Set<string>>(new Set(cached?.removed ?? []));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pending, setPending] = useState<{ title: string; items: Item[] } | null>(null);

  useEffect(() => {
    if (!cached?.folder) {
      api
        .homePath()
        .then((home) => {
          setFolder((current) => current || home);
          setDraft((current) => current || home);
        })
        .catch(() => {});
    }
  }, []);

  useEffect(() => {
    cached = report && folder ? { folder, report, removed: [...removed] } : null;
  }, [folder, report, removed]);

  // Looks under the chosen folder. Core bounds the file count, bytes read and time; the
  // hub runs it off the UI thread, so the rest of the window stays responsive.
  const find = async () => {
    const path = draft.trim();
    if (!path) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    setPending(null);
    try {
      const next = await api.duplicatesScan(path);
      setFolder(path);
      setReport(next);
      setRemoved(new Set());
    } catch (e) {
      setReport(null);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const moveToTrash = async (items: Item[]) => {
    setPending(null);
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await api.duplicatesTrash(items.map((item) => ({ kept: item.kept, path: item.path })));
      const movedPaths = result.moved.map((m) => m.path);
      setRemoved((prev) => new Set([...prev, ...movedPaths]));
      setNotice(
        `Moved ${copies(result.moved.length)} to the ${TRASH}, ${bytes(result.moved_bytes)}. The kept copies are untouched.` +
          (result.skipped.length ? ` ${result.skipped.length} skipped.` : ""),
      );
      if (result.skipped.length) {
        setError(result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n"));
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const groups = (report?.groups ?? [])
    .map((group) => ({
      ...group,
      extras: group.extras.filter((path) => !removed.has(path)),
    }))
    .filter((group) => group.extras.length > 0);
  const reclaimable = groups.reduce((sum, group) => sum + group.extras.length * group.size_bytes, 0);
  const itemsFor = (kept: string, size: number, paths: string[]): Item[] =>
    paths.map((path) => ({ kept, path, bytes: size }));

  return (
    <div className="duplicates">
      <div className="toolbar dup-controls">
        <input
          className="search dup-path"
          aria-label="Folder to check for duplicates"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") find();
          }}
          placeholder="/Users/you/Documents"
        />
        <Button size="sm" variant="ghost" onClick={() => api.homePath().then((home) => setDraft(home)).catch(() => {})} disabled={busy}>
          Home
        </Button>
        <Button size="sm" onClick={find} disabled={busy || !draft.trim()}>
          <Copy size={12} /> {busy ? "Looking…" : "Find copies"}
        </Button>
      </div>

      {busy && <div className="muted small">Reading files in {shortPath(draft.trim())} to compare them. This can take a little while.</div>}
      {notice && <div className="notice small"><span>{notice}</span></div>}
      {error && <div className="error" style={{ whiteSpace: "pre-wrap" }}>{error}</div>}

      {report && !busy && (
        <>
          <div className="muted small">
            {groups.length === 0
              ? `No extra copies in ${shortPath(folder)}.`
              : `${copies(groups.reduce((n, g) => n + g.extras.length, 0))} in ${groups.length} group${groups.length === 1 ? "" : "s"} · ${bytes(reclaimable)} can be freed by moving the extras to the ${TRASH}`}
            {` · checked ${report.files_considered.toLocaleString()} files`}
            {report.truncated ? " · the folder is very large, so only part of it was checked" : ""}
            {report.skipped.length ? ` · ${report.skipped.length} skipped (links, cloud placeholders, unreadable)` : ""}
          </div>
          {report.diagnostics.map((line) => (
            <div key={line} className="muted small">{line}</div>
          ))}
        </>
      )}

      {!report && !busy && !error && <EmptyState icon={<Copy size={22} />} title="Choose a folder, then find copies" />}

      {groups.map((group) => {
        const all = itemsFor(group.kept_path, group.size_bytes, group.extras);
        return (
          <section className="card-block dup-group" key={group.kept_path}>
            <div className="block-head">
              <span className="strong">
                {copies(group.extras.length + 1)} · {bytes(group.size_bytes)} each
              </span>
              <Button size="sm" onClick={() => setPending({ title: `${copies(all.length)} of ${shortPath(group.kept_path)}`, items: all })} disabled={busy}>
                Move {all.length} extra{all.length === 1 ? "" : "s"} to {TRASH}
              </Button>
            </div>
            <div className="dup-row">
              <Badge tone="ok">Keep</Badge>
              <span className="name small" title={group.kept_path}>{shortPath(group.kept_path)}</span>
              <span />
              <span />
            </div>
            {group.extras.map((path) => (
              <div className="dup-row sub" key={path}>
                <span />
                <span className="name small" title={path}>{shortPath(path)}</span>
                <span className="size small">{bytes(group.size_bytes)}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => setPending({ title: shortPath(path), items: itemsFor(group.kept_path, group.size_bytes, [path]) })}
                  disabled={busy}
                >
                  Move to {TRASH}
                </Button>
              </div>
            ))}
          </section>
        );
      })}

      {pending && (
        <ConfirmDialog
          title={pending.items.length === 1 ? `Move this copy to the ${TRASH}?` : `Move ${pending.items.length} copies to the ${TRASH}?`}
          description={`${pending.title}: ${copies(pending.items.length)}, ${bytes(pending.items.reduce((s, i) => s + i.bytes, 0))} will move to the ${TRASH}. The copy you keep stays where it is. Nothing is deleted: you can put them back from the ${TRASH}.`}
          confirmLabel={`Move to ${TRASH}`}
          onConfirm={() => moveToTrash(pending.items)}
          onCancel={() => setPending(null)}
        />
      )}
    </div>
  );
}
