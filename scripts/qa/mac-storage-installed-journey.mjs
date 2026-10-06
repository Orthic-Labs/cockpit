import assert from "node:assert/strict";
import { access, mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { addStorageGrowthFile } from "./mac-storage-fixtures.mjs";

function imageSize(bytes) {
  const word = offset => bytes[offset] * 256 + bytes[offset + 1];
  if (bytes[0] === 0x89 && bytes[1] === 0x50) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    return { width: view.getUint32(16), height: view.getUint32(20) };
  }
  assert.ok(bytes[0] === 0xff && bytes[1] === 0xd8, "Expected an actual JPEG/PNG capture");
  for (let offset = 2; offset + 9 < bytes.length;) {
    assert.equal(bytes[offset], 0xff, "Malformed screenshot marker");
    const marker = bytes[offset + 1];
    if (marker >= 0xc0 && marker <= 0xcf && ![0xc4, 0xc8, 0xcc].includes(marker)) {
      return { width: word(offset + 7), height: word(offset + 5) };
    }
    const length = word(offset + 2);
    assert.ok(length >= 2, "Malformed screenshot segment");
    offset += 2 + length;
  }
  throw Error("Screenshot dimensions unavailable");
}

function controlLines(ax) { return ax.split("\n").filter(line => /^\s*\d+ /.test(line)); }

function locate(ax, predicate, phase) {
  const matches = controlLines(ax).filter(predicate);
  assert.equal(matches.length, 1, `${phase}: expected one AX control, found ${matches.length}`);
  return Number(matches[0].trim().split(" ")[0]);
}

function hasButton(label) {
  return line => {
    const match = line.match(/^\s*\d+ (?:button|toolbar item) (.*?)(?:,|$)/);
    return match?.[1] === label;
  };
}

function controlIncluding(kind, value) {
  return line => line.includes(`${kind} `) && line.includes(value);
}

async function fingerprint(file) {
  const bytes = await readFile(file);
  const info = await stat(file);
  return { path: file, size: info.size, dev: info.dev, ino: info.ino, sha256: createHash("sha256").update(bytes).digest("hex") };
}

function assertFingerprint(actual, expected, label) {
  assert.equal(actual.size, expected.size, `${label}: size changed`);
  assert.equal(actual.sha256, expected.sha256, `${label}: SHA-256 changed`);
  assert.equal(actual.dev, expected.dev, `${label}: volume identity changed`);
  assert.equal(actual.ino, expected.ino, `${label}: file identity changed`);
}

/**
 * Full installed Mac journey. All controls are freshly located from AX output;
 * dashboard DOM seams, injected state, mocks, and shell UI control are excluded.
 * `onRelaunch` is required because app quit/relaunch belongs to the CUA harness;
 * it must return `{ app: freshApp, restarted: true }` after observed CUA work.
 */
