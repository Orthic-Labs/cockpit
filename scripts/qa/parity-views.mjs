#!/usr/bin/env node
// Side-by-side Mac vs Windows parity page for every view in qa/notch-views.json.
//
// Usage:
//   node scripts/qa/parity-views.mjs --mac <dir> --win <dir> [--gaps <windows-gaps.txt>] --out <dir>
//
// Inputs: <dir>/<id>.png for each view id (the Mac renderer and viewshots.rs both write this shape).
// --gaps is the windows-gaps.txt written beside the Windows PNGs: one tab-separated
// "id<TAB>title<TAB>reason" row per gap. Blank lines and lines starting with # are ignored.
// Output: <out>/index.html plus copies of the PNGs in <out>/mac and <out>/win.

import {
  closeSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, '..', '..');
const viewsPath = join(repoRoot, 'qa', 'notch-views.json');

const USAGE = 'usage: node scripts/qa/parity-views.mjs --mac <dir> --win <dir> [--gaps <windows-gaps.txt>] --out <dir>';
const FLAGS = ['--mac', '--win', '--gaps', '--out'];
const PNG_SIGNATURE = '89504e470d0a1a0a';
const RETINA_SCALE = 2;

// Status chip labels, in the order they are reported.
const STATUS = {
  both: 'Both',
  gap: 'Windows gap',
  missingMac: 'Missing Mac',
  missingWin: 'Missing Windows',
};
const STATUS_CLASS = {
  [STATUS.both]: 'chip-both',
  [STATUS.gap]: 'chip-gap',
  [STATUS.missingMac]: 'chip-missing',
  [STATUS.missingWin]: 'chip-missing',
};

function fail(message) {
  process.stderr.write(`parity-views: ${message}\n`);
  process.exit(2);
}

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    if (flag === '--help' || flag === '-h') {
      process.stdout.write(`${USAGE}\n`);
      process.exit(0);
    }
    if (!FLAGS.includes(flag)) fail(`unknown argument ${flag}\n${USAGE}`);
    const value = argv[i + 1];
    if (value === undefined || value.startsWith('--')) fail(`${flag} needs a value\n${USAGE}`);
    args[flag.slice(2)] = value;
    i += 1;
  }
  for (const key of ['mac', 'win', 'out']) {
    if (!args[key]) fail(`missing --${key}\n${USAGE}`);
  }
  return args;
}

// Reads width and height from the PNG IHDR chunk without decoding the image.
function pngSize(file) {
  const header = Buffer.alloc(24);
  const fd = openSync(file, 'r');
  try {
    if (readSync(fd, header, 0, 24, 0) !== 24) throw new Error(`file too short to be a PNG: ${file}`);
  } finally {
    closeSync(fd);
  }
  if (header.subarray(0, 8).toString('hex') !== PNG_SIGNATURE || header.toString('ascii', 12, 16) !== 'IHDR') {
    throw new Error(`not a PNG: ${file}`);
  }
  return { width: header.readUInt32BE(16), height: header.readUInt32BE(20) };
}

function readGaps(file) {
  const ids = new Set();
  for (const raw of readFileSync(file, 'utf8').split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const token = line.split(/[\t ]/)[0].replace(/\.png$/, '');
    ids.add(token);
  }
  return ids;
}

