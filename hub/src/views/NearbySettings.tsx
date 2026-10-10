import { useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Pencil, X } from "lucide-react";
import { Button, SegmentedControl, Toggle } from "@rightkit/app-shell/react";
import { isWindows } from "../api";

export type TrustChoice = "allow" | "ask" | "deny";

/** One device with a trust record (`trust.devices` in share-state.json). */
export interface TrustDevice {
  fingerprint: string;
  /** The owner's name for it; empty until given. */
  label: string;
  /** What the device calls itself (a claim). */
  alias: string;
  model: string | null;
  kind: string | null;
  state: TrustChoice;
  present: boolean;
  /** It has proved it holds the key behind its fingerprint. */
  verified: boolean;
  firstSeenMs: number;
  lastSeenMs: number;
  refused: number;
}

/** What the sharing service reports (hub/src-tauri/src/share.rs). */
export interface ShareState {
  running: boolean;
  error: string | null;
  alias?: string;
  saveDir?: string;
  devices: { fingerprint: string; alias: string; deviceType?: string | null; ip: string; state?: TrustChoice }[];
  /** Per-device Allow / Ask / Deny; new devices start as `defaultState`. */
  trust?: { defaultState: "ask" | "deny"; devices: TrustDevice[] };
  warnings: string[];
  /** macOS Local Network access: "granted" once a multicast send worked, "blocked" when macOS refuses. */
  localNetwork: "unknown" | "granted" | "blocked";
  /** The agent bridge (pulse_core::bridge): chats on this computer and on linked ones (over ssh). */
  bridge?: {
    enabled: boolean;
    active?: boolean;
    localChats?: number;
    links?: { device: string; ssh: string; chats: number | null; error: string | null; lastOkMs?: number | null; lastErrorMs?: number | null }[];
    activity?: { sentMs?: number; receivedMs?: number; sent?: number; received?: number; lastOutcome?: string | null };
    chats?: { name: string; kind: "claude" | "codex"; status: string; device: string; local: boolean; cwd?: string; updatedMs?: number | null; liveness?: string; unread?: number; evicted?: number }[];
    lastError?: string | null;
    device?: string;
  };
}


interface BridgeChange { target: string; path: string; action: string; note: string }
interface BridgeReport { dryRun: boolean; changes: BridgeChange[] }
const TARGET_NAMES: Record<string, string> = { claude: "Claude", codex: "Codex" };

/** Polls the sharing service; also refreshes when the service says something changed. */
export function useShareState(): ShareState | null {
  const [state, setState] = useState<ShareState | null>(null);
  useEffect(() => {
    let alive = true;
    const load = () => {
      invoke<ShareState>("share_state")
        .then((value) => { if (alive) setState(value); })
        .catch(() => { if (alive) setState(null); });
    };
    load();
    const timer = window.setInterval(load, 3000);
    const unlisten = listen("share-devices", load);
    return () => {
      alive = false;
      window.clearInterval(timer);
      void unlisten.then((f) => f());
    };
  }, []);
  return state;
}

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="ck-sgroup">
      <h2>{title}</h2>
      <div className="ck-card">{children}</div>
    </section>
  );
}

function Row({ label, note, children }: { label: string; note?: string; children: ReactNode }) {
  return (
    <div className="ck-set">
      <div className="ck-text">
        <strong>{label}</strong>
        {note && <div className="ck-sub">{note}</div>}
      </div>
      <div className="ck-ctl">{children}</div>
    </div>
  );
}

