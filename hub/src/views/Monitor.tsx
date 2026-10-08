import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Button, ConfirmDialog } from "@rightkit/app-shell/react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { api, bytes, type ProcessRow, type Status } from "../api";
import { Bar } from "./Storage";

type Level = "ok" | "warn" | "bad";
type SortKey = "memory" | "cpu" | "name";

const SORTS: { key: SortKey; label: string }[] = [
  { key: "memory", label: "Memory" },
  { key: "cpu", label: "CPU" },
  { key: "name", label: "Name" },
];

// Protected macOS processes. They never get Quit or Force Quit from here.
const PROTECTED_NAMES = new Set(["kernel_task", "windowserver", "launchd", "loginwindow"]);
// Pulse itself. Quitting it from its own window would close the hub.
const PULSE_NAMES = new Set(["pulse", "pulse-hub", "pulse hub"]);

const LEVEL_COLOR: Record<Level, string> = {
  ok: "var(--rk-ok)",
  warn: "var(--rk-warn)",
  bad: "var(--rk-bad)",
};

/** Usage thresholds: 0–70% ok, 70–90% warn, 90–100% bad. */
function levelFor(fraction: number): Level {
  if (fraction >= 0.9) return "bad";
  if (fraction >= 0.7) return "warn";
  return "ok";
}

function pressureLevel(pressure: string): Level {
  const s = pressure.toLowerCase();
  if (/critical|urgent/.test(s)) return "bad";
  if (/warn|elevated/.test(s)) return "warn";
  return "ok";
}

/** System and critical rows: refused by the backend, not quittable, or protected by name. */
function isSystem(p: ProcessRow): boolean {
  const n = p.name.toLowerCase();
  return !p.can_act || p.refusal != null || PROTECTED_NAMES.has(n) || PULSE_NAMES.has(n);
}

function matchesQuery(p: ProcessRow, q: string): boolean {
  if (p.name.toLowerCase().includes(q)) return true;
  if ((p.bundle_id ?? "").toLowerCase().includes(q)) return true;
  return p.members.some((m) => m.name.toLowerCase().includes(q));
}

