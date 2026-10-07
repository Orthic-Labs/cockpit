import { useEffect, useMemo, useState } from "react";
import { api, bytes, tone, type Folder, type Row, type Status } from "../api";

export function Storage() {
  const [status, setStatus] = useState<Status | null>(null);
  const [folder, setFolder] = useState<Folder | null>(null);
  const [results, setResults] = useState<Row[] | null>(null);
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
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

  const scan = () => run(async () => setFolder(await api.scan()));
  const open = (path: string) => run(async () => setFolder(await api.children(path)));

  useEffect(() => {
    api.status().then(setStatus).catch(() => {});
    scan();
  }, []);

  useEffect(() => {
    if (!folder || query.trim().length < 2) {
      setResults(null);
      return;
    }
    const handle = setTimeout(() => {
      api.search(query.trim()).then(setResults).catch((e) => setError(String(e)));
    }, 200);
    return () => clearTimeout(handle);
  }, [query, folder]);

  const startup = status?.disks.find((d) => d.mount_point === "/") ?? status?.disks[0];
  const crumbs = useMemo(() => {
    if (!folder) return [];
    const parts: { name: string; path: string }[] = [];
    let path = folder.path;
    while (path && path.length >= folder.root.length) {
      parts.unshift({ name: path === folder.root ? "~" : path.split("/").pop() || path, path });
      if (path === folder.root) break;
      path = path.slice(0, path.lastIndexOf("/")) || "/";
    }
    return parts;
  }, [folder]);

  const rows = results ?? folder?.rows ?? [];
  const largest = Math.max(1, ...rows.map((r) => r.bytes));

  return (
    <div className="view">
      {startup && startup.total_bytes ? (
        <div className="volume">
          <div className="volume-head">
            <span className="strong">Macintosh HD</span>
            <span className="muted">
              {bytes(startup.available_bytes)} free of {bytes(startup.total_bytes)}
            </span>
          </div>
          <Bar
            fraction={1 - (startup.available_bytes ?? 0) / startup.total_bytes}
            height={6}
          />
        </div>
      ) : null}

      <div className="toolbar">
        <input
          className="search"
          placeholder="Search files"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <button className="btn" onClick={scan} disabled={busy}>
          {busy ? "Scanning…" : "Rescan"}
        </button>
      </div>

      {results ? (
        <div className="muted small">{results.length} matches</div>
      ) : (
        <div className="crumbs">
          {crumbs.map((c, i) => (
            <span key={c.path}>
              {i > 0 && <span className="muted"> / </span>}
              <button className="crumb" onClick={() => open(c.path)} disabled={busy}>
                {c.name}
              </button>
            </span>
          ))}
        </div>
      )}

      {folder?.incomplete && !results && (
        <div className="muted small">
          Partial scan: the file limit was reached or some folders couldn't be read, so sizes may be low.
        </div>
      )}
      {error && <div className="error">{error}</div>}
      {!folder && busy && <div className="muted">Scanning your home folder…</div>}

      <div className="list">
        {rows.map((r) => (
          <div
            key={r.path}
            className={`row${r.is_dir && !results ? " clickable" : ""}`}
            onClick={() => (r.is_dir && !results ? open(r.path) : undefined)}
            onDoubleClick={() => api.reveal(r.path)}
            title={r.path}
          >
            <span className="name">
              <span className="icon">{r.is_dir ? "▸" : "·"}</span>
              {results ? r.path.replace(folder?.root ?? "", "~") : r.name}
            </span>
            <Bar fraction={r.bytes / largest} color="var(--accent)" />
            <span className="size">{bytes(r.bytes)}</span>
          </div>
        ))}
      </div>
    </div>
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
