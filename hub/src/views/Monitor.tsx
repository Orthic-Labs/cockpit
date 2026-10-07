import { useEffect, useState } from "react";
import { api, bytes, type Process, type Status } from "../api";
import { Bar } from "./Storage";

export function Monitor() {
  const [status, setStatus] = useState<Status | null>(null);
  const [procs, setProcs] = useState<Process[]>([]);

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      const [s, p] = await Promise.all([api.status(), api.processes()]).catch(() => [null, null]);
      if (!alive) return;
      if (s) setStatus(s as Status);
      if (p) setProcs(p as Process[]);
    };
    tick();
    const handle = setInterval(tick, 3000);
    return () => {
      alive = false;
      clearInterval(handle);
    };
  }, []);

  if (!status) return <div className="view muted">Reading…</div>;

  const cpu = (status.cpu_usage_percent.value ?? 0) / 100;
  const memUsed = status.memory_used_bytes.value ?? 0;
  const memTotal = status.memory_total_bytes.value ?? 1;
  const swapUsed = status.swap_used_bytes.value ?? 0;
  const swapTotal = status.swap_total_bytes.value ?? 0;

  return (
    <div className="view">
      <div className="grid">
        <Gauge label="CPU" value={`${Math.round(cpu * 100)}%`} fraction={cpu} />
        <Gauge
          label={`Memory${status.memory_pressure.value ? ` · ${status.memory_pressure.value}` : ""}`}
          value={`${bytes(memUsed)} of ${bytes(memTotal)}`}
          fraction={memUsed / memTotal}
        />
        <Gauge
          label="Swap"
          value={swapTotal ? `${bytes(swapUsed)} of ${bytes(swapTotal)}` : "None"}
          fraction={swapTotal ? swapUsed / swapTotal : 0}
        />
        {status.disks
          .filter((d) => d.total_bytes && !d.mount_point.startsWith("/System/"))
          .map((d) => (
            <Gauge
              key={d.mount_point}
              label={d.mount_point === "/" ? "Macintosh HD" : d.mount_point.split("/").pop() || d.mount_point}
              value={`${bytes(d.available_bytes)} free`}
              fraction={1 - (d.available_bytes ?? 0) / (d.total_bytes ?? 1)}
            />
          ))}
      </div>

      <div className="section">Using most memory</div>
      <div className="list">
        {procs.map((p) => (
          <div key={`${p.identity.pid}-${p.identity.start_time}`} className="row">
            <span className="name">{p.name}</span>
            <span className="muted small">{Math.round(p.cpu_usage_percent)}% CPU</span>
            <span className="size">{bytes(p.memory.value)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function Gauge({ label, value, fraction }: { label: string; value: string; fraction: number }) {
  return (
    <div className="gauge">
      <div className="gauge-head">
        <span className="muted small">{label}</span>
        <span className="small">{value}</span>
      </div>
      <Bar fraction={fraction} />
    </div>
  );
}