/** The "Agent bridge" block: chats talking to chats on nearby computers. */
function AgentBridge({ share }: { share: ShareState | null }) {
  const bridge = share?.bridge;
  const enabled = bridge?.enabled !== false;
  const [message, setMessage] = useState<string | null>(null);
  const [report, setReport] = useState<BridgeReport | null>(null);
  const [busy, setBusy] = useState(false);

  const toggle = (on: boolean) => {
    invoke<void>("bridge_set_enabled", { on }).catch((e) => setMessage(String(e)));
  };
  const install = () => {
    setBusy(true);
    setMessage(null);
    invoke<BridgeReport>("bridge_install_skill")
      .then((r) => setReport(r))
      .catch((e) => { setReport(null); setMessage(String(e)); })
      .finally(() => setBusy(false));
  };

  let status = "";
  if (!enabled) status = "Off.";
  else {
    const local = bridge?.localChats ?? 0;
    const links = (bridge?.links ?? []).map((l) => {
      if (l.error) {
        const since = l.lastErrorMs ? `offline ${ageText(l.lastErrorMs)}` : "offline";
        return `${l.device}: ${since}: ${l.error}`;
      }
      const ok = l.lastOkMs ? `, ok ${ageText(l.lastOkMs)}` : "";
      return `${l.chats ?? 0} on ${l.device}${ok}`;
    });
    status = [`${local} ${local === 1 ? "chat" : "chats"} here.`, ...links.map((t) => `${t}.`)].join(" ");
  }

  const activity = bridge?.activity;
  const outcome = activity?.sentMs
    ? `Last message: ${activity.lastOutcome ?? "delivered"} · ${ageText(activity.sentMs)} ago`
    : null;

  return (
    <>
      <Row
        label="Agent bridge"
        note="Lets Claude and Codex chats on this computer message chats on linked computers over ssh. Link one with: pulse bridge link <device> <ssh-host>"
      >
        <Toggle checked={enabled} onChange={toggle} label="Agent bridge" />
      </Row>
      {enabled && (
        <Row
          label="Pulse skill"
          note="Teaches Claude and Codex chats how to message chats on other computers. Restart a chat to see it."
        >
          <Button size="sm" variant="secondary" disabled={busy} onClick={install}>
            Install the Pulse skill for Claude and Codex
          </Button>
        </Row>
      )}
      {report && report.changes.map((c) => (
        <div key={c.target} className="ck-sub ck-foot">
          {TARGET_NAMES[c.target] ?? c.target}: {c.action}. {c.path}
        </div>
      ))}
      {message && <div className="error" role="alert">{message}</div>}
      <div className={bridge?.lastError ? "error" : "ck-sub ck-foot"} role="status">
        {bridge?.lastError ?? status}
      </div>
      {enabled && outcome && <div className="ck-sub ck-foot" role="status">{outcome}</div>}
      {enabled && (bridge?.chats?.length ?? 0) > 0 && <ChatList chats={bridge!.chats!} />}
    </>
  );
}

