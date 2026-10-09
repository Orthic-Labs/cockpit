import { useEffect, useState, type ReactNode } from "react";
import { Button } from "@rightkit/app-shell/react";

/** The launcher's user-made items. Stored by the notch as one JSON string. */
export interface LauncherConfig {
  pinnedApps: string[];
  fileFolders: string[];
  appHotkeys: { path: string; hotkey: string }[];
  quicklinks: { name: string; keyword: string; template: string }[];
  snippets: { name: string; keyword: string; body: string }[];
  commands: { name: string; keyword: string; command: string; hotkey: string }[];
}

const blank = (): LauncherConfig => ({
  pinnedApps: [],
  fileFolders: [],
  appHotkeys: [],
  quicklinks: [],
  snippets: [],
  commands: [],
});

export function parseLauncherConfig(raw: unknown): LauncherConfig {
  if (typeof raw !== "string" || raw === "") return blank();
  try {
    const parsed = JSON.parse(raw) as Partial<LauncherConfig>;
    return {
      pinnedApps: parsed.pinnedApps ?? [],
      fileFolders: parsed.fileFolders ?? [],
      appHotkeys: parsed.appHotkeys ?? [],
      quicklinks: parsed.quicklinks ?? [],
      snippets: parsed.snippets ?? [],
      commands: parsed.commands ?? [],
    };
  } catch {
    return blank();
  }
}

/** A line that adds one value to a list. */
function AddLine({ placeholder, onAdd }: { placeholder: string; onAdd: (value: string) => void }) {
  const [text, setText] = useState("");
  const add = () => {
    const value = text.trim();
    if (!value) return;
    onAdd(value);
    setText("");
  };
  return (
    <div className="ck-line">
      <input
        className="ck-input"
        aria-label={placeholder}
        value={text}
        placeholder={placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => { if (e.key === "Enter") add(); }}
      />
      <Button size="sm" variant="secondary" disabled={!text.trim()} onClick={add}>Add</Button>
    </div>
  );
}

function Section({ title, note, children }: { title: string; note?: string; children: ReactNode }) {
  return (
    <div className="ck-set ck-stack">
      <div className="ck-text">
        <strong>{title}</strong>
        {note && <div className="ck-sub">{note}</div>}
      </div>
      <div className="ck-list">{children}</div>
    </div>
  );
}

/**
 * Edits the launcher's lists: folders for file search, pinned apps, app and
 * command hotkeys, quicklinks, snippets and custom commands. Changes are held
 * until Save, then sent as one `launcherConfig` value.
 */
