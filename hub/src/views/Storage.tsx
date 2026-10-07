import { useEffect, useMemo, useState } from "react";
import { Badge, Button, ConfirmDialog, EmptyState } from "@rightkit/app-shell/react";
import { ChevronRight, File, Folder as FolderIcon, HardDrive, RefreshCw, ShieldCheck, Undo2, Usb } from "lucide-react";
import {
  api,
  bytes,
  signedBytes,
  tone,
  type CleanupFinding,
  type CleanupReport,
  type Folder,
  type Growth,
  type Row,
  type Volume,
} from "../api";
import { KINDS, kindOf, squarify } from "../chart";

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
const VISIBLE_FINDINGS = 4;

export function Storage() {
  const [volumes, setVolumes] = useState<Volume[]>([]);
  const [active, setActive] = useState<string>("/");
  const [folder, setFolder] = useState<Folder | null>(null);
  const [results, setResults] = useState<Row[] | null>(null);
  const [query, setQuery] = useState("");
  const [growth, setGrowth] = useState<Growth | null>(null);
  const [report, setReport] = useState<CleanupReport | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [pending, setPending] = useState<CleanupFinding[] | null>(null);
  const [undo, setUndo] = useState<{ id: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

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

  const loadFindings = () =>
    api.cleanupScan().then(setReport).catch(() => setReport(null));

  // The startup disk is scanned from the home folder; other volumes from their root.
  const scanVolume = (mount: string, list: Volume[] = volumes) =>
    run(async () => {
      setActive(mount);
      setQuery("");
      const volume = list.find((v) => v.mount_point === mount);
      const internal = !volume || volume.internal;
      setFolder(await api.scan(internal ? undefined : mount));
      if (internal) {
        // Not awaited: the list is usable while these load.
        api.growth().then(setGrowth).catch(() => setGrowth(null));
      } else {
        setGrowth(null);
      }
    });
  const open = (path: string) => run(async () => setFolder(await api.children(path)));

  useEffect(() => {
    api
      .volumes()
      .then((list) => {
        setVolumes(list);
        scanVolume("/", list);
      })
      .catch(() => scanVolume("/", []));
    loadFindings();
  }, []);

  useEffect(() => {
    if (!folder || query.trim().length < 2) {
      setResults(null);
      return;
    }
    const handle = setTimeout(() => {
      api.search(query.trim()).then(setResults).catch((e) => setError(String(e)));
    }, 200);
    return () => clearTimeout(handle);
  }, [query, folder]);

  const crumbs = useMemo(() => {
    if (!folder) return [];
    const parts: { name: string; path: string }[] = [];
    let path = folder.path;
    while (path && path.length >= folder.root.length) {
      parts.unshift({ name: path === folder.root ? folder.root_label : path.split("/").pop() || path, path });
      if (path === folder.root) break;
      path = path.slice(0, path.lastIndexOf("/")) || "/";
    }
    return parts;
  }, [folder]);

  const eligible = useMemo(
    () => (report?.findings ?? []).filter((f) => f.eligible).sort((a, b) => b.bytes - a.bytes),
    [report],
  );
  const others = (report?.findings ?? []).filter((f) => !f.eligible && f.bytes > 0).sort((a, b) => b.bytes - a.bytes);
  const safeItems = eligible.filter((f) => f.risk === "safe");
  const freeable = report ? report.safe_bytes + report.review_bytes : 0;
  const shown = showAll ? eligible : eligible.slice(0, VISIBLE_FINDINGS);
  const internalActive = volumes.find((v) => v.mount_point === active)?.internal ?? active === "/";

  const confirmMove = () =>
    run(async () => {
      const items = pending ?? [];
      setPending(null);
      const result = await api.cleanupApply(items);
      if (result.activity_id) {
        setUndo({
          id: result.activity_id,
          text: `Moved ${bytes(result.moved_bytes)} to the Trash${result.skipped.length ? `; ${result.skipped.length} skipped` : ""}.`,
        });
      } else if (result.skipped.length) {
        setError(result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n"));
      }
      api.volumes().then(setVolumes).catch(() => {});
      await loadFindings();
    });

  const undoMove = () =>
    run(async () => {
      if (!undo) return;
      await api.cleanupRestore(undo.id);
      setUndo(null);
      await loadFindings();
    });

  const rows = results ?? folder?.rows ?? [];
  const largest = Math.max(1, ...rows.map((r) => r.bytes));
  const total = Math.max(1, rows.reduce((sum, r) => sum + r.bytes, 0));

  return (
    <div className="view storage">
      <div className="volumes">
        {volumes.map((v) => {
          const Icon = v.internal ? HardDrive : Usb;
          return (
            <button
              key={v.mount_point}
              className={`volume-card${v.mount_point === active ? " active" : ""}`}
              onClick={() => scanVolume(v.mount_point)}
              disabled={busy}
              title={v.mount_point}
            >
              <span className="volume-card-head">
                <Icon size={15} strokeWidth={1.75} />
                <span className="strong name">{v.name}</span>
              </span>
              <Bar fraction={1 - v.available_bytes / v.total_bytes} height={5} />
              <span className="muted small">
                {bytes(v.available_bytes)} free of {bytes(v.total_bytes)}
              </span>
            </button>
          );
        })}
      </div>

      {internalActive && (
        <section className="card-block">
          <div className="block-head">
            <span className="strong headline">
              <ShieldCheck size={15} strokeWidth={1.75} />
              {!report ? "Looking for what is safe to clear…" : freeable > 0 ? `You can free ${bytes(freeable)}` : "Nothing obvious to clear"}
            </span>
            {safeItems.length > 1 && (
              <Button size="sm" onClick={() => setPending(safeItems)} disabled={busy}>
                Clear all safe ({bytes(report?.safe_bytes ?? 0)})
              </Button>
            )}
          </div>
          {undo && (
            <div className="notice small">
              <span>{undo.text}</span>
              <Button size="sm" variant="ghost" onClick={undoMove} disabled={busy}>
                <Undo2 size={12} /> Undo
              </Button>
            </div>
          )}
          {shown.map((f) => (
            <div key={f.id} className="finding" title={f.path}>
              <div className="finding-main">
                <span className="name">
                  {f.name} <Badge tone={f.risk === "safe" ? "ok" : "warn"}>{f.risk === "safe" ? "Safe" : "Review"}</Badge>
                </span>
                <span className="muted small finding-reason">{f.reason}</span>
              </div>
              <span className="size strong">
                {f.partial ? "≥ " : ""}
                {bytes(f.bytes)}
              </span>
              <Button size="sm" onClick={() => setPending([f])} disabled={busy}>
                Move to Trash
              </Button>
            </div>
          ))}
          {eligible.length > VISIBLE_FINDINGS && (
            <button className="crumb more" onClick={() => setShowAll(!showAll)}>
              {showAll ? "Show fewer" : `Show all ${eligible.length}`}
            </button>
          )}
          {others.slice(0, 3).map((f) => (
            <div key={f.id} className="muted small other">
              <span className="name">
                {f.name}: {bytes(f.bytes)}. {f.reason}
              </span>
            </div>
          ))}
        </section>
      )}

      {internalActive && growth?.available && (growth.grown.length > 0 || growth.shrunk.length > 0) && (
        <div className="growth-line small">
          <span className="muted">
            Since last scan
            {growth.since ? ` (${new Date(growth.since * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })})` : ""}:
          </span>
          {[...growth.grown, ...growth.shrunk].slice(0, 4).map((c) => (
            <button key={c.path} className="chip" onClick={() => open(c.path)} disabled={busy} title={c.path}>
              {c.path.split("/").pop()}{" "}
              <span style={{ color: c.bytes > 0 ? "var(--warn)" : "var(--ok)" }}>{signedBytes(c.bytes)}</span>
            </button>
          ))}
        </div>
      )}

      <div className="toolbar">
        <input className="search" placeholder="Search files" value={query} onChange={(e) => setQuery(e.target.value)} />
        <Button size="sm" onClick={() => scanVolume(active)} disabled={busy}>
          <RefreshCw size={12} /> {busy ? "Scanning…" : "Rescan"}
        </Button>
      </div>

      {results ? (
        <div className="muted small">{results.length} matches</div>
      ) : (
        <div className="crumbs">
          {crumbs.map((c, i) => (
            <span key={c.path}>
              {i > 0 && <ChevronRight size={11} className="muted crumb-sep" />}
              <button className="crumb" onClick={() => open(c.path)} disabled={busy}>
                {c.name}
              </button>
            </span>
          ))}
        </div>
      )}

      {folder?.needs_access && !results && (
        <div className="notice small">
          <span>Some folders couldn't be read. Grant Full Disk Access to include them.</span>
          <Button size="sm" onClick={() => api.openFullDiskAccess()}>
            Open Full Disk Access
          </Button>
        </div>
      )}
      {folder && !folder.needs_access && folder.limited && !results && (
        <div className="muted small">This is a very large folder, so sizes may be a little low.</div>
      )}
      {error && <div className="error" style={{ whiteSpace: "pre-wrap" }}>{error}</div>}
      {!folder && busy && <div className="muted">Scanning…</div>}
      {folder && rows.length === 0 && !busy && (
        <EmptyState icon={<FolderIcon size={22} />} title={results ? "No matches" : "This folder is empty"} />
      )}

      {rows.length > 0 && (
        <div className="explorer">
          <div className="list">
            {rows.map((r) => {
              const kind = KINDS[kindOf(r.path, r.is_dir)];
              const pct = (r.bytes / total) * 100;
              return (
                <div
                  key={r.path}
                  className={`row folder-row${r.is_dir && !results ? " clickable" : ""}`}
                  onClick={() => (r.is_dir && !results ? open(r.path) : undefined)}
                  onDoubleClick={() => api.reveal(r.path)}
                  title={`${r.path}\n${kind.label}`}
                >
                  <span className="name">
                    <i className="kind-dot" style={{ background: kind.color }} />
                    {r.is_dir ? <FolderIcon size={12} className="muted" /> : <File size={12} className="muted" />}{" "}
                    {results ? shortPath(r.path) : r.name}
                  </span>
                  <Bar fraction={r.bytes / largest} color={kind.color} />
                  <span className="pct muted small">{pct >= 1 ? `${Math.round(pct)}%` : "<1%"}</span>
                  <span className="size">{bytes(r.bytes)}</span>
                </div>
              );
            })}
          </div>
          {!results && <Treemap rows={rows} open={open} />}
        </div>
      )}

      {pending && (
        <ConfirmDialog
          title={pending.length === 1 ? `Move ${pending[0].name} to the Trash?` : `Move ${pending.length} items to the Trash?`}
          description={`${bytes(pending.reduce((s, f) => s + f.bytes, 0))} will move to the Trash. Nothing is deleted: you can put it back, or empty the Trash yourself.`}
          confirmLabel="Move to Trash"
          onConfirm={confirmMove}
          onCancel={() => setPending(null)}
        />
      )}
    </div>
  );
}

const W = 300;
const H = 240;

/** The current folder as one level of tiles, coloured by kind. Click a folder tile to open it. */
function Treemap({ rows, open }: { rows: Row[]; open: (path: string) => void }) {
  const items = rows.filter((r) => r.bytes > 0).slice(0, 40);
  if (items.length === 0) return null;
  const tiles = squarify(items.map((r) => r.bytes), W, H);
  return (
    <svg className="treemap" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Folder contents by size">
      {items.map((r, i) => {
        const t = tiles[i];
        const kind = KINDS[kindOf(r.path, r.is_dir)];
        const chars = Math.floor((t.w - 8) / 5.6);
        return (
          <g key={r.path} onClick={() => r.is_dir && open(r.path)} style={{ cursor: r.is_dir ? "pointer" : "default" }}>
            <title>{`${r.name}: ${bytes(r.bytes)}`}</title>
            <rect x={t.x + 0.5} y={t.y + 0.5} width={Math.max(t.w - 1, 0)} height={Math.max(t.h - 1, 0)} rx={2} fill={kind.color} opacity={0.88} />
            {t.w > 46 && t.h > 16 && chars > 3 && (
              <text x={t.x + 4} y={t.y + 12} fontSize={9.5} fill="#fff" style={{ pointerEvents: "none" }}>
                {r.name.length > chars ? r.name.slice(0, chars - 1) + "…" : r.name}
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}

export function Bar({ fraction, height = 4, color }: { fraction: number; height?: number; color?: string }) {
  const f = Math.min(Math.max(fraction, 0), 1);
  return (
    <div className="bar" style={{ height }}>
      <i style={{ width: `${f * 100}%`, background: color ?? tone(f) }} />
    </div>
  );
}
