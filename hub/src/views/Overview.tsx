import { useEffect, useState, type ReactNode } from "react";
import { ChevronRight, CircleUser, Gauge, HardDrive, LayoutGrid, ShieldCheck, Sparkles } from "lucide-react";
import { ago, api, appsApi, bytes, type CachedApps, type CleanupReport, type Status, type UpdateReport, type Volume } from "../api";
import type { Account, Limit, NotchState } from "./Settings";
import { systemReadings } from "./Monitor";

type Level = "ok" | "warn" | "bad";

const COLOR: Record<Level, string> = {
  ok: "var(--rk-ok)",
  warn: "var(--rk-warn)",
  bad: "var(--rk-bad)",
};

const HOUR = 3600;
const DAY = 86_400;
/** Cards show at most this many drives and accounts; the rest are a "+N more" line. */
const MAX_DRIVES = 3;
const MAX_ACCOUNTS = 3;

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
 * Each card is one button that opens its section, so its gauges open it too.
 */
export function Overview({ notch, onNavigate }: { notch: NotchView; onNavigate: (section: string) => void }) {
  const [status, setStatus] = useState<Status | null>(null);
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
  const swapFraction = swapUsed != null && swapTotal ? swapUsed / swapTotal : 0;
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

  return (
    <div className="view ov">
      {missing > 0 && (
        <button type="button" className="ov-attention" onClick={() => onNavigate("permissions")}>
          <ShieldCheck size={14} strokeWidth={1.75} aria-hidden="true" />
          <span>{missing === 1 ? "1 permission needs approval" : `${missing} permissions need approval`}</span>
          <ChevronRight size={13} aria-hidden="true" />
        </button>
      )}

      <div className="ov-grid">
        <OvCard wide icon={<Gauge size={14} strokeWidth={1.75} />} title="Performance" onOpen={() => onNavigate("monitor")}>
          <span className="ov-meters">
            <Meter label="CPU" value={cpu == null ? "—" : `${Math.round(cpu)}%`} fraction={(cpu ?? 0) / 100} />
            <Meter
              label="Memory"
              value={memUsed == null ? "—" : `${bytes(memUsed)} of ${bytes(memTotal)}`}
              fraction={memFraction}
              aside={pressure ? <span className={`ov-pill ${pressureLevel(pressure)}`}>{pressure}</span> : null}
            />
            <Meter
              label="Swap"
              value={swapTotal == null ? "—" : swapTotal === 0 ? "None" : bytes(swapUsed)}
              fraction={swapFraction}
            />
          </span>
          {network && (
            <span className="ov-muted">
              Network ↓ {bytes(network.down)}/s · ↑ {bytes(network.up)}/s · {network.kind}
            </span>
          )}
        </OvCard>

        <OvCard icon={<CircleUser size={14} strokeWidth={1.75} />} title="Usage" onOpen={() => onNavigate("accounts")}>
          <span className="ov-meters">
            {notchDown ? (
              <Muted>Notch not running</Muted>
            ) : !notch.state ? (
              <Muted>Reading…</Muted>
            ) : accounts.length === 0 ? (
              <Muted>No accounts shown</Muted>
            ) : (
              <>
                {accounts.slice(0, MAX_ACCOUNTS).map((a) => (
                  <AccountUsage key={a.id} account={a} />
                ))}
                {accounts.length > MAX_ACCOUNTS && <Muted>+{accounts.length - MAX_ACCOUNTS} more</Muted>}
              </>
            )}
          </span>
        </OvCard>

        <OvCard icon={<HardDrive size={14} strokeWidth={1.75} />} title="Storage" onOpen={() => onNavigate("storage")}>
          <span className="ov-meters">
            {volumes == null ? (
              <Muted>Reading…</Muted>
            ) : drives.length === 0 ? (
              <Muted>No drives found</Muted>
            ) : (
              <>
                {drives.slice(0, MAX_DRIVES).map((v) => {
                  const used = v.total_bytes > 0 ? 1 - v.available_bytes / v.total_bytes : 0;
                  return (
                    <Meter
                      key={v.mount_point}
                      label={v.name}
                      value={`${bytes(v.available_bytes)} free of ${bytes(v.total_bytes)}`}
                      fraction={used}
                    />
                  );
                })}
                {drives.length > MAX_DRIVES && <Muted>+{drives.length - MAX_DRIVES} more</Muted>}
              </>
            )}
          </span>
        </OvCard>

        <OvCard icon={<Sparkles size={14} strokeWidth={1.75} />} title="Cleanup" onOpen={() => onNavigate("cleanup")}>
          {cleanup === undefined ? (
            <span className="ov-big">Reading…</span>
          ) : cleanup == null ? (
            <>
              <span className="ov-big">Not scanned yet</span>
              <Muted>Open Cleanup to look for what can go</Muted>
            </>
          ) : (
            <>
              <span className="ov-big">You can free {bytes(canGo)}</span>
              <Muted>Last scan {ago(cleanup.scanned_at)}</Muted>
            </>
          )}
        </OvCard>

        <OvCard icon={<LayoutGrid size={14} strokeWidth={1.75} />} title="Apps" onOpen={() => onNavigate("apps")}>
          {apps === undefined ? (
            <span className="ov-big">Reading…</span>
          ) : apps == null || apps.saved_at == null ? (
            <>
              <span className="ov-big">Not scanned yet</span>
              <Muted>Open Apps to list them</Muted>
            </>
          ) : (
            <>
              <span className="ov-big">{installed.length} apps</span>
              <Muted>{bytes(installedBytes)} installed</Muted>
            </>
          )}
          {updates !== undefined && (
            <span className={available > 0 ? "ov-update" : "ov-muted"}>
              {updates == null || updates.checked_at == null
                ? "Updates not checked yet"
                : available > 0
                  ? `${available} update${available === 1 ? "" : "s"} available`
                  : "No updates found"}
            </span>
          )}
        </OvCard>
      </div>
    </div>
  );
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
  return (
    <span className="ov-account">
      <span className="ov-account-name">{account.name}</span>
      {rows.length === 0 ? (
        <Muted>No readings yet</Muted>
      ) : (
        rows.map((r) => <Meter key={r.label} label={r.label} value={percent(r.fraction)} fraction={r.fraction} />)
      )}
    </span>
  );
}

function OvCard({
  icon,
  title,
  wide,
  onOpen,
  children,
}: {
  icon: ReactNode;
  title: string;
  wide?: boolean;
  onOpen: () => void;
  children: ReactNode;
}) {
  return (
    <button type="button" className={`ov-card${wide ? " ov-card--wide" : ""}`} onClick={onOpen}>
      <span className="ov-card-head">
        <span className="ov-card-title">
          {icon}
          {title}
        </span>
        <ChevronRight size={13} className="ov-card-go" aria-hidden="true" />
      </span>
      {children}
    </button>
  );
}

function Meter({
  label,
  value,
  fraction,
  aside,
}: {
  label: string;
  value: string;
  fraction: number;
  aside?: ReactNode;
}) {
  return (
    <span className="ov-meter">
      <span className="ov-meter-row">
        <span className="ov-meter-label">{label}</span>
        <span className="ov-meter-value">
          {aside}
          {value}
        </span>
      </span>
      <Bar fraction={fraction} />
    </span>
  );
}

function Bar({ fraction }: { fraction: number }) {
  const f = clamp(fraction);
  return (
    <span className="ov-bar">
      <i style={{ width: `${f * 100}%`, background: COLOR[levelFor(f)] }} />
    </span>
  );
}

function Muted({ children }: { children: ReactNode }) {
  return <span className="ov-muted">{children}</span>;
}
