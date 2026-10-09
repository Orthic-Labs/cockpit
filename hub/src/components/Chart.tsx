import { useState, type CSSProperties, type PointerEvent } from "react";
import "../views/monitor.css";

type Pt = [number, number] | null;

export interface Series {
  label: string;
  /** A CSS colour, normally a `var(--rk-…)`. */
  color: string;
  /** One value per time; null leaves a gap. */
  values: (number | null)[];
  dashed?: boolean;
}

/** Usage colour: below 70% ok, 70–90% warn, 90% and up bad. */
export function levelColor(percent: number): string {
  if (percent >= 90) return "var(--rk-bad)";
  if (percent >= 70) return "var(--rk-warn)";
  return "var(--rk-ok)";
}

/** Contiguous runs of known points, so a gap in the data is a gap in the line. */
function runs(points: Pt[]): [number, number][][] {
  const out: [number, number][][] = [];
  let run: [number, number][] = [];
  for (const p of points) {
    if (p) run.push(p);
    else if (run.length) {
      out.push(run);
      run = [];
    }
  }
  if (run.length) out.push(run);
  return out;
}

const fmt = (n: number) => n.toFixed(1);
const line = (run: [number, number][]) => run.map(([x, y], i) => `${i ? "L" : "M"}${fmt(x)} ${fmt(y)}`).join("");
const area = (run: [number, number][], h: number) =>
  `${line(run)}L${fmt(run[run.length - 1][0])} ${h}L${fmt(run[0][0])} ${h}Z`;

function niceMax(v: number): number {
  if (v <= 0) return 1;
  const pow = 10 ** Math.floor(Math.log10(v));
  const f = v / pow;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * pow;
}

function known(series: Series[]): number[] {
  return series.flatMap((s) => s.values.filter((v): v is number => v != null));
}

function summary(label: string, series: Series[], format: (v: number) => string, minutes: number): string {
  const first = series[0];
  if (!first) return `${label}: no data yet`;
  const all = known(series);
  if (all.length === 0) return `${label}: no data yet`;
  const parts = series.map((s) => {
    const v = [...s.values].reverse().find((x) => x != null);
    return v == null ? null : `${series.length > 1 ? `${s.label} ` : ""}${format(v)}`;
  });
  return `${label}: now ${parts.filter(Boolean).join(", ")}; over the last ${minutes} minutes from ${format(Math.min(...all))} to ${format(Math.max(...all))}`;
}

/** A small inline line for a card. One or two series on a shared scale. */
export function Sparkline({
  label,
  series,
  max,
  height = 28,
  format = (v) => `${Math.round(v)}`,
  minutes = 10,
}: {
  label: string;
  series: Series[];
  /** Top of the scale. Omitted: the largest value, rounded up. */
  max?: number;
  height?: number;
  format?: (v: number) => string;
  /** Only for the accessible summary. */
  minutes?: number;
}) {
  const W = 200;
  const top = max ?? niceMax(Math.max(0, ...known(series)));
  const n = Math.max(0, ...series.map((s) => s.values.length));
  const x = (i: number) => (n <= 1 ? W : (i / (n - 1)) * W);
  const y = (v: number) => 1.5 + (1 - Math.min(Math.max(v / top, 0), 1)) * (height - 3);
  return (
    <svg
      className="chart-spark"
      role="img"
      aria-label={summary(label, series, format, minutes)}
      viewBox={`0 0 ${W} ${height}`}
      preserveAspectRatio="none"
      style={{ height }}
    >
      {series.map((s, si) => {
        const pts: Pt[] = s.values.map((v, i) => (v == null ? null : [x(i), y(v)]));
        return runs(pts).map((r, ri) => (
          <g key={`${si}-${ri}`} style={{ color: s.color }}>
            {si === 0 && r.length > 1 && <path className="chart-fill" d={area(r, height)} />}
            <path className={`chart-line${s.dashed ? " chart-dashed" : ""}`} d={line(r)} />
          </g>
        ));
      })}
    </svg>
  );
}

const timeText = (t: number) =>
  new Date(t).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });

/**
 * Time on x (the last `rangeMs` ending at the newest sample), value on y.
 * Hover shows a vertical rule with the time and each series' value.
 */
export function AreaChart({
  label,
  times,
  series,
  rangeMs,
  max,
  format,
  height = 120,
}: {
  label: string;
  /** Sample times, oldest first; every series has one value per time. */
  times: number[];
  series: Series[];
  rangeMs: number;
  /** Fixed top of the scale (100 for percentages). Omitted: the largest value, rounded up. */
  max?: number;
  format: (v: number) => string;
  height?: number;
}) {
  const [hover, setHover] = useState<number | null>(null);
  const W = 600;
  const end = times.length ? times[times.length - 1] : 0;
  const start = end - rangeMs;
  const from = Math.max(0, times.findIndex((t) => t >= start));
  const visible = times.slice(from);
  const shown = series.map((s) => ({ ...s, values: s.values.slice(from) }));
  const top = max ?? niceMax(Math.max(0, ...known(shown)));
  const x = (t: number) => ((t - start) / rangeMs) * W;
  const y = (v: number) => 1.5 + (1 - Math.min(Math.max(v / top, 0), 1)) * (height - 3);
  const minutes = Math.round(rangeMs / 60_000);

  const move = (e: PointerEvent<HTMLDivElement>) => {
    if (visible.length === 0) return;
    const rect = e.currentTarget.getBoundingClientRect();
    if (rect.width <= 0) return;
    const t = start + ((e.clientX - rect.left) / rect.width) * rangeMs;
    let best = 0;
    for (let i = 1; i < visible.length; i++) {
      if (Math.abs(visible[i] - t) < Math.abs(visible[best] - t)) best = i;
    }
    setHover(best);
  };

  const at = hover != null && hover < visible.length ? hover : null;
  const frac = at == null ? 0 : (visible[at] - start) / rangeMs;
  const tipStyle: CSSProperties = { left: `${frac * 100}%`, transform: frac > 0.6 ? "translateX(calc(-100% - 8px))" : "translateX(8px)" };

  return (
    <div className="chart">
      <div className="chart-plot" style={{ height }} onPointerMove={move} onPointerLeave={() => setHover(null)} onPointerCancel={() => setHover(null)}>
        <svg
          className="chart-svg"
          role="img"
          aria-label={summary(label, shown, format, minutes)}
          viewBox={`0 0 ${W} ${height}`}
          preserveAspectRatio="none"
        >
          <line className="chart-grid" x1="0" x2={W} y1={y(top)} y2={y(top)} />
          <line className="chart-grid" x1="0" x2={W} y1={y(top / 2)} y2={y(top / 2)} />
          <line className="chart-grid chart-base" x1="0" x2={W} y1={height - 0.5} y2={height - 0.5} />
          {shown.map((s, si) => {
            const pts: Pt[] = s.values.map((v, i) => (v == null ? null : [x(visible[i]), y(v)]));
            return runs(pts).map((r, ri) => (
              <g key={`${si}-${ri}`} style={{ color: s.color }}>
                {si === 0 && r.length > 1 && <path className="chart-fill" d={area(r, height)} />}
                <path className={`chart-line${s.dashed ? " chart-dashed" : ""}`} d={line(r)} />
              </g>
            ));
          })}
        </svg>
        <span className="chart-ymax" aria-hidden="true">{format(top)}</span>
        {at != null && (
          <>
            <div className="chart-rule" style={{ left: `${frac * 100}%` }} aria-hidden="true" />
            <div className="chart-tip" style={tipStyle} aria-hidden="true">
              <span className="chart-tip-time">{timeText(visible[at])}</span>
              {shown.map((s) => {
                const v = s.values[at];
                return (
                  <span key={s.label} className="chart-tip-row">
                    <i style={{ background: s.color }} />
                    {s.label}: {v == null ? "no reading" : format(v)}
                  </span>
                );
              })}
            </div>
          </>
        )}
      </div>
      <div className="chart-axis" aria-hidden="true">
        <span>{minutes} min ago</span>
        <span>now</span>
      </div>
    </div>
  );
}