/** "42s", "5min", "3h" or "2d" since `ms`. */
function ageText(ms: number): string {
  const secs = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}min`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

type Chat = NonNullable<NonNullable<ShareState["bridge"]>["chats"]>[number];

function ChatRow({ c, kind }: { c: Chat; kind: "claude" | "codex" }) {
  const cwd = c.cwd ?? "";
  const folder = cwd.split(/[\\/]/).filter(Boolean).pop() ?? "";
  return (
    <li className="ck-chats-row">
      <span className={`ck-chats-kind ck-chats-kind-${kind}`}>{kind === "claude" ? "Claude" : "Codex"}</span>
      <span className="ck-chats-name" title={c.name}>{c.name}</span>
      <span className="ck-chats-folder" title={cwd || undefined}>{folder}</span>
      <span className="ck-chats-when">
        {c.updatedMs ? <span className="ck-chats-age">{ageText(c.updatedMs)} ago</span> : null}
        <span className={`ck-chats-status ck-chats-status-${c.status}`}>
          {c.status}{c.liveness ? ` · ${c.liveness}` : ""}
        </span>
      </span>
      {c.local && ((c.unread ?? 0) > 0 || (c.evicted ?? 0) > 0) && (
        <span className="ck-sub ck-chats-inbox">
          Inbox: {c.unread ?? 0} unread{(c.evicted ?? 0) > 0 ? `, ${c.evicted} evicted` : ""}
        </span>
      )}
    </li>
  );
}

/** Every chat Pulse can message, grouped by computer: Claude chats live; Codex threads active when written to in the last ten minutes. */
function ChatList({ chats }: { chats: Chat[] }) {
  const devices = Array.from(new Set(chats.map((c) => c.device)));
  const ordered = [...devices.filter((d) => chats.some((c) => c.device === d && c.local)), ...devices.filter((d) => !chats.some((c) => c.device === d && c.local))];
  return (
    <div className="ck-chats" aria-label="Chats Pulse can message">
      {ordered.map((device) => {
        const here = chats.filter((c) => c.device === device);
        const claude = here.filter((c) => c.kind === "claude");
        const codex = here.filter((c) => c.kind === "codex");
        return (
          <section key={device} className="ck-chats-device">
            <h3 className="ck-chats-head">{device} <span className="ck-sub">{claude.length} Claude · {codex.length} Codex</span></h3>
            <ul className="ck-chats-list">
              {claude.map((c, i) => <ChatRow key={`c-${i}-${c.name}`} c={c} kind="claude" />)}
              {codex.slice(0, 8).map((c, i) => <ChatRow key={`x-${i}-${c.name}`} c={c} kind="codex" />)}
              {codex.length > 8 && <li className="ck-sub ck-chats-more">+{codex.length - 8} more Codex threads (`pulse bridge peers` lists all)</li>}
            </ul>
          </section>
        );
      })}
    </div>
  );
}

const KIND_NAMES: Record<string, string> = {
  mobile: "Phone", desktop: "Computer", web: "Browser", headless: "Terminal", server: "Server",
};

const VERIFIED_NOTE = "Not verified: this app can't prove its identity. Allow applies only on this network address.";

function DeviceRow({ d, run }: { d: TrustDevice; run: (command: string, args: Record<string, unknown>) => void }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(d.label);
  const cancelled = useRef(false);
  useEffect(() => { if (!editing) setDraft(d.label); }, [d.label, editing]);
  const name = d.label || d.alias || "Unnamed device";
  const commit = () => {
    if (cancelled.current) { cancelled.current = false; return; }
    const next = draft.trim();
    if (next !== d.label) run("share_trust_label", { fingerprint: d.fingerprint, label: next });
    setEditing(false);
  };
  const what = [d.kind ? KIND_NAMES[d.kind] ?? "Device" : null, d.model].filter(Boolean).join(" · ");
  const when = d.present ? "nearby now" : `last seen ${ageText(d.lastSeenMs)} ago`;
  const refused = d.refused > 0 ? `refused ${d.refused} ${d.refused === 1 ? "time" : "times"}` : null;
  return (
    <div className="ck-device" data-device={d.fingerprint}>
      <div className="ck-text">
        <div className="ck-account-name">
          {editing ? (
            <input
              className="ck-input ck-claude-edit"
              autoFocus
              value={draft}
              placeholder={d.alias || "Name this device"}
              onChange={(e) => setDraft(e.target.value)}
              onFocus={(e) => e.target.select()}
              onBlur={commit}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                if (e.key === "Escape") { cancelled.current = true; setDraft(d.label); setEditing(false); }
              }}
              aria-label={`Name for ${name}`}
              title="Enter saves, Escape cancels, empty restores the device's own name"
            />
          ) : (
            <button type="button" className="ck-claude-name" onClick={() => setEditing(true)}
              aria-label={`Rename ${name}`} title="Rename this device">
              <span className="ck-claude-name-text">{name}</span>
              <Pencil size={12} strokeWidth={1.75} aria-hidden="true" />
            </button>
          )}
          <span className={`ck-status ck-status-${d.verified ? "granted" : "unknown"}`} title={d.verified ? "This device proved its identity." : VERIFIED_NOTE}>
            {d.verified ? "verified" : "not verified"}
          </span>
        </div>
        <div className="ck-sub">
          {[what, when, refused].filter(Boolean).join(" · ")}
        </div>
        {!d.verified && d.state === "allow" && <div className="ck-sub">{VERIFIED_NOTE}</div>}
      </div>
      <div className="ck-ctl">
        <span className="ck-ctls">
          <SegmentedControl
            label={`Trust for ${name}`}
            value={d.state}
            options={[{ value: "allow", label: "Allow" }, { value: "ask", label: "Ask" }, { value: "deny", label: "Deny" }]}
            onChange={(v) => run("share_trust_set", { fingerprint: d.fingerprint, state: v })}
          />
          {!d.present && (
            <button type="button" className="ck-forget" aria-label={`Forget ${name}`}
              onClick={() => run("share_trust_forget", { fingerprint: d.fingerprint })}
              title="Removes this device from the list; it returns when it is next seen.">
              <X size={14} strokeWidth={1.75} aria-hidden="true" />
            </button>
          )}
        </span>
      </div>
    </div>
  );
}

/** Every device seen on the network or in a request, with Allow / Ask / Deny. */
function DevicesGroup({ share }: { share: ShareState }) {
  const trust = share.trust;
  const [message, setMessage] = useState<string | null>(null);
  const run = (command: string, args: Record<string, unknown>) => {
    setMessage(null);
    invoke<void>(command, args).catch((e) => setMessage(String(e)));
  };
  if (!trust) return null;
  const devices = [...trust.devices].sort((a, b) =>
    Number(b.present) - Number(a.present) || b.lastSeenMs - a.lastSeenMs);
  return (
    <Group title="Devices">
      <Row
        label="New devices"
        note="What a device gets the first time it is seen. Allow sends and receives with no question; Ask asks every time; Deny refuses."
      >
        <SegmentedControl
          label="New devices"
          value={trust.defaultState}
          options={[{ value: "ask", label: "Ask" }, { value: "deny", label: "Deny" }]}
          onChange={(v) => run("share_trust_default", { state: v })}
        />
      </Row>
      {devices.length === 0 && <div className="ck-sub ck-foot">No devices seen yet.</div>}
      {devices.map((d) => <DeviceRow key={d.fingerprint} d={d} run={run} />)}
      {message && <div className="error" role="alert">{message}</div>}
    </Group>
  );
}

/** General > Nearby sharing: send and receive files with LocalSend and other Pulse Macs. */
export function NearbyGroup({ s, set }: {
  s: Record<string, unknown>;
  set: (key: string, value: unknown) => void;
}) {
  const share = useShareState();
  const enabled = s.nearbyEnabled !== false;
  const stored = typeof s.nearbyAlias === "string" ? s.nearbyAlias : "";
  const [alias, setAlias] = useState(stored);
  useEffect(() => setAlias(stored), [stored]);
  const folder = typeof s.nearbySaveFolder === "string" ? s.nearbySaveFolder : "";

  const commitAlias = () => {
    if (alias.trim() !== stored.trim()) set("nearbyAlias", alias.trim());
  };
  const chooseFolder = () => {
    invoke<string | null>("file_choose_folder")
      .then((picked) => { if (picked) set("nearbySaveFolder", picked); })
      .catch(() => undefined);
  };

  let status = "";
  if (!enabled) status = "Off.";
  else if (!share) status = "Starting…";
  else if (share.error) status = share.error;
  else if (share.running) {
    const count = share.devices.length;
    status = `Visible as "${share.alias ?? ""}". ${count === 0 ? "No devices nearby." : `${count} nearby.`}`;
  }

  return (
    <>
    <Group title="Nearby sharing">
      <Row
        label="Send and receive files nearby"
        note={`Works with the LocalSend app on iPhone, Android, ${isWindows ? "Mac" : "Windows"} and Linux. Pulse can be found on your network while this is on.`}
      >
        <Toggle checked={enabled} onChange={(v) => set("nearbyEnabled", v)} label="Nearby sharing" />
      </Row>
      {enabled && (
        <>
          <Row label="Device name" note={isWindows ? "How other devices list this PC. Empty uses the PC's name." : "How other devices list this Mac. Empty uses the Mac's name."}>
            <input
              className="ck-input ck-input-name"
              value={alias}
              placeholder={share?.alias ?? (isWindows ? "This PC" : "This Mac")}
              onChange={(e) => setAlias(e.target.value)}
              onBlur={commitAlias}
              onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
              aria-label="Device name"
            />
          </Row>
          <Row label="Save received files to" note={folder || "Downloads"}>
            <span className="ck-ctls">
              <Button size="sm" variant="secondary" onClick={chooseFolder}>Choose…</Button>
              {folder && <Button size="sm" variant="ghost" onClick={() => set("nearbySaveFolder", "")}>Use Downloads</Button>}
            </span>
          </Row>
          <AgentBridge share={share} />
        </>
      )}
      <div className={share?.error ? "error" : "ck-sub ck-foot"} role={share?.error ? "alert" : "status"}>{status}</div>
      {enabled && share?.warnings.map((w) => <div key={w} className="ck-sub ck-foot">{w}</div>)}
    </Group>
    {enabled && share?.running && <DevicesGroup share={share} />}
    </>
  );
}

/**
 * The Permissions row for macOS Local Network access. macOS has no way to ask
 * for its state, so it is read from whether multicast sends are refused.
 */
export function LocalNetworkRow({ enabled }: { enabled: boolean }) {
  const share = useShareState();
  if (!enabled) return null;
  const blocked = share?.localNetwork === "blocked";
  const granted = share?.localNetwork === "granted";
  const status = blocked ? "needsApproval" : granted ? "granted" : "unknown";
  const text = blocked ? "Needs approval" : granted ? "Granted" : "Checking…";
  return (
    <div className="ck-permrow">
      <div className="ck-set">
        <div className="ck-text">
          <strong>{isWindows ? "Windows Firewall" : "Local network"}</strong>
          <div className="ck-sub">
            {isWindows
              ? "Lets Pulse find and talk to nearby devices for sharing files. Windows Firewall must allow Pulse on private networks."
              : "Lets Pulse find and talk to nearby devices for sharing files. macOS asks the first time a device connects."}
          </div>
        </div>
        <div className="ck-ctl">
          <span className="ck-ctls">
            <span className={`ck-status ck-status-${status}`}>{text}</span>
            <Button size="sm" variant="secondary" disabled={granted}
              onClick={() => invoke<void>("open_local_network_settings")}>{isWindows ? "Open Firewall" : "Open Settings"}</Button>
          </span>
        </div>
      </div>
    </div>
  );
}
