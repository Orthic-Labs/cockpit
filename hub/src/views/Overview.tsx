import { useEffect, useState, type ReactNode } from "react";
import {
  ArrowDown,
  ArrowUp,
  ChevronRight,
  CircleCheck,
  CircleUser,
  CircleX,
  Gauge,
  HardDrive,
  LayoutGrid,
  ShieldCheck,
  Sparkles,
  TriangleAlert,
} from "lucide-react";
import { ago, api, appsApi, bytes, type CachedApps, type CleanupReport, type Status, type UpdateReport, type Volume } from "../api";
import type { Account, Limit, NotchState } from "./Settings";
import { systemReadings } from "./Monitor";
import { Sparkline, levelColor } from "../components/Chart";
import { lastMinutes, memPercent, useMetrics } from "../metrics";

type Level = "ok" | "warn" | "bad";

const HOUR = 3600;
const DAY = 86_400;

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

const clamp = (f: number) => Math.min(Math.max(f, 0), 1);
const percent = (f: number) => `${Math.round(clamp(f) * 100)}%`;

/** The notch's state as `useNotch` returns it. `error` is set while the notch is not running. */
export interface NotchView {
  state: NotchState | null;
  error: string | null;
}

/**
 * The start page. Every number here is either live (performance, every 3 s) or
 * read from what is already saved. Nothing on this page starts a scan.
 * Each card header opens its section; the headline sentence is the worst of the live readings.
 */