export async function runInstalledStorageJourney(initialApp, options) {
  const { appBundle, fixture, output, onRelaunch, onCheckpoint = () => {}, onRingHover } = options;
  assert.ok(initialApp && typeof initialApp.getAXState === "function", "CUA app is required");
  assert.ok(path.isAbsolute(appBundle) && path.isAbsolute(output), "appBundle/output must be absolute");
  assert.ok(fixture?.root && fixture?.discard && fixture?.duplicateA && fixture?.duplicateB && fixture?.sourcePng && fixture?.growthFile,
    "fixture must include root, discard, duplicateA, duplicateB, sourcePng, growthFile");
  assert.equal(typeof onRelaunch, "function", "onRelaunch callback is required for restart coverage");
  await mkdir(output, { recursive: true });
  let app = initialApp;
  const report = { status: "running", appBundle, startedAt: new Date().toISOString(), phases: [], checkpoints: [] };
  const originalFixtureFiles = [fixture.duplicateA, fixture.duplicateB, fixture.discard, fixture.sourcePng];
  const bundleFiles = ["Contents/MacOS/Cockpit", "Contents/Helpers/cockpit", "Contents/Resources/dashboard/app.js", "Contents/Resources/dashboard/index.html"]
    .map(file => path.join(appBundle, file));
  const before = await Promise.all([...originalFixtureFiles, ...bundleFiles].map(fingerprint));
  report.inputs = before;
  let phase = "launch-and-native-scan";

  const state = () => app.getAXState({ emit: false, disableDiffing: true });
  const waitFor = async (predicate, description, timeout = 30_000) => {
    const deadline = Date.now() + timeout;
    let ax = await state();
    do {
      if (predicate(ax)) return ax;
      ax = await state();
    } while (Date.now() < deadline);
    throw Error(`${phase}: ${description}`);
  };
  const click = async predicate => { const ax = await state(); await app.click(locate(ax, predicate, phase)); return state(); };
  const checkpoint = async name => {
    const ax = await state();
    await writeFile(path.join(output, `${name}.txt`), ax);
    const capture = await app.getScreenshot({ emit: false });
    const frame = imageSize(capture);
    await writeFile(path.join(output, `${name}.jpg`), capture);
    report.checkpoints.push({ name, frame, at: new Date().toISOString() });
    await writeFile(path.join(output, "result.json"), `${JSON.stringify(report, null, 2)}\n`);
    await onCheckpoint(name);
    return ax;
  };
  const phaseDone = async (name, detail = {}) => { report.phases.push({ name, status: "passed", at: new Date().toISOString(), ...detail }); await checkpoint(name); };

  const pickerChoose = async (target) => {
    await app.pressKey("super+shift+g");
    await app.typeText(target);
    await app.pressKey("Return");
    const picker = await waitFor(ax => ax.includes(path.basename(target)), `native picker must show ${target}`);
    const buttons = controlLines(picker).filter(line => /^\s*\d+ (?:button|toolbar item) (?:Open|Choose|Select)(?:,|$)/.test(line));
    assert.equal(buttons.length, 1, `${phase}: native picker must expose one observed acceptance button`);
    await app.click(Number(buttons[0].trim().split(" ")[0]));
  };
  const navigate = async label => click(hasButton(label));
  const assertLiveFile = async file => { await access(file); return fingerprint(file); };

  try {
    phase = "launch-and-native-scan";
    let ax = await state();
    if (ax.includes("system dialog Cockpit — Notch")) ax = await click(controlIncluding("button", "Open Cockpit dashboard"));
    await click(line => /(?:button|toolbar item) Scan folder(?:,|$)/.test(line));
    await pickerChoose(fixture.root);
    ax = await waitFor(value => value.includes("Storage") && /Loaded \d+ entries/.test(value), "native folder scan must load Storage view");
    assert.ok(ax.includes(path.basename(fixture.root)), "native scan must render fixture root evidence");
    const firstSnapshot = await Promise.all(originalFixtureFiles.map(fingerprint));
    report.firstSnapshot = firstSnapshot;
    await phaseDone(phase);

    phase = "changed-fixture-rescan-and-growth";
    const growth = await addStorageGrowthFile(fixture);
    await access(growth.path);
    await click(line => /(?:button|toolbar item) Scan folder(?:,|$)/.test(line));
    await pickerChoose(fixture.root);
    ax = await waitFor(value => value.includes("Storage") && /Loaded \d+ entries/.test(value), "changed fixture scan must load Storage view");
    assert.ok(ax.includes("Folder growth") && ax.includes("Growing") && ax.includes(fixture.root) && /\+\d/.test(ax),
      "changed fixture scan must render positive folder growth for fixture root");
    for (let index = 0; index < originalFixtureFiles.length; index += 1) {
      assertFingerprint(await fingerprint(originalFixtureFiles[index]), firstSnapshot[index], `first snapshot original ${originalFixtureFiles[index]}`);
    }
    await phaseDone(phase, { growth });

    phase = "storage-drill-and-inspector";
    ax = await click(controlIncluding("button", `Open ${fixture.nested}`));
    assert.ok(ax.includes(fixture.duplicateB), "storage drill must render nested duplicate");
    ax = await click(controlIncluding("button", `Inspect ${fixture.duplicateB}`));
    assert.ok(ax.includes("Inspector") && ax.includes(fixture.duplicateB) && ax.includes("Metadata"), "inspector must show selected fixture");
    await phaseDone(phase);

    phase = "find-filter";
    await navigate("Find");
    ax = await click(line => line.includes("search text field") && line.includes("Name or path"));
    await app.pressKey("super+a");
    await app.typeText(path.basename(fixture.discard));
    ax = await waitFor(value => value.includes(`Value: ${path.basename(fixture.discard)}`), "AX search field must retain typed filter");
    await app.pressKey("Return");
    ax = await waitFor(value => value.includes("1 matches") && value.includes(fixture.discard), "filter must show exactly discard fixture");
    await phaseDone(phase);

    phase = "native-duplicates-opt-in";
    await navigate("Duplicates");
    ax = await click(line => /button (?:Inspect content for duplicates|Re-run content inspection)(?:,|$)/.test(line));
    ax = await waitFor(value => value.includes("Exact duplicate groups") && value.includes(fixture.duplicateA) && value.includes(fixture.duplicateB), "native duplicate inspection must report both fixture paths");
    assert.ok(ax.includes("2 files considered") || ax.includes("1 groups shown"), "duplicate summary must be rendered");
    await phaseDone(phase);

    phase = "native-trash-review-and-apply";
    await navigate("Storage");
    ax = await click(controlIncluding("button", `Inspect ${fixture.discard}`));
    ax = await click(line => /button .*review move to trash(?:,|$)/i.test(line));
    ax = await waitFor(value => value.includes("Move selected files to Trash?") && value.includes("Move to Trash"), "native Trash review must be visible");
    await app.click(locate(ax, hasButton("Move to Trash"), phase));
    ax = await waitFor(value => value.includes("Review ready") || value.includes("Cleanup complete"), "native review must return to dashboard");
    ax = await click(hasButton("Apply cleanup"));
    ax = await waitFor(value => value.includes("Cleanup complete") && value.includes("moved to Trash"), "native cleanup apply must complete");
    await assert.rejects(() => access(fixture.discard), /ENOENT/, "discard fixture must be absent after native apply");
    await navigate("Cleanup");
    ax = await waitFor(value => value.includes("Cleanup history"), "cleanup history must render applied plan");
    assert.ok(!/\b(?:interrupted|indeterminate|failed|partial)\b/i.test(ax), "partial native cleanup outcome cannot be reported as success");
    await phaseDone(phase);

    phase = "quit-relaunch-and-undo";
    const relaunched = await onRelaunch({ app, phase, fixture, report });
    assert.ok(relaunched?.restarted === true, "onRelaunch must report observed quit/relaunch completion");
    app = relaunched.app;
    assert.ok(app && typeof app.getAXState === "function", "onRelaunch must return fresh CUA app");
    ax = await waitFor(value => value.includes("Storage") || value.includes("Loaded"), "relaunch must return to native dashboard");
    await navigate("Activity");
    ax = await click(hasButton("Refresh activity"));
    ax = await waitFor(value => value.includes("Cleanup, scan, compression") && value.includes("Cleanup history"), "activity must reload durable cleanup journal");
    ax = await click(hasButton("Undo"));
    ax = await waitFor(value => value.includes("Undo complete") && value.includes("restored"), "native Undo must complete after relaunch");
    const restored = await assertLiveFile(fixture.discard);
    assertFingerprint(restored, before[originalFixtureFiles.indexOf(fixture.discard)], "restored discard fixture");
    await navigate("Cleanup");
    ax = await waitFor(value => value.includes("Cleanup history"), "cleanup history must render restored plan");
    assert.ok(!/\b(?:interrupted|indeterminate|failed|partial)\b/i.test(ax), "partial native Undo outcome cannot be reported as success");
    await phaseDone(phase, { restored: { sha256: restored.sha256, dev: restored.dev, ino: restored.ino } });

    phase = "native-compression-picker";
    const sourceBefore = await assertLiveFile(fixture.sourcePng);
    await navigate("Compress");
    await click(hasButton("Choose media & compress"));
    await pickerChoose(fixture.sourcePng);
    await pickerChoose(fixture.compressionOutput ?? fixture.root);
    ax = await waitFor(value => value.includes("Latest result") && value.includes(fixture.sourcePng) && value.includes("Output"), "native compression must report source/output");
    const expectedOutputPrefix = `${path.basename(fixture.sourcePng, path.extname(fixture.sourcePng))}-compressed-`;
    assert.ok(ax.includes(expectedOutputPrefix) && ax.includes(".jpg"), "compression output must use native generated source-image-compressed-*.jpg path");
    const outputDirectory = fixture.compressionOutput ?? fixture.root;
    const generated = (await readdir(outputDirectory)).filter(name => name.startsWith(expectedOutputPrefix) && name.endsWith(".jpg"));
    assert.equal(generated.length, 1, "Compression must create exactly one actual output");
    const encodedPath = path.join(outputDirectory, generated[0]);
    const encoded = await readFile(encodedPath);
    assert.ok(encoded.length > 0 && encoded[0] === 0xff && encoded[1] === 0xd8, "Output must contain actual JPEG data");
    assert.deepEqual(imageSize(encoded), { width: 96, height: 64 }, "Default compression must preserve fixture dimensions");
    report.compressionOutput = await fingerprint(encodedPath);
    const sourceAfter = await assertLiveFile(fixture.sourcePng);
    assertFingerprint(sourceAfter, sourceBefore, "compression source");
    await phaseDone(phase);

    for (const view of ["Apps", "Monitor", "Activity"]) {
      phase = `native-${view.toLowerCase()}`;
      await navigate(view);
      const action = view === "Apps" ? "Refresh app inventory" : view === "Monitor" ? "Refresh readings" : "Refresh activity";
      ax = await click(hasButton(action));
      const expected = view === "Apps" ? "Application inventory" : view === "Monitor" ? "Storage volumes" : "Cleanup, scan, compression";
      await waitFor(value => value.includes(expected), `${view} native reading must render`);
      await phaseDone(phase);
    }

    phase = "per-ring-hover";
    if (typeof onRingHover === "function") {
      const observed = await onRingHover({ app, state, report });
      assert.ok(observed && Array.isArray(observed.rings) && observed.rings.length > 0, "ring hover callback must return observed ring AX evidence");
      for (const ring of observed.rings) {
        assert.ok(typeof ring.metric === "string" && ring.metric.length > 0 && ring.rendered === true,
          "ring hover evidence must identify each rendered metric-specific card");
      }
      report.phases.push({ name: phase, status: "passed", at: new Date().toISOString(), rings: observed.rings });
      await checkpoint(phase);
    } else {
      report.phases.push({ name: phase, status: "unrun", reason: "CUA ring-hover callback was not supplied", at: new Date().toISOString() });
      await checkpoint(phase);
    }

    const finalFixtureFiles = [...originalFixtureFiles, fixture.growthFile];
    const after = await Promise.all([...finalFixtureFiles, ...bundleFiles].map(fingerprint));
    for (let index = 0; index < originalFixtureFiles.length; index += 1) {
      assertFingerprint(after[index], before[index], `read-only fixture integrity ${before[index].path}`);
    }
    for (let index = 0; index < bundleFiles.length; index += 1) {
      assertFingerprint(after[finalFixtureFiles.length + index], before[originalFixtureFiles.length + index], `bundle integrity ${bundleFiles[index]}`);
    }
    report.finalFingerprints = after.slice(0, finalFixtureFiles.length);
    report.status = "passed";
    report.finishedAt = new Date().toISOString();
    await writeFile(path.join(output, "result.json"), `${JSON.stringify(report, null, 2)}\n`);
    return report;
  } catch (error) {
    report.status = "failed";
    report.failedPhase = phase;
    report.error = String(error);
    report.finishedAt = new Date().toISOString();
    await writeFile(path.join(output, "failure.txt"), await state()).catch(() => {});
    await app.getScreenshot({ emit: false }).then(bytes => writeFile(path.join(output, "failure.jpg"), bytes)).catch(() => {});
    await writeFile(path.join(output, "result.json"), `${JSON.stringify(report, null, 2)}\n`);
    throw error;
  }
}
