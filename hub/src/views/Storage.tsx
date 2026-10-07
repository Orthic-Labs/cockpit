import { useEffect, useMemo, useState } from "react";
import { Badge, Button, ConfirmDialog, EmptyState } from "@rightkit/app-shell/react";
import { Activity, ChevronDown, ChevronRight, File, Folder as FolderIcon, HardDrive, Info, RefreshCw, ShieldCheck, Terminal, Undo2, Usb } from "lucide-react";
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
const VISIBLE_GROUPS = 6;
const VISIBLE_NOTES = 3;

interface Group {
  key: string;
  label: string;
  items: CleanupFinding[];
  bytes: number;
  partial: boolean;
  risk: "safe" | "review";
  reason: string;
  discovered: boolean;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

function groupFindings(list: CleanupFinding[]): Group[] {
  const map = new Map<string, Group>();
  for (const f of list) {
    let g = map.get(f.rule_id);
    if (!g) {
      const label = f.rule_name || f.name;
      g = { key: f.rule_id, label, items: [], bytes: 0, partial: false, risk: "safe", reason: f.reason, discovered: f.name.startsWith(`${label} · `) };
      map.set(f.rule_id, g);
    }
    g.items.push(f);
    g.bytes += f.bytes;
    g.partial ||= f.partial;
    if (f.risk !== "safe") g.risk = "review";
  }
  return [...map.values()].sort((a, b) => b.bytes - a.bytes);
}

const groupCount = (g: Group) =>
  g.items.length === 1 ? "" : g.discovered ? plural(g.items.length, "project") : plural(g.items.length, "item");

function noteIcon(f: CleanupFinding) {
  const text = `${f.action} ${f.reason}`.toLowerCase();
  if (text.includes("xcrun") || text.includes("terminal") || text.includes("command")) return Terminal;
  if (text.includes("in use") || text.includes("running")) return Activity;
  return Info;
}

export function Storage() {
  const [volumes, setVolumes] = useState<Volume[]>([]);
  const [active, setActive] = useState<string>("/");
  const [folder, setFolder] = useState<Folder | null>(null);
  const [results, setResults] = useState<Row[] | null>(null);
  const [query, setQuery] = useState("");
  const [growth, setGrowth] = useState<Growth | null>(null);
  const [report, setReport] = useState<CleanupReport | null>(null);
  const [showAll, setShowAll] = useState(false);
  const [showNotes, setShowNotes] = useState(false);
  const [openGroups, setOpenGroups] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState<{ label: string; items: CleanupFinding[] } | null>(null);
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
  const groups = useMemo(() => groupFindings(eligible), [eligible]);
  const shown = showAll ? groups : groups.slice(0, VISIBLE_GROUPS);
  const notes = showNotes ? others : others.slice(0, VISIBLE_NOTES);
  const toggleGroup = (key: string) =>
    setOpenGroups((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  const internalActive = volumes.find((v) => v.mount_point === active)?.internal ?? active === "/";

  const confirmMove = () =>
    run(async () => {
      const items = pending?.items ?? [];
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

  const installers = volumes.filter((v) => v.disk_image);
  const [ejecting, setEjecting] = useState<string | null>(null);
  const [ejectError, setEjectError] = useState<string | null>(null);
  const eject = async (v: Volume) => {
    setEjecting(v.mount_point);
    setEjectError(null);
    try {
      await api.eject(v.mount_point);
    } catch (e) {
      setEjectError(`${v.name}: ${String(e)}`);
    }
    setEjecting(null);
    api.volumes().then(setVolumes).catch(() => {});
  };

  const rows = results ?? folder?.rows ?? [];
  const largest = Math.max(1, ...rows.map((r) => r.bytes));
  const total = Math.max(1, rows.reduce((sum, r) => sum + r.bytes, 0));

  return (
    <div className="view storage">
      <div className="volumes">
        {volumes.filter((v) => !v.disk_image).map((v) => {
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

      {installers.length > 0 && (
        <div className="installers muted small">
          <span>Mounted installers</span>
          {installers.map((v) => (
            <span className="installer-chip" key={v.mount_point} title={v.mount_point}>
              <span className="name">{v.name}</span>
              <span>{bytes(v.total_bytes)}</span>
              <Button size="sm" variant="ghost" onClick={() => eject(v)} disabled={ejecting === v.mount_point}>
                Eject
              </Button>
            </span>
          ))}
          {ejectError && <span className="error">{ejectError}</span>}
        </div>
      )}

      {internalActive && (
        <section className="card-block">
          <div className="block-head">
            <span className="strong headline">
              <ShieldCheck size={15} strokeWidth={1.75} />
              {!report ? "Looking for what is safe to clear…" : freeable > 0 ? `You can free ${bytes(freeable)}` : "Nothing obvious to clear"}
            </span>
            {safeItems.length > 1 && (
              <Button size="sm" onClick={() => setPending({ label: "Safe items", items: safeItems })} disabled={busy}>
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
          <div className="findings-scroll">
            {shown.map((g) => {
              const expanded = openGroups.has(g.key);
              const count = groupCount(g);
              const single = g.items.length === 1;
              return (
                <div key={g.key} className="finding-group">
                  <div className="finding" title={single ? g.items[0].path : undefined}>
                    <button
                      className="chev"
                      onClick={() => toggleGroup(g.key)}
                      disabled={single}
                      aria-label={expanded ? "Hide items" : "Show items"}
                      aria-expanded={expanded}
                    >
                      {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                    </button>
                    <div className="finding-main">
                      <span className="name">
                        {g.label}
                        {count && <span className="muted"> · {count}</span>}{" "}
                        <Badge tone={g.risk === "safe" ? "ok" : "warn"}>{g.risk === "safe" ? "Safe" : "Review"}</Badge>
                      </span>
                      <span className="muted small finding-reason">{g.reason}</span>
                    </div>
                    <span className="size strong">
                      {g.partial ? "≥ " : ""}
                      {bytes(g.bytes)}
                    </span>
                    <Button size="sm" onClick={() => setPending({ label: g.label, items: g.items })} disabled={busy}>
                      Move to Trash
                    </Button>
                  </div>
                  {expanded &&
                    g.items.map((f) => (
                      <div key={f.id} className="finding sub" title={f.path}>
                        <span />
                        <span className="name small">{f.name}</span>
                        <span className="size small">
                          {f.partial ? "≥ " : ""}
                          {bytes(f.bytes)}
                        </span>
                        <Button size="sm" variant="ghost" onClick={() => setPending({ label: f.name, items: [f] })} disabled={busy}>
                          Move to Trash
                        </Button>
                      </div>
                    ))}
                </div>
              );
            })}
            {groups.length > VISIBLE_GROUPS && (
              <button className="crumb more" onClick={() => setShowAll(!showAll)}>
                {showAll ? "Show fewer" : `Show ${groups.length - VISIBLE_GROUPS} more`}
              </button>
            )}
            {others.length > 0 && (
              <div className="notes">
                <div className="notes-head muted small">Notes</div>
                {notes.map((f) => {
                  const Icon = noteIcon(f);
                  return (
                    <div key={f.id} className="note muted small" title={f.path}>
                      <Icon size={12} strokeWidth={1.75} />
                      <span className="note-text">
                        {f.rule_name || f.name} · {f.reason}
                      </span>
                      <span className="note-size">{bytes(f.bytes)}</span>
                    </div>
                  );
                })}
                {others.length > VISIBLE_NOTES && (
                  <button className="crumb more" onClick={() => setShowNotes(!showNotes)}>
                    {showNotes ? "Show fewer notes" : `Show ${others.length - VISIBLE_NOTES} more notes`}
                  </button>
                )}
              </div>
            )}
          </div>
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
          title={pending.items.length === 1 ? `Move ${pending.items[0].name} to the Trash?` : `Move ${pending.items.length} items to the Trash?`}
          description={`${pending.items.length > 1 ? `${pending.label}: ` : ""}${plural(pending.items.length, "item")}, ${bytes(pending.items.reduce((s, f) => s + f.bytes, 0))} will move to the Trash. Nothing is deleted: you can put it back, or empty the Trash yourself.`}
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