export function Overview({ notch, onNavigate }: { notch: NotchView; onNavigate: (section: string) => void }) {
  const [status, setStatus] = useState<Status | null>(null);
  const recent = lastMinutes(useMetrics().samples, 10);
  const [volumes, setVolumes] = useState<Volume[] | null>(null);
  // `undefined` while reading; `null` when nothing is saved.
  const [cleanup, setCleanup] = useState<CleanupReport | null | undefined>(undefined);
  const [apps, setApps] = useState<CachedApps | null | undefined>(undefined);
  const [updates, setUpdates] = useState<UpdateReport | null | undefined>(undefined);

  // Live numbers, only while this page is mounted (the hub unmounts it when another section opens).
  useEffect(() => {
    let alive = true;
    const tick = () => {
      api
        .status()
        .then((s) => {
          if (alive) setStatus(s);
        })
        .catch(() => {});
    };
    tick();
    const handle = setInterval(tick, 3000);
    return () => {
      alive = false;
      clearInterval(handle);
    };
  }, []);

  // Drive sizes change slowly: read on open, then every 30 s.
  useEffect(() => {
    let alive = true;
    const read = () => {
      api
        .volumes()
        .then((v) => {
          if (alive) setVolumes(v);
        })
        .catch(() => {
          if (alive) setVolumes([]);
        });
    };
    read();
    const handle = setInterval(read, 30_000);
    return () => {
      alive = false;
      clearInterval(handle);
    };
  }, []);

  // Saved results only. These commands never scan.
  useEffect(() => {
    let alive = true;
    api
      .cleanupCached()
      .then((r) => {
        if (alive) setCleanup(r);
      })
      .catch(() => {
        if (alive) setCleanup(null);
      });
    appsApi
      .cached()
      .then((c) => {
        if (alive) setApps(c);
      })
      .catch(() => {
        if (alive) setApps(null);
      });
    appsApi
      .updatesCached()
      .then((u) => {
        if (alive) setUpdates(u);
      })
      .catch(() => {
        if (alive) setUpdates(null);
      });
    return () => {
      alive = false;
    };
  }, []);

  const cpu = status?.cpu_usage_percent.value ?? null;
  const memUsed = status?.memory_used_bytes.value ?? null;
  const memTotal = status?.memory_total_bytes.value ?? null;
  const memFraction = memUsed != null && memTotal ? memUsed / memTotal : 0;
  const swapUsed = status?.swap_used_bytes.value ?? null;
  const swapTotal = status?.swap_total_bytes.value ?? null;
  const pressure = status?.memory_pressure.value ?? null;

  const drives = (volumes ?? []).filter((v) => !v.disk_image);

  const canGo = cleanup ? cleanup.safe_bytes + cleanup.review_bytes : 0;

  const installed = apps?.apps ?? [];
  const installedBytes = installed.reduce((n, a) => n + a.size_bytes, 0);
  const available = updates?.apps.filter((a) => a.state === "available").length ?? 0;

  const notchDown = notch.error != null;
  const accounts = notch.state?.accounts.filter((a) => a.connected) ?? [];
  const missing = notch.state?.permissions?.filter((p) => p.required && p.status !== "granted").length ?? 0;
  const network = systemReadings(notch.state)?.network ?? null;

  const memLevel = maxLevel(levelFor(memFraction), pressure ? pressureLevel(pressure) : "ok");
  const cpuLevel = levelFor((cpu ?? 0) / 100);
  const fullest = drives.reduce<Volume | null>((worst, v) => (usedOf(v) > (worst ? usedOf(worst) : -1) ? v : worst), null);
  const diskLevel = fullest ? levelFor(usedOf(fullest)) : "ok";
  // Swap has no fixed limit: it is busy once it passes half of the memory.
  const swapBusy = swapUsed != null && memTotal ? swapUsed / memTotal >= 0.5 : false;

  const headline = chooseHeadline({
    status,
    cpu,
    memUsed,
    memTotal,
    memFraction,
    memLevel,
    cpuLevel,
    fullest,
    diskLevel,
    swapBusy,
    swapUsed,
    canGo,
    cleanupScanned: cleanup != null,
    available,
  });

  const HeroIcon = headline.level === "bad" ? CircleX : headline.level === "warn" ? TriangleAlert : CircleCheck;

  return (
    <div className="view ov">
      {missing > 0 && (
        <button type="button" className="ov-attention" onClick={() => onNavigate("permissions")}>
          <ShieldCheck size={14} strokeWidth={1.75} aria-hidden="true" />
          <span>{missing === 1 ? "1 permission needs approval" : `${missing} permissions need approval`}</span>
          <ChevronRight size={13} aria-hidden="true" />
        </button>
      )}

      <section className="ov-hero" role="status">
        <span className={`ov-hero-mark ${headline.level}`}>
          <HeroIcon size={24} strokeWidth={1.75} aria-hidden="true" />
        </span>
        <div className="ov-hero-copy">
          <h2 className="ov-hero-title">{headline.title}</h2>
          <p className="ov-hero-reason">{headline.reason}</p>
        </div>
        <div className="ov-hero-actions">
          {headline.actions.map((a, i) => (
            <button key={a.label} type="button" className={`ov-action${i === 0 ? " primary" : ""}`} onClick={() => onNavigate(a.section)}>
              {a.label}
            </button>
          ))}
        </div>
      </section>

      <div className="ov-row ov-row--2">
        <OvCard icon={<Gauge size={16} strokeWidth={1.75} />} title="Performance" go="Monitor" onOpen={() => onNavigate("monitor")}>
          <div className="ov-meters ov-meters--two">
            <Meter label="CPU" value={cpu == null ? "—" : `${Math.round(cpu)}`} unit="%" fraction={(cpu ?? 0) / 100} measured={cpu != null}
              spark={
                recent.length > 1 && (
                  <Sparkline
                    label="CPU"
                    max={100}
                    series={[{ label: "CPU", color: levelColor(recent[recent.length - 1].cpu), values: recent.map((r) => r.cpu) }]}
                    format={(v) => `${Math.round(v)}%`}
                  />
                )
              }
            />
            <Meter
              label="Memory"
              value={memUsed == null ? "—" : bytes(memUsed)}
              fraction={memFraction}
              measured={memUsed != null}
              level={memLevel}
              note={memUsed == null ? undefined : `of ${bytes(memTotal)} · ${percent(memFraction)}${pressure ? ` · ${pressure}` : ""}`}
              spark={
                recent.length > 1 && (
                  <Sparkline
                    label="Memory"
                    max={100}
                    series={[{ label: "Memory", color: levelColor(memPercent(recent[recent.length - 1])), values: recent.map(memPercent) }]}
                    format={(v) => `${Math.round(v)}%`}
                  />
                )
              }
            />
            <div className="ov-stat">
              <span className="ov-label">Swap</span>
              <span className="ov-value">
                {swapTotal == null ? "—" : swapTotal === 0 ? "None" : bytes(swapUsed)}
                {swapTotal != null && swapTotal > 0 && <small> in use</small>}
              </span>
            </div>
            <div className="ov-stat">
              <span className="ov-label">{network ? `Network · ${network.kind}` : "Network"}</span>
              <span className="ov-value ov-net">
                {network ? (
                  <>
                    <span>
                      <ArrowDown size={14} aria-label="Down" /> {bytes(network.down)}
                      <small>/s</small>
                    </span>
                    <span>
                      <ArrowUp size={14} aria-label="Up" /> {bytes(network.up)}
                      <small>/s</small>
                    </span>
                  </>
                ) : (
                  <small>Not available</small>
                )}
              </span>
              {recent.some((r) => r.netDown != null) && (
                <div className="overview-spark">
                  <Sparkline
                    label="Network"
                    series={[
                      { label: "Down", color: "var(--rk-accent)", values: recent.map((r) => r.netDown) },
                      { label: "Up", color: "var(--rk-ink-2)", values: recent.map((r) => r.netUp), dashed: true },
                    ]}
                    format={(v) => `${bytes(v)}/s`}
                  />
                </div>
              )}
            </div>
          </div>
        </OvCard>

        <OvCard icon={<CircleUser size={16} strokeWidth={1.75} />} title="AI usage" go="Accounts" onOpen={() => onNavigate("accounts")}>
          <div className="ov-meters">
            {notchDown ? (
              <span className="ov-sub">Notch not running</span>
            ) : !notch.state ? (
              <span className="ov-sub">Reading…</span>
            ) : accounts.length === 0 ? (
              <span className="ov-sub">No accounts shown</span>
            ) : (
              accounts.map((a) => <AccountUsage key={a.id} account={a} />)
            )}
          </div>
        </OvCard>
      </div>

      <div className="ov-row ov-row--3">
        <OvCard icon={<HardDrive size={16} strokeWidth={1.75} />} title="Storage" go="Storage" onOpen={() => onNavigate("storage")}>
          {volumes == null ? (
            <span className="ov-sub">Reading…</span>
          ) : drives.length === 0 ? (
            <span className="ov-sub">No drives found</span>
          ) : (
            drives.map((v) => <Volume_ key={v.mount_point} volume={v} />)
          )}
        </OvCard>

        <OvCard icon={<Sparkles size={16} strokeWidth={1.75} />} title="Cleanup" go={cleanup == null ? "Scan" : "Review"} onOpen={() => onNavigate("cleanup")}>
          {cleanup === undefined ? (
            <span className="ov-big">Reading…</span>
          ) : cleanup == null ? (
            <>
              <span className="ov-big">Not scanned</span>
              <span className="ov-sub">Look for what can go</span>
              <button type="button" className="ov-action primary ov-cta" onClick={() => onNavigate("cleanup")}>
                Scan for clutter
              </button>
            </>
          ) : (
            <>
              <span className="ov-big">{bytes(canGo)}</span>
              <span className="ov-sub">can be cleared · last scan {ago(cleanup.scanned_at)}</span>
            </>
          )}
        </OvCard>

        <OvCard icon={<LayoutGrid size={16} strokeWidth={1.75} />} title="Apps" go="Apps" onOpen={() => onNavigate("apps")}>
          {apps === undefined ? (
            <span className="ov-big">Reading…</span>
          ) : apps == null || apps.saved_at == null ? (
            <>
              <span className="ov-big">Not listed</span>
              <span className="ov-sub">Open Apps to list what is installed</span>
            </>
          ) : (
            <>
              <span className="ov-big">{installed.length} apps</span>
              <span className="ov-sub">{bytes(installedBytes)} installed</span>
            </>
          )}
          {updates !== undefined &&
            (available > 0 ? (
              <button type="button" className="ov-pill" onClick={() => onNavigate("apps")}>
                <i aria-hidden="true" />
                {available} update{available === 1 ? "" : "s"} available
              </button>
            ) : (
              <span className="ov-sub">
                {updates == null || updates.checked_at == null ? "Updates not checked yet" : "No updates found"}
              </span>
            ))}
        </OvCard>
      </div>
    </div>
  );
}

