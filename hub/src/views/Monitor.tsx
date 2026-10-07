import { useEffect, useState } from "react";
import { Button, ConfirmDialog } from "@rightkit/app-shell/react";
import { api, bytes, type ProcessRow, type Status } from "../api";
import { Bar } from "./Storage";

export function Monitor() {
  const [status, setStatus] = useState<Status | null>(null);
  const [procs, setProcs] = useState<ProcessRow[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [stuck, setStuck] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [forcing, setForcing] = useState<ProcessRow | null>(null);

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
      </div>

      <div className="section">Apps and processes, most memory first</div>
      <div className="list">
        {procs.slice(0, 60).map((p) => (
          <div key={p.key}>
            <div className="row procs">
              <span
                className="name clickable"
                onClick={() => setOpen(open === p.key ? null : p.key)}
                title={p.refusal ?? p.app_path ?? p.name}
              >
                <span className="icon">{p.members.length > 1 ? (open === p.key ? "▾" : "▸") : "·"}</span>
                {p.name}
                {p.members.length > 1 && <span className="muted small"> {p.members.length} processes</span>}
                {notes[p.key] && <span className="muted small"> · {notes[p.key]}</span>}
              </span>
              <span className="muted small">{Math.round(p.cpu_usage_percent)}%</span>
              <span className="size">{bytes(p.memory_bytes)}</span>
              <span className="actions">
                {p.can_act && (
                  <>
                    <Button size="sm" variant="secondary" disabled={busy === p.key} onClick={() => quit(p)}>
                      Quit
                    </Button>
                    <Button
                      size="sm"
                      variant={stuck.has(p.key) ? "danger" : "ghost"}
                      disabled={busy === p.key}
                      onClick={() => setForcing(p)}
                    >
                      Force Quit
                    </Button>
                  </>
                )}
              </span>
            </div>
            {open === p.key &&
              p.members.map((m) => (
                <div key={`${m.identity.pid}-${m.identity.start_time}`} className="row procs sub">
                  <span className="name muted">{m.name}</span>
                  <span className="muted small">{Math.round(m.cpu_usage_percent)}%</span>
                  <span className="size">{bytes(m.memory_bytes)}</span>
                  <span />
                </div>
              ))}
          </div>
        ))}
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
