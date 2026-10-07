import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@rightkit/app-shell/shell.css";
import "./styles.css";
import { App } from "./App";
import { getCurrentWindow } from "@tauri-apps/api/window";

// The hub has no Dock icon, so macOS does not bring it forward on launch.
void getCurrentWindow().setFocus().catch(() => {});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
