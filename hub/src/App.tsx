import { useState } from "react";
import { AppShell } from "@rightkit/app-shell/react";
import { Storage } from "./views/Storage";
import { Monitor } from "./views/Monitor";

const groups = [
  {
    items: [
      { id: "storage", label: "Storage", icon: <span className="nav-icon">◧</span>, keywords: ["disk", "files"] },
      { id: "monitor", label: "Monitor", icon: <span className="nav-icon">◉</span>, keywords: ["cpu", "memory"] },
    ],
  },
];

const titles: Record<string, string> = { storage: "Storage", monitor: "Monitor" };

export function App() {
  const [active, setActive] = useState("storage");
  return (
    <AppShell
      groups={groups}
      activeId={active}
      onNavigate={setActive}
      title={titles[active]}
      searchMode="none"
      sidebarWidth={150}
      wordmark={<span className="wordmark">Cockpit</span>}
    >
      {active === "storage" ? <Storage /> : <Monitor />}
    </AppShell>
  );
}
