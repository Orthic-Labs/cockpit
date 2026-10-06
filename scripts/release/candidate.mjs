import { appendFileSync, mkdirSync, readFileSync, writeFileSync, existsSync, readdirSync, statSync, mkdtempSync, rmSync, realpathSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import path from 'node:path';
import os from 'node:os';
const env = process.env;
if (env.GITHUB_ACTIONS !== 'true') throw new Error('Release execution requires generated hosted CI');
const action = process.argv[2];
const run = (cmd, args, options={}) => {
  const r = spawnSync(cmd, args, {stdio:'inherit', ...options});
  if (r.status !== 0) throw new Error(`${cmd} failed (${r.status})`);
  return r.stdout;
};
const revision = env.RIGHT_GIT_SOURCE_REVISION;
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
  run('bash',['scripts/gate.sh']);
  run('cargo',['build','--locked','--release','--bin','cockpit']);
  run('node',['scripts/release/mac-payload.mjs','candidate']);
} else if(action==='check') {
  const root=env.RIGHT_GIT_ARTIFACT_ROOT;
  const app=env.COCKPIT_CHECK_APP || path.join(root,'cockpit','mac','Cockpit.app');
  run('plutil',['-lint',path.join(app,'Contents/Info.plist')]);
  for(const f of ['Contents/MacOS/Cockpit','Contents/Helpers/cockpit','Contents/Resources/dashboard/index.html','Contents/Resources/dashboard/app.mjs','Contents/Resources/dashboard/style.css']) if(!existsSync(path.join(app,f)))throw new Error(`Missing ${f}`);
  const fixture=realpathSync(mkdtempSync(path.join(env.RUNNER_TEMP || os.tmpdir(),'cockpit-package-smoke-')));
  try{
    writeFileSync(path.join(fixture,'example.txt'),'Cockpit fixture');
    const out=run(path.join(app,'Contents/Helpers/cockpit'),['scan',fixture,'--save','--state-dir',path.join(fixture,'state'),'--json'],{encoding:'utf8',stdio:'pipe'});
    const scan=JSON.parse(out);
    if(!scan.snapshot?.report?.entries?.some(e=>e.path.endsWith('/example.txt')))throw new Error('Bundled scanner smoke failed');
    const launch=spawnSync(path.join(app,'Contents/MacOS/Cockpit'),['--package-smoke-root',fixture],{encoding:'utf8',timeout:150_000});
    if(launch.status!==0 || !launch.stderr?.includes('dashboard_smoke_pass'))throw new Error(`Native dashboard smoke failed: ${launch.stderr || launch.error}`);
  } finally {rmSync(fixture,{recursive:true,force:true});}
} else throw new Error(`Unknown action: ${action}`);
