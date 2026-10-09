import { appendFileSync, mkdirSync, readFileSync, writeFileSync, existsSync, readdirSync, statSync, mkdtempSync, rmSync, realpathSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import path from 'node:path';
import os from 'node:os';
const env = { ...process.env };
if (env.GITHUB_ACTIONS !== 'true') throw new Error('Release execution requires generated hosted CI');
const action = process.argv[2];
const run = (cmd, args, options={}) => {
  const r = spawnSync(cmd, args, {stdio:'inherit', ...options});
  if (r.status !== 0) throw new Error(`${cmd} failed (${r.status})`);
  return r.stdout;
};
const revision = env.RIGHT_GIT_SOURCE_REVISION;
if ((action === 'build' || action === 'check') && process.platform === 'win32') {
  // Windows candidate leg: the ci lane already ran the Windows gate; payload and smoke live in windows-payload.mjs.
  run(process.execPath, [path.join('scripts', 'release', 'windows-payload.mjs'), action === 'build' ? 'candidate' : 'check']);
  process.exit(0);
}
if (action === 'admit') {
  const version = env.RIGHT_GIT_RELEASE_VERSION;
  const head = run('git', ['rev-parse','HEAD'], {encoding:'utf8',stdio:'pipe'}).trim();
  if (!/^[a-f0-9]{40}$/.test(revision) || head !== revision) throw new Error('Exact checkout required');
  run('git', ['merge-base','--is-ancestor',revision,'origin/main']);
  if (version !== JSON.parse(readFileSync('package.json')).version) throw new Error('Version mismatch');
  if (env.RIGHT_GIT_PUBLISH === 'true') throw new Error('Preview publication is disabled');
  const fields = {version, source_revision:revision, signed_qualification:env.RIGHT_GIT_SIGNED_QUALIFICATION, publish:'false', dry_run:env.RIGHT_GIT_DRY_RUN, artifact_suffix:`${version}-${revision}`};
  appendFileSync(env.GITHUB_OUTPUT, Object.entries(fields).map(([k,v])=>`${k}=${v}\n`).join(''));
} else if (action === 'summary') {
  const root = env.RIGHT_GIT_STAGE_ROOT;
  mkdirSync(root,{recursive:true});
  const hashes = [];
  const walk = d => { if(!existsSync(d)) return; for(const n of readdirSync(d)){ const f=path.join(d,n); const s=statSync(f); if(s.isDirectory())walk(f); else hashes.push({file:path.relative(env.RIGHT_GIT_STAGE_EVIDENCE_ROOT,f),size_bytes:s.size,sha256:createHash('sha256').update(readFileSync(f)).digest('hex')}); }};
  if(env.RIGHT_GIT_STAGE_ACTION==='finalize')walk(env.RIGHT_GIT_STAGE_EVIDENCE_ROOT);
  writeFileSync(path.join(root,'stage-summary.json'),JSON.stringify({schema_version:1,stage:env.RIGHT_GIT_STAGE,producer:env.RIGHT_GIT_STAGE_PRODUCER,status:env.RIGHT_GIT_STAGE_STATUS||'STARTED',version:env.RIGHT_GIT_RELEASE_VERSION,source_revision:revision,platform:env.RIGHT_GIT_RELEASE_PLATFORM,architecture:env.RIGHT_GIT_RELEASE_ARCHITECTURE,run_id:env.RIGHT_GIT_RUN_ID,run_attempt:env.RIGHT_GIT_RUN_ATTEMPT,artifacts:hashes},null,2)+'\n');
} else if(action==='build') {
  if(process.platform!=='darwin') throw new Error('Mac native host required');
  run('bash',['scripts/gate.sh'],{env:{...env,PULSE_SKIP_HUB_QA:'1'}}); // also builds the notch (xcodebuild) into $RUNNER_TEMP
  run('cargo',['build','--locked','--release','--bin','pulse']);
  run('pnpm',['--dir','hub','install','--frozen-lockfile']);
  run('pnpm',['--dir','hub','tauri','build','--bundles','app','--no-sign']);
  run('node',['scripts/release/mac-payload.mjs','candidate']);
} else if(action==='check') {
  const root=env.RIGHT_GIT_ARTIFACT_ROOT;
  const app=env.PULSE_CHECK_APP || path.join(root,'pulse','mac','Pulse.app');
  run('plutil',['-lint',path.join(app,'Contents/Info.plist')]);
  for(const f of ['Contents/MacOS/Pulse','Contents/Helpers/pulse','Contents/Helpers/Pulse.app/Contents/MacOS/pulse-hub','Contents/Helpers/Pulse.app/Contents/Info.plist','Contents/Helpers/PulseHelper','Contents/Helpers/pulse-elevate','Contents/Helpers/smartctl','Contents/Resources/ThirdParty/smartmontools/GPL-2.0.txt','Contents/Resources/ThirdParty/smartmontools/README.md','Contents/PlugIns/PulseFinder.appex/Contents/MacOS/PulseFinder','Contents/PlugIns/PulseFinder.appex/Contents/Info.plist','Contents/Library/LaunchDaemons/dev.orthic.pulse.helper.plist']) if(!existsSync(path.join(app,f)))throw new Error(`Missing ${f}`);
  const plist=run('plutil',['-convert','json','-o','-',path.join(app,'Contents/Info.plist')],{encoding:'utf8',stdio:'pipe'});
  const info=JSON.parse(plist);
  if(info.CFBundleIdentifier!=='dev.orthic.pulse'||info.LSUIElement!==true)throw new Error('Notch Info.plist: wrong identity or Dock presence');
  // The daemon plist: BundleProgram must name the helper inside the bundle.
  const daemon=JSON.parse(run('plutil',['-convert','json','-o','-',path.join(app,'Contents/Library/LaunchDaemons/dev.orthic.pulse.helper.plist')],{encoding:'utf8',stdio:'pipe'}));
  if(daemon.Label!=='dev.orthic.pulse.helper'||!daemon.MachServices?.['dev.orthic.pulse.helper']||!(daemon.AssociatedBundleIdentifiers||[]).includes('dev.orthic.pulse'))throw new Error('Helper plist: wrong label, Mach service or associated bundle');
  if(typeof daemon.BundleProgram!=='string'||daemon.BundleProgram.startsWith('/')||daemon.BundleProgram.split('/').includes('..'))throw new Error('Helper plist: BundleProgram must be a relative path inside the bundle');
  const program=path.join(app,daemon.BundleProgram);
  if(!existsSync(program)||!statSync(program).isFile())throw new Error(`Helper plist: BundleProgram ${daemon.BundleProgram} is not a file in the bundle`);
  if(env.PULSE_CHECK_APP){
    // Signed: each tool carries the identifier the helper's connection requirement names, and Pulse's team.
    for(const [f,id] of [['Contents/Helpers/PulseHelper','dev.orthic.pulse.helper'],['Contents/Helpers/pulse-elevate','dev.orthic.pulse.elevate']]){
      const d=spawnSync('codesign',['-dv','--verbose=2',path.join(app,f)],{encoding:'utf8'}).stderr||'';
      if(!d.includes(`Identifier=${id}\n`)||!d.includes('TeamIdentifier=6KLGD3LLKF')||!/flags=0x[0-9a-f]+\(.*runtime/.test(d))throw new Error(`${f}: expected identifier ${id}, team 6KLGD3LLKF and hardened runtime`);
    }
  }
  if(env.PULSE_CHECK_APP){
    // Finder extension: Pulse's team, its own identifier, hardened runtime, sandboxed.
    const appex=path.join(app,'Contents/PlugIns/PulseFinder.appex');
    const d=spawnSync('codesign',['-dv','--verbose=2',appex],{encoding:'utf8'}).stderr||'';
    if(!d.includes('Identifier=dev.orthic.pulse.finder\n')||!d.includes('TeamIdentifier=6KLGD3LLKF')||!/flags=0x[0-9a-f]+\(.*runtime/.test(d))throw new Error('PulseFinder.appex: expected identifier dev.orthic.pulse.finder, team 6KLGD3LLKF and hardened runtime');
    const e=spawnSync('codesign',['-d','--entitlements','-','--xml',appex],{encoding:'utf8'}).stdout||'';
    if(!e.includes('com.apple.security.app-sandbox'))throw new Error('PulseFinder.appex is not sandboxed');
  }
  if(env.PULSE_CHECK_APP){
    // Signed app: exactly the entitlements Pulse needs, never Electron's defaults.
    const ent=spawnSync('codesign',['-d','--entitlements','-','--xml',app],{encoding:'utf8'}).stdout||'';
    if(!ent.includes('com.apple.security.automation.apple-events'))throw new Error('Signed app lacks the Apple Events entitlement');
    if(/device\.(camera|audio-input)|personal-information\.location/.test(ent))throw new Error('Signed app carries unexpected camera/microphone/location entitlements');
  }
  const fixture=realpathSync(mkdtempSync(path.join(env.RUNNER_TEMP || os.tmpdir(),'pulse-package-smoke-')));
  const state=realpathSync(mkdtempSync(path.join(env.RUNNER_TEMP || os.tmpdir(),'pulse-package-state-')));
  try{
    writeFileSync(path.join(fixture,'example.txt'),'Pulse fixture');
    const out=run(path.join(app,'Contents/Helpers/pulse'),['scan',fixture,'--save','--state-dir',state,'--json'],{encoding:'utf8',stdio:'pipe'});
    const scan=JSON.parse(out);
    if(!scan.snapshot?.report?.entries?.some(e=>e.path.endsWith('/example.txt')))throw new Error('Bundled scanner smoke failed');
  } finally {rmSync(fixture,{recursive:true,force:true});rmSync(state,{recursive:true,force:true});}
} else throw new Error(`Unknown action: ${action}`);
