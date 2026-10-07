#!/usr/bin/env node

import { cp, mkdir, readFile, writeFile, chmod, stat, rm } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const releaseRoot = join(repoRoot, 'release');
const stagingRoot = join(repoRoot, 'dist', 'staging');
const appName = 'Cockpit.app';

const paths = {
  app: join(stagingRoot, appName),
  appExecutable: join(stagingRoot, appName, 'Contents', 'MacOS', 'Cockpit'),
  helper: join(stagingRoot, appName, 'Contents', 'Helpers', 'cockpit'),
  hub: join(stagingRoot, appName, 'Contents', 'Helpers', 'Cockpit Hub.app'),
  privilegedHelper: join(stagingRoot, appName, 'Contents', 'Helpers', 'CockpitHelper'),
  elevate: join(stagingRoot, appName, 'Contents', 'Helpers', 'cockpit-elevate'),
  hubExecutable: join(stagingRoot, appName, 'Contents', 'Helpers', 'Cockpit Hub.app', 'Contents', 'MacOS', 'cockpit-hub'),
  raw: join(stagingRoot, 'raw'),
  output: join(repoRoot, 'dist', 'releases', 'mac', 'Cockpit.dmg')
};

function fail(message) {
  throw new Error(`[cockpit mac payload] ${message}`);
}

async function requireFile(path, label) {
  try {
    const info = await stat(path);
    if (!info.isFile()) fail(`${label} is not a regular file: ${path}`);
  } catch (error) {
    if (error?.code === 'ENOENT') fail(`${label} is missing: ${path}`);
    throw error;
  }
}

async function requireDirectory(path, label) {
  try {
    const info = await stat(path);
    if (!info.isDirectory()) fail(`${label} is not a directory: ${path}`);
  } catch (error) {
    if (error?.code === 'ENOENT') fail(`${label} is missing: ${path}`);
    throw error;
  }
}

async function copyExecutable(source, target, label) {
  await requireFile(source, label);
  await mkdir(dirname(target), { recursive: true });
  await cp(source, target, { force: true });
  await chmod(target, 0o755);
}

async function copyTree(source, target, label) {
  await requireDirectory(source, label);
  await mkdir(dirname(target), { recursive: true });
  await cp(source, target, { recursive: true, force: true });
}

// Cockpit.app is the notch (Codenotch fork, xcodebuild) with the CLI and the
// hub (Tauri) inside Contents/Helpers. The notch's own Info.plist is kept.
function sourcePaths() {
  const temp = process.env.RUNNER_TEMP || '/tmp';
  return {
    notch: process.env.COCKPIT_NOTCH_APP || join(temp, 'cockpit-notch', 'Build', 'Products', 'Release', appName),
    helper: process.env.COCKPIT_CLI_BINARY || join(repoRoot, 'target', 'release', 'cockpit'),
    hub: process.env.COCKPIT_HUB_APP || join(repoRoot, 'hub', 'src-tauri', 'target', 'release', 'bundle', 'macos', 'Cockpit Hub.app')
  };
}

// The privileged helper (SMAppService daemon), its launchd plist and the
// cockpit-elevate client. xcodebuild puts the two tools beside Cockpit.app.
async function placePrivilegedHelper(app, notchApp) {
  const products = dirname(notchApp);
  await copyExecutable(join(products, 'CockpitHelper'), join(app, 'Contents', 'Helpers', 'CockpitHelper'), 'privileged helper');
  await copyExecutable(join(products, 'cockpit-elevate'), join(app, 'Contents', 'Helpers', 'cockpit-elevate'), 'cockpit-elevate');
  const plist = join(app, 'Contents', 'Library', 'LaunchDaemons', 'dev.orthic.cockpit.helper.plist');
  await mkdir(dirname(plist), { recursive: true });
  await cp(join(repoRoot, 'mac/Notch/Helper/dev.orthic.cockpit.helper.plist'), plist, { force: true });
}

