// Native QA lane for the hub, equivalent to `right-qa native` but with the wdio
// output streamed, so a hang shows where it stopped. Uses right-qa's own config
// builder and isolated workspace.
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { createQaWorkspace, createWdioConfig, defineRightQaConfig } from "@rightkit/qa";
import rawConfig from "../right-qa.config.mjs";

const app = defineRightQaConfig(rawConfig);
const wdioConfig = createWdioConfig(app, { platform: "darwin" });
const cacheRoot = path.resolve(app.appRoot, ".cache/rightkit-qa");
const workspace = createQaWorkspace({ root: cacheRoot, appKey: app.app });

const qaRequire = createRequire(import.meta.resolve("@rightkit/qa"));
const undici = pathToFileURL(qaRequire.resolve("undici")).href;
const wdioBin = path.resolve(path.dirname(qaRequire.resolve("@wdio/cli")), "../bin/wdio.js");
const generated = path.join(workspace.root, "wdio.generated.mjs");
fs.writeFileSync(
  generated,
  [
    `import { Agent, setGlobalDispatcher } from ${JSON.stringify(undici)};`,
    "setGlobalDispatcher(new Agent());",
    `const config = ${JSON.stringify(wdioConfig, null, 2)};`,
    "config.beforeSession = () => { setGlobalDispatcher(new Agent()); };",
    "export { config };",
    "",
  ].join("\n"),
);

const timeoutMs = 240_000;
const child = spawn(process.execPath, [wdioBin, "run", generated], {
  cwd: app.appRoot,
  stdio: ["ignore", "inherit", "inherit"],
  detached: true,
  env: { ...process.env, ...workspace.env, RIGHTKIT_QA_HIDDEN: "1", TAURI_WEBDRIVER_PORT: "4445" },
});
const killTree = () => { try { process.kill(-child.pid, "SIGKILL"); } catch {} };
const timer = setTimeout(() => { console.error(`QA run exceeded ${timeoutMs}ms; killing`); killTree(); }, timeoutMs);
child.on("exit", (code) => {
  clearTimeout(timer);
  killTree();
  const passed = code === 0;
  fs.writeFileSync(
    path.join(workspace.root, "evidence.json"),
    JSON.stringify({ app: app.app, runId: workspace.runId, status: passed ? "passed" : "failed", code }, null, 2) + "\n",
  );
  console.log(`hub QA ${passed ? "passed" : "failed"} (exit ${code}); evidence ${workspace.root}`);
  process.exit(passed ? 0 : 1);
});
