#!/usr/bin/env node
// Side-by-side Mac vs Windows screens page: every screen and control state, with the inventories merged.
//
// Usage:
//   node scripts/qa/parity-screens.mjs --mac <dir> --win <dir> [--docs docs] [--html .parity-screens]
//   node scripts/qa/parity-screens.mjs <mac-dir> <win-dir>
//
// Inputs (both folders are scanned recursively for *.png, inventory.json and inventory.md):
//   Mac     the CI evidence folder (pulse-hub-qa): screenshots/<surface>__<control>__<hover|pressed|after>.png,
//           screenshots/NN-<section>.png, views/mac/<view-id>.png, screenshots/inventory.json
//   Windows D:\Claude\pulse-qa\win: <surface>__<control>__<state>.png plus inventory.md
// Pairs are matched by surface, control and state name (other PNGs by file name, a leading "NN-" ignored).
// Output: <docs>/parity-screens.md, with the PNGs copied to <docs>/parity-screens/{mac,win}/ and
// referenced by those relative paths, and <html>/index.html (git-ignored, like .parity-views) with the
// same pairs and its own copies of the PNGs in <html>/{mac,win}/.

import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const USAGE = 'usage: node scripts/qa/parity-screens.mjs --mac <dir> --win <dir> [--docs docs] [--html .parity-screens]';
const STATE_ORDER = ['hover', 'pressed', 'after'];

