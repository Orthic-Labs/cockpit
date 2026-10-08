import { useCallback, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Badge, Button, SegmentedControl, Toggle } from "@rightkit/app-shell/react";

export interface Limit {
  label: string;
  /** 0–1, how much of the window is used. */
  usedFraction: number;
  /** Length of the window; absent when the provider did not say. */
  seconds?: number;
}

export interface Account {
  id: string;
  name: string;
  connected: boolean;
  usesKeychain: boolean;
  refusedAccess: boolean;
  needsRenewal: boolean;
  summary?: string;
  signInTitle?: string;
  signInExplanation: string;
  limits?: Limit[];
}

interface AppRef {
  id: string;
  name: string;
}

interface Conveniences {
  accessibility: boolean;
  inputMonitoring: boolean;
  wanted: boolean;
  fnStatus: "off" | "running" | "needsAccessibility" | "failed";
  fnDetail: string;
  active: boolean;
  runningApps: AppRef[];
  autoQuitApps: AppRef[];
  cutPasteResults: { name: string; ok: boolean; detail: string }[];
}

export interface NotchState {
  permissions?: Permission[];
  permissionErrors?: Record<string, string>;
  conveniences?: Conveniences;
  version: string;
  settings: Record<string, string | number | boolean>;
  options: Record<string, string[]>;
  displays: { id: string; name: string }[];
  accounts: Account[];
  providerOrder: string[];
  launcherStatus?: string | null;
  helper?: "notRegistered" | "needsReenable" | "requiresApproval" | "enabled" | "notFound";
  helperError?: string | null;
}

export interface Permission {
  id: string;
  title: string;
  why: string;
  status: "granted" | "needsApproval" | "off" | "unknown";
  required: boolean;
}

type Send = (command: Record<string, unknown>) => void;

export function useNotch() {
  const [state, setState] = useState<NotchState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    invoke<NotchState>("notch_state")
      .then((s) => {
        setState(s);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);
  useEffect(() => {
    const un = listen("notch-state", load).then((unlisten) => { load(); return unlisten; });
    const poll = setInterval(load, 3000);
    return () => {
      clearInterval(poll);
      void un.then((f) => f());
    };
  }, [load]);
  const send: Send = (command) => {
    void invoke("notch_command", { command }).catch((e) => setError(String(e)));
  };
  return { state, error, send };
}

/** "darkGlass" → "Dark glass". */
const label = (raw: string) =>
  raw.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/^./, (c) => c.toUpperCase()).replace(/ ([A-Z])/g, (_, c) => ` ${c.toLowerCase()}`);

