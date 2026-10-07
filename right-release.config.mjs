import { readFileSync } from 'node:fs';
const version=JSON.parse(readFileSync(new URL('./package.json',import.meta.url))).version;
export default {
 schema:1, app:'cockpit', version, packageManager:'pnpm@11.24.0', hostedWorkflows:'right-git-ci-only',
 distribution:{provider:'github-releases',repository:'Orthic-Labs/cockpit'},
 // Cockpit's native workspace is the repo-root Cargo.toml (core + CLI); the
 // hub's Tauri crate is a separate workspace under hub/src-tauri.
 nativeAssembly:{cargoManifest:'Cargo.toml',cargoLockSource:'manifest'},
 buildInputs:{include:['mac/**','core/**','rules/**','hub/**','scripts/**','release/**','Cargo.toml','Cargo.lock','package.json','pnpm-lock.yaml','pnpm-workspace.yaml','.rightgit.json','.github/workflows/**','right-release.config.mjs'],required:['Cargo.lock','pnpm-lock.yaml','right-release.config.mjs']},
 targets:{mac:{
  signed:true,signingContract:'macos-developer-id-notarized-portable-v1',packageKind:'dmg',architecture:'arm64',publishBlocked:'Preview installer delivery; publication is separate',
  prePackage:{cmd:'pnpm',args:['run','rightkit:prepare:mac']},
  sign:{prePackageFiles:['dist/staging/Cockpit.app/Contents/MacOS/Cockpit','dist/staging/Cockpit.app/Contents/Helpers/cockpit','dist/staging/Cockpit.app/Contents/Helpers/Cockpit Hub.app/Contents/MacOS/cockpit-hub'],receipt:'.right-release/receipts/macos-signing.json'},
  package:{cmd:'pnpm',args:['run','rightkit:package:mac']},
  artifacts:['dist/releases/mac/Cockpit.dmg'],
  notarize:{file:'dist/releases/mac/Cockpit.dmg',receipt:'.right-release/receipts/macos-notarization.json'},
  hardening:['dist/releases/mac/Cockpit.dmg'],
  installer:{artifacts:[{file:'dist/releases/mac/Cockpit.dmg',key:'cockpit/installers/mac/current/Cockpit.dmg'}]},updater:{artifacts:[]}
 }}
};