const usedOf = (v: Volume) => (v.total_bytes > 0 ? 1 - v.available_bytes / v.total_bytes : 0);
const RANK: Record<Level, number> = { ok: 0, warn: 1, bad: 2 };
const WORD: Record<Level, string> = { ok: "Fine", warn: "Busy", bad: "Critical" };
const LEVEL_ICON = { ok: CircleCheck, warn: TriangleAlert, bad: CircleX } as const;
const maxLevel = (a: Level, b: Level): Level => (RANK[b] > RANK[a] ? b : a);

interface HeadlineAction {
  label: string;
  section: string;
}

interface Headline {
  level: Level;
  title: string;
  reason: string;
  actions: HeadlineAction[];
}

interface HeadlineInput {
  status: Status | null;
  cpu: number | null;
  memUsed: number | null;
  memTotal: number | null;
  memFraction: number;
  memLevel: Level;
  cpuLevel: Level;
  fullest: Volume | null;
  diskLevel: Level;
  swapBusy: boolean;
  swapUsed: number | null;
  canGo: number;
  cleanupScanned: boolean;
  available: number;
}

/**
 * One sentence for the whole page: the worst of memory, CPU, a full drive and swap.
 * Ties keep that order. When all are fine, the saved cleanup and updates decide the actions.
 */
