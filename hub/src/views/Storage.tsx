import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent } from "react";
import { Badge, Button, ConfirmDialog, EmptyState, SegmentedControl, useContextMenu } from "@rightkit/app-shell/react";
import { Activity, ChevronDown, ChevronRight, Copy, File, Folder as FolderIcon, FolderInput, FolderOpen, HardDrive, Info, MoreHorizontal, RefreshCw, ShieldCheck, Terminal, Trash2, Undo2, Usb } from "lucide-react";
import {
  api,
  ago,
  bytes,
  isStale,
  isWindows,
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

// Windows has the Recycle Bin and File Explorer; the Mac has the Trash and Finder.
const TRASH = isWindows ? "Recycle Bin" : "Trash";
const FILE_MANAGER = isWindows ? "File Explorer" : "Finder";
import { ChromeSnapshotsLine } from "./ChromeSnapshots";
import { DriveHealthLine, DriveHealthPanel } from "./DriveHealth";
import { Duplicates } from "./Duplicates";
import "./health.css";
import "./storage.css";

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
const VISIBLE_GROUPS = 6;
const VISIBLE_NOTES = 3;

type Pane = "findings" | "folders" | "changes" | "duplicates";

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

/** Without a live refresh, a saved scan younger than this is shown as it is on open. */
const SNAPSHOT_SECS = 6 * 60 * 60;
const isOld = (secs: number) => Date.now() / 1000 - secs > SNAPSHOT_SECS;

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
  // One workspace at a time; Duplicates is the exact-content copy finder.
  const [pane, setPane] = useState<Pane>("findings");
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
    const command = isWindows ? e.ctrlKey && !e.altKey && !e.metaKey : e.metaKey && !e.altKey && !e.ctrlKey;
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
      { id: "finder", label: `${row.is_dir ? "Open" : "Reveal"} in ${FILE_MANAGER}`, icon: <FolderOpen size={13} />, run: () => showInFinder(row.path) },
      { id: "copy-path", label: "Copy path", icon: <Copy size={13} />, shortcut: "mod+c", separatorBefore: true, run: () => copyText(row.path) },
      { id: "copy-name", label: "Copy name", icon: <Copy size={13} />, run: () => copyText(row.name) },
      { id: "move", label: "Move to…", icon: <FolderInput size={13} />, separatorBefore: true, run: () => moveItem(row) },
      { id: "trash", label: `Move to ${TRASH}`, icon: <Trash2 size={13} />, shortcut: "mod+Backspace", danger: true, separatorBefore: true, run: () => askTrash(row) },
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
      // Without a live refresh a walk of the whole folder is long and nothing
      // keeps the view current, so a recent saved scan waits for Rescan.
      const saved = await api.lastScan().catch(() => null);
      if (!alive.current) return;
      setOpening(false);
      if (saved) {
        show(saved, list);
        const old = status?.live_refresh === false ? isOld(saved.scanned_at) : isStale(saved.scanned_at);
        if (old) scanVolume(mountFor(saved.root, list), list);
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
  // With nothing to clear, open on the folders instead of an empty list (once,
  // so a pane the user picked is never taken away).
  const pickedPane = useRef(false);
  useEffect(() => {
    if (!pickedPane.current && report && eligible.length === 0) setPane("folders");
  }, [report, eligible.length]);
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
          text: `Moved ${bytes(result.moved_bytes)} to the ${TRASH}${result.skipped.length ? `; ${result.skipped.length} skipped` : ""}.`,
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

  const segments = (
    <SegmentedControl
      label="Storage view"
      value={pane}
      options={[
        { value: "findings", label: "Findings" },
        { value: "folders", label: "Folders" },
        { value: "changes", label: "Changes" },
        { value: "duplicates", label: "Duplicates" },
      ]}
      onChange={(value) => {
        pickedPane.current = true;
        setPane(value as Pane);
      }}
    />
  );

  if (pane === "duplicates") {
    return (
      <div className="view st">
        <div className="st-bar">{segments}</div>
        <div className="st-scroll" tabIndex={0} aria-label="Duplicates">
          <Duplicates />
        </div>
      </div>
    );
  }

  const rows = results ?? folder?.rows ?? [];
  const largest = Math.max(1, ...rows.map((r) => r.bytes));
  const total = Math.max(1, rows.reduce((sum, r) => sum + r.bytes, 0));

  const volumeList = volumes.filter((v) => !v.disk_image);
  const activeVolume = volumeList.find((v) => v.mount_point === active);
  const ActiveIcon = activeVolume && !activeVolume.internal ? Usb : HardDrive;
  const card = health?.drives.find((d) => d.mount === active);
  const healthShown = healthOpen === active;
  const toolAvailable = health?.tool_available ?? true;
  const scanText =
    folder && scanning
      ? `Updated ${ago(folder.scanned_at)} · Refreshing…`
      : scanning
        ? "Scanning…"
        : folder
          ? `Updated ${ago(folder.scanned_at)}${folder.from_snapshot ? " (saved)" : ""}`
          : "";
  const scopeText = internalActive ? "Home folder" : "Whole drive";
  const changes = growth?.available ? [...growth.grown, ...growth.shrunk] : [];
  const openFromChange = (path: string) => {
    setPane("folders");
    open(path);
  };
  const headline = !report ? "Looking for what is safe to clear…" : freeable > 0 ? `${bytes(freeable)} eligible to move` : "Nothing obvious to clear";

  return (
    <div className="view st">
      <section className="st-volume" aria-label="Volume">
        <div className="st-volume-line">
          <ActiveIcon size={15} strokeWidth={1.75} className="st-volume-icon" aria-hidden="true" />
          {volumeList.length > 0 ? (
            <select
              className="select st-select"
              aria-label="Volume"
              title={activeVolume?.mount_point}
              value={active}
              disabled={busy}
              onChange={(e) => scanVolume(e.target.value)}
            >
              {volumeList.map((v) => (
                <option key={v.mount_point} value={v.mount_point}>
                  {v.internal ? `${v.name} · Startup` : v.name}
                </option>
              ))}
            </select>
          ) : (
            <span className="strong">No volumes</span>
          )}
          {activeVolume && (
            <span className="st-free">
              <span className="strong">{bytes(activeVolume.available_bytes)} free</span>
              <span className="muted">of {bytes(activeVolume.total_bytes)}</span>
            </span>
          )}
          <Button size="sm" onClick={() => scanVolume(active)} disabled={busy || scanning}>
            <RefreshCw size={12} /> Rescan
          </Button>
        </div>
        {activeVolume && activeVolume.total_bytes > 0 && <Bar fraction={1 - activeVolume.available_bytes / activeVolume.total_bytes} height={6} />}
        {(card || health) && (
          <div className="st-volume-foot">
            <DriveHealthLine card={card} toolAvailable={toolAvailable} />
            {card && (
              <button className="st-link" onClick={() => setHealthOpen(healthShown ? null : active)} aria-expanded={healthShown}>
                {healthShown ? "Hide drive health" : "Drive health"}
              </button>
            )}
          </div>
        )}
      </section>

      <div className="st-meta">
        <span className="st-meta-text">{[scopeText, scanText].filter(Boolean).join(" · ")}</span>
        {installers.length > 0 && <span className="st-meta-text">Mounted installers</span>}
        {installers.map((v) => (
          <span className="st-chip" key={v.mount_point} title={v.mount_point}>
            <span className="st-chip-name">{v.name}</span>
            <span>{bytes(v.total_bytes)}</span>
            <Button size="sm" variant="ghost" onClick={() => eject(v)} disabled={ejecting === v.mount_point}>
              Eject
            </Button>
          </span>
        ))}
        {ejectError && <span className="error">{ejectError}</span>}
      </div>

      <div className="st-bar">{segments}</div>

      {error && (
        <div className="st-notice st-notice--bad" role="alert">
          {error}
        </div>
      )}
      {undo && (
        <div className="st-notice" role="status">
          <span>{undo.text}</span>
          <Button size="sm" variant="ghost" onClick={undoMove} disabled={busy}>
            <Undo2 size={12} /> Undo
          </Button>
        </div>
      )}
      {folder?.needs_access && !results && (
        <div className="st-notice st-notice--warn" role="status">
          <span>{isWindows ? "Some folders couldn't be read. Allow file system access in Windows Settings to include them." : "Some folders couldn't be read. Grant Full Disk Access to include them."}</span>
          <Button size="sm" onClick={() => api.openFullDiskAccess()}>
            {isWindows ? "Open File system settings" : "Open Full Disk Access"}
          </Button>
        </div>
      )}
      {folder && !folder.needs_access && folder.limited && !results && (
        <div className="st-sub">This is a very large folder, so sizes may be a little low.</div>
      )}

      <div className="st-scroll" tabIndex={0} aria-label="Storage content">
        {healthShown && (
          <DriveHealthPanel card={card} alerts={health?.alerts ?? []} toolAvailable={toolAvailable} />
        )}

        {pane === "findings" &&
          (internalActive ? (
            <section className="st-card" aria-label="Findings">
              <div className="st-card-head">
                <div>
                  <h2 className="st-h">
                    <ShieldCheck size={15} strokeWidth={1.75} aria-hidden="true" />
                    {headline}
                  </h2>
                  <p className="st-sub">
                    Home{report ? ` · scanned ${ago(report.scanned_at)}` : ""}
                    {report && findingsBusy ? " · Refreshing…" : ""}
                  </p>
                </div>
                {safeItems.length > 1 && (
                  <Button size="sm" variant="primary" onClick={() => setPending({ label: "Safe items", items: safeItems })} disabled={busy}>
                    Clear all safe ({bytes(report?.safe_bytes ?? 0)})
                  </Button>
                )}
              </div>
              <ChromeSnapshotsLine info={report?.chrome_snapshots} />
              {shown.map((g) => {
                const expanded = openGroups.has(g.key);
                const count = groupCount(g);
                return (
                  <div key={g.key} className="st-finding">
                    <button className="st-disclose" onClick={() => toggleGroup(g.key)} aria-expanded={expanded}>
                      {expanded ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
                      <span className="st-finding-text">
                        <span className="st-finding-title">
                          <span className="strong">
                            {g.label}
                            {count && <span className="muted"> · {count}</span>}
                          </span>
                          <Badge tone={g.risk === "safe" ? "ok" : "warn"}>{g.risk === "safe" ? "Safe" : "Review"}</Badge>
                        </span>
                        <span className="st-reason">{g.reason}</span>
                      </span>
                    </button>
                    <span className="st-measure">
                      <span className="st-size">
                        {g.partial ? "≥ " : ""}
                        {bytes(g.bytes)}
                      </span>
                      <Button size="sm" onClick={() => setPending({ label: g.label, items: g.items })} disabled={busy}>
                        Move to {TRASH}
                      </Button>
                    </span>
                    {expanded && (
                      <div className="st-sub-items">
                        {g.items.map((f) => (
                          <div key={f.id} className="st-sub-item">
                            <span>
                              {g.items.length > 1 && <span>{f.name}</span>}
                              <span className="st-path" style={{ display: "block" }}>
                                {shortPath(f.path)}
                              </span>
                            </span>
                            <span className="st-size">
                              {f.partial ? "≥ " : ""}
                              {bytes(f.bytes)}
                            </span>
                            <Button size="sm" variant="ghost" onClick={() => setPending({ label: f.name, items: [f] })} disabled={busy}>
                              Move to {TRASH}
                            </Button>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}
              {groups.length > VISIBLE_GROUPS && (
                <button className="st-more-btn" onClick={() => setShowAll(!showAll)}>
                  {showAll ? "Show fewer" : `Show ${groups.length - VISIBLE_GROUPS} more`}
                </button>
              )}
              {others.length > 0 && (
                <div className="st-notes">
                  <div className="st-notes-head">Notes</div>
                  {notes.map((f) => {
                    const Icon = noteIcon(f);
                    return (
                      <div key={f.id} className="st-note" title={f.path}>
                        <Icon size={12} strokeWidth={1.75} aria-hidden="true" />
                        <span className="st-note-text">
                          {f.rule_name || f.name} · {f.reason}
                        </span>
                        <span className="st-note-size">{bytes(f.bytes)}</span>
                      </div>
                    );
                  })}
                  {others.length > VISIBLE_NOTES && (
                    <button className="st-more-btn" onClick={() => setShowNotes(!showNotes)}>
                      {showNotes ? "Show fewer notes" : `Show ${others.length - VISIBLE_NOTES} more notes`}
                    </button>
                  )}
                </div>
              )}
            </section>
          ) : (
            <EmptyState
              icon={<ShieldCheck size={22} />}
              title="Findings cover your home folder"
              action={
                <Button size="sm" onClick={() => setPane("folders")}>
                  Browse this drive
                </Button>
              }
            />
          ))}

        {pane === "folders" && (
          <section className="st-card" aria-label="Folders">
            {results ? (
              <div className="st-sub">{plural(results.length, "match")}</div>
            ) : (
              <nav className="st-crumbs" aria-label="Folder path">
                {crumbs.map((c, i) => (
                  <span key={c.path}>
                    {i > 0 && <ChevronRight size={11} className="st-crumb-sep" aria-hidden="true" />}
                    <button className="st-crumb" onClick={() => open(c.path)} disabled={busy} aria-current={i === crumbs.length - 1 ? "page" : undefined}>
                      {c.name}
                    </button>
                  </span>
                ))}
              </nav>
            )}
            <input
              className="search st-search"
              type="search"
              placeholder="Search files"
              aria-label="Search files"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {!folder && (opening ? <div className="muted">Loading…</div> : scanning && <div className="muted">Scanning…</div>)}
            {folder && rows.length === 0 && !busy && !scanning && (
              <EmptyState icon={<FolderIcon size={22} />} title={results ? "No matches" : "This folder is empty"} />
            )}
            {rows.length > 0 && (
              <>
                {!results && <Treemap rows={rows} open={open} finder={showInFinder} menu={itemMenu.open} />}
                <div className="st-list">
                  {rows.map((r) => {
                    // A click with detail > 1 is the second click of a double-click: it must not drill in again.
                    const kind = KINDS[kindOf(r.path, r.is_dir)];
                    const pct = (r.bytes / total) * 100;
                    return (
                      <div
                        key={r.path}
                        className={`st-row${r.is_dir && !results ? " clickable" : ""}`}
                        tabIndex={actionable(r) ? 0 : undefined}
                        onClick={(e) => (r.is_dir && !results && e.detail < 2 ? open(r.path) : undefined)}
                        onDoubleClick={() => (actionable(r) ? showInFinder(r.path) : undefined)}
                        onKeyDown={(e) => onRowKey(e, r)}
                        onContextMenu={(e) => (actionable(r) ? itemMenu.open(e, r) : e.preventDefault())}
                        title={`${r.path}\n${kind.label}`}
                      >
                        <span className="st-name">
                          <i className="st-dot" style={{ background: kind.color }} />
                          {r.is_dir ? <FolderIcon size={13} aria-hidden="true" /> : <File size={13} aria-hidden="true" />}
                          <span>{results ? shortPath(r.path) : r.name}</span>
                        </span>
                        <Bar fraction={r.bytes / largest} color={kind.color} />
                        <span className="st-pct">{pct >= 1 ? `${Math.round(pct)}%` : "<1%"}</span>
                        <span className="st-size">{bytes(r.bytes)}</span>
                        {actionable(r) ? (
                          <button
                            className="st-row-more"
                            aria-label={`Actions for ${r.name}`}
                            aria-haspopup="menu"
                            onClick={(e) => {
                              e.stopPropagation();
                              itemMenu.open(e, r);
                            }}
                            onKeyDown={(e) => e.stopPropagation()}
                            onDoubleClick={(e) => e.stopPropagation()}
                          >
                            <MoreHorizontal size={15} aria-hidden="true" />
                          </button>
                        ) : (
                          <span />
                        )}
                      </div>
                    );
                  })}
                </div>
              </>
            )}
          </section>
        )}

        {pane === "changes" &&
          (!internalActive ? (
            <EmptyState icon={<Info size={22} />} title="Changes are tracked for your home folder" />
          ) : changes.length === 0 ? (
            <EmptyState icon={<Info size={22} />} title={growth?.available ? "No changes since the last scan" : "No comparison yet"}>
              {growth?.available ? null : (growth?.reason ?? "Changes appear after a second scan.")}
            </EmptyState>
          ) : (
            <section className="st-card" aria-label="Changes">
              <div className="st-card-head">
                <div>
                  <h2 className="st-h">Home changes</h2>
                  <p className="st-sub">
                    Since last scan
                    {growth?.since ? ` (${new Date(growth.since * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })})` : ""}
                  </p>
                </div>
              </div>
              <div className="st-changes">
                {changes.map((c) => (
                  <button key={c.path} className="st-change" onClick={() => openFromChange(c.path)} disabled={busy} title={c.path}>
                    <span>{shortPath(c.path)}</span>
                    <span className={`st-delta ${c.bytes > 0 ? "up" : "down"}`}>{signedBytes(c.bytes)}</span>
                  </button>
                ))}
              </div>
            </section>
          ))}
      </div>

      {itemMenu.element}

      {pending && (
        <ConfirmDialog
          title={pending.items.length === 1 ? `Move ${pending.items[0].name} to the ${TRASH}?` : `Move ${pending.items.length} items to the ${TRASH}?`}
          description={`${pending.items.length > 1 ? `${pending.label}: ` : ""}${plural(pending.items.length, "item")}, ${bytes(pending.items.reduce((s, f) => s + f.bytes, 0))} will move to the ${TRASH}. Nothing is deleted: you can put it back, or empty the ${TRASH} yourself.`}
          confirmLabel={`Move to ${TRASH}`}
          onConfirm={confirmMove}
          onCancel={() => setPending(null)}
        />
      )}

      {itemTrash && (
        <ConfirmDialog
          title={`Move ${itemTrash.row.name} to the ${TRASH}?`}
          description={`${shortPath(itemTrash.row.path)}, ${bytes(itemTrash.row.bytes)} will move to the ${TRASH}. Nothing is deleted: you can put it back, or empty the ${TRASH} yourself.`}
          confirmLabel={`Move to ${TRASH}`}
          onConfirm={confirmItemTrash}
          onCancel={() => setItemTrash(null)}
        />
      )}

      {itemMove && (
        <ConfirmDialog
          title={`Copy ${itemMove.row.name} to another drive?`}
          description={`${shortPath(itemMove.destination)} is on another drive, so ${itemMove.row.name} is copied to ${shortPath(itemMove.target)}, and the original moves to the ${TRASH}.`}
          confirmLabel={`Copy and move to ${TRASH}`}
          onConfirm={confirmItemMove}
          onCancel={() => setItemMove(null)}
        />
      )}
    </div>
  );
}

const W = 600;
const H = 96;

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
    <svg className="st-treemap" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Folder contents by size">
      {items.map((r, i) => {
        const t = tiles[i];
        const kind = KINDS[kindOf(r.path, r.is_dir)];
        const chars = Math.floor((t.w - 8) / 6.4);
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
              <text x={t.x + 4} y={t.y + 13} fontSize={11} fill="#fff" style={{ pointerEvents: "none" }}>
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