function esc(value) {
  return String(value).replace(/[&<>"']/g, (char) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  })[char]);
}

function slugify(text) {
  return String(text).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'area';
}

// Copies a PNG into the output tree unless it already is the destination file.
function copyPng(source, destination) {
  mkdirSync(dirname(destination), { recursive: true });
  if (resolve(source) !== resolve(destination)) copyFileSync(source, destination);
}

function figure(label, side, image) {
  if (!image) {
    return `<figure class="side side-${side}"><figcaption>${label}</figcaption>` +
      `<div class="absent">No ${label} image</div></figure>`;
  }
  return `<figure class="side side-${side}"><figcaption>${label}</figcaption>` +
    `<img src="${esc(image.src)}" width="${image.width}" height="${image.height}" alt="${esc(`${label}: ${image.alt}`)}" loading="lazy">` +
    '</figure>';
}

function renderRow(view, status, mac, win) {
  const chipClass = STATUS_CLASS[status];
  return [
    `<section class="row" id="view-${esc(view.id)}">`,
    '<header class="row-meta">',
    `<div class="row-top"><code>${esc(view.id)}</code><span class="chip ${chipClass}">${esc(status)}</span></div>`,
    `<h3>${esc(view.title)}</h3>`,
    `<p>${esc(view.description)}</p>`,
    '</header>',
    '<div class="pair">',
    figure('Mac', 'mac', mac),
    figure('Windows', 'win', win),
    '</div>',
    '</section>',
  ].join('\n');
}

function renderPage({ groups, counts, unknownGaps }) {
  const areaIndex = [...groups.entries()].map(([area, rows]) =>
    `<a href="#area-${esc(slugify(area))}">${esc(area)} <span>${rows.length}</span></a>`).join('');

  const sections = [...groups.entries()].map(([area, rows]) =>
    `<section class="area" id="area-${esc(slugify(area))}">` +
    `<h2>${esc(area)} <span class="count">${rows.length} views</span></h2>` +
    rows.join('\n') +
    '</section>').join('\n');

  const unknownNote = unknownGaps.length
    ? `<p class="warn">Gap ids not in notch-views.json: ${esc(unknownGaps.join(', '))}</p>`
    : '';

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Notch parity: Mac vs Windows</title>
<style>
:root { color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; background: #111; color: #e6e6e6; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; line-height: 1.45; }
main { padding: 0 16px 48px; max-width: 1400px; margin: 0 auto; }
header.top { padding: 20px 0 12px; }
h1 { font-size: 22px; margin: 0 0 10px; font-weight: 650; }
.summary { display: flex; flex-wrap: wrap; gap: 8px; margin: 0; padding: 0; list-style: none; }
.summary li { background: #1c1c1e; border: 1px solid #2c2c2e; border-radius: 8px; padding: 6px 10px; font-size: 14px; }
.summary b { font-variant-numeric: tabular-nums; }
.warn { color: #ffd77a; font-size: 14px; margin: 10px 0 0; }
nav.index { position: sticky; top: 0; z-index: 10; background: #111; border-bottom: 1px solid #2c2c2e; padding: 10px 0; margin: 0 -16px; padding-left: 16px; padding-right: 16px; display: flex; flex-wrap: wrap; gap: 6px 10px; font-size: 13px; }
nav.index a { color: #9ec7ff; text-decoration: none; background: #1c1c1e; border-radius: 6px; padding: 4px 8px; }
nav.index a span { color: #888; }
section.area { margin-top: 28px; }
section.area h2 { font-size: 18px; margin: 0 0 12px; padding-bottom: 6px; border-bottom: 1px solid #2c2c2e; scroll-margin-top: 60px; }
section.area h2 .count { color: #888; font-size: 13px; font-weight: 400; margin-left: 6px; }
.row { background: #171717; border: 1px solid #2a2a2a; border-radius: 10px; padding: 14px; margin-bottom: 14px; scroll-margin-top: 60px; }
.row-top { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; justify-content: space-between; }
.row code { font-size: 13px; color: #b5b5b5; word-break: break-all; }
.row h3 { font-size: 15px; margin: 8px 0 2px; font-weight: 600; }
.row p { font-size: 13px; color: #a0a0a0; margin: 0 0 10px; }
.chip { font-size: 12px; border-radius: 999px; padding: 2px 10px; white-space: nowrap; font-weight: 600; }
.chip-both { background: #17361f; color: #9ff0b6; }
.chip-gap { background: #3d2f0a; color: #ffd77a; }
.chip-missing { background: #3b1717; color: #ffb3b3; }
.pair { display: grid; grid-template-columns: repeat(2, minmax(0, auto)); gap: 16px; align-items: start; justify-content: start; }
figure { margin: 0; min-width: 0; }
figcaption { font-size: 12px; color: #888; text-transform: uppercase; letter-spacing: 0.06em; margin-bottom: 6px; }
figure img { display: block; max-width: 100%; height: auto; border-radius: 6px; border: 1px solid #2a2a2a; }
.absent { border: 1px dashed #444; border-radius: 6px; color: #777; font-size: 13px; padding: 24px 16px; text-align: center; min-width: 200px; }
@media (max-width: 699px) {
  .pair { grid-template-columns: minmax(0, 1fr); }
  .row { padding: 12px; }
}
</style>
</head>
<body>
<main>
<header class="top">
<h1>Notch parity: Mac vs Windows</h1>
<ul class="summary">
<li>Views <b>${counts.total}</b></li>
<li>Both <b>${counts.both}</b></li>
<li>Windows gap <b>${counts.gap}</b></li>
<li>Missing Mac <b>${counts.missingMac}</b></li>
<li>Missing Windows <b>${counts.missingWin}</b></li>
</ul>
${unknownNote}
</header>
<nav class="index">${areaIndex}</nav>
${sections}
</main>
</body>
</html>
`;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const macDir = resolve(args.mac);
  const winDir = resolve(args.win);
  const outDir = resolve(args.out);

  const views = JSON.parse(readFileSync(viewsPath, 'utf8'));
  if (!Array.isArray(views)) fail(`${viewsPath} must hold a JSON array`);

  let gaps = new Set();
  if (args.gaps) {
    const gapsPath = resolve(args.gaps);
    if (!existsSync(gapsPath)) fail(`gaps file not found: ${gapsPath}`);
    gaps = readGaps(gapsPath);
  }

  const viewIds = new Set(views.map((view) => view.id));
  const unknownGaps = [...gaps].filter((id) => !viewIds.has(id));

  const counts = { total: views.length, both: 0, gap: 0, missingMac: 0, missingWin: 0 };
  const groups = new Map();

  for (const view of views) {
    const id = view.id;
    const macFile = join(macDir, `${id}.png`);
    const winFile = join(winDir, `${id}.png`);
    const macPresent = existsSync(macFile);
    const winPresent = existsSync(winFile);
    const isGap = gaps.has(id);

    let status;
    if (!macPresent) status = STATUS.missingMac;
    else if (isGap) status = STATUS.gap;
    else if (!winPresent) status = STATUS.missingWin;
    else status = STATUS.both;

    if (status === STATUS.both) counts.both += 1;
    else if (status === STATUS.gap) counts.gap += 1;
    else if (status === STATUS.missingMac) counts.missingMac += 1;
    else counts.missingWin += 1;

    const mac = macPresent ? placeImage(macFile, `mac/${id}.png`, join(outDir, 'mac', `${id}.png`), view) : null;
    const win = winPresent ? placeImage(winFile, `win/${id}.png`, join(outDir, 'win', `${id}.png`), view) : null;

    const area = view.area || 'other';
    if (!groups.has(area)) groups.set(area, []);
    groups.get(area).push(renderRow(view, status, mac, win));
  }

  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, 'index.html'), renderPage({ groups, counts, unknownGaps }));

  process.stdout.write(`parity-views: ${counts.total} views -> ${join(outDir, 'index.html')}\n`);
  process.stdout.write(`  Both: ${counts.both}\n`);
  process.stdout.write(`  Windows gap: ${counts.gap}\n`);
  process.stdout.write(`  Missing Mac: ${counts.missingMac}\n`);
  process.stdout.write(`  Missing Windows: ${counts.missingWin}\n`);
  if (unknownGaps.length) process.stdout.write(`  Gap ids not in notch-views.json: ${unknownGaps.length}\n`);
}

// Copies one PNG into the output tree and returns the attributes for its <img>.
function placeImage(sourceFile, relativeSrc, destinationFile, view) {
  const { width, height } = pngSize(sourceFile);
  copyPng(sourceFile, destinationFile);
  return {
    src: relativeSrc,
    width: Math.round(width / RETINA_SCALE),
    height: Math.round(height / RETINA_SCALE),
    alt: view.title,
  };
}

main();
