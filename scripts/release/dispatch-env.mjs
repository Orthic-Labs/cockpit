// Normalize product dispatch inputs into RightKit's admitted-source context.
// Generated non-chain signing lane expects this environment before right-release.
import { appendFileSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
const env=process.env;
if(env.GITHUB_ACTIONS==='true' && env.GITHUB_EVENT_NAME==='workflow_dispatch') {
 const inputs=JSON.parse(readFileSync(env.GITHUB_EVENT_PATH,'utf8')).inputs;
 const source=inputs?.source_revision;
 const head=spawnSync('git',['rev-parse','HEAD'],{encoding:'utf8'});
 if(!/^[a-f0-9]{40}$/.test(source||'') || head.status!==0 || head.stdout.trim()!==source)throw new Error('Dispatch source must match exact checkout');
 if(spawnSync('git',['merge-base','--is-ancestor',source,'origin/main']).status!==0)throw new Error('Source must be reachable from main');
 appendFileSync(env.GITHUB_ENV,`RIGHT_GIT_SOURCE_REVISION=${source}\n`);
}
