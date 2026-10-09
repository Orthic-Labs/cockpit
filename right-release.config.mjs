import { readFileSync } from 'node:fs';
const version=JSON.parse(readFileSync(new URL('./package.json',import.meta.url))).version;
export default {
 schema:1, app:'pulse', version, packageManager:'pnpm', hostedWorkflows:'right-git-ci-only',
 distribution:{provider:'github-releases',repository:'Orthic-Labs/pulse'},
 // Pulse's native workspace is the repo-root Cargo.toml (core + CLI); the
 // hub's Tauri crate is a separate workspace under hub/src-tauri.
 nativeAssembly:{cargoManifest:'Cargo.toml',cargoLockSource:'manifest'},
 buildInputs:{include:['mac/**','windows/**','core/**','third_party/**','NOTICE','LICENSE','rules/**','hub/**','scripts/**','release/**','Cargo.toml','Cargo.lock','package.json','pnpm-lock.yaml','pnpm-workspace.yaml','.rightgit.json','.github/workflows/**','right-release.config.mjs'],required:['Cargo.lock','pnpm-lock.yaml','right-release.config.mjs']},
 targets:{win:{
  signed:true,signingContract:'windows-raw-exe-authenticode-before-portable-v1',packageKind:'nsis',architecture:'x64',publishBlocked:'Preview installer delivery; publication is separate',
  // Portable contract: right-release Authenticode-signs every payload exe; the package step builds the per-user NSIS
  // installer and signs it with RightKit's signer (scripts/release/windows-payload.mjs).
  prePackage:{cmd:'pnpm',args:['run','rightkit:prepare:win']},
  sign:{prePackageFiles:['dist/staging/windows/Pulse/Pulse.exe','dist/staging/windows/Pulse/pulse-hub.exe','dist/staging/windows/Pulse/Helpers/pulse.exe']},
  package:{cmd:'pnpm',args:['run','rightkit:package:win'],timeoutMs:20*60*1000},
  artifacts:['dist/releases/windows/Pulse-Setup-x64.exe'],
  hardening:['dist/releases/windows/Pulse-Setup-x64.exe'],
  installer:{artifacts:[{file:'dist/releases/windows/Pulse-Setup-x64.exe',key:'pulse/installers/windows/current/Pulse-Setup-x64.exe'}]},updater:{artifacts:[]}
 },mac:{
  signed:true,signingContract:'macos-developer-id-notarized-portable-v1',packageKind:'dmg',architecture:'arm64',publishBlocked:'Preview installer delivery; publication is separate',
  prePackage:{cmd:'pnpm',args:['run','rightkit:prepare:mac']},
  sign:{prePackageFiles:['dist/staging/Pulse.app/Contents/Helpers/smartctl','dist/staging/Pulse.app/Contents/Helpers/PulseHelper','dist/staging/Pulse.app/Contents/Helpers/pulse-elevate','dist/staging/Pulse.app/Contents/PlugIns/PulseFinder.appex/Contents/MacOS/PulseFinder','dist/staging/Pulse.app/Contents/MacOS/Pulse','dist/staging/Pulse.app/Contents/Helpers/pulse','dist/staging/Pulse.app/Contents/Helpers/Pulse.app/Contents/MacOS/pulse-hub'],receipt:'.right-release/receipts/macos-signing.json'},
  package:{cmd:'pnpm',args:['run','rightkit:package:mac']},
  artifacts:['dist/releases/mac/Pulse.dmg'],
  notarize:{file:'dist/releases/mac/Pulse.dmg',receipt:'.right-release/receipts/macos-notarization.json'},
  hardening:['dist/releases/mac/Pulse.dmg'],
  installer:{artifacts:[{file:'dist/releases/mac/Pulse.dmg',key:'pulse/installers/mac/current/Pulse.dmg'}]},updater:{artifacts:[]}
 }}
};
