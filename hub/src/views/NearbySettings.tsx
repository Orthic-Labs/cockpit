import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Button, Toggle } from "@rightkit/app-shell/react";
import { isWindows } from "../api";

/** What the sharing service reports (hub/src-tauri/src/share.rs). */
export interface ShareState {
  running: boolean;
  error: string | null;
  alias?: string;
  saveDir?: string;
  devices: { fingerprint: string; alias: string; deviceType?: string | null; ip: string }[];
  warnings: string[];
  /** macOS Local Network access: "granted" once a multicast send worked, "blocked" when macOS refuses. */
  localNetwork: "unknown" | "granted" | "blocked";
  /** The agent bridge (pulse_core::bridge): chats on this computer and on linked ones (over ssh). */
  bridge?: {
    enabled: boolean;
    active?: boolean;
    localChats?: number;
    links?: { device: string; ssh: string; chats: number | null; error: string | null }[];
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
    const links = (bridge?.links ?? []).map((l) =>
      l.error ? `${l.device}: ${l.error}` : `${l.chats ?? 0} on ${l.device}`);
    status = [`${local} ${local === 1 ? "chat" : "chats"} here.`, ...links.map((t) => `${t}.`)].join(" ");
  }

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
    </>
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
    <Group title="Nearby sharing">
      <Row
        label="Send and receive files nearby"
        note="Works with the LocalSend app on iPhone, Android, Windows and Linux. Pulse can be found on your network while this is on."
      >
        <Toggle checked={enabled} onChange={(v) => set("nearbyEnabled", v)} label="Nearby sharing" />
      </Row>
      {enabled && (
        <>
          <Row label="Device name" note="How other devices list this Mac. Empty uses the Mac's name.">
            <input
              className="ck-input ck-input-name"
              value={alias}
              placeholder={share?.alias ?? "Mac (Pulse)"}
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
          <Row
            label="Accept from known devices automatically"
            note="Devices you have accepted before skip the question. Anyone else always asks."
          >
            <Toggle checked={s.nearbyAcceptKnown === true} onChange={(v) => set("nearbyAcceptKnown", v)}
              label="Accept from known devices automatically" />
          </Row>
          <AgentBridge share={share} />
        </>
      )}
      <div className={share?.error ? "error" : "ck-sub ck-foot"} role={share?.error ? "alert" : "status"}>{status}</div>
      {enabled && share?.warnings.map((w) => <div key={w} className="ck-sub ck-foot">{w}</div>)}
    </Group>
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