function comparator(sort: SortKey): (a: ProcessRow, b: ProcessRow) => number {
  if (sort === "cpu") return (a, b) => b.cpu_usage_percent - a.cpu_usage_percent;
  if (sort === "name") return (a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  return (a, b) => b.memory_bytes - a.memory_bytes;
}

export function Monitor() {
  const [status, setStatus] = useState<Status | null>(null);
  const [procs, setProcs] = useState<ProcessRow[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [stuck, setStuck] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [forcing, setForcing] = useState<ProcessRow | null>(null);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<SortKey>("memory");

  const note = (key: string, text: string) => setNotes((n) => ({ ...n, [key]: text }));

  const quit = async (r: ProcessRow) => {
    setBusy(r.key);
    note(r.key, "Asking to quit…");
    try {
      const outcome = await api.quit(r);
      if (outcome === "quit") note(r.key, "Quit.");
      else {
        note(r.key, "Still running. Force Quit is available.");
        setStuck((s) => new Set(s).add(r.key));
      }
    } catch (e) {
      note(r.key, String(e));
    } finally {
      setBusy(null);
    }
  };

  const force = async (r: ProcessRow) => {
    setForcing(null);
    setBusy(r.key);
    try {
      note(r.key, (await api.forceQuit(r)) === "quit" ? "Force quit." : "Still running.");
    } catch (e) {
      note(r.key, String(e));
    } finally {
      setBusy(null);
    }
  };

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      const [s, p] = await Promise.all([api.status(), api.processRows()]).catch(() => [null, null]);
      if (!alive) return;
      if (s) setStatus(s as Status);
      if (p) setProcs(p as ProcessRow[]);
    };
    tick();
    const handle = setInterval(tick, 3000);
    return () => {
      alive = false;
      clearInterval(handle);
    };
  }, []);

  // Top 3 by memory are flagged regardless of the current sort.
  const topMemory = useMemo(
    () => new Set([...procs].sort((a, b) => b.memory_bytes - a.memory_bytes).slice(0, 3).map((p) => p.key)),
    [procs],
  );

  const trimmed = query.trim();
  const visible = useMemo(() => {
    const q = trimmed.toLowerCase();
    const rows = q ? procs.filter((p) => matchesQuery(p, q)) : [...procs];
    return rows.sort(comparator(sort));
  }, [procs, trimmed, sort]);

  if (!status) return <div className="view muted">Reading…</div>;

  const cpuRaw = status.cpu_usage_percent.value;
  const cpuFraction = (cpuRaw ?? 0) / 100;
  const memUsed = status.memory_used_bytes.value;
  const memTotal = status.memory_total_bytes.value;
  const memFraction = memUsed != null && memTotal ? memUsed / memTotal : 0;
  const swapUsed = status.swap_used_bytes.value;
  const swapTotal = status.swap_total_bytes.value;
  const swapFraction = swapUsed != null && swapTotal ? swapUsed / swapTotal : 0;
  const pressure = status.memory_pressure.value;
  const sortLabel = SORTS.find((s) => s.key === sort)?.label ?? "Memory";

  return (
    <div className="view">
      <div className="mon-summary">
        <SummaryCard
          label="CPU"
          value={cpuRaw == null ? "—" : `${Math.round(cpuRaw)}%`}
          sub="Across all cores"
          fraction={cpuFraction}
        />
        <SummaryCard
          label="Memory"
          aside={pressure ? <Pill level={pressureLevel(pressure)}>{pressure}</Pill> : null}
          value={memUsed == null ? "—" : bytes(memUsed)}
          sub={memTotal == null ? "Total unknown" : `of ${bytes(memTotal)} used`}
          fraction={memFraction}
        />
        <SummaryCard
          label="Swap"
          value={swapTotal == null ? "—" : swapTotal === 0 ? "None" : bytes(swapUsed)}
          sub={swapTotal == null ? "Unknown" : swapTotal === 0 ? "No swap space" : `of ${bytes(swapTotal)} used`}
          fraction={swapFraction}
        />
      </div>

      <div className="mon-toolbar">
        <input
          type="search"
          className="mon-search"
          value={query}
          placeholder="Search apps and processes"
          aria-label="Search apps and processes"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.preventDefault();
              setQuery("");
            }
          }}
        />
        <div className="mon-seg" role="group" aria-label="Sort by">
          {SORTS.map((s) => (
            <button key={s.key} type="button" aria-pressed={sort === s.key} onClick={() => setSort(s.key)}>
              {s.label}
            </button>
          ))}
        </div>
      </div>

      <div className="mon-section">
        <span>
          {trimmed
            ? `${visible.length} matching “${trimmed}”`
            : `Apps and processes, sorted by ${sortLabel.toLowerCase()}${sort === "name" ? " A to Z" : ", highest first"}`}
        </span>
        <span className="mon-legend">
          <span className="mon-dot" aria-hidden="true" />
          Above 90% CPU, or top 3 by memory
        </span>
      </div>

      <div className="mon-list">
        <div className="mon-cols mon-head">
          <span>Name</span>
          <span className="mon-num">CPU %</span>
          <span className="mon-num">Memory</span>
          <span className="mon-num">Actions</span>
        </div>

        {visible.length === 0 && (
          <div className="mon-empty muted">
            {trimmed ? `No apps or processes match “${trimmed}”.` : "No apps or processes to show yet."}
          </div>
        )}

        {visible.map((p) => {
          const system = isSystem(p);
          const expanded = open === p.key;
          const hasMembers = p.members.length > 1;
          const reasons: string[] = [];
          if (p.cpu_usage_percent > 90) reasons.push("Above 90% CPU");
          if (topMemory.has(p.key)) reasons.push("Top 3 by memory");
          const reasonText = reasons.join(", ");
          const isStuck = stuck.has(p.key);
          const title = p.refusal ?? p.app_path ?? p.name;

          return (
            <div key={p.key} className="mon-group">
              <div className="mon-cols mon-row">
                <div className="mon-name">
                  {reasons.length > 0 && (
                    <span className="mon-dot" role="img" aria-label={reasonText} title={reasonText} />
                  )}
                  {hasMembers ? (
                    <button
                      type="button"
                      className="mon-disclosure"
                      aria-expanded={expanded}
                      aria-controls={`mon-members-${p.key}`}
                      title={title}
                      onClick={() => setOpen(expanded ? null : p.key)}
                    >
                      {expanded ? <ChevronDown size={12} aria-hidden="true" /> : <ChevronRight size={12} aria-hidden="true" />}
                      <span className="mon-label">{p.name}</span>
                      <span className="muted small">{p.members.length} processes</span>
                    </button>
                  ) : (
                    <span className="mon-label" title={title}>
                      {p.name}
                    </span>
                  )}
                  {system && <span className="mon-pill">System</span>}
                </div>
                <span className="mon-num">{Math.round(p.cpu_usage_percent)}%</span>
                <span className="mon-num">{bytes(p.memory_bytes)}</span>
                <span className="mon-actions">
                  {!system && p.can_act && (
                    <>
                      <Button size="sm" variant="secondary" disabled={busy === p.key} onClick={() => quit(p)}>
                        Quit
                      </Button>
                      <span className={isStuck ? undefined : "mon-danger"}>
                        <Button
                          size="sm"
                          variant={isStuck ? "danger" : "ghost"}
                          disabled={busy === p.key}
                          onClick={() => setForcing(p)}
                        >
                          Force Quit
                        </Button>
                      </span>
                    </>
                  )}
                </span>
              </div>
              {notes[p.key] && <div className="mon-note muted small">{notes[p.key]}</div>}
              {expanded && hasMembers && (
                <div id={`mon-members-${p.key}`}>
                  {p.members.map((m) => (
                    <div key={`${m.identity.pid}-${m.identity.start_time}`} className="mon-cols mon-member">
                      <span className="mon-label muted">{m.name}</span>
                      <span className="mon-num muted small">{Math.round(m.cpu_usage_percent)}%</span>
                      <span className="mon-num">{bytes(m.memory_bytes)}</span>
                      <span />
                    </div>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {forcing && (
        <ConfirmDialog
          danger
          title={`Force Quit ${forcing.name}?`}
          description="This ends it immediately without letting it save. Unsaved work in it will be lost."
          confirmLabel="Force Quit"
          onConfirm={() => force(forcing)}
          onCancel={() => setForcing(null)}
        />
      )}
    </div>
  );
}

function SummaryCard({
  label,
  aside,
  value,
  sub,
  fraction,
}: {
  label: string;
  aside?: ReactNode;
  value: string;
  sub: string;
  fraction: number;
}) {
  return (
    <section className="mon-card" aria-label={label}>
      <div className="mon-card-head">
        <span>{label}</span>
        {aside}
      </div>
      <div className="mon-card-value">{value}</div>
      <Bar fraction={fraction} color={LEVEL_COLOR[levelFor(fraction)]} />
      <div className="mon-card-sub muted small">{sub}</div>
    </section>
  );
}

function Pill({ level, children }: { level: Level; children: ReactNode }) {
  return <span className={`mon-pill ${level}`}>{children}</span>;
}