async function writeCandidateManifest(root) {
  const manifest = {
    schema_version: 1,
    product: 'cockpit',
    platform: 'mac',
    app: join(root, appName),
    portable_app: join(root, `${appName}.zip`),
    raw: {
      app: join(root, 'raw', 'Cockpit'),
      helper: join(root, 'raw', 'cockpit')
    }
  };
  await writeFile(join(root, 'candidate-manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
}

async function dittoZip(source, target) {
  await mkdir(dirname(target), { recursive: true });
  await new Promise((resolvePromise, reject) => {
    const child = spawn('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', source, target], {
      stdio: 'inherit'
    });
    child.once('error', reject);
    child.once('exit', code => code === 0 ? resolvePromise() : reject(new Error(`ditto exited ${code}`)));
  });
}

async function candidate() {
  const artifactRoot = process.env.RIGHT_GIT_ARTIFACT_ROOT;
  if (!artifactRoot) fail('RIGHT_GIT_ARTIFACT_ROOT is required for candidate mode');
  const root = join(resolve(artifactRoot), 'cockpit', 'mac');
  const source = sourcePaths();
  const app = join(root, appName);
  const appExecutable = join(app, 'Contents', 'MacOS', 'Cockpit');
  const helper = join(app, 'Contents', 'Helpers', 'cockpit');

  await mkdir(root, { recursive: true });
  await rm(app, { recursive: true, force: true });
  await copyTree(source.notch, app, 'notch app (xcodebuild)');
  await requireFile(appExecutable, 'notch executable');
  await copyExecutable(source.helper, helper, 'Cockpit CLI');
  await copyTree(source.hub, join(app, 'Contents', 'Helpers', 'Cockpit Hub.app'), 'hub app (Tauri)');
  await placePrivilegedHelper(app, source.notch);
  await cp(join(repoRoot, 'mac/Notch/LICENSE'), join(app, 'Contents/Resources/codeNOTCH-LICENSE.txt'));
  await copyExecutable(appExecutable, join(root, 'raw', 'Cockpit'), 'Mac app executable');
  await copyExecutable(source.helper, join(root, 'raw', 'cockpit'), 'Cockpit CLI');
  await dittoZip(app, join(root, `${appName}.zip`));
  await writeCandidateManifest(root);
  console.log(`[cockpit mac payload] candidate: ${root}`);
}

async function findCandidateApp(root) {
  const candidates = [join(root, 'cockpit', 'mac', appName)];
  for (const candidatePath of candidates) {
    try {
      await requireDirectory(candidatePath, 'candidate app');
      return candidatePath;
    } catch (error) {
      if (!String(error?.message || '').includes('is missing')) throw error;
    }
  }
  fail(`no ${appName} found under LEGION_UNSIGNED_CANDIDATE_ROOT: ${root}`);
}

async function prepare() {
  const sourceRoot = process.env.LEGION_UNSIGNED_CANDIDATE_ROOT;
  if (!sourceRoot) fail('LEGION_UNSIGNED_CANDIDATE_ROOT is required for prepare mode');
  const sourceApp = await findCandidateApp(resolve(sourceRoot));
  await mkdir(stagingRoot, { recursive: true });
  await rm(paths.app, { recursive: true, force: true });
  await cp(sourceApp, paths.app, { recursive: true, force: true });
  await requireFile(paths.appExecutable, 'staged app executable');
  await requireFile(paths.helper, 'staged Cockpit CLI');
  // GitHub artifact handoff normalizes file modes; restore both known executables.
  await chmod(paths.appExecutable, 0o755);
  await chmod(paths.helper, 0o755);
  for (const [file, label] of [[paths.privilegedHelper, 'privileged helper'], [paths.elevate, 'cockpit-elevate']]) {
    await requireFile(file, `staged ${label}`);
    await chmod(file, 0o755);
  }
  await requireFile(paths.hubExecutable, 'staged hub executable');
  await chmod(paths.hubExecutable, 0o755);
  await mkdir(paths.raw, { recursive: true });
  await copyExecutable(paths.appExecutable, join(paths.raw, 'Cockpit'), 'staged app executable');
  await copyExecutable(paths.helper, join(paths.raw, 'cockpit'), 'staged Cockpit CLI');
  console.log(`[cockpit mac payload] prepared: ${stagingRoot}`);
}

function once(emitter, event) {
  return new Promise((resolvePromise, reject) => {
    emitter.once(event, resolvePromise);
    emitter.once('error', reject);
  });
}

async function packageMac({ local = false } = {}) {
  await requireDirectory(paths.app, 'staged Cockpit.app');
  await requireFile(paths.appExecutable, 'staged app executable');
  await requireFile(paths.helper, 'staged Cockpit CLI');
  await requireFile(paths.hubExecutable, 'staged hub executable');
  const identity = process.env.APPLE_DEVELOPER_ID;
  if (!identity) fail('APPLE_DEVELOPER_ID is required; refusing unconfigured signing identity');

  const require = createRequire(import.meta.url);
  const { resolveMacosDeveloperIdIdentity } = await import('@rightkit/release/macos-signing-identity.mjs');
  const { sign } = require('@electron/osx-sign');
  const appdmg = require('appdmg');
  const entitlements = join(releaseRoot, 'entitlements.plist');
  await sign({
    app: paths.app,
    identity: resolveMacosDeveloperIdIdentity({ env: { ...process.env, APPLE_DEVELOPER_ID: identity } }),
    platform: 'darwin',
    type: 'distribution',
    // osx-sign v2 reads entitlements per file; a top-level `entitlements` is
    // ignored and Electron's defaults (camera, mic, location…) are applied.
    // The privileged helper and cockpit-elevate need no entitlements.
    optionsForFile: file => /\/Contents\/Helpers\/(CockpitHelper|cockpit-elevate)$/.test(file)
      ? { hardenedRuntime: true }
      : { hardenedRuntime: true, entitlements },
    preAutoEntitlements: false,
    preEmbedProvisioningProfile: false,
    gatekeeperAssess: false
  });
  if (!local) await new Promise((resolvePromise,reject) => {
    const child=spawn(process.execPath,[join(repoRoot,'scripts/release/candidate.mjs'),'check'],{
      stdio:'inherit',env:{...process.env,COCKPIT_CHECK_APP:paths.app}
    });
    child.once('error',reject);
    child.once('exit',code=>code===0?resolvePromise():reject(new Error(`Signed app smoke exited ${code}`)));
  });

  await mkdir(dirname(paths.output), { recursive: true });
  const specification = JSON.parse(await readFile(join(releaseRoot, 'appdmg.json'), 'utf8'));
  specification['code-sign'] = { 'signing-identity': identity, identifier: 'dev.orthic.cockpit.dmg' };
  const builder = appdmg({ target: paths.output, basepath: stagingRoot, specification });
  await once(builder, 'finish');
  console.log(`[cockpit mac payload] package: ${paths.output}`);
}

const mode = process.argv[2];
try {
  if (process.platform !== 'darwin') fail('Native macOS host is required');
  // Explicit local packaging reuses an already-qualified candidate; it performs
  // no source build, CI impersonation, notarization or publication.
  if (mode === 'package-local') await packageMac({ local: true });
  else if (process.env.GITHUB_ACTIONS !== 'true') fail('Generated native macOS CI is required');
  else if (mode === 'candidate') await candidate();
  else if (mode === 'prepare') await prepare();
  else if (mode === 'package') await packageMac();
  else fail('usage: mac-payload.mjs <candidate|prepare|package|package-local>');
} catch (error) {
  console.error(error?.stack || error);
  process.exitCode = 1;
}