export function Settings({ section, notch, onNavigate }: {
  section: string;
  notch: ReturnType<typeof useNotch>;
  onNavigate: (section: string) => void;
}) {
  const { state, error, send } = notch;
  if (!state) {
    return <div className="view muted">{error ?? "Reading the notch's settings…"}</div>;
  }
  const s = state.settings;
  const set = (key: string, value: unknown) => send({ command: "set", key, value });
  const bool = (key: string, text: string, note?: string) => (
    <Row label={text} note={note}>
      <Toggle checked={Boolean(s[key])} onChange={(v) => set(key, v)} label={text} />
    </Row>
  );
  const choice = (key: string, text: string, segmented = true) => {
    const options = state.options[key] ?? [];
    return (
      <Row label={text}>
        {segmented && options.length <= 4 ? (
          <SegmentedControl
            label={text}
            value={String(s[key])}
            options={options.map((o) => ({ value: o, label: label(o) }))}
            onChange={(v) => set(key, v)}
          />
        ) : (
          <select className="select" value={String(s[key])} onChange={(e) => set(key, e.target.value)}>
            {options.map((o) => (
              <option key={o} value={o}>{label(o)}</option>
            ))}
          </select>
        )}
      </Row>
    );
  };
  const slider = (key: string, text: string, min: number, max: number, step: number, percent = false) => (
    <Row label={text}>
      <span className="slider">
        <input type="range" min={min} max={max} step={step} value={Number(s[key])}
          onChange={(e) => set(key, Number(e.target.value))} />
        <span className="muted small num">
          {percent ? `${Math.round(Number(s[key]) * 100)}%` : `${Number(s[key]).toFixed(2)}×`}
        </span>
      </span>
    </Row>
  );

  return (
    <div className="view settings">
      {error && <div className="error">{error}</div>}

      {section === "permissions" && (
        <Group title="Permissions">
          {!state.permissions ? <div className="muted small">Waiting for permission status from Pulse notch…</div> : state.permissions.map((permission) => (
            <div key={permission.id} className="permission-row">
              <Row label={permission.title} note={permission.why}>
                <span className="permission-controls">
                  <span className={`permission-status ${permission.status}`}>
                    {permission.status === "granted" ? "Granted" : permission.status === "needsApproval" ? "Needs approval" : permission.status === "off" ? "Off" : "Unknown"}
                  </span>
                  <Button size="sm" variant="secondary" disabled={permission.status === "granted"}
                    onClick={() => send({ command: "permissionRequest", id: permission.id })}>
                    {permission.id === "automation" ? "Allow" : permission.id === "fullDiskAccess" || permission.status === "needsApproval" ? "Open Settings" : "Allow"}
                  </Button>
                </span>
              </Row>
              {state.permissionErrors?.[permission.id] && <div className="error">{state.permissionErrors[permission.id]}</div>}
            </div>
          ))}
        </Group>
      )}

      {section === "accounts" && <Accounts state={state} send={send} />}

      {section === "appearance" && (
        <>
          <Group title="Placement">
            {choice("notchEdge", "Edge")}
            {choice("notchScope", "Displays")}
            <Row label="Display">
              <select className="select" value={String(s.displayPreference)}
                onChange={(e) => set("displayPreference", e.target.value)}>
                <option value="followActiveWindow">Follow active window</option>
                {state.displays.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
              </select>
            </Row>
            {choice("notchVisibility", "Show")}
            {bool("foldsForFullScreen", "Fold for full-screen apps")}
            <Row label="Position"><Button size="sm" variant="secondary" onClick={() => send({ command: "resetPosition" })}>Reset position</Button></Row>
          </Group>
          <Group title="Size and surface">
            {choice("notchSize", "Size")}
            {bool("usesCustomNotchScale", "Custom size")}
            {s.usesCustomNotchScale ? slider("customNotchScale", "Scale", 0.5, 1.5, 0.05) : null}
            {choice("notchSurfaceStyle", "Surface")}
          </Group>
          <Group title="Rings">
            {bool("showsNotchReadings", "Show % under rings")}
            {choice("weeklyRing", "Second ring")}
            {bool("weeklyRingDashed", "Dashed second ring")}
            {bool("weeklyHeadline", "Weekly limit as main ring")}
            {bool("weeklyReading", "Show both readings")}
            {bool("claudeDailyPaceRing", "Claude daily pace ring")}
            {bool("showUsagePace", "Show usage pace")}
            {bool("showCodexExtraLimits", "Codex extra limits")}
            {choice("resetTimeFormat", "Reset time")}
          </Group>
          <Group title="Colour">
            {choice("accentColor", "Accent", false)}
            {choice("colorTransitionStyle", "Colour transition")}
            {slider("watchLimit", "Watch limit", 0.1, 0.95, 0.05, true)}
            {slider("criticalLimit", "Critical limit", 0.15, 1, 0.05, true)}
          </Group>
          <Group title="Language">{choice("language", "Language", false)}</Group>
        </>
      )}

      {section === "notifications" && (
        <>
          <Group title="Where">
            {choice("notificationChannel", "Channel")}
            {choice("peekDuration", "Open the notch for", false)}
          </Group>
          <Group title="Agent sessions">
            {bool("announceSessionEnd", "When a session finishes")}
            {bool("sessionEndSound", "Play a sound")}
          </Group>
          <Group title="Limits">
            {bool("announceUsageReset", "When a limit resets")}
            {bool("usageResetSound", "Play a sound on reset")}
            {bool("announceSessionLimitReached", "When the session limit is reached")}
            {bool("announceWeeklyLimitReached", "When the weekly limit is reached")}
            {bool("limitReachedSound", "Play a sound at a limit")}
          </Group>
          <Group title="Try them">
            <div className="buttons">
              <Button size="sm" variant="secondary" onClick={() => send({ command: "sendTestNotification" })}>Send a test</Button>
              <Button size="sm" variant="secondary" onClick={() => send({ command: "previewResetAlert" })}>Preview reset</Button>
              <Button size="sm" variant="secondary" onClick={() => send({ command: "previewSessionLimitAlert" })}>Preview session limit</Button>
              <Button size="sm" variant="secondary" onClick={() => send({ command: "previewWeeklyLimitAlert" })}>Preview weekly limit</Button>
            </div>
          </Group>
        </>
      )}

      {section === "general" && (
        <>
          <Group title="Startup">{bool("launchAtLogin", "Open Pulse at login")}</Group>
          <Group title="Uninstalling">
            <Row
              label="Uninstall without password"
              note="Lets Pulse move root-owned apps and their files to the Trash with no administrator password. Approve once in System Settings."
            >
              <Toggle
                checked={state.helper === "enabled" || state.helper === "requiresApproval"}
                onChange={(v) => send({ command: v ? "helperEnable" : "helperDisable" })}
                label="Uninstall without password"
              />
            </Row>
            <div className="muted small">
              {state.helper === "enabled"
                ? "On. Root-owned items go to the Trash without a password."
                : state.helper === "requiresApproval"
                  ? "Waiting for your approval in System Settings."
                  : state.helper === "notFound"
                    ? "This build of Pulse does not include the helper."
                    : state.helper === "needsReenable"
                      ? "Off after rename. Re-enable in Permissions, then approve Pulse in Login Items."
                      : "Off. Root-owned items ask for an administrator password in Finder."}
            </div>
            <Button size="sm" variant="ghost" onClick={() => onNavigate("permissions")}>Manage permissions</Button>
            {state.helperError ? <div className="error">{state.helperError}</div> : null}
          </Group>
          <Group title="Launcher">
            {bool("launcherEnabled", "Enable the launcher",
              "Search apps, files and Pulse commands, and calculate. Off by default.")}
            {s.launcherEnabled ? (
              <>
                {choice("launcherHotkey", "Shortcut")}
                <div className="muted small">
                  Command space only works after Spotlight's shortcut is turned off in System Settings.
                </div>
                {state.launcherStatus ? <div className="error">{state.launcherStatus}</div> : null}
              </>
            ) : null}
          </Group>
          <Group title="Keyboard">
            {bool("convFnCommand", "Fn works as Command", "Fn+C/V/X/A/Z/S/F/T/W; Fn+arrows move by word")}
            {Boolean(s.convFnCommand) && state.conveniences && (
              <div className="muted small">
                {state.conveniences.fnStatus === "running"
                  ? "Running."
                  : state.conveniences.fnStatus === "needsAccessibility"
                    ? "Needs Accessibility permission."
                    : `Failed: ${state.conveniences.fnDetail || "unknown reason"}.`}
              </div>
            )}
            {Boolean(s.convFnCommand) && <Button size="sm" variant="ghost" onClick={() => onNavigate("permissions")}>Manage permissions</Button>}
          </Group>
          <Group title="Readings">
            {bool("asksProviderOnLook", "Ask the provider every time you look",
              "Spends a request each time. Useful to check against a provider's own page.")}
            <Row label="Refresh"><Button size="sm" variant="secondary" onClick={() => send({ command: "refresh" })}>Refresh now</Button></Row>
          </Group>
          {state.conveniences && (
            <ConveniencesGroup c={state.conveniences} s={s} set={set} onNavigate={onNavigate} />
          )}
          <div className="muted small">Pulse notch {state.version} · built on Codenotch (MIT)</div>
        </>
      )}
    </div>
  );
}

function ConveniencesGroup({ c, s, set, onNavigate }: {
  c: Conveniences;
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
  onNavigate: (section: string) => void;
}) {
  const [pick, setPick] = useState("");
  const listed = c.autoQuitApps.map((a) => a.id);
  const addable = c.runningApps.filter((a) => !listed.includes(a.id));
  const toggle = (key: string, text: string, note: string) => (
    <Row label={text} note={note}>
      <Toggle checked={Boolean(s[key])} onChange={(v) => set(key, v)} label={text} />
    </Row>
  );
  return (
    <Group title="Conveniences">
      <Row
        label="Accessibility"
        note={c.accessibility
          ? "Allowed."
          : "Needs Accessibility permission. Until it is granted these stay off."}
      >
        <Button size="sm" variant="secondary" onClick={() => onNavigate("permissions")}>Permissions</Button>
      </Row>
      {toggle("convFinderCutPaste", "Cut and paste in Finder",
        "⌘X marks the selected items, ⌘V in a Finder window moves them there. Never overwrites; a name clash gets \" 2\".")}
      {Boolean(s.convFinderCutPaste) && c.accessibility && !c.inputMonitoring && (
        <div className="muted small">
          Key shortcuts may also need Input Monitoring (System Settings, Privacy and Security).
        </div>
      )}
      {c.cutPasteResults.length > 0 && (
        <div className="muted small">
          Last paste: {c.cutPasteResults.map((r) => `${r.name} — ${r.ok ? r.detail : `failed: ${r.detail}`}`).join("; ")}
        </div>
      )}
      {toggle("convWindowMaximizer", "Green button maximizes",
        "Fills the screen without a full-screen Space. Option-click keeps the usual behaviour.")}
      {toggle("convDockClickMinimize", "Dock click minimizes",
        "Clicking the Dock icon of the frontmost app minimizes its windows.")}
      {toggle("convDiskImageInstaller", "Offer to install and eject",
        "When a disk image holding one app mounts, the notch offers to copy it to Applications and eject the image. Needs no permission.")}
      {Boolean(s.convDiskImageInstaller) && toggle("convDiskImageTrashDownload", "Move the downloaded disk image to the Trash",
        "After a successful install, the .dmg you opened goes to the Trash. Off by default.")}
      {toggle("convAutoQuit", "Auto Quit",
        "Quits the apps below when their last window closes. Only apps you add; windows on other Spaces or minimized keep an app open.")}
      {Boolean(s.convAutoQuit) && (
        <>
          {c.autoQuitApps.map((a) => (
            <Row key={a.id} label={a.name} note={a.id}>
              <Button size="sm" variant="ghost"
                onClick={() => set("convAutoQuitApps", listed.filter((id) => id !== a.id))}>Remove</Button>
            </Row>
          ))}
          <Row label="Add a running app">
            <span className="buttons">
              <select className="select" value={pick} onChange={(e) => setPick(e.target.value)}>
                <option value="">Choose…</option>
                {addable.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
              </select>
              <Button size="sm" variant="secondary" disabled={!pick}
                onClick={() => { set("convAutoQuitApps", [...listed, pick]); setPick(""); }}>Add</Button>
            </span>
          </Row>
        </>
      )}
    </Group>
  );
}

function Accounts({ state, send }: { state: NotchState; send: Send }) {
  const order = state.providerOrder;
  const rank = (id: string) => {
    const i = order.indexOf(id);
    return i < 0 ? 999 : i;
  };
  const accounts = [...state.accounts].sort((a, b) => rank(a.id) - rank(b.id));
  const move = (id: string, by: number) => {
    const ids = accounts.map((a) => a.id);
    const i = ids.indexOf(id);
    const j = i + by;
    if (j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    send({ command: "order", value: ids });
  };
  return (
    <Group title="Logins Pulse reads (it never signs in itself)">
      {accounts.map((a, i) => (
        <div key={a.id} className="account">
          <div className="account-main">
            <div className="account-name">
              {a.name}
              {a.refusedAccess && <Badge tone="warn">Access refused</Badge>}
              {a.needsRenewal && <Badge tone="warn">Sign-in needed</Badge>}
            </div>
            <div className="muted small">{a.summary ?? a.signInExplanation}</div>
            <div className="buttons">
              {!a.summary && a.signInTitle && (
                <Button size="sm" variant="secondary" onClick={() => send({ command: "signIn", provider: a.id })}>{a.signInTitle}</Button>
              )}
              {a.usesKeychain && a.refusedAccess && (
                <Button size="sm" variant="secondary" onClick={() => send({ command: "allowAccess", provider: a.id })}>Allow access…</Button>
              )}
              {a.summary && (
                <Button size="sm" variant="ghost" onClick={() => send({ command: "signOut", provider: a.id })}
                  title={`Clears what Pulse read. You stay signed in to ${a.name.split(" ")[0]} itself.`}>Forget reading</Button>
              )}
            </div>
          </div>
          <div className="account-side">
            <span className="order">
              <button className="mini" disabled={i === 0} onClick={() => move(a.id, -1)} aria-label="Move up">↑</button>
              <button className="mini" disabled={i === accounts.length - 1} onClick={() => move(a.id, 1)} aria-label="Move down">↓</button>
            </span>
            <Toggle checked={a.connected} onChange={(v) => send({ command: "connect", provider: a.id, value: v })} label={`Show ${a.name}`} />
          </div>
        </div>
      ))}
    </Group>
  );
}

function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="group">
      <div className="section">{title}</div>
      <div className="group-body">{children}</div>
    </section>
  );
}

function Row({ label: text, note, children }: { label: string; note?: string; children: ReactNode }) {
  return (
    <div className="setting">
      <div>
        <div>{text}</div>
        {note && <div className="muted small">{note}</div>}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}
