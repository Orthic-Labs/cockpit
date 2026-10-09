import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AppShell } from "@rightkit/app-shell/react";
import { Storage } from "./views/Storage";
import { Monitor } from "./views/Monitor";
import { Apps } from "./views/Apps";
import { Cleanup } from "./views/Cleanup";
import { Overview } from "./views/Overview";
import { Settings, useNotch } from "./views/Settings";
import { Bell, CircleUser, Gauge, HardDrive, LayoutDashboard, LayoutGrid, Palette, Settings2, ShieldCheck, Sparkles } from "lucide-react";

/** Caption buttons on Windows and Linux (macOS keeps its native traffic lights). The window
 *  is frameless there (tauri.windows.conf.json), so the shell draws minimise/maximise/close. */
const shellBridge = {
  window: {
    minimize: () => getCurrentWindow().minimize(),
    toggleMaximize: () => getCurrentWindow().toggleMaximize(),
    close: () => getCurrentWindow().close(),
  },
};

const icon = (Icon: typeof HardDrive) => <Icon size={15} strokeWidth={1.75} />;

const groups = [
  {
    items: [
      { id: "overview", label: "Overview", icon: icon(LayoutDashboard), keywords: ["summary", "home", "dashboard"] },
      { id: "storage", label: "Storage", icon: icon(HardDrive), keywords: ["disk", "files"] },
      { id: "cleanup", label: "Cleanup", icon: icon(Sparkles), keywords: ["trash", "cache", "clean"] },
      { id: "monitor", label: "Monitor", icon: icon(Gauge), keywords: ["cpu", "memory"] },
      { id: "apps", label: "Apps", icon: icon(LayoutGrid), keywords: ["uninstall", "applications", "leftovers"] },
    ],
  },
  {
    title: "Settings",
    items: [
      { id: "permissions", label: "Permissions", icon: icon(ShieldCheck), keywords: ["accessibility", "approval", "helper", "privacy"] },
      { id: "accounts", label: "Accounts", icon: icon(CircleUser), keywords: ["claude", "codex", "sign in"] },
      { id: "appearance", label: "Appearance", icon: icon(Palette), keywords: ["notch", "size", "edge"] },
      { id: "notifications", label: "Notifications", icon: icon(Bell), keywords: ["alerts", "sound"] },
      { id: "general", label: "General", icon: icon(Settings2), keywords: ["login", "startup"] },
    ],
  },
];

const titles: Record<string, string> = {
  overview: "Overview",
  storage: "Storage",
  cleanup: "Cleanup",
  monitor: "Monitor",
  apps: "Apps",
  accounts: "Accounts",
  appearance: "Appearance",
  notifications: "Notifications",
  general: "General",
  permissions: "Permissions",
};

/** `--section settings` (from the notch's settings handle) opens Accounts. */
const resolve = (section: string | null | undefined) =>
  !section ? null : section === "settings" ? "accounts" : titles[section] ? section : null;

export function App() {
  const [active, setActive] = useState("overview");
  const notch = useNotch();
  const missingPermissions = notch.state?.permissions?.filter((permission) => permission.required && permission.status !== "granted").length ?? 0;
  const sidebarGroups = groups.map((group) => ({
    ...group,
    items: group.items.map((item) => item.id === "permissions" ? {
      ...item,
      icon: <span className="permission-nav-icon">
        {icon(ShieldCheck)}
        {missingPermissions > 0 && <span className="permission-count" aria-label={`${missingPermissions} required permissions missing`}>{missingPermissions}</span>}
      </span>,
    } : item),
  }));

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
      bridge={shellBridge}
      groups={sidebarGroups}
      activeId={active}
      onNavigate={setActive}
      title={titles[active]}
      searchMode="none"
      sidebarWidth={170}
      sidebarToggle={false}
      wordmark={<span className="wordmark">Pulse</span>}
    >
      {active === "overview" ? <Overview notch={notch} onNavigate={setActive} /> : active === "storage" ? <Storage /> : active === "cleanup" ? <Cleanup /> : active === "monitor" ? <Monitor /> : active === "apps" ? <Apps /> : <Settings section={active} notch={notch} onNavigate={setActive} />}
    </AppShell>
  );
}