function fail(message) {
  process.stderr.write(`parity-screens: ${message}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const args = { docs: join(repoRoot, 'docs'), html: join(repoRoot, '.parity-screens') };
  const positional = [];
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--help' || flag === '-h') {
      process.stdout.write(`${USAGE}\n`);
      process.exit(0);
    }
    if (flag.startsWith('--')) {
      const key = flag.slice(2);
      if (!['mac', 'win', 'docs', 'html'].includes(key)) fail(`unknown argument ${flag}\n${USAGE}`);
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) fail(`${flag} needs a value\n${USAGE}`);
      args[key] = value;
      i += 1;
    } else {
      positional.push(flag);
    }
  }
  if (!args.mac && positional.length) args.mac = positional.shift();
  if (!args.win && positional.length) args.win = positional.shift();
  if (!args.mac || !args.win) fail(`need a Mac evidence dir and a Windows screens dir\n${USAGE}`);
  return args;
}

const slugify = (text) => String(text).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
const surfaceKey = (text) => slugify(text).replace(/^(hub|settings|section)-/, '');
const screenKey = (file) => slugify(file.replace(/\.png$/i, '').replace(/^\d+-/, ''));

function walk(dir, out = []) {
  if (!existsSync(dir)) fail(`${dir} does not exist`);
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path, out);
    else out.push(path);
  }
  return out;
}

// One folder: its control shots (surface__control__state), its other screens, its inventory entries.
function scan(dir) {
  const files = walk(dir);
  const shots = new Map(); // key -> { path, file, surface, control, state }
  const screens = new Map(); // key -> { path, file }
  for (const path of files) {
    const file = basename(path);
    if (!/\.png$/i.test(file)) continue;
    const parts = file.replace(/\.png$/i, '').split('__');
    if (parts.length === 3) {
      const [surface, control, state] = parts;
      const key = `${surfaceKey(surface)}__${slugify(control)}__${slugify(state)}`;
      shots.set(key, { path, file, surface: surfaceKey(surface), control: slugify(control), state: slugify(state) });
    } else {
      screens.set(screenKey(file), { path, file });
    }
  }
  return { shots, screens, inventory: readInventory(files) };
}

function readInventory(files) {
  const json = files.find((f) => basename(f) === 'inventory.json');
  if (json) {
    const data = JSON.parse(readFileSync(json, 'utf8'));
    const list = Array.isArray(data) ? data : data.controls ?? [];
    return list.map((e) => normalise({
      surface: e.surface,
      label: e.label ?? e.control,
      slug: e.slug,
      role: e.role,
      hover: e.hover_style_changed ?? e.hover,
      pressed: e.press_pixels_changed ?? e.pressed,
      action: e.action,
      clicked: e.clicked,
    }));
  }
  const md = files.find((f) => basename(f) === 'inventory.md');
  return md ? parseMarkdownTables(readFileSync(md, 'utf8')) : [];
}

function parseMarkdownTables(text) {
  const out = [];
  let columns = null;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line.startsWith('|')) {
      columns = null;
      continue;
    }
    const cells = line.replace(/^\||\|$/g, '').split('|').map((c) => c.trim());
    if (cells.every((c) => /^:?-{2,}:?$/.test(c))) continue;
    if (!columns) {
      columns = cells.map((c) => c.toLowerCase());
      continue;
    }
    const pick = (re) => {
      const at = columns.findIndex((c) => re.test(c));
      return at >= 0 ? cells[at] ?? '' : '';
    };
    out.push(normalise({
      surface: pick(/surface|section|page|screen/),
      label: pick(/control|label|name|element/),
      role: pick(/role|type|kind/),
      hover: pick(/hover/),
      pressed: pick(/press/),
      action: pick(/action|click|result|after|outcome/),
      clicked: undefined,
    }));
  }
  return out.filter((e) => e.label);
}

const yesNo = (v) => {
  if (v === true || /^(yes|true|y)\b/i.test(String(v ?? ''))) return true;
  if (v === false || /^(no|false|n)\b/i.test(String(v ?? ''))) return false;
  return null;
};

function normalise(e) {
  const action = String(e.action ?? '');
  return {
    surface: surfaceKey(e.surface ?? ''),
    label: String(e.label ?? ''),
    key: slugify(e.slug ?? e.label ?? ''),
    role: String(e.role ?? ''),
    hover: yesNo(e.hover),
    pressed: yesNo(e.pressed),
    action,
    kind: actionKind(action),
  };
}

function actionKind(action) {
  const a = action.toLowerCase().trim();
  if (!a) return 'unknown';
  if (a.startsWith('not clicked') || a.startsWith('not captured') || a.startsWith('skipped')) return 'skipped';
  if (/^(no (observable )?change|nothing|none|no visible change)/.test(a)) return 'none';
  return 'change';
}

function copyTo(source, destDir, file) {
  mkdirSync(destDir, { recursive: true });
  copyFileSync(source, join(destDir, file));
}

const esc = (v) => String(v).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const cell = (v) => String(v ?? '').replace(/\|/g, '/').replace(/\r?\n/g, ' ');
const stateRank = (s) => (STATE_ORDER.includes(s) ? STATE_ORDER.indexOf(s) : STATE_ORDER.length);

function main() {
  const args = parseArgs(process.argv.slice(2));
  const mac = scan(resolve(args.mac));
  const win = scan(resolve(args.win));
  const docs = resolve(args.docs);
  const html = resolve(args.html);

  // Copy every PNG once into the docs and html trees; both are referenced by file name.
  for (const [side, set] of [['mac', mac], ['win', win]]) {
    for (const item of [...set.shots.values(), ...set.screens.values()]) {
      copyTo(item.path, join(docs, 'parity-screens', side), item.file);
      copyTo(item.path, join(html, side), item.file);
    }
  }

  // Control shots, matched by surface/control/state.
  const shotKeys = [...new Set([...mac.shots.keys(), ...win.shots.keys()])];
  const info = (key) => mac.shots.get(key) ?? win.shots.get(key);
  shotKeys.sort((a, b) => {
    const x = info(a), y = info(b);
    return x.surface.localeCompare(y.surface) || x.control.localeCompare(y.control) || stateRank(x.state) - stateRank(y.state) || a.localeCompare(b);
  });
  const screenKeys = [...new Set([...mac.screens.keys(), ...win.screens.keys()])].sort();

  const matchedShots = shotKeys.filter((k) => mac.shots.has(k) && win.shots.has(k)).length;
  const matchedScreens = screenKeys.filter((k) => mac.screens.has(k) && win.screens.has(k)).length;

  // Inventories merged by surface + control.
  const inv = new Map();
  for (const [side, set] of [['mac', mac], ['win', win]]) {
    for (const e of set.inventory) {
      const key = `${e.surface}__${e.key}`;
      const row = inv.get(key) ?? { surface: e.surface, label: e.label, key: e.key };
      row[side] = e;
      inv.set(key, row);
    }
  }
  const rows = [...inv.values()].sort((a, b) => a.surface.localeCompare(b.surface) || a.key.localeCompare(b.key));
  for (const r of rows) r.parity = parity(r);
  const counts = { same: 0, 'Mac only': 0, 'Windows only': 0, differs: 0 };
  for (const r of rows) counts[r.parity] += 1;

  const img = (side, item) => (item ? `![${cell(item.file)}](parity-screens/${side}/${encodeURI(item.file)})` : '—');
  const md = [];
  md.push('# Pulse screens: Mac and Windows side by side', '');
  md.push(`Mac evidence \`${basename(resolve(args.mac))}\`, Windows screens \`${basename(resolve(args.win))}\`. Images are relative to this file under \`parity-screens/{mac,win}/\`.`, '');
  md.push(`- Control shots: ${shotKeys.length} keys, ${matchedShots} on both sides, ${[...mac.shots.keys()].filter((k) => !win.shots.has(k)).length} Mac only, ${[...win.shots.keys()].filter((k) => !mac.shots.has(k)).length} Windows only.`);
  md.push(`- Screens (named PNGs): ${screenKeys.length} keys, ${matchedScreens} on both sides.`);
  md.push(`- Inventory: ${rows.length} controls: ${counts.same} same, ${counts.differs} differs, ${counts['Mac only']} Mac only, ${counts['Windows only']} Windows only.`, '');

  md.push('## Screens', '', '| Screen | Mac | Windows |', '|---|---|---|');
  for (const key of screenKeys) md.push(`| ${key} | ${img('mac', mac.screens.get(key))} | ${img('win', win.screens.get(key))} |`);

  md.push('', '## Controls: hover, pressed and after click', '');
  let surface = null;
  for (const key of shotKeys) {
    const it = info(key);
    if (it.surface !== surface) {
      surface = it.surface;
      md.push('', `### ${surface}`, '', '| Control | State | Mac | Windows |', '|---|---|---|---|');
    }
    md.push(`| ${it.control} | ${it.state} | ${img('mac', mac.shots.get(key))} | ${img('win', win.shots.get(key))} |`);
  }

  const onlyMac = [...[...mac.shots.keys()].filter((k) => !win.shots.has(k)), ...[...mac.screens.keys()].filter((k) => !win.screens.has(k)).map((k) => `screen:${k}`)];
  const onlyWin = [...[...win.shots.keys()].filter((k) => !mac.shots.has(k)), ...[...win.screens.keys()].filter((k) => !mac.screens.has(k)).map((k) => `screen:${k}`)];
  md.push('', '## Unmatched', '', `### Mac only (${onlyMac.length})`, '', ...(onlyMac.length ? onlyMac.map((k) => `- ${k}`) : ['none']));
  md.push('', `### Windows only (${onlyWin.length})`, '', ...(onlyWin.length ? onlyWin.map((k) => `- ${k}`) : ['none']));

  md.push('', '## Function inventory', '');
  md.push('Parity: **same** = on both sides with the same kind of click outcome (changed something, changed nothing, or left unclicked) and the same press feedback where both report it; **differs** = on both but not; **Mac only** / **Windows only** = listed by one side. Mac hover and press come from computed style and PNG bytes; the Windows columns are what that side recorded.', '');
  md.push('| Surface | Control | Role | Mac hover | Mac press | Mac action | Windows hover | Windows press | Windows action | Parity |', '|---|---|---|---|---|---|---|---|---|---|');
  const yn = (v) => (v === true ? 'yes' : v === false ? 'no' : '-');
  for (const r of rows) {
    const m = r.mac, w = r.win;
    md.push(`| ${cell(r.surface)} | ${cell(r.label)} | ${cell((m ?? w).role)} | ${m ? yn(m.hover) : '-'} | ${m ? yn(m.pressed) : '-'} | ${cell(m?.action ?? '-')} | ${w ? yn(w.hover) : '-'} | ${w ? yn(w.pressed) : '-'} | ${cell(w?.action ?? '-')} | ${r.parity} |`);
  }
  mkdirSync(docs, { recursive: true });
  writeFileSync(join(docs, 'parity-screens.md'), `${md.join('\n')}\n`);

  // The HTML index: the same pairs with the images shown at a readable size.
  const fig = (side, item) => (item ? `<figure><img src="${esc(side)}/${encodeURIComponent(item.file)}" loading="lazy" alt="${esc(item.file)}"></figure>` : '<div class="absent">none</div>');
  const body = [];
  body.push('<h2>Screens</h2>');
  for (const key of screenKeys) body.push(`<section><h3>${esc(key)}</h3><div class="pair">${fig('mac', mac.screens.get(key))}${fig('win', win.screens.get(key))}</div></section>`);
  surface = null;
  for (const key of shotKeys) {
    const it = info(key);
    if (it.surface !== surface) {
      surface = it.surface;
      body.push(`<h2>${esc(surface)}</h2>`);
    }
    body.push(`<section><h3>${esc(it.control)} <small>${esc(it.state)}</small></h3><div class="pair">${fig('mac', mac.shots.get(key))}${fig('win', win.shots.get(key))}</div></section>`);
  }
  mkdirSync(html, { recursive: true });
  writeFileSync(join(html, 'index.html'), `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Pulse screens parity</title>
<style>:root{color-scheme:light dark}body{font:14px system-ui;margin:16px;background:Canvas;color:CanvasText}.pair{display:grid;grid-template-columns:1fr 1fr;gap:12px}img{max-width:100%;border:1px solid #8884}.absent{opacity:.5;padding:24px;border:1px dashed #8886}h3 small{opacity:.6;font-weight:400}</style>
<h1>Pulse screens: Mac | Windows</h1>${body.join('\n')}\n`);
  process.stdout.write(`parity-screens: ${shotKeys.length} control shots (${matchedShots} paired), ${screenKeys.length} screens (${matchedScreens} paired), ${rows.length} inventory rows -> ${join(docs, 'parity-screens.md')}, ${join(html, 'index.html')}\n`);
}

function parity(r) {
  if (!r.win) return 'Mac only';
  if (!r.mac) return 'Windows only';
  const m = r.mac, w = r.win;
  if (m.kind !== 'unknown' && w.kind !== 'unknown' && m.kind !== w.kind) return 'differs';
  if (m.pressed !== null && w.pressed !== null && m.pressed !== w.pressed) return 'differs';
  return 'same';
}

main();
