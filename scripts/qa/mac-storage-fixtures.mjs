import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { lstat, mkdir, open, readFile, readdir, realpath, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { deflateSync } from "node:zlib";

const DUPLICATE_BYTES = 128 * 1024;

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const typeBytes = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBytes, data]);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([length, body, checksum]);
}

// Deterministic, dependency-free RGBA PNG for native media-picker coverage.
function makePng(width = 96, height = 64) {
  const rows = [];
  for (let y = 0; y < height; y += 1) {
    const row = Buffer.alloc(1 + width * 4);
    for (let x = 0; x < width; x += 1) {
      const offset = 1 + x * 4;
      row[offset] = (x * 3 + y) % 256;
      row[offset + 1] = (y * 5 + 31) % 256;
      row[offset + 2] = (x + y * 2 + 67) % 256;
      row[offset + 3] = 255;
    }
    rows.push(row);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header[8] = 8;
  header[9] = 6;
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  return Buffer.concat([
    signature,
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(Buffer.concat(rows), { level: 9 })),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

async function exclusiveWrite(file, bytes) {
  const handle = await open(file, "wx", 0o600);
  try { await handle.writeFile(bytes); } finally { await handle.close(); }
}

function under(parent, child) {
  const root = path.resolve(parent);
  const value = path.resolve(child);
  return value === root || value.startsWith(`${root}${path.sep}`);
}

async function assertPrivateDirectory(directory) {
  const info = await lstat(directory);
  assert.equal(info.isSymbolicLink(), false, `Fixture directory cannot be a symlink: ${directory}`);
  assert.equal(info.isDirectory(), true, `Fixture path must be a directory: ${directory}`);
  assert.equal(info.mode & 0o777, 0o700, `Fixture directory must be owner-only: ${directory}`);
  if (typeof process.getuid === "function") assert.equal(info.uid, process.getuid(), `Fixture directory owner changed: ${directory}`);
}

/**
 * Make an isolated real-file fixture for mac-storage-installed-journey.mjs.
 * `destination` is caller-owned and must be a new directory below Documents.
 * No existing path is ever removed or overwritten.
 */
export async function createStorageFixture({ destination }) {
  assert.ok(typeof destination === "string" && path.isAbsolute(destination), "destination must be absolute");
  const documents = path.join(os.homedir(), "Documents");
  assert.ok(under(documents, destination) && path.resolve(destination) !== path.resolve(documents),
    "fixture destination must be a new path below ~/Documents");
  const trash = path.join(os.homedir(), ".Trash");
  const destinationParent = path.dirname(path.resolve(destination));
  const [documentsReal, parentReal, homeInfo] = await Promise.all([realpath(documents), realpath(destinationParent), stat(os.homedir())]);
  assert.ok(under(documentsReal, path.join(parentReal, path.basename(destination))), "canonical fixture path must remain below Documents");
  const [parentInfo, trashInfo] = await Promise.all([stat(parentReal), stat(trash).catch(() => homeInfo)]);
  assert.equal(parentInfo.dev, trashInfo.dev, "fixture must share volume with ~/.Trash");
  await mkdir(destination, { recursive: false, mode: 0o700 });

  const nested = path.join(destination, "nested-folder");
  const compressionOutput = path.join(destination, "compression-output");
  await mkdir(nested, { mode: 0o700 });
  await mkdir(compressionOutput, { mode: 0o700 });
  await Promise.all([destination, nested, compressionOutput].map(assertPrivateDirectory));
  const duplicate = Buffer.alloc(DUPLICATE_BYTES, 0x5a);
  const paths = {
    root: destination,
    nested,
    compressionOutput,
    duplicateA: path.join(destination, "duplicate-a.bin"),
    duplicateB: path.join(nested, "duplicate-b.bin"),
    duplicateC: path.join(nested, "duplicate-c.bin"),
    discard: path.join(destination, "discard-me.txt"),
    sourcePng: path.join(destination, "source-image.png"),
    sourceVideo: path.join(destination, "source-video.mp4"),
    growthFile: path.join(destination, "growth-after-first-scan.bin"),
    hiddenFile: path.join(destination, ".fixture-hidden.txt"),
    replayFile: path.join(destination, "fixture-replay-after-relaunch.txt"),
  };
  paths.indexFiles = Array.from({ length: 6 }, (_, index) =>
    path.join(destination, `fixture-index-${String(index + 1).padStart(2, "0")}.txt`));
  await exclusiveWrite(paths.duplicateA, duplicate);
  await exclusiveWrite(paths.duplicateB, duplicate);
  await exclusiveWrite(paths.duplicateC, duplicate);
  await exclusiveWrite(paths.discard, Buffer.from("Cockpit installed journey discard fixture\n", "utf8"));
  await exclusiveWrite(paths.sourcePng, makePng());
  // Synthetic 96x64, two-second H.264 fixture; no user media or codec dependency.
  await exclusiveWrite(paths.sourceVideo, await readFile(new URL("./fixtures/storage-source-video.mp4", import.meta.url)));
  await exclusiveWrite(paths.hiddenFile, Buffer.from("Hidden filename-index fixture\n", "utf8"));
  await Promise.all(paths.indexFiles.map((file, index) => exclusiveWrite(file, Buffer.from(`Filename index fixture ${index + 1}\n`, "utf8"))));
  await Promise.all([paths.duplicateA, paths.duplicateB, paths.duplicateC, paths.discard, paths.sourcePng, paths.sourceVideo, paths.hiddenFile, ...paths.indexFiles].map(async file => {
    const info = await lstat(file);
    assert.equal(info.isSymbolicLink(), false, `Fixture child cannot be a symlink: ${file}`);
    assert.equal(info.isFile(), true, `Fixture child must be a regular file: ${file}`);
  }));

  const names = await readdir(destination);
  assert.deepEqual(new Set(names), new Set([
    "compression-output", "discard-me.txt", "duplicate-a.bin", "nested-folder", "source-image.png", "source-video.mp4", ".fixture-hidden.txt",
    ...paths.indexFiles.map(file => path.basename(file)),
  ]));
  const fingerprint = async file => ({
    path: file,
    bytes: (await stat(file)).size,
    sha256: (await import("node:crypto")).createHash("sha256").update(await readFile(file)).digest("hex"),
  });
  return {
    ...paths,
    files: await Promise.all([paths.duplicateA, paths.duplicateB, paths.duplicateC, paths.discard, paths.sourcePng, paths.sourceVideo, paths.hiddenFile, ...paths.indexFiles].map(fingerprint)),
    duplicateBytes: DUPLICATE_BYTES,
    trash,
  };
}

/** Add one new exclusive file after first native scan, for persisted growth evidence. */
export async function addStorageGrowthFile(fixture) {
  assert.ok(fixture?.growthFile && under(fixture.root, fixture.growthFile), "growth file must belong to fixture root");
  const bytes = Buffer.alloc(32 * 1024, 0x37);
  await exclusiveWrite(fixture.growthFile, bytes);
  return { path: fixture.growthFile, bytes: bytes.length };
}

/** Add one exclusive filename-index replay file while the app is closed. */
export async function addStorageReplayFile(fixture) {
  assert.ok(fixture?.replayFile && under(fixture.root, fixture.replayFile), "replay file must belong to fixture root");
  const bytes = Buffer.from("Filename index replay fixture after relaunch\n", "utf8");
  await exclusiveWrite(fixture.replayFile, bytes);
  return { path: fixture.replayFile, bytes: bytes.length };
}

/** Create one private, valid, no-process app bundle at caller-selected ~/Applications path. */
export async function createDisposableAppBundle({ destination }) {
  assert.ok(typeof destination === "string" && path.isAbsolute(destination), "application destination must be absolute");
  const applications = path.join(os.homedir(), "Applications");
  assert.ok(under(applications, destination) && path.extname(destination).toLowerCase() === ".app",
    "application destination must be a new .app path below ~/Applications");
  const applicationsInfo = await lstat(applications).catch(() => null);
  if (!applicationsInfo) await mkdir(applications, { mode: 0o700 });
  const [applicationsReal, trashInfo] = await Promise.all([
    realpath(applications),
    stat(path.join(os.homedir(), ".Trash")).catch(() => stat(os.homedir())),
  ]);
  const destinationParent = path.dirname(path.resolve(destination));
  const destinationParentReal = await realpath(destinationParent);
  const canonicalDestination = path.join(destinationParentReal, path.basename(destination));
  assert.ok(under(applicationsReal, canonicalDestination), "canonical application path must remain below ~/Applications");
  const parentInfo = await stat(destinationParent);
  assert.equal(parentInfo.dev, trashInfo.dev, "application fixture must share volume with ~/.Trash");
  await mkdir(destination, { recursive: false, mode: 0o700 });
  const contents = path.join(destination, "Contents");
  const resources = path.join(contents, "Resources");
  await mkdir(contents, { mode: 0o700 });
  await mkdir(resources, { mode: 0o700 });
  await Promise.all([destination, contents, resources].map(assertPrivateDirectory));

  const uuid = randomUUID();
  const bundleID = `com.cockpit.fixture.${uuid.replaceAll("-", "")}`;
  const infoPlist = path.join(contents, "Info.plist");
  const marker = path.join(resources, "journey-marker.txt");
  const plist = `<?xml version="1.0" encoding="UTF-8"?>\n<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>${bundleID}</string><key>CFBundleName</key><string>${path.basename(destination, ".app")}</string><key>CFBundleDisplayName</key><string>${path.basename(destination, ".app")}</string><key>CFBundleVersion</key><string>1</string><key>CFBundleShortVersionString</key><string>1.0</string></dict></plist>\n`;
  await exclusiveWrite(infoPlist, Buffer.from(plist, "utf8"));
  await exclusiveWrite(marker, Buffer.from(`Cockpit installed app fixture ${uuid}\n`, "utf8"));
  for (const file of [infoPlist, marker]) {
    const info = await lstat(file);
    assert.equal(info.isSymbolicLink(), false, `Application fixture child cannot be a symlink: ${file}`);
    assert.equal(info.isFile(), true, `Application fixture child must be a regular file: ${file}`);
  }
  return { path: destination, bundleID, name: path.basename(destination, ".app"), infoPlist, marker, uuid };
}