function chooseHeadline(i: HeadlineInput): Headline {
  const clear: HeadlineAction[] = i.canGo > 0 ? [{ label: `Clear ${bytes(i.canGo)}`, section: "cleanup" }] : [];
  const memText = i.memUsed == null ? "" : `${bytes(i.memUsed)} of ${bytes(i.memTotal)} memory in use (${percent(i.memFraction)})`;
  const cpuText = i.cpu == null ? "" : `CPU at ${Math.round(i.cpu)}%`;
  const rest = "Open Monitor to see what is using it.";

  if (!i.status) {
    return { level: "ok", title: "Reading your Mac…", reason: "Live readings appear in a moment.", actions: [] };
  }

  const issues: { level: Level; make: () => Headline }[] = [
    {
      level: i.memLevel,
      make: () => ({
        level: i.memLevel,
        title: i.memLevel === "bad" ? "Memory is critically high" : "Memory is getting busy",
        reason: `${memText}. ${rest}`,
        actions: [{ label: "Open Monitor", section: "monitor" }, ...clear].slice(0, 2),
      }),
    },
    {
      level: i.cpuLevel,
      make: () => ({
        level: i.cpuLevel,
        title: i.cpuLevel === "bad" ? "CPU is critically high" : "CPU is getting busy",
        reason: `${cpuText}. ${rest}`,
        actions: [{ label: "Open Monitor", section: "monitor" }, ...clear].slice(0, 2),
      }),
    },
    {
      level: i.diskLevel,
      make: () => ({
        level: i.diskLevel,
        title: i.diskLevel === "bad" ? "A drive is almost full" : "A drive is filling up",
        reason: `${i.fullest?.name} has ${bytes(i.fullest?.available_bytes)} free of ${bytes(i.fullest?.total_bytes)}.`,
        actions: [{ label: "Open Storage", section: "storage" }, ...clear].slice(0, 2),
      }),
    },
    {
      level: i.swapBusy ? "warn" : "ok",
      make: () => ({
        level: "warn",
        title: "Swap is heavily used",
        reason: `${bytes(i.swapUsed)} of swap is in use, so memory has been tight. ${rest}`,
        actions: [{ label: "Open Monitor", section: "monitor" }, ...clear].slice(0, 2),
      }),
    },
  ];

  let worst: (typeof issues)[number] | null = null;
  for (const it of issues) if (it.level !== "ok" && (!worst || RANK[it.level] > RANK[worst.level])) worst = it;
  if (worst) return worst.make();

  const parts = [cpuText, memText].filter(Boolean).join(" and ");
  const extras: string[] = [];
  if (i.canGo > 0) extras.push(`${bytes(i.canGo)} can be cleared`);
  if (i.available > 0) extras.push(`${i.available} app update${i.available === 1 ? "" : "s"} available`);
  const actions: HeadlineAction[] = [];
  if (!i.cleanupScanned) actions.push({ label: "Scan for clutter", section: "cleanup" });
  else if (i.canGo > 0) actions.push({ label: `Review ${bytes(i.canGo)} to clear`, section: "cleanup" });
  if (i.available > 0) actions.push({ label: "Update apps", section: "apps" });
  return {
    level: "ok",
    title: "Your Mac is running well",
    reason: `${parts}: both comfortable. Storage is fine.${extras.length ? ` ${extras.join(" · ")}.` : ""}`,
    actions: actions.slice(0, 2),
  };
}