export function LauncherLists({ raw, onSave }: { raw: unknown; onSave: (next: LauncherConfig) => void }) {
  const key = typeof raw === "string" ? raw : "";
  const [draft, setDraft] = useState<LauncherConfig>(() => parseLauncherConfig(key));
  const [dirty, setDirty] = useState(false);

  // The notch's copy changed (or our save landed): show it.
  useEffect(() => {
    setDraft(parseLauncherConfig(key));
    setDirty(false);
  }, [key]);

  const edit = (next: LauncherConfig) => {
    setDraft(next);
    setDirty(true);
  };
  const input = (value: string, placeholder: string, onChange: (value: string) => void, flex = 1) => (
    <input
      className="ck-input"
      aria-label={placeholder}
      value={value}
      placeholder={placeholder}
      style={{ flex: `${flex} 1 ${flex * 110}px` }}
      onChange={(e) => onChange(e.target.value)}
    />
  );
  const removeAt = <T,>(list: T[], index: number) => list.filter((_, i) => i !== index);
  const hotkeyNote = "Needs a modifier: for example opt+1, cmd+shift+k or ctrl+opt+d.";

  return (
    <>
      <Section title="Folders for file search" note="Spotlight searches file names only inside these folders. Empty turns file search off.">
        {draft.fileFolders.map((path, i) => (
          <div key={`folder-${i}`} className="ck-line">
            <span className="ck-sub ck-path">{path}</span>
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, fileFolders: removeAt(draft.fileFolders, i) })}>Remove</Button>
          </div>
        ))}
        <AddLine placeholder="/Users/you/Documents" onAdd={(v) => edit({ ...draft, fileFolders: [...draft.fileFolders, v] })} />
      </Section>

      <Section title="Pinned apps" note="Pin an app in the launcher with Command-P. Pinned apps show first when nothing is typed.">
        {draft.pinnedApps.map((path, i) => (
          <div key={`pin-${i}`} className="ck-line">
            <span className="ck-sub ck-path">{path}</span>
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, pinnedApps: removeAt(draft.pinnedApps, i) })}>Remove</Button>
          </div>
        ))}
        <AddLine placeholder="/Applications/Safari.app" onAdd={(v) => edit({ ...draft, pinnedApps: [...draft.pinnedApps, v] })} />
      </Section>

      <Section title="App hotkeys" note={`Press to show the app, or hide it when it is in front. ${hotkeyNote}`}>
        {draft.appHotkeys.map((item, i) => (
          <div key={`app-hotkey-${i}`} className="ck-line">
            {input(item.path, "/Applications/Safari.app", (v) => {
              const next = [...draft.appHotkeys];
              next[i] = { ...item, path: v };
              edit({ ...draft, appHotkeys: next });
            }, 2)}
            {input(item.hotkey, "opt+1", (v) => {
              const next = [...draft.appHotkeys];
              next[i] = { ...item, hotkey: v };
              edit({ ...draft, appHotkeys: next });
            })}
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, appHotkeys: removeAt(draft.appHotkeys, i) })}>Remove</Button>
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, appHotkeys: [...draft.appHotkeys, { path: "", hotkey: "" }] })}>Add an app hotkey</Button>
      </Section>

      <Section title="Quicklinks" note="{query} is replaced by the text after the keyword. {clipboard} and {date} are also filled in.">
        {draft.quicklinks.map((item, i) => (
          <div key={`quicklink-${i}`} className="ck-line">
            {input(item.name, "Name", (v) => {
              const next = [...draft.quicklinks];
              next[i] = { ...item, name: v };
              edit({ ...draft, quicklinks: next });
            }, 1)}
            {input(item.keyword, "Keyword", (v) => {
              const next = [...draft.quicklinks];
              next[i] = { ...item, keyword: v };
              edit({ ...draft, quicklinks: next });
            }, 1)}
            {input(item.template, "https://github.com/search?q={query}", (v) => {
              const next = [...draft.quicklinks];
              next[i] = { ...item, template: v };
              edit({ ...draft, quicklinks: next });
            }, 3)}
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, quicklinks: removeAt(draft.quicklinks, i) })}>Remove</Button>
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, quicklinks: [...draft.quicklinks, { name: "", keyword: "", template: "" }] })}>Add a quicklink</Button>
      </Section>

      <Section title="Snippets" note="Markdown text pasted into the app you were using. {date}, {clipboard} and {argument} are filled in.">
        {draft.snippets.map((item, i) => (
          <div key={`snippet-${i}`} className="ck-list">
            <div className="ck-line">
              {input(item.name, "Name", (v) => {
                const next = [...draft.snippets];
                next[i] = { ...item, name: v };
                edit({ ...draft, snippets: next });
              })}
              {input(item.keyword, "Keyword", (v) => {
                const next = [...draft.snippets];
                next[i] = { ...item, keyword: v };
                edit({ ...draft, snippets: next });
              })}
              <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, snippets: removeAt(draft.snippets, i) })}>Remove</Button>
            </div>
            <textarea
              className="ck-input"
              aria-label="Snippet text"
              rows={4}
              value={item.body}
              placeholder={"Thanks,\n{clipboard}"}
              onChange={(e) => {
                const next = [...draft.snippets];
                next[i] = { ...item, body: e.target.value };
                edit({ ...draft, snippets: next });
              }}
            />
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, snippets: [...draft.snippets, { name: "", keyword: "", body: "" }] })}>Add a snippet</Button>
      </Section>

      <Section title="Custom commands" note="Run with /bin/zsh -lc; the output shows in the launcher. Optional hotkey runs it from anywhere.">
        {draft.commands.map((item, i) => (
          <div key={`command-${i}`} className="ck-line">
            {input(item.name, "Name", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, name: v };
              edit({ ...draft, commands: next });
            })}
            {input(item.keyword, "Keyword", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, keyword: v };
              edit({ ...draft, commands: next });
            })}
            {input(item.command, "df -h /", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, command: v };
              edit({ ...draft, commands: next });
            }, 2)}
            {input(item.hotkey, "hotkey", (v) => {
              const next = [...draft.commands];
              next[i] = { ...item, hotkey: v };
              edit({ ...draft, commands: next });
            })}
            <Button size="sm" variant="ghost" onClick={() => edit({ ...draft, commands: removeAt(draft.commands, i) })}>Remove</Button>
          </div>
        ))}
        <Button size="sm" variant="secondary"
          onClick={() => edit({ ...draft, commands: [...draft.commands, { name: "", keyword: "", command: "", hotkey: "" }] })}>Add a command</Button>
      </Section>

      <div className="ck-set ck-savebar">
        <span className="ck-sub" role="status">{dirty ? "Not saved yet." : "Launcher lists are saved."}</span>
        <Button size="sm" variant="secondary" disabled={!dirty} onClick={() => onSave(draft)}>Save launcher lists</Button>
      </div>
    </>
  );
}
