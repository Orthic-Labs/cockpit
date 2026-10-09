import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button } from "@rightkit/app-shell/react";

interface AccountInfo { id: string; active: boolean; sessions: number; syncable: boolean }
interface BackupInfo { ts: string; created_ms: number; status: string; files: number }
interface State {
  supported: boolean;
  reason?: string;
  running?: boolean;
  accounts?: AccountInfo[];
  backups?: BackupInfo[];
}
type Outcome =
  | { status: "still_open" }
  | { status: "done"; accounts: number; sessions: number; files_changed: number; conflicts: number; backup: string | null; reopened: boolean };

const when = (ms: number) => new Date(ms).toLocaleString();
const message = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "Something went wrong.");
const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/**
 * Claude Desktop's Code sessions, kept the same across every account on this
 * Mac. One button, pressed after signing in to a new account: quit Claude if
 * open, sync every account's sessions with a backup, reopen Claude.
 */
export function ClaudeSync() {
  const [state, setState] = useState<State | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [line, setLine] = useState<{ text: string; undo?: string; retry?: () => void } | null>(null);

  const load = useCallback(() => {
    invoke<State>("claude_sync_state").then(setState).catch((e) => setLine({ text: message(e) }));
  }, []);
  useEffect(() => {
    load();
    const timer = window.setInterval(load, 5000);
    return () => window.clearInterval(timer);
  }, [load]);

  if (!state || !state.supported) {
    return state ? <div className="ck-sub">Session sync: {state.reason}</div> : null;
  }

  const restartSync = () => {
    setBusy("Closing Claude, syncing sessions…");
    setLine(null);
    invoke<Outcome>("claude_restart_sync")
      .then((o) => {
        if (o.status === "still_open") {
          setLine({ text: "Claude is still open", retry: restartSync });
        } else {
          setLine({
            text: `Synced ${plural(o.sessions, "session")} across ${plural(o.accounts, "account")}` +
              (o.conflicts ? `, ${plural(o.conflicts, "conflict")} left as they were` : "") +
              (o.reopened ? "" : ". Reopen Claude yourself"),
            undo: o.backup ?? undefined,
          });
        }
      })
      .catch((e) => setLine({ text: message(e) }))
      .finally(() => { setBusy(null); load(); });
  };
  const restore = (ts: string) => {
    setBusy("Closing Claude, restoring…");
    setLine(null);
    invoke<{ status: string }>("claude_restore", { ts, force: false })
      .then((r) => setLine(r.status === "still_open"
        ? { text: "Claude is still open", retry: () => restore(ts) }
        : { text: "Restored the earlier session lists" }))
      .catch((e) => setLine({ text: message(e) }))
      .finally(() => { setBusy(null); load(); });
  };

  const accounts = state.accounts ?? [];
  const backups = state.backups ?? [];

  return (
    <div className="ck-claude-sync">
      <div className="ck-line">
        <Button size="sm" variant="secondary" disabled={busy !== null} onClick={restartSync}>
          Restart Claude and sync chats
        </Button>
        {busy && <span className="ck-sub" role="status">{busy}</span>}
        {line && (
          <span className="ck-sub" role="status">
            {line.text}
            {line.undo && <> <Button size="sm" variant="ghost" onClick={() => restore(line.undo!)}>Undo</Button></>}
            {line.retry && <> <Button size="sm" variant="ghost" onClick={line.retry}>Retry</Button></>}
          </span>
        )}
      </div>
      <div className="ck-sub">
        Press it after signing in to a new account. Quits Claude if it is open, merges Code sessions across every account (with a backup), and reopens it.
      </div>
      <div className="ck-subhead">Accounts found on this Mac</div>
      {accounts.map((i) => (
        <div key={i.id} className="ck-line">
          <strong>{i.id.slice(0, 8)}</strong>
          {i.active && <Badge tone="ok">Signed in</Badge>}
          <span className="ck-sub">{i.syncable ? `${i.sessions} sessions` : "no session folder yet"}</span>
        </div>
      ))}
      <div className="ck-subhead">Backups (newest 10 kept)</div>
      {backups.length === 0 && <div className="ck-sub">None yet.</div>}
      {backups.map((b) => (
        <div key={b.ts} className="ck-line">
          <span>{when(b.created_ms)}</span>
          <span className="ck-sub">{b.files} files · {b.status}</span>
          <Button size="sm" variant="ghost" disabled={busy !== null || b.status === "restored"} onClick={() => restore(b.ts)}>Restore</Button>
        </div>
      ))}
    </div>
  );
}
