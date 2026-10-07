import { useCallback, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Badge, Button, SegmentedControl, Toggle } from "@rightkit/app-shell/react";

interface Account {
  id: string;
  name: string;
  connected: boolean;
  usesKeychain: boolean;
  refusedAccess: boolean;
  needsRenewal: boolean;
  summary?: string;
  signInTitle?: string;
  signInExplanation: string;
}

interface NotchState {
  version: string;
  settings: Record<string, string | number | boolean>;
  options: Record<string, string[]>;
  displays: { id: string; name: string }[];
  accounts: Account[];
  providerOrder: string[];
}

type Send = (command: Record<string, unknown>) => void;

function useNotch() {
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
    load();
    const un = listen("notch-state", load);
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

export function Settings({ section }: { section: string }) {
  const { state, error, send } = useNotch();
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
          <Group title="Startup">{bool("launchAtLogin", "Open Cockpit at login")}</Group>
          <Group title="Readings">
            {bool("asksProviderOnLook", "Ask the provider every time you look",
              "Spends a request each time. Useful to check against a provider's own page.")}
            <Row label="Refresh"><Button size="sm" variant="secondary" onClick={() => send({ command: "refresh" })}>Refresh now</Button></Row>
          </Group>
          <div className="muted small">Cockpit notch {state.version} · built on Codenotch (MIT)</div>
        </>
      )}
    </div>
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
    <Group title="Logins Cockpit reads (it never signs in itself)">
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
                  title={`Clears what Cockpit read. You stay signed in to ${a.name.split(" ")[0]} itself.`}>Forget reading</Button>
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
