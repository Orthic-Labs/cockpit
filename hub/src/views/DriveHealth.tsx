import { Badge } from "@rightkit/app-shell/react";
import { bytes, type DriveCard, type HealthAlert, type HealthReading } from "../api";
import "./health.css";

const day = (secs: number) =>
  new Date(secs * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
const celsius = (value: number | null) => (value == null ? null : `${Math.round(value)} °C`);

/** Warning as the notch counts it: failed self-assessment, a critical warning, or media errors. */
export function isWarning(reading: HealthReading): boolean {
  return reading.passed === false || (reading.critical_warning ?? 0) !== 0 || (reading.media_errors ?? 0) > 0;
}

/** Temperature, wear and writes, for whichever of them the drive reports. */
function readingParts(reading: HealthReading): string[] {
  return [
    celsius(reading.temperature_c),
    reading.wear_percent == null ? null : `${reading.wear_percent}% worn`,
    reading.written_bytes == null ? null : `${bytes(reading.written_bytes)} written`,
  ].filter((part): part is string => part !== null);
}

/** One drive's health on its volume card. Never blocks the card's scan action. */
export function DriveHealthLine({ card, toolAvailable }: { card: DriveCard | undefined; toolAvailable: boolean }) {
  if (!card) return null;
  if (!card.disk) return <span className="health-line muted small">Health not read for this volume</span>;
  if (card.status === "unknown") {
    return (
      <span className="health-line muted small">
        {toolAvailable ? "Health not read yet" : "Install smartmontools for drive health"}
      </span>
    );
  }
  if (card.status === "unavailable") {
    return (
      <span className="health-line small">
        <span className="muted">Health unavailable through this connection</span>
        {card.latest && (
          <span className="muted">
            · last reading {day(card.latest.at)}: {readingParts(card.latest).join(" · ")}
          </span>
        )}
      </span>
    );
  }
  if (!card.latest) return null;
  const warning = card.status === "warning";
  return (
    <span className="health-line small">
      <Badge tone={warning ? "warn" : "ok"}>{warning ? "Warning" : "OK"}</Badge>
      {readingParts(card.latest).map((part) => (
        <span key={part} className="muted">
          {part}
        </span>
      ))}
    </span>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="health-metric">
      <span className="muted small">{label}</span>
      <span className="strong">{value}</span>
    </div>
  );
}

/** A line of timestamped values; needs two readings to draw. */
function Sparkline({ label, unit, points }: { label: string; unit: string; points: { at: number; value: number }[] }) {
  if (points.length === 0) return null;
  const last = points[points.length - 1];
  const values = points.map((p) => p.value);
  const low = Math.min(...values);
  const high = Math.max(...values);
  const first = points[0].at;
  const span = high - low || 1;
  const width = 260;
  const height = 44;
  const x = (at: number) => {
    const range = last.at - first;
    return range === 0 ? width / 2 : 2 + ((at - first) / range) * (width - 4);
  };
  const y = (value: number) => height - 4 - ((value - low) / span) * (height - 8);
  const path = points.map((p, i) => `${i === 0 ? "M" : "L"}${x(p.at).toFixed(1)},${y(p.value).toFixed(1)}`).join(" ");
  return (
    <div className="health-spark">
      <div className="small">
        <span className="muted">{label}</span> <span className="strong">{last.value}{unit}</span>{" "}
        <span className="muted">
          {low}–{high}
          {unit} · {day(first)}
          {first !== last.at ? ` – ${day(last.at)}` : ""}
        </span>
      </div>
      {points.length < 2 ? (
        <span className="muted small">One reading so far; the line appears with the next.</span>
      ) : (
        <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" role="img" aria-label={`${label} over time`}>
          <path d={path} fill="none" stroke="var(--rk-accent)" strokeWidth={1.5} vectorEffect="non-scaling-stroke" />
        </svg>
      )}
    </div>
  );
}

function series(history: HealthReading[], pick: (r: HealthReading) => number | null) {
  return history.flatMap((r) => {
    const value = pick(r);
    return value == null ? [] : [{ at: r.at, value }];
  });
}

/** The detail panel that opens from a drive's card. */
export function DriveHealthPanel({
  card,
  alerts,
  toolAvailable,
}: {
  card: DriveCard | undefined;
  alerts: HealthAlert[];
  toolAvailable: boolean;
}) {
  if (!card) return null;
  const latest = card.latest;
  const ownAlerts = alerts.filter((alert) => alert.disk === card.disk);
  return (
    <section className="card-block health-panel" aria-label="Drive health">
      <div className="block-head">
        <span className="strong">{card.model ?? card.disk ?? "Drive"}</span>
        <span className="muted small">{card.mount}</span>
      </div>
      {!card.disk && <div className="muted small">Pulse could not match this volume to a physical drive.</div>}
      {card.disk && !toolAvailable && <div className="muted small">Install smartmontools for drive health.</div>}
      {card.status === "unavailable" && (
        <div className="notice small">
          <span>Health unavailable through this connection.</span>
          {latest && <span className="muted">Last good reading: {day(latest.at)}</span>}
        </div>
      )}
      {latest && (
        <div className="health-grid">
          <Metric label="Health" value={isWarning(latest) ? "Warning" : "OK"} />
          <Metric label="Temperature" value={celsius(latest.temperature_c) ?? "—"} />
          <Metric label="Wear" value={latest.wear_percent == null ? "—" : `${latest.wear_percent}%`} />
          <Metric label="Written" value={latest.written_bytes == null ? "—" : bytes(latest.written_bytes)} />
          <Metric label="Power-on time" value={latest.power_on_hours == null ? "—" : `${latest.power_on_hours.toLocaleString()} h`} />
          <Metric label="Critical warnings" value={latest.critical_warning == null ? "—" : String(latest.critical_warning)} />
          <Metric label="Media errors" value={latest.media_errors == null ? "—" : String(latest.media_errors)} />
        </div>
      )}
      {latest && (
        <div className="health-section">
          <span className="muted small">Self-tests</span>
          {latest.self_tests.length === 0 && <span className="muted small">No self-test results reported.</span>}
          {latest.self_tests.map((test, index) => (
            <div key={`${test.kind}-${index}`} className="health-test small">
              <span>{test.kind}</span>
              <span className="muted">
                {test.result}
                {test.power_on_hours != null ? ` · at ${test.power_on_hours.toLocaleString()} h` : ""}
              </span>
            </div>
          ))}
        </div>
      )}
      <Sparkline label="Temperature" unit=" °C" points={series(card.history, (r) => r.temperature_c)} />
      <Sparkline label="Wear" unit="%" points={series(card.history, (r) => r.wear_percent)} />
      <div className="health-section">
        <span className="muted small">Alerts</span>
        {ownAlerts.length === 0 && <span className="muted small">No alerts.</span>}
        {ownAlerts.slice(0, 10).map((alert) => (
          <span key={alert.id} className="small">
            {day(alert.at)} · {alert.message}
          </span>
        ))}
      </div>
    </section>
  );
}
