import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent } from "react";
import { Badge, Button, ConfirmDialog, EmptyState, SegmentedControl, useContextMenu } from "@rightkit/app-shell/react";
import { Activity, ChevronDown, ChevronRight, Copy, File, Folder as FolderIcon, FolderInput, FolderOpen, HardDrive, Info, RefreshCw, ShieldCheck, Terminal, Trash2, Undo2, Usb } from "lucide-react";
import {
  api,
  ago,
  bytes,
  isStale,
  signedBytes,
  tone,
  type CleanupFinding,
  type CleanupReport,
  type FileIdentity,
  type Folder,
  type Growth,
  type HealthReport,
  type Row,
  type Volume,
} from "../api";
import { KINDS, kindOf, squarify } from "../chart";
import { ChromeSnapshotsLine } from "./ChromeSnapshots";
import { DriveHealthLine, DriveHealthPanel } from "./DriveHealth";
import { Duplicates } from "./Duplicates";
import "./health.css";

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

/** Findings survive leaving and returning to Storage; they are only rescanned on demand. */
let cachedReport: CleanupReport | null = null;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** Menu and keyboard actions apply to real items, never the "smaller files" total. */
const actionable = (r: Row) => !r.summary;

/** The volume a saved scan belongs to: its external drive, else the startup disk. */
const mountFor = (root: string, list: Volume[]) =>
  list.find((v) => !v.internal && v.mount_point === root)?.mount_point ?? "/";

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
  const [report, setReport] = useState<CleanupReport | null>(cachedReport);
  const [showAll, setShowAll] = useState(false);
  const [showNotes, setShowNotes] = useState(false);
  // Space is the scan view; Duplicates is the exact-content copy finder.
  const [tab, setTab] = useState<"space" | "duplicates">("space");
  const [health, setHealth] = useState<HealthReport | null>(null);
  // The volume whose drive-health panel is open, by mount point.
  const [healthOpen, setHealthOpen] = useState<string | null>(null);
  const [openGroups, setOpenGroups] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState<{ label: string; items: CleanupFinding[] } | null>(null);
  // One item waiting on a Trash confirmation, or on confirming a copy across drives.
  const [itemTrash, setItemTrash] = useState<{ row: Row; id: FileIdentity } | null>(null);
  const [itemMove, setItemMove] = useState<{ row: Row; id: FileIdentity; destination: string; target: string } | null>(null);
  const [undo, setUndo] = useState<{ id: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [scanning, setScanning] = useState(false);
  // True until the saved view has been asked for, so the page never shows an empty "Scanning…" first.
  const [opening, setOpening] = useState(true);
  const [findingsBusy, setFindingsBusy] = useState(false);
  // Latest scan request; an older one that comes back (cancelled) is ignored.
  const scanToken = useRef(0);
  const alive = useRef(true);
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

  const refreshFindings = () => {
    setFindingsBusy(true);
    api
      .cleanupScan()
      .then((r) => {
        cachedReport = r;
        setReport(r);
      })
      .catch(() => {})
      .finally(() => setFindingsBusy(false));
  };

  // Findings show at once from the saved copy. A background rescan follows when
  // there is none, when it is old, or when a move has just changed the list.
  const loadFindings = (force = false) => {
    if (cachedReport) {
      setReport(cachedReport);
      if (force || isStale(cachedReport.scanned_at)) refreshFindings();
      return;
    }
    api
      .cleanupCached()
      .then((saved) => {
        if (saved && !cachedReport) {
          cachedReport = saved;
          setReport(saved);
        }
        if (!saved || force || isStale(saved.scanned_at)) refreshFindings();
      })
      .catch(() => refreshFindings());
  };

  const isInternal = (mount: string, list: Volume[]) => {
    const volume = list.find((v) => v.mount_point === mount);
    return !volume || volume.internal;
  };

  // Show data that is already scanned: pick the matching drive card, then the
  // growth line (home only) and, once, the cleanup findings.
  const show = (f: Folder, list: Volume[]) => {
    const external = list.find((v) => !v.internal && v.mount_point === f.root);
    const mount = external ? external.mount_point : (list.find((v) => v.internal)?.mount_point ?? "/");
    setActive(mount);
    setFolder(f);
    if (external) {
      setGrowth(null);
    } else {
      api.growth().then(setGrowth).catch(() => setGrowth(null));
      loadFindings();
    }
  };

  // The startup disk is scanned from the home folder; other volumes from their root.
  // Starting a scan cancels the one running; there is never more than one.
  const scanVolume = async (mount: string, list: Volume[] = volumes) => {
    const token = ++scanToken.current;
    setActive(mount);
    setQuery("");
    setError(null);
    setScanning(true);
    try {
      const f = await api.scan(isInternal(mount, list) ? undefined : mount);
      if (token !== scanToken.current || !alive.current) return;
      show(f, list);
      setScanning(false);
    } catch (e) {
      if (token !== scanToken.current || !alive.current) return;
      setScanning(false);
      if (!String(e).includes("cancelled")) setError(String(e));
    }
  };

  // A scan started before this view was reopened keeps running; wait for it.
  const follow = async (list: Volume[]) => {
    const token = ++scanToken.current;
    setScanning(true);
    for (;;) {
      await sleep(1500);
      if (token !== scanToken.current || !alive.current) return;
      const status = await api.scanStatus().catch(() => null);
      if (status && !status.running) break;
    }
    const f = await api.lastScan().catch(() => null);
    if (token !== scanToken.current || !alive.current) return;
    if (f) show(f, list);
    setScanning(false);
  };

  const open = (path: string) => run(async () => setFolder(await api.children(path)));

  // Single-item actions. The identity is read when the person asks for an
  // action; the Rust side checks it again right before anything moves.
  const showInFinder = (path: string) => api.finderOpen(path).catch((e) => setError(String(e)));
  const copyText = (text: string) => navigator.clipboard.writeText(text).catch((e) => setError(`Could not copy: ${String(e)}`));
  // Drops a moved or trashed item from what is on screen. The saved scan index
  // is not rewritten here, so a rescan (or re-drilling) shows it until then.
  const dropRow = (path: string) => {
    if (results) setResults(results.filter((r) => r.path !== path));
    else setFolder((f) => (f ? { ...f, rows: f.rows.filter((r) => r.path !== path) } : f));
  };
  const askTrash = (row: Row) => run(async () => setItemTrash({ row, id: await api.fileIdentity(row.path) }));
  const confirmItemTrash = () => {
    const pending = itemTrash;
    if (!pending) return;
    setItemTrash(null);
    run(async () => {
      try {
        await api.fileTrash(pending.row.path, pending.id);
        dropRow(pending.row.path);
      } finally {
        api.volumes().then(setVolumes).catch(() => {});
      }
    });
  };
  const finishMove = async (row: Row, id: FileIdentity, destination: string, copyAcrossVolumes: boolean) => {
    try {
      await api.fileMove(row.path, id, destination, copyAcrossVolumes);
      dropRow(row.path);
    } finally {
      api.volumes().then(setVolumes).catch(() => {});
    }
  };
  // Pick a destination, then move at once within one drive, or confirm a copy across drives first.
  const moveItem = (row: Row) =>
    run(async () => {
      const id = await api.fileIdentity(row.path);
      const destination = await api.fileChooseFolder();
      if (!destination) return;
      const plan = await api.fileMovePlan(row.path, id, destination);
      if (plan.same_volume) await finishMove(row, id, destination, false);
      else setItemMove({ row, id, destination, target: plan.target });
    });
  const confirmItemMove = () => {
    const pending = itemMove;
    if (!pending) return;
    setItemMove(null);
    run(() => finishMove(pending.row, pending.id, pending.destination, true));
  };
  // Double-click and Return open in Finder; ⌘C copies the path; ⌘⌫ asks to trash.
  const onRowKey = (e: ReactKeyboardEvent, row: Row) => {
    if (!actionable(row)) return;
    const command = e.metaKey && !e.altKey && !e.ctrlKey;
    if (command && e.key.toLowerCase() === "c") {
      e.preventDefault();
      copyText(row.path);
    } else if (e.key === "Enter") {
      e.preventDefault();
      showInFinder(row.path);
    } else if (command && e.key === "Backspace") {
      e.preventDefault();
      askTrash(row);
    }
  };
  // Declared after the handlers above: the builder runs during render.
  const itemMenu = useContextMenu<Row>(
    (row) => [
      { id: "finder", label: row.is_dir ? "Open in Finder" : "Reveal in Finder", icon: <FolderOpen size={13} />, run: () => showInFinder(row.path) },
      { id: "copy-path", label: "Copy path", icon: <Copy size={13} />, shortcut: "mod+c", separatorBefore: true, run: () => copyText(row.path) },
      { id: "copy-name", label: "Copy name", icon: <Copy size={13} />, run: () => copyText(row.name) },
      { id: "move", label: "Move to…", icon: <FolderInput size={13} />, separatorBefore: true, run: () => moveItem(row) },
      { id: "trash", label: "Move to Trash", icon: <Trash2 size={13} />, shortcut: "mod+Backspace", danger: true, separatorBefore: true, run: () => askTrash(row) },
    ],
    "Item actions",
  );

  useEffect(() => {
    alive.current = true;
    (async () => {
      const list = await api.volumes().catch(() => [] as Volume[]);
      if (!alive.current) return;
      setVolumes(list);
      const status = await api.scanStatus().catch(() => null);
      if (!alive.current) return;
      if (status?.running) {
        const root = status.running_root;
        const external = list.find((v) => !v.internal && v.mount_point === root);
        setActive(external ? external.mount_point : (list.find((v) => v.internal)?.mount_point ?? "/"));
        setOpening(false);
        follow(list);
        return;
      }
      // The saved view opens at once. A rescan runs behind it only when the
      // saved one is old; with no saved view at all, the first scan starts now.
      const saved = await api.lastScan().catch(() => null);
      if (!alive.current) return;
      setOpening(false);
      if (saved) {
        show(saved, list);
        if (isStale(saved.scanned_at)) scanVolume(mountFor(saved.root, list), list);
      } else {
        scanVolume("/", list);
      }
    })();
    return () => {
      alive.current = false;
    };
  }, []);

  useEffect(() => {
    if (!folder || query.trim().length < 2) {
      setResults(null);
      return;
    }
    const handle = setTimeout(() => {
      // TODO(search-limit): scanner::search takes only the query today. Once the
      // filename search lands as search(query, limit), pass a limit here and in api.ts.
      api.search(query.trim()).then(setResults).catch((e) => setError(String(e)));
    }, 200);
    return () => clearTimeout(handle);
  }, [query, folder]);

  // Drive health for every drive card. The hub samples smartctl itself at most
  // every ten minutes, so polling here only reads the saved view.
  useEffect(() => {
    const mounts = volumes.filter((v) => !v.disk_image).map((v) => v.mount_point);
    if (mounts.length === 0) return;
    const run = () => api.driveHealth(mounts).then(setHealth).catch(() => {});
    run();
    const handle = setInterval(run, 60_000);
    return () => clearInterval(handle);
  }, [volumes]);

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
      loadFindings(true);
    });

  const undoMove = () =>
    run(async () => {
      if (!undo) return;
      await api.cleanupRestore(undo.id);
      setUndo(null);
      loadFindings(true);
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

  const tabBar = (
    <SegmentedControl
      label="Storage view"
      value={tab}
      options={[
        { value: "space", label: "Space" },
        { value: "duplicates", label: "Duplicates" },
      ]}
      onChange={(value) => setTab(value as "space" | "duplicates")}
    />
  );

  if (tab === "duplicates") {
    return (
      <div className="view storage">
        {tabBar}
        <Duplicates />
      </div>
    );
  }

  const rows = results ?? folder?.rows ?? [];
  const largest = Math.max(1, ...rows.map((r) => r.bytes));
  const total = Math.max(1, rows.reduce((sum, r) => sum + r.bytes, 0));

  return (
    <div className="view storage">
      {tabBar}
      <div className="volumes">
        {volumes.filter((v) => !v.disk_image).map((v) => {
          const Icon = v.internal ? HardDrive : Usb;
          const card = health?.drives.find((d) => d.mount === v.mount_point);
          const open = healthOpen === v.mount_point;
          return (
            <div className="volume-cell" key={v.mount_point}>
              <button
                className={`volume-card${v.mount_point === active ? " active" : ""}`}
                onClick={() => (scanning && v.mount_point === active ? undefined : scanVolume(v.mount_point))}
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
                <DriveHealthLine card={card} toolAvailable={health?.tool_available ?? true} />
              </button>
              {card && (
                <button
                  className="crumb health-toggle small"
                  onClick={() => setHealthOpen(open ? null : v.mount_point)}
                  aria-expanded={open}
                >
                  {open ? "Hide drive health" : "Drive health"}
                </button>
              )}
            </div>
          );
        })}
      </div>
      {healthOpen && (
        <DriveHealthPanel
          card={health?.drives.find((d) => d.mount === healthOpen)}
          alerts={health?.alerts ?? []}
          toolAvailable={health?.tool_available ?? true}
        />
      )}

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
              {report && findingsBusy && <span className="muted small"> · Refreshing…</span>}
            </span>
            {safeItems.length > 1 && (
              <Button size="sm" onClick={() => setPending({ label: "Safe items", items: safeItems })} disabled={busy}>
                Clear all safe ({bytes(report?.safe_bytes ?? 0)})
              </Button>
            )}
          </div>
          <ChromeSnapshotsLine info={report?.chrome_snapshots} />
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
              <span style={{ color: c.bytes > 0 ? "var(--rk-warn)" : "var(--rk-ok)" }}>{signedBytes(c.bytes)}</span>
            </button>
          ))}
        </div>
      )}

      <div className="toolbar">
        <input className="search" placeholder="Search files" value={query} onChange={(e) => setQuery(e.target.value)} />
        <span className="muted small">
          {folder && scanning
            ? `Updated ${ago(folder.scanned_at)} · Refreshing…`
            : scanning
              ? "Scanning…"
              : folder
                ? `Updated ${ago(folder.scanned_at)}${folder.from_snapshot ? " (saved)" : ""} ·`
                : ""}
        </span>
        <Button size="sm" onClick={() => scanVolume(active)} disabled={busy || scanning}>
          <RefreshCw size={12} /> Rescan
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
      {!folder && (opening ? <div className="muted">Loading…</div> : scanning && <div className="muted">Scanning…</div>)}
      {folder && rows.length === 0 && !busy && !scanning && (
        <EmptyState icon={<FolderIcon size={22} />} title={results ? "No matches" : "This folder is empty"} />
      )}

      {rows.length > 0 && (
        <div className="explorer">
          <div className="list">
            {rows.map((r) => {
              // A click with detail > 1 is the second click of a double-click: it must not drill in again.
              const kind = KINDS[kindOf(r.path, r.is_dir)];
              const pct = (r.bytes / total) * 100;
              return (
                <div
                  key={r.path}
                  className={`row folder-row${r.is_dir && !results ? " clickable" : ""}`}
                  tabIndex={actionable(r) ? 0 : undefined}
                  onClick={(e) => (r.is_dir && !results && e.detail < 2 ? open(r.path) : undefined)}
                  onDoubleClick={() => (actionable(r) ? showInFinder(r.path) : undefined)}
                  onKeyDown={(e) => onRowKey(e, r)}
                  onContextMenu={(e) => (actionable(r) ? itemMenu.open(e, r) : e.preventDefault())}
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
          {!results && <Treemap rows={rows} open={open} finder={showInFinder} menu={itemMenu.open} />}
        </div>
      )}

      {itemMenu.element}

      {pending && (
        <ConfirmDialog
          title={pending.items.length === 1 ? `Move ${pending.items[0].name} to the Trash?` : `Move ${pending.items.length} items to the Trash?`}
          description={`${pending.items.length > 1 ? `${pending.label}: ` : ""}${plural(pending.items.length, "item")}, ${bytes(pending.items.reduce((s, f) => s + f.bytes, 0))} will move to the Trash. Nothing is deleted: you can put it back, or empty the Trash yourself.`}
          confirmLabel="Move to Trash"
          onConfirm={confirmMove}
          onCancel={() => setPending(null)}
        />
      )}

      {itemTrash && (
        <ConfirmDialog
          title={`Move ${itemTrash.row.name} to the Trash?`}
          description={`${shortPath(itemTrash.row.path)}, ${bytes(itemTrash.row.bytes)} will move to the Trash. Nothing is deleted: you can put it back, or empty the Trash yourself.`}
          confirmLabel="Move to Trash"
          onConfirm={confirmItemTrash}
          onCancel={() => setItemTrash(null)}
        />
      )}

      {itemMove && (
        <ConfirmDialog
          title={`Copy ${itemMove.row.name} to another drive?`}
          description={`${shortPath(itemMove.destination)} is on another drive, so ${itemMove.row.name} is copied to ${shortPath(itemMove.target)}, and the original moves to the Trash.`}
          confirmLabel="Copy and move to Trash"
          onConfirm={confirmItemMove}
          onCancel={() => setItemMove(null)}
        />
      )}
    </div>
  );
}

const W = 300;
const H = 240;

/**
 * The current folder as one level of tiles, coloured by kind. Click a folder
 * tile to open it; double-click opens any item in Finder; right-click for the item menu.
 */
function Treemap({
  rows,
  open,
  finder,
  menu,
}: {
  rows: Row[];
  open: (path: string) => void;
  finder: (path: string) => void;
  menu: (event: ReactMouseEvent | MouseEvent, row: Row) => void;
}) {
  const items = rows.filter((r) => r.bytes > 0).slice(0, 40);
  if (items.length === 0) return null;
  const tiles = squarify(items.map((r) => r.bytes), W, H);
  return (
    <svg className="treemap" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Folder contents by size">
      {items.map((r, i) => {
        const t = tiles[i];
        const kind = KINDS[kindOf(r.path, r.is_dir)];
        const chars = Math.floor((t.w - 8) / 5.6);
        // A click with detail > 1 is the second click of a double-click: it must not drill in again.
        return (
          <g
            key={r.path}
            onClick={(e) => {
              if (r.is_dir && e.detail < 2) open(r.path);
            }}
            onDoubleClick={() => {
              if (actionable(r)) finder(r.path);
            }}
            onContextMenu={(e) => (actionable(r) ? menu(e, r) : e.preventDefault())}
            style={{ cursor: r.is_dir ? "pointer" : "default" }}
          >
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
