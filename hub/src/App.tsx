import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AppShell } from "@rightkit/app-shell/react";
import { Storage } from "./views/Storage";
import { Monitor } from "./views/Monitor";
import { Apps } from "./views/Apps";
import { Cleanup } from "./views/Cleanup";
import { Settings } from "./views/Settings";
import { Bell, CircleUser, Gauge, HardDrive, LayoutGrid, Palette, Settings2, Sparkles } from "lucide-react";

const icon = (Icon: typeof HardDrive) => <Icon size={15} strokeWidth={1.75} />;

const groups = [
  {
    items: [
      { id: "storage", label: "Storage", icon: icon(HardDrive), keywords: ["disk", "files"] },
      { id: "cleanup", label: "Cleanup", icon: icon(Sparkles), keywords: ["trash", "cache", "clean"] },
      { id: "monitor", label: "Monitor", icon: icon(Gauge), keywords: ["cpu", "memory"] },
      { id: "apps", label: "Apps", icon: icon(LayoutGrid), keywords: ["uninstall", "applications", "leftovers"] },
    ],
  },
  {
    title: "Settings",
    items: [
      { id: "accounts", label: "Accounts", icon: icon(CircleUser), keywords: ["claude", "codex", "sign in"] },
      { id: "appearance", label: "Appearance", icon: icon(Palette), keywords: ["notch", "size", "edge"] },
      { id: "notifications", label: "Notifications", icon: icon(Bell), keywords: ["alerts", "sound"] },
      { id: "general", label: "General", icon: icon(Settings2), keywords: ["login", "startup"] },
    ],
  },
];

const titles: Record<string, string> = {
  storage: "Storage",
  cleanup: "Cleanup",
  monitor: "Monitor",
  apps: "Apps",
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
      {active === "storage" ? <Storage /> : active === "cleanup" ? <Cleanup /> : active === "monitor" ? <Monitor /> : active === "apps" ? <Apps /> : <Settings section={active} />}
    </AppShell>
  );
}
