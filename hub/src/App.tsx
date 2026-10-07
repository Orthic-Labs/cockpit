import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AppShell } from "@rightkit/app-shell/react";
import { Storage } from "./views/Storage";
import { Monitor } from "./views/Monitor";
import { Settings } from "./views/Settings";

const icon = (glyph: string) => <span className="nav-icon">{glyph}</span>;

const groups = [
  {
    items: [
      { id: "storage", label: "Storage", icon: icon("◧"), keywords: ["disk", "files"] },
      { id: "monitor", label: "Monitor", icon: icon("◉"), keywords: ["cpu", "memory"] },
    ],
  },
  {
    title: "Settings",
    items: [
      { id: "accounts", label: "Accounts", icon: icon("◎"), keywords: ["claude", "codex", "sign in"] },
      { id: "appearance", label: "Appearance", icon: icon("◐"), keywords: ["notch", "size", "edge"] },
      { id: "notifications", label: "Notifications", icon: icon("◔"), keywords: ["alerts", "sound"] },
      { id: "general", label: "General", icon: icon("⚙"), keywords: ["login", "startup"] },
    ],
  },
];

const titles: Record<string, string> = {
  storage: "Storage",
  monitor: "Monitor",
  accounts: "Accounts",
  appearance: "Appearance",
  notifications: "Notifications",
  general: "General",
};

const settingsIds = new Set(["accounts", "appearance", "notifications", "general"]);

/** `--section settings` (from the notch's settings handle) opens Accounts. */
const resolve = (section: string | null | undefined) =>
  !section ? null : section === "settings" ? "accounts" : titles[section] ? section : null;

export function App() {
  const [active, setActive] = useState("storage");

  useEffect(() => {
    invoke<string | null>("initial_section").then((s) => {
      const id = resolve(s);
      if (id) setActive(id);
    });
    const un = listen<string>("show-section", (e) => {
      const id = resolve(e.payload);
      if (id) setActive(id);
    });
    return () => void un.then((f) => f());
  }, []);

  return (
    <AppShell
      groups={groups}
      activeId={active}
      onNavigate={setActive}
      title={titles[active]}
      searchMode="none"
      sidebarWidth={170}
      wordmark={<span className="wordmark">Cockpit</span>}
      onOpenSettings={() => setActive("accounts")}
      settingsActive={settingsIds.has(active)}
    >
      {active === "storage" ? <Storage /> : active === "monitor" ? <Monitor /> : <Settings section={active} />}
    </AppShell>
  );
}