/** Picks the five-hour and weekly windows; falls back to the first two the provider sent. */
function pickLimits(limits: Limit[] = []): { label: string; fraction: number }[] {
  const five = limits.find((l) => l.seconds != null && Math.abs(l.seconds - 5 * HOUR) < 60);
  const week = limits.find((l) => l.seconds != null && l.seconds >= 6 * DAY && l.seconds <= 8 * DAY);
  const rows: { label: string; fraction: number }[] = [];
  if (five) rows.push({ label: "5-hour", fraction: five.usedFraction });
  if (week) rows.push({ label: "Weekly", fraction: week.usedFraction });
  if (rows.length > 0) return rows;
  return limits.slice(0, 2).map((l) => ({ label: l.label, fraction: l.usedFraction }));
}

function AccountUsage({ account }: { account: Account }) {
  const rows = pickLimits(account.limits);
  if (rows.length === 0) return <span className="ov-sub">{account.name} · no readings yet</span>;
  return (
    <>
      {rows.map((r) => (
        <Meter key={r.label} label={`${account.name} · ${r.label}`} value={`${Math.round(clamp(r.fraction) * 100)}`} unit="%" fraction={r.fraction} />
      ))}
    </>
  );
}

function Volume_({ volume }: { volume: Volume }) {
  const used = usedOf(volume);
  const level = levelFor(used);
  return (
    <div className="ov-vol">
      <div className="ov-meter-row">
        <span className={`ov-level ${level}`}>
          <LevelMark level={level} size={14} />
          <b>{volume.name}</b>
        </span>
        <span className="ov-value ov-value--sm">
          {bytes(volume.available_bytes)} <small>free</small>
        </span>
      </div>
      <Bar fraction={used} level={level} label={`${volume.name} used`} />
      <span className="ov-note">
        of {bytes(volume.total_bytes)} · {percent(used)} used{level === "ok" ? "" : ` · ${WORD[level]}`}
      </span>
    </div>
  );
}

function LevelMark({ level, size }: { level: Level; size: number }) {
  const Icon = LEVEL_ICON[level];
  return (
    <>
      <Icon size={size} strokeWidth={1.75} aria-hidden="true" />
      <span className="ov-vh">{WORD[level]}</span>
    </>
  );
}

function OvCard({
  icon,
  title,
  go,
  onOpen,
  children,
}: {
  icon: ReactNode;
  title: string;
  go: string;
  onOpen: () => void;
  children: ReactNode;
}) {
  return (
    <section className="ov-card" aria-label={title}>
      <button type="button" className="ov-card-head" onClick={onOpen} aria-label={`${title}, open ${go}`}>
        <span className="ov-card-title">
          {icon}
          {title}
        </span>
        <span className="ov-card-go">
          {go}
          <ChevronRight size={14} aria-hidden="true" />
        </span>
      </button>
      {children}
    </section>
  );
}

function Meter({
  label,
  value,
  unit,
  fraction,
  level,
  note,
  spark,
  measured = true,
}: {
  label: string;
  value: string;
  unit?: string;
  fraction: number;
  level?: Level;
  note?: string;
  /** A sparkline of the last few minutes, under the bar. */
  spark?: ReactNode;
  measured?: boolean;
}) {
  const lv = level ?? levelFor(clamp(fraction));
  return (
    <div className="ov-meter">
      <div className="ov-meter-row">
        <span className="ov-label">{label}</span>
        <span className="ov-value">
          {measured && (
            <span className={`ov-level ${lv}`}>
              <LevelMark level={lv} size={14} />
            </span>
          )}
          {value}
          {unit && <small>{unit}</small>}
        </span>
      </div>
      <Bar fraction={fraction} level={lv} label={label} />
      {spark ? <div className="overview-spark">{spark}</div> : null}
      {note != null ? (
        <span className="ov-note">{note}</span>
      ) : (
        lv !== "ok" && <span className={`ov-note ${lv}`}>{WORD[lv]}</span>
      )}
    </div>
  );
}

function Bar({ fraction, level, label }: { fraction: number; level: Level; label: string }) {
  const f = clamp(fraction);
  return (
    <span className="ov-bar" role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(f * 100)}>
      <i className={level} style={{ width: `${Math.max(f * 100, 1)}%` }} />
    </span>
  );
}
