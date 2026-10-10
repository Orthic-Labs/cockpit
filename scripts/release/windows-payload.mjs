#!/usr/bin/env node
// Windows release payload for Pulse, mirroring mac-payload.mjs.
//   candidate  (windows-2025 candidate job, unsigned): build and stage the payload
//   check      (candidate job): verify the staged payload and smoke the bundled CLI
//   prepare    (windows-sign job, right-release prePackage): copy the candidate to dist/staging
//   package    (windows-sign job, right-release package, after the exes are Authenticode-signed):
//              build the per-user NSIS installer, then sign it with RightKit's signer
// Payload layout (per-user install to %LOCALAPPDATA%\Programs\Pulse):
//   Pulse.exe            notch (windows/ crate, built as pulse-windows-prototype.exe)
//   pulse-hub.exe        Tauri hub (beside Pulse.exe, the first place hub.rs looks)
//   Helpers\pulse.exe    CLI (a separate folder: Windows paths are case-insensitive, so it cannot sit
//                        beside Pulse.exe)
//   Helpers\smartctl.exe smartmontools 7.5 Windows x64, extracted from the official installer (see smartctl below)
//   ThirdParty\          third_party licences;  NOTICE.txt, LICENSE.txt
// Release update asset: dist/releases/windows/Pulse-Setup-x64.exe
import { cp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { existsSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const version = JSON.parse(await readFile(join(repoRoot, 'package.json'), 'utf8')).version;
const triple = 'x86_64-pc-windows-msvc';
const stagingRoot = join(repoRoot, 'dist', 'staging', 'windows');
const stage = join(stagingRoot, 'Pulse');
const installerName = 'Pulse-Setup-x64.exe';
const output = join(repoRoot, 'dist', 'releases', 'windows', installerName);
const payloadFiles = ['Pulse.exe', 'pulse-hub.exe', join('Helpers', 'pulse.exe'), join('Helpers', 'smartctl.exe'), 'notch-views.json', 'NOTICE.txt', 'LICENSE.txt'];

function fail(message) { throw new Error(`[pulse windows payload] ${message}`); }
async function requireFile(path, label) {
  const info = await stat(path).catch(error => error?.code === 'ENOENT' ? fail(`${label} is missing: ${path}`) : Promise.reject(error));
  if (!info.isFile()) fail(`${label} is not a regular file: ${path}`);
}
function run(cmd, args, options = {}) {
  const result = spawnSync(cmd, args, { stdio: 'inherit', ...options });
  if (result.status !== 0) fail(`${cmd} ${args.slice(0, 3).join(' ')} failed (${result.status ?? result.error})`);
  return result;
}
async function copyFile(source, target, label) {
  await requireFile(source, label);
  await mkdir(dirname(target), { recursive: true });
  await cp(source, target, { force: true });
}

// smartctl for Windows: the official smartmontools 7.5 Windows installer (GitHub release RELEASE_7_5,
// the same file SourceForge serves), SHA-256 pinned, fetched in CI and never committed. Only
// bin\smartctl.exe (the x64 build; bin32\ is x86) is extracted from it with 7-Zip (preinstalled on the
// GitHub Windows runners); nothing in the installer is run. The extracted exe's own SHA-256 is pinned
// too. Upstream does not Authenticode-sign it, so right-release signs Helpers/smartctl.exe with the
// other payload exes (sign.prePackageFiles in right-release.config.mjs). A missing download, a hash
// mismatch or a missing 7-Zip fails the build: the Windows payload always carries smartctl.exe.
// Licence text, NOTICE and the source link ship beside it in ThirdParty\smartmontools.
const smartctl = {
  url: 'https://github.com/smartmontools/smartmontools/releases/download/RELEASE_7_5/smartmontools-7.5.win32-setup.exe',
  installerSha256: '896337fcc253220614cf8cdbd5cf2321c5aa326a37a04160a672a281e6104c70',
  member: 'bin/smartctl.exe',
  sha256: 'b5db94e5082c042be44994b7a4fa8f7b5c8e713b2ab1c9a560d8f7a7995ea27d'
};
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

function find7z() {
  const candidates = [process.env.PULSE_7Z, 'C:\\Program Files\\7-Zip\\7z.exe', 'C:\\Program Files (x86)\\7-Zip\\7z.exe'];
  const found = candidates.find(candidatePath => candidatePath && existsSync(candidatePath));
  if (found) return found;
  const where = spawnSync('where', ['7z'], { encoding: 'utf8' });
  if (where.status === 0) return where.stdout.split(/\r?\n/)[0].trim();
  return fail('7-Zip (7z.exe) not found on the runner; needed to extract smartctl.exe from the smartmontools installer (set PULSE_7Z)');
}

// The verified smartctl.exe path in the runner temp cache.
async function fetchSmartctl() {
  const cacheDir = join(process.env.RUNNER_TEMP || os.tmpdir(), 'pulse-smartctl');
  const cached = join(cacheDir, `smartctl-7.5-${smartctl.sha256}.exe`);
  try {
    if (sha256(await readFile(cached)) === smartctl.sha256) return cached;
  } catch (error) { if (error?.code !== 'ENOENT') throw error; }
  const response = await fetch(smartctl.url, { redirect: 'follow' }).catch(error => fail(`smartctl installer download failed: ${error?.message ?? error}`));
  if (!response.ok) fail(`smartctl installer download failed: HTTP ${response.status} from ${smartctl.url}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  const actual = sha256(bytes);
  if (actual !== smartctl.installerSha256) fail(`smartmontools installer SHA-256 mismatch: expected ${smartctl.installerSha256}, got ${actual}`);
  const work = mkdtempSync(join(process.env.RUNNER_TEMP || os.tmpdir(), 'pulse-smartctl-work-'));
  try {
    const installer = join(work, 'smartmontools-setup.bin');
    const out = join(work, 'out');
    writeFileSync(installer, bytes);
    run(find7z(), ['e', '-y', `-o${out}`, installer, smartctl.member], { stdio: ['ignore', 'ignore', 'inherit'] });
    const extracted = await readFile(join(out, 'smartctl.exe')).catch(() => fail(`${smartctl.member} was not found in the smartmontools installer`));
    const exeHash = sha256(extracted);
    if (exeHash !== smartctl.sha256) fail(`smartctl.exe SHA-256 mismatch: expected ${smartctl.sha256}, got ${exeHash}`);
    await mkdir(cacheDir, { recursive: true });
    await writeFile(cached, extracted);
  } finally { rmSync(work, { recursive: true, force: true }); }
  return cached;
}

function sources() {
  // A CARGO_TARGET_DIR (RightKit-managed or CI) is shared by all three builds; otherwise each crate/workspace uses its own target.
  const shared = process.env.CARGO_TARGET_DIR;
  return {
    notch: process.env.PULSE_NOTCH_EXE || join(shared || join(repoRoot, 'windows', 'target'), 'release', 'pulse-windows-prototype.exe'),
    cli: process.env.PULSE_CLI_EXE || join(shared || join(repoRoot, 'target'), 'release', 'pulse.exe'),
    hub: process.env.PULSE_HUB_EXE || join(shared || join(repoRoot, 'hub', 'src-tauri', 'target'), triple, 'release', 'pulse-hub.exe')
  };
}

// Release builds of the three binaries (the real hub frontend is built by tauri's beforeBuildCommand).
function buildSources() {
  run('cargo', ['build', '--locked', '--release', '--bin', 'pulse'], { cwd: repoRoot });
  run('cargo', ['build', '--locked', '--release', '--manifest-path', 'windows/Cargo.toml'], { cwd: repoRoot });
  run('pnpm', ['--dir', 'hub', 'install', '--frozen-lockfile'], { cwd: repoRoot, shell: true });
  run('pnpm', ['--dir', 'hub', 'exec', 'tauri', 'build', '--no-bundle', '--no-sign', '--target', triple], { cwd: repoRoot, shell: true });
  return sources();
}

// Shared by the release candidate and the dev artifact: lay the payload out and require it complete.
async function stagePayload(payload, source) {
  await rm(payload, { recursive: true, force: true });
  await copyFile(source.notch, join(payload, 'Pulse.exe'), 'notch exe');
  await copyFile(source.hub, join(payload, 'pulse-hub.exe'), 'hub exe');
  await copyFile(source.cli, join(payload, 'Helpers', 'pulse.exe'), 'Pulse CLI');
  await copyFile(await fetchSmartctl(), join(payload, 'Helpers', 'smartctl.exe'), 'smartctl');
  await cp(join(repoRoot, 'third_party'), join(payload, 'ThirdParty'), { recursive: true, force: true });
  // The notch's view fixtures, so `Pulse.exe --render-views` works on an installed build (K8).
  await copyFile(join(repoRoot, 'qa', 'notch-views.json'), join(payload, 'notch-views.json'), 'view fixtures');
  await copyFile(join(repoRoot, 'NOTICE'), join(payload, 'NOTICE.txt'), 'NOTICE');
  await copyFile(join(repoRoot, 'LICENSE'), join(payload, 'LICENSE.txt'), 'LICENSE');
  for (const file of [...payloadFiles, join('ThirdParty', 'smartmontools', 'GPL-2.0.txt')]) await requireFile(join(payload, file), `staged ${file}`);
}

async function candidate() {
  const artifactRoot = process.env.RIGHT_GIT_ARTIFACT_ROOT;
  if (!artifactRoot) fail('RIGHT_GIT_ARTIFACT_ROOT is required for candidate mode');
  // The ci lane's gate already ran the Windows tests on this runner; this only compiles the release payload.
  const source = buildSources();
  const root = join(resolve(artifactRoot), 'pulse', 'windows');
  const payload = join(root, 'Pulse');
  await stagePayload(payload, source);
  await writeFile(join(root, 'candidate-manifest.json'), `${JSON.stringify({ schema_version: 1, product: 'pulse', platform: 'windows', architecture: 'x86_64', version, payload }, null, 2)}\n`, 'utf8');
  console.log(`[pulse windows payload] candidate: ${root}`);
}

// Dev artifact (ci lane, push to main): unsigned payload at dist/dev/windows for RightKit's upload step.
async function dev() {
  const payload = resolve(process.env.PULSE_DEV_OUT || join(repoRoot, 'dist', 'dev', 'windows'));
  await stagePayload(payload, buildSources());
  console.log(`[pulse windows payload] dev payload: ${payload}`);
}

async function check() {
  const artifactRoot = process.env.RIGHT_GIT_ARTIFACT_ROOT;
  if (!artifactRoot) fail('RIGHT_GIT_ARTIFACT_ROOT is required for check mode');
  const payload = join(resolve(artifactRoot), 'pulse', 'windows', 'Pulse');
  for (const file of [...payloadFiles, join('ThirdParty', 'smartmontools', 'GPL-2.0.txt')]) await requireFile(join(payload, file), file);
  const fixture = realpathSync(mkdtempSync(join(process.env.RUNNER_TEMP || os.tmpdir(), 'pulse-package-smoke-')));
  const state = realpathSync(mkdtempSync(join(process.env.RUNNER_TEMP || os.tmpdir(), 'pulse-package-state-')));
  try {
    writeFileSync(join(fixture, 'example.txt'), 'Pulse fixture');
    const result = spawnSync(join(payload, 'Helpers', 'pulse.exe'), ['scan', fixture, '--save', '--state-dir', state, '--json'], { encoding: 'utf8' });
    if (result.status !== 0) fail(`bundled CLI scan failed (${result.status}): ${result.stderr}`);
    const scan = JSON.parse(result.stdout);
    if (!scan.snapshot?.report?.entries?.some(entry => /[\\/]example\.txt$/.test(entry.path))) fail('Bundled scanner smoke failed');
  } finally { rmSync(fixture, { recursive: true, force: true }); rmSync(state, { recursive: true, force: true }); }
  console.log('[pulse windows payload] check passed');
}

async function prepare() {
  const sourceRoot = process.env.LEGION_UNSIGNED_CANDIDATE_ROOT;
  if (!sourceRoot) fail('LEGION_UNSIGNED_CANDIDATE_ROOT is required for prepare mode');
  const payload = join(resolve(sourceRoot), 'pulse', 'windows', 'Pulse');
  for (const file of payloadFiles) await requireFile(join(payload, file), `candidate ${file}`);
  await rm(stage, { recursive: true, force: true });
  await mkdir(stagingRoot, { recursive: true });
  await cp(payload, stage, { recursive: true, force: true });
  console.log(`[pulse windows payload] prepared: ${stage}`);
}

function findMakensis() {
  const local = process.env.LOCALAPPDATA || '';
  const candidates = [process.env.PULSE_MAKENSIS, 'C:\\Program Files (x86)\\NSIS\\makensis.exe', 'C:\\Program Files\\NSIS\\makensis.exe', join(local, 'tauri', 'NSIS', 'makensis.exe')];
  const found = candidates.find(candidatePath => candidatePath && existsSync(candidatePath));
  if (found) return found;
  const where = spawnSync('where', ['makensis'], { encoding: 'utf8' });
  if (where.status === 0) return where.stdout.split(/\r?\n/)[0].trim();
  return fail('makensis (NSIS 3) not found; install NSIS on the signing runner or set PULSE_MAKENSIS');
}

async function packageWindows() {
  for (const file of payloadFiles) await requireFile(join(stage, file), `staged ${file}`);
  await mkdir(dirname(output), { recursive: true });
  await rm(output, { force: true });
  run(findMakensis(), ['/V2', `/DSTAGE=${stage}`, `/DOUT=${output}`, `/DVERSION=${version}`, join(repoRoot, 'scripts', 'release', 'windows', 'pulse.nsi')]);
  await requireFile(output, 'installer');
  // The payload exes were signed by right-release before this step. The installer is signed here with
  // RightKit's own signer (Azure Artifact Signing, same env as the exes); no signing code lives in Pulse.
  const signer = join(repoRoot, 'node_modules', '@rightkit', 'release', 'sign-windows.mjs');
  await requireFile(signer, 'RightKit sign-windows');
  run(process.execPath, [signer, '--receipt', join(repoRoot, '.right-release', 'receipts', 'windows-installer-signing.json'), output]);
  console.log(`[pulse windows payload] package: ${output}`);
}

const mode = process.argv[2];
try {
  if (process.platform !== 'win32') fail('Native Windows host is required');
  if (process.env.GITHUB_ACTIONS !== 'true') fail('Generated native Windows CI is required');
  if (mode === 'candidate') await candidate();
  else if (mode === 'dev') await dev();
  else if (mode === 'check') await check();
  else if (mode === 'prepare') await prepare();
  else if (mode === 'package') await packageWindows();
  else fail('usage: windows-payload.mjs <candidate|dev|check|prepare|package>');
} catch (error) {
  console.error(error?.stack || error);
  process.exitCode = 1;
}
