import { useEffect, useMemo, useState, type ReactNode } from "react";
import { Button, ConfirmDialog } from "@rightkit/app-shell/react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { api, bytes, type ProcessRow, type Status } from "../api";
import { useNotch, type NotchState } from "./Settings";
import { AreaChart, levelColor, type Series } from "../components/Chart";
import { memPercent, useMetrics, type Sample } from "../metrics";
import "./monitor.css";

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

/** The notch's System readings beyond CPU and memory (`system` in its state file). */
export interface SystemReadings {
  network?: { interface: string; kind: string; down: number; up: number };
  battery?: { percent: number; charging: boolean; cycles?: number; health?: number };
  fans?: { name: string; rpm: number }[];
  temperatures?: { name: string; celsius: number }[];
}

/** The System readings from the notch's state, or null while the notch has none. */
export function systemReadings(state: NotchState | null): SystemReadings | null {
  return (state as (NotchState & { system?: SystemReadings }) | null)?.system ?? null;
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
  const [sensorsOpen, setSensorsOpen] = useState(false);
  const notch = useNotch();
  const system = systemReadings(notch.state);

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
  const swapUsed = status.swap_used_bytes.value;
  const swapTotal = status.swap_total_bytes.value;
  const pressure = status.memory_pressure.value;
  const leader = [...procs].sort(comparator(sort === "name" ? "memory" : sort))[0];
  const sortLabel = SORTS.find((s) => s.key === sort)?.label ?? "Memory";

  const network = system?.network;
  const sensorCount = (system?.fans?.length ?? 0) + (system?.temperatures?.length ?? 0) + (system?.battery ? 1 : 0);

  return (
    <div className="view monitor-view">
      <div className={`monitor-notice ${pressure ? pressureLevel(pressure) : "warn"}`} role="status">
        <h2>{pressure ? `Memory pressure ${pressure.toLowerCase()}` : "Memory pressure unknown"}</h2>
        <p>
          {leader
            ? `${leader.name} · ${sort === "cpu" ? `${Math.round(leader.cpu_usage_percent)}% CPU` : `${bytes(leader.memory_bytes)} memory`}`
            : "No process rows reported."}
        </p>
      </div>

      <MonitorCharts
        cpu={cpuRaw}
        swapText={swapTotal == null ? "Swap unknown" : swapTotal === 0 ? "No swap space" : `Swap ${bytes(swapUsed)} of ${bytes(swapTotal)}`}
        network={network ? { down: network.down, up: network.up, kind: network.kind } : null}
        notchDown={notch.error != null}
      />

      {sensorCount > 0 && system && (
        <div className="mon-sensors">
          <button
            type="button"
            className="mon-disclosure"
            aria-expanded={sensorsOpen}
            aria-controls="mon-sensors-list"
            onClick={() => setSensorsOpen((open) => !open)}
          >
            {sensorsOpen ? <ChevronDown size={12} aria-hidden="true" /> : <ChevronRight size={12} aria-hidden="true" />}
            <span className="mon-label">Sensors</span>
            <span className="muted small">
              {[
                system.fans?.length ? `${system.fans.length} fan${system.fans.length === 1 ? "" : "s"}` : null,
                system.temperatures?.length ? `${system.temperatures.length} temperatures` : null,
                system.battery ? "battery" : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
          </button>
          {sensorsOpen && (
            <div id="mon-sensors-list" className="mon-sensors-list">
              {system.battery && (
                <SensorRow label="Battery" value={batteryText(system.battery)} />
              )}
              {system.fans?.map((f, i) => (
                <SensorRow key={`fan-${i}`} label={f.name} value={`${Math.round(f.rpm).toLocaleString()} rpm`} />
              ))}
              {[...(system.temperatures ?? [])]
                .sort((a, b) => a.name.localeCompare(b.name))
                .map((t, i) => (
                  <SensorRow key={`temp-${i}`} label={t.name} value={`${t.celsius.toFixed(1)} °C`} />
                ))}
            </div>
          )}
        </div>
      )}

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
        <h2 className="monitor-h2">Top consumers</h2>
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
                    </button>
                  ) : (
                    <span className="mon-label" title={title}>
                      {p.name}
                    </span>
                  )}
                  {system && <span className="mon-pill">System</span>}
                  <span className="muted small monitor-meta">
                    {p.members.length} process{p.members.length === 1 ? "" : "es"} · {system ? "Protected system process" : "Running"}
                  </span>
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

      <p className="caption muted small">CPU: 100% = one core. System CPU above is 0–100% across all cores.</p>

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

function SensorRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="mon-sensor">
      <span className="mon-label">{label}</span>
      <span>{value}</span>
    </div>
  );
}

