import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@rightkit/app-shell/shell.css";
import "./styles.css";
import { App } from "./App";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
