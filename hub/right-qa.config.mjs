import path from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = path.dirname(fileURLToPath(import.meta.url));
// The hub is its own cargo workspace, so its debug binary lives under src-tauri
// unless a managed build redirects CARGO_TARGET_DIR.
const debugRoot = path.join(process.env.CARGO_TARGET_DIR ?? path.join(appRoot, "src-tauri", "target"), "debug");

export default {
  app: "cockpit-hub",
  appRoot,
  windowLabel: "main",
  specs: ["qa/**/*.e2e.mjs"],
  binaries: {
    win32: path.join(debugRoot, "cockpit-hub.exe"),
    darwin: path.join(debugRoot, "cockpit-hub"),
  },
};