function batteryText(b: NonNullable<SystemReadings["battery"]>): string {
  const parts = [`${b.percent}%`, b.charging ? "charging" : null];
  if (b.cycles != null) parts.push(`${b.cycles} cycles`);
  if (b.health != null) parts.push(`${Math.round(b.health * 100)}% health`);
  return parts.filter(Boolean).join(" · ");
}

const RANGES = [5, 15, 30];

/** CPU, memory with swap, and network over the last 5, 15 or 30 minutes. The hub keeps the history while its window is closed. */
function MonitorCharts({
  cpu: cpuNow,
  swapText,
  network: net,
  notchDown,
}: {
  cpu: number | null;
  swapText: string;
  network: { down: number; up: number; kind: string } | null;
  notchDown: boolean;
}) {
  const { samples, latest } = useMetrics();
  const [minutes, setMinutes] = useState(15);
  const times = useMemo(() => samples.map((s) => s.ts), [samples]);
  const column = (pick: (s: Sample) => number | null): (number | null)[] => samples.map(pick);

  const cpu: Series[] = [{ label: "CPU", color: levelColor(latest?.cpu ?? 0), values: column((s) => s.cpu) }];
  const memory: Series[] = [
    { label: "Memory", color: levelColor(latest ? memPercent(latest) : 0), values: column((s) => s.memUsed) },
    { label: "Swap", color: "var(--rk-ink-2)", values: column((s) => s.swapUsed), dashed: true },
  ];
  const network: Series[] = [
    { label: "Down", color: "var(--rk-accent)", values: column((s) => s.netDown) },
    { label: "Up", color: "var(--rk-ink-2)", values: column((s) => s.netUp), dashed: true },
  ];
  const rate = (v: number) => `${bytes(v)}/s`;
  const memTop = Math.max(latest?.memTotal ?? 0, ...samples.map((s) => s.swapUsed));

  return (
    <section className="monitor-charts" aria-label="History">
      <div className="monitor-charts-head">
        <h2 className="monitor-h2">History, last {minutes} minutes</h2>
        <div className="mon-seg" role="group" aria-label="Time range">
          {RANGES.map((m) => (
            <button key={m} type="button" aria-pressed={minutes === m} onClick={() => setMinutes(m)}>
              {m} min
            </button>
          ))}
        </div>
      </div>
      {samples.length < 2 ? (
        <div className="monitor-waiting muted">Collecting readings…</div>
      ) : (
        <div className="monitor-charts-grid">
          <ChartCard title="System CPU" now={cpuNow != null ? `${Math.round(cpuNow)}%` : latest ? `${Math.round(latest.cpu)}%` : "—"} extra="All cores, 0–100%">
            <AreaChart label="CPU" times={times} series={cpu} rangeMs={minutes * 60_000} max={100} format={(v) => `${Math.round(v)}%`} height={96} />
          </ChartCard>
          <ChartCard
            title="Memory used"
            now={latest ? `${bytes(latest.memUsed)} / ${bytes(latest.memTotal)}` : "—"}
            legend={memory}
            extra={swapText}
          >
            <AreaChart label="Memory" times={times} series={memory} rangeMs={minutes * 60_000} max={memTop || undefined} format={(v) => bytes(v)} height={96} />
          </ChartCard>
          <ChartCard
            title="Network"
            now={latest?.netDown != null ? `↓ ${rate(latest.netDown)}` : net ? `↓ ${rate(net.down)}` : "—"}
            legend={network}
            extra={latest?.netUp != null ? `↑ ${rate(latest.netUp)}` : net ? `↑ ${rate(net.up)} · ${net.kind}` : notchDown ? "Notch not running" : "No active interface"}
          >
            <AreaChart label="Network" times={times} series={network} rangeMs={minutes * 60_000} format={rate} height={96} />
          </ChartCard>
        </div>
      )}
    </section>
  );
}

function ChartCard({
  title,
  now,
  extra,
  legend,
  children,
}: {
  title: string;
  now: string;
  extra?: string;
  legend?: Series[];
  children: ReactNode;
}) {
  return (
    <div className="monitor-chart-card">
      <div className="monitor-chart-head">
        <span>{title}</span>
        <span className="monitor-chart-now">{now}</span>
        {extra && <span>{extra}</span>}
      </div>
      {children}
      {legend && (
        <div className="monitor-legend" aria-hidden="true">
          {legend.map((s) => (
            <span key={s.label}>
              <i className={s.dashed ? "dashed" : undefined} style={{ borderTopColor: s.color }} />
              {s.label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
