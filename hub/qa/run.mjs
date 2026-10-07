// Runs right-qa's native lane with the wdio output streamed to this process, so a
// hang shows where it stopped instead of only a timeout. Same config and evidence.
import { spawn } from "node:child_process";
import config from "../right-qa.config.mjs";
import { runNativeQa } from "@rightkit/qa";

const runProcess = (command, args, { cwd, timeoutMs, env }) =>
  new Promise((resolve) => {
    const child = spawn(command, args, { cwd, env, stdio: ["ignore", "inherit", "inherit"], detached: true });
    const timer = setTimeout(() => {
      console.error(`QA run exceeded ${timeoutMs}ms; killing`);
      try { process.kill(-child.pid, "SIGKILL"); } catch {}
    }, timeoutMs);
    child.on("exit", (code, signal) => {
      clearTimeout(timer);
      try { process.kill(-child.pid, "SIGKILL"); } catch {}
      resolve({ code: code ?? 1, signal, stdout: "", stderr: "" });
    });
  });

const { exitCode, evidencePath } = await runNativeQa(config, { runProcess, timeoutMs: 150_000 });
console.log(`right-qa evidence: ${evidencePath}`);
process.exit(exitCode);
