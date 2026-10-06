import assert from "node:assert/strict";
import { access, mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { addStorageGrowthFile, addStorageReplayFile } from "./mac-storage-fixtures.mjs";

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
  return line => line.includes(value) && (line.includes(`${kind} `) || (kind === "button" && value.startsWith("Inspect ") && /cell .*Inspect /.test(line)));
}

function locateInHeadingSection(ax, heading, predicate, phase) {
  const lines = ax.split("\n");
  const indexOf = line => Number(line.match(/^\s*(\d+)\b/)?.[1]);
  const headingAt = lines.findIndex(line => {
    const match = line.match(/^\s*\d+ heading (.*?)(?:, Value:.*)?$/);
    return match?.[1] === heading;
  });
  assert.ok(headingAt >= 0, `${phase}: AX heading ${heading} must be present`);
  const start = indexOf(lines[headingAt]);
  const nextHeadingAt = lines.findIndex((line, index) => index > headingAt && /^\s*\d+ heading /.test(line));
  const end = nextHeadingAt >= 0 ? indexOf(lines[nextHeadingAt]) : Number.POSITIVE_INFINITY;
  const candidates = lines.filter(line => {
    const index = indexOf(line);
    return Number.isFinite(index) && index >= start && index < end && predicate(line);
  });
  assert.equal(candidates.length, 1, `${phase}: expected one AX control in ${heading}, found ${candidates.length}`);
  return indexOf(candidates[0]);
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
 * For replay coverage, callback must invoke supplied `prepareWhileClosed` after
 * quitting and before relaunching, then return `preparedWhileClosed: true`.
 */
export async function runInstalledStorageJourney(initialApp, options) {
  const { appBundle, fixture, output, onRelaunch, onCheckpoint = () => {}, onRingHover } = options;
  assert.ok(initialApp && typeof initialApp.getAXState === "function", "CUA app is required");
  assert.ok(path.isAbsolute(appBundle) && path.isAbsolute(output), "appBundle/output must be absolute");
  assert.ok(fixture?.root && fixture?.discard && fixture?.duplicateA && fixture?.duplicateB && fixture?.duplicateC && fixture?.sourcePng && fixture?.growthFile && fixture?.hiddenFile && fixture?.replayFile,
    "fixture must include root, discard, duplicateA, duplicateB, duplicateC, sourcePng, growthFile, hiddenFile, replayFile");
  assert.ok(Array.isArray(fixture.indexFiles) && fixture.indexFiles.length >= 6, "fixture must include six filename-index files for pagination");
  assert.ok(fixture?.disposableApp?.path && fixture?.disposableApp?.bundleID && fixture?.disposableApp?.infoPlist && fixture?.disposableApp?.marker,
    "fixture must include disposableApp path, bundleID, infoPlist, marker");
  assert.equal(typeof onRelaunch, "function", "onRelaunch callback is required for restart coverage");
  await mkdir(output, { recursive: true });
  let app = initialApp;
  const report = { status: "running", appBundle, startedAt: new Date().toISOString(), phases: [], checkpoints: [] };
  const originalFixtureFiles = [fixture.duplicateA, fixture.duplicateB, fixture.duplicateC, fixture.discard, fixture.sourcePng, fixture.hiddenFile, ...fixture.indexFiles];
  const appFixture = fixture.disposableApp;
  const bundleFingerprint = async bundle => {
    const info = await stat(bundle.path);
    assert.equal(info.isDirectory(), true, `application bundle must remain a directory: ${bundle.path}`);
    return { path: bundle.path, dev: info.dev, ino: info.ino, files: await Promise.all([bundle.infoPlist, bundle.marker].map(fingerprint)) };
  };
  const bundleFiles = ["Contents/MacOS/Cockpit", "Contents/Helpers/cockpit", "Contents/Resources/dashboard/app.js", "Contents/Resources/dashboard/index.html"]
    .map(file => path.join(appBundle, file));
  const before = await Promise.all([...originalFixtureFiles, ...bundleFiles].map(fingerprint));
  const beforeApp = await bundleFingerprint(appFixture);
  report.inputs = before;
  report.applicationInput = beforeApp;
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
    // Navigate observed native file rows. Go to Folder's AX value can change
    // without committing its path, so it is not sufficient picker evidence.
    const home = (await import("node:os")).homedir();
    assert.ok(target.startsWith(home + "/"), "picker fixture must belong to user home");
    await app.pressKey("super+shift+h");
    let picker = await state();
    let current = home;
    const components = target.slice(home.length + 1).split("/");
    for (let index = 0; index < components.length; index += 1) {
      current = path.join(current, components[index]);
      const expected = current;
      picker = await waitFor(ax => controlLines(ax).some(line => {
        const url = line.match(/URL: (file:\/\/[^,]+),/);
        return url && decodeURIComponent(new URL(url[1]).pathname).replace(/\/$/, "") === expected;
      }), `native chooser must expose fixture row ${expected}`);
      const row = locate(picker, line => {
        const url = line.match(/URL: (file:\/\/[^,]+),/);
        return url && decodeURIComponent(new URL(url[1]).pathname).replace(/\/$/, "") === expected;
      }, phase);
      await app.click(row);
      picker = await state();
    }
    const buttons = controlLines(picker).filter(line => /^\s*\d+ (?:button|toolbar item) (?:Open|Choose|Select)(?:,|$)/.test(line));
    assert.equal(buttons.length, 1, `${phase}: native picker must expose one observed acceptance button`);
    await app.click(Number(buttons[0].trim().split(" ")[0]));
  };
  const navigate = async label => click(hasButton(label));
  const assertLiveFile = async file => { await access(file); return fingerprint(file); };
  const replaceText = async (label, value) => {
    const current = await state();
    const dateLine = controlLines(current).find(line => line.includes(`date time area ${label},`));
    if (dateLine) {
      const parts = value.match(/^(\d{4})-(\d{2})-(\d{2})$/);
      assert.ok(parts, `date ${label} must use explicit ISO input`);
      const dateControls = valueAx => {
        const lines = controlLines(valueAx);
        const start = lines.findIndex(line => line.includes(`date time area ${label},`));
        assert.ok(start >= 0, `date ${label} must remain available`);
        const end = lines.findIndex((line, index) => index > start && /date time area|^\s*\d+ text (?:Created|Modified|Results)/.test(line));
        return lines.slice(start + 1, end < 0 ? lines.length : end);
      };
      const segments = dateControls(current).filter(line => /stepper .* (?:month|day|year),/.test(line));
      assert.equal(segments.length, 3, `date ${label} must expose three native segments`);
      assert.ok(segments[0].includes("month,"), "observed native date order must begin with month");
      assert.ok(segments[1].includes("day,") && segments[2].includes("year,"), "observed native date order must continue day/year");
      await app.click(Number(segments[0].trim().split(" ")[0]));
      await app.pressKey("super+a");
      await app.typeText(parts[2]);
      await app.pressKey("Right");
      await app.typeText(parts[3]);
      await app.pressKey("Right");
      await app.typeText(parts[1]);
      await app.pressKey("Tab");
      await waitFor(valueAx => {
        const controls = dateControls(valueAx);
        return [["month", parts[2]], ["day", parts[3]], ["year", parts[1]]].every(([segment, expected]) =>
          controls.some(line => line.includes(`${segment}, Value: ${Number(expected)}`)));
      }, `AX date ${label} must retain all ISO segments`);
      return;
    }
    await click(line => /(?:text|number) field/.test(line) && line.includes(label));
    await app.pressKey("super+a");
    await app.typeText(value);
    await waitFor(valueAx => controlLines(valueAx).some(line => line.includes(label) && line.includes(value)),
      `AX field ${label} must retain ${value}`);
  };
  const chooseSelectValue = async (label, value) => {
    await click(line => line.includes(label) && /(?:pop up button|popup button|combo box|select)/i.test(line));
    await app.typeText(value);
    await app.pressKey("Return");
    await waitFor(valueAx => controlLines(valueAx).some(line => line.includes(label) && /(?:pop up button|popup button|combo box|select)/i.test(line)),
      `AX select ${label} must remain available after choosing ${value}`);
  };
  const clickInHeadingSection = async (heading, predicate) => {
    const ax = await state();
    await app.click(locateInHeadingSection(ax, heading, predicate, phase));
    return state();
  };

  try {
    phase = "launch-and-native-scan";
    let ax = await state();
    if (ax.includes("system dialog Cockpit — Notch")) ax = await click(controlIncluding("button", "Open Cockpit dashboard"));
    await click(line => /(?:button|toolbar item) Scan folder(?:,|$)/.test(line));
    await pickerChoose(fixture.root);
    await navigate("Storage");
    ax = await waitFor(value => value.includes(path.basename(fixture.root)) && /Loaded \d+ entries/.test(value), "native folder scan must load selected fixture in Storage view");
    assert.ok(ax.includes(path.basename(fixture.root)), "native scan must render fixture root evidence");
    const firstSnapshot = await Promise.all(originalFixtureFiles.map(fingerprint));
    report.firstSnapshot = firstSnapshot;
    await phaseDone(phase);

    phase = "all-dashboard-sections-render";
    for (const [label, expected] of [
      ["Storage", /Storage map/], ["Find", /Filename index/],
      ["Duplicates", /Duplicates readings unavailable|Duplicate groups/], ["Cleanup", /Findings/],
      ["Apps", /Apps readings unavailable|Application inventory/],
      ["Monitor", /Monitor readings unavailable|Storage volumes/],
      ["Activity", /Activity readings unavailable|Timeline/], ["Compress", /Compression controls/],
    ]) {
      ax = await navigate(label);
      ax = await waitFor(value => expected.test(value), `${label} rendered controls must become accessible`);
      assert.ok(expected.test(ax), `${label} must render actual content or explicit unavailable state`);
      assert.ok(!/^\s*\d+ text (?:null|undefined)$|\[object Object\]/m.test(ax), `${label} must not render absent nodes as text`);
      await checkpoint(`section-${label.toLowerCase()}`);
    }
    await navigate("Storage");
    await phaseDone(phase);

    phase = "changed-fixture-rescan-and-growth";
    const growth = await addStorageGrowthFile(fixture);
    await access(growth.path);
    await click(line => /(?:button|toolbar item) Scan folder(?:,|$)/.test(line));
    await pickerChoose(fixture.root);
    ax = await waitFor(value => value.includes(path.basename(fixture.root)) && /Loaded \d+ entries/.test(value) && value.includes("Folder growth") && /growing/i.test(value) && value.includes(fixture.root) && /\+\d/.test(value), "changed fixture scan must render growth in Storage view");
    assert.ok(ax.includes("Folder growth") && /growing/i.test(ax) && ax.includes(fixture.root) && /\+\d/.test(ax),
      "changed fixture scan must render positive folder growth for fixture root");
    for (let index = 0; index < originalFixtureFiles.length; index += 1) {
      assertFingerprint(await fingerprint(originalFixtureFiles[index]), firstSnapshot[index], `first snapshot original ${originalFixtureFiles[index]}`);
    }
    await phaseDone(phase, { growth });

    phase = "storage-drill-and-inspector";
    ax = await click(controlIncluding("button", `Open ${fixture.nested}`));
    assert.ok(ax.includes(fixture.duplicateB), "storage drill must render nested duplicate");
    ax = await clickInHeadingSection("Entries in view", line => line.includes(`cell (selectable) Inspect ${fixture.duplicateB}`));
    assert.ok(ax.includes("Inspector") && ax.includes(fixture.duplicateB) && /metadata/i.test(ax), "inspector must show selected fixture");
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

    phase = "filename-index-build-and-hidden-search";
    await navigate("Find");
    ax = await waitFor(value => /button (?:Build|Rescan) filename index(?:,|$)/m.test(value),
      "Find must expose observed native filename-index build/resume control");
    const initialIndexButton = controlLines(ax).find(line => /button (?:Build|Rescan) filename index(?:,|$)/.test(line));
    assert.ok(initialIndexButton, "filename-index action must be an observed AX button");
    ax = await click(hasButton(initialIndexButton.match(/^\s*\d+ button (.*?)(?:,|$)/)?.[1]));
    ax = await waitFor(value => /Filename index/.test(value) && /\d+ entries/.test(value) && /button Rescan filename index(?:,|$)/m.test(value),
      "Build filename index must expose native indexed-search state & Rescan status");
    await replaceText("Name or path", path.basename(fixture.hiddenFile));
    ax = await click(hasButton("Search"));
    ax = await waitFor(value => value.includes("1 indexed matches · 0 offset") && value.includes(fixture.hiddenFile),
      "indexed search must find hidden fixture by filename");
    ax = await click(hasButton(`Inspect indexed ${fixture.hiddenFile}`));
    await app.pressKey("Return");
    ax = await waitFor(value => value.includes("Indexed metadata") && value.includes(fixture.hiddenFile) && /created/i.test(value) && /modified/i.test(value),
      "keyboard indexed-row inspection must render metadata card");
    await phaseDone(phase);

    phase = "filename-index-filters-and-pagination";
    await replaceText("Name or path", "fixture");
    await replaceText("Extension", "txt");
    await chooseSelectValue("Kind", "file");
    await replaceText("Min bytes", "1");
    await replaceText("Max bytes", "1048576");
    await replaceText("Created after", "2000-01-01");
    await replaceText("Created before", "2999-12-31");
    await replaceText("Modified after", "2000-01-01");
    await replaceText("Modified before", "2999-12-31");
    await chooseSelectValue("Results per page", "5");
    ax = await click(hasButton("Search"));
    ax = await waitFor(value => /\d+ indexed matches · 0 offset/.test(value), "indexed filters must render result count and offset");
    const indexedCount = Number(ax.match(/(\d+) indexed matches · 0 offset/)?.[1] ?? 0);
    assert.ok(indexedCount >= 7, `indexed filters must retain at least seven fixture matches, got ${indexedCount}`);
    ax = await click(hasButton("Next results"));
    ax = await waitFor(value => value.includes("indexed matches · 5 offset"), "indexed results must advance by five");
    ax = await click(hasButton("Previous results"));
    ax = await waitFor(value => value.includes("indexed matches · 0 offset"), "indexed results must return to first page");
    await phaseDone(phase, { indexedCount, pageSize: 5 });

    phase = "filename-index-relaunch-replay";
    let replay;
    const replayRelaunch = await onRelaunch({
      app,
      phase,
      fixture,
      report,
      prepareWhileClosed: async () => {
        replay = await addStorageReplayFile(fixture);
        return replay;
      },
    });
    assert.ok(replayRelaunch?.restarted === true && replayRelaunch?.preparedWhileClosed === true,
      "onRelaunch must report quit, closed-file preparation, and relaunch completion");
    app = replayRelaunch.app;
    assert.ok(app && typeof app.getAXState === "function", "filename-index relaunch must return fresh CUA app");
    ax = await waitFor(value => value.includes("Storage") || value.includes("Loaded"), "filename-index relaunch must return to native dashboard");
    await navigate("Find");
    ax = await waitFor(value => /button (?:Build|Rescan) filename index(?:,|$)/m.test(value),
      "Find must expose explicit filename-index Build or Rescan control after relaunch");
    const replayIndexButton = controlLines(ax).find(line => /button (?:Build|Rescan) filename index(?:,|$)/.test(line));
    assert.ok(replayIndexButton, "relaunch filename-index action must be an observed AX button");
    ax = await click(hasButton(replayIndexButton.match(/^\s*\d+ button (.*?)(?:,|$)/)?.[1]));
    ax = await waitFor(value => /Filename index/.test(value) && /\d+ entries/.test(value) && /button Rescan filename index(?:,|$)/m.test(value), "filename-index replay must expose resumed index state & Rescan status");
    await replaceText("Name or path", path.basename(fixture.indexFiles[0]));
    ax = await click(hasButton("Search"));
    ax = await waitFor(value => value.includes("1 indexed matches · 0 offset") && value.includes(fixture.indexFiles[0]),
      "filename-index relaunch must retain an earlier indexed entry");
    await replaceText("Name or path", path.basename(fixture.replayFile));
    ax = await click(hasButton("Search"));
    ax = await waitFor(value => value.includes("1 indexed matches · 0 offset") && value.includes(fixture.replayFile),
      "filename created while app was closed must appear after index replay");
    ax = await click(hasButton("Refresh index status"));
    ax = await waitFor(value => /(?:\d+ entries|Stale|Partial coverage|Watching)/i.test(value), "filename-index status refresh must render observed status");
    const staleObserved = /\bStale\b/i.test(ax);
    ax = await click(hasButton("Search saved scan instead"));
    ax = await waitFor(value => value.includes("Search entries") || value.includes("Search saved scan"), "saved-scan search mode must be restorable");
    assert.ok(replay?.path === fixture.replayFile && replayRelaunch.preparedWhileClosed === true, "replay fixture must be created during closed interval");
    await phaseDone(phase, { replay, resumedExistingPath: fixture.indexFiles[0], staleObserved });

    phase = "native-duplicates-opt-in";
    await navigate("Duplicates");
    ax = await click(line => /button (?:Inspect content for duplicates|Re-run content inspection)(?:,|$)/.test(line));
    ax = await waitFor(value => value.includes("Exact duplicate groups") && value.includes(fixture.duplicateA) && value.includes(fixture.duplicateB) && value.includes(fixture.duplicateC), "native duplicate inspection must report all duplicate fixture paths");
    assert.ok(ax.includes("2 files considered") || ax.includes("1 groups shown"), "duplicate summary must be rendered");
    await phaseDone(phase);

    phase = "duplicate-change-refusal";
    await navigate("Cleanup");
    ax = await waitFor(value => value.includes("Duplicate extras") && value.includes(fixture.duplicateB) && value.includes(fixture.duplicateC), "cleanup must expose both duplicate extras for explicit review");
    await click(controlIncluding("checkbox", `Stage ${fixture.duplicateB}`));
    await click(controlIncluding("checkbox", `Stage ${fixture.duplicateC}`));
    const refusedChanges = [];
    for (const changedPath of [fixture.duplicateB, fixture.duplicateA]) {
      ax = await click(hasButton("Review selected files"));
      ax = await waitFor(value => value.includes("Move selected files to Trash?") && value.includes("Move to Trash"), "native duplicate review must be visible");
      await app.click(locate(ax, hasButton("Move to Trash"), phase));
      await waitFor(value => value.includes("Review ready") && value.includes("Duplicate bytes confirmed"), "duplicate review must confirm complete equality");
      const original = await readFile(changedPath);
      const changed = Buffer.from(original);
      assert.ok(changed.length > 0, "duplicate fixture must contain bytes");
      changed[0] ^= 0xff;
      try {
        await writeFile(changedPath, changed, { flag: "r+" });
        ax = await click(hasButton("Apply cleanup"));
        ax = await waitFor(value => /Action failed:.*duplicate (?:identity changed|content no longer matches|descriptor raced)/i.test(value), "changed duplicate must refuse Trash before claim");
        for (const file of [fixture.duplicateA, fixture.duplicateB, fixture.duplicateC]) await access(file);
        for (const file of [fixture.duplicateA, fixture.duplicateB, fixture.duplicateC].filter(file => file !== changedPath)) {
          assertFingerprint(await fingerprint(file), before[originalFixtureFiles.indexOf(file)], "refused duplicate action preserves other originals");
        }
        refusedChanges.push({ path: changedPath, refusal: ax.match(/Action failed:.*duplicate[^\n]*/i)?.[0] });
        await checkpoint(`refused-change-${path.basename(changedPath)}`);
      } finally {
        await writeFile(changedPath, original, { flag: "r+" });
      }
      assertFingerprint(await fingerprint(changedPath), before[originalFixtureFiles.indexOf(changedPath)], "restored changed fixture");
      await navigate("Duplicates");
      await click(line => /button (?:Inspect content for duplicates|Re-run content inspection)(?:,|$)/.test(line));
      await waitFor(value => value.includes("Exact duplicate groups") && value.includes(fixture.duplicateC), "fresh duplicate inspection must return after refused action");
      await navigate("Cleanup");
      await waitFor(value => value.includes("Duplicate extras") && value.includes(fixture.duplicateB), "duplicate selection must remain available after refused action");
    }
    await phaseDone(phase, { refusedChanges });

    phase = "native-trash-multi-review-and-apply";
    ax = await click(hasButton("Review selected files"));
    ax = await waitFor(value => value.includes("Move selected files to Trash?") && value.includes("Move to Trash"), "native Trash review must be visible");
    await app.click(locate(ax, hasButton("Move to Trash"), phase));
    ax = await waitFor(value => value.includes("Review ready") || value.includes("Cleanup complete"), "native review must return to dashboard");
    ax = await click(hasButton("Apply cleanup"));
    ax = await waitFor(value => value.includes("Cleanup complete") && value.includes("2 files moved to Trash"), "native multi-file cleanup apply must complete");
    await assert.rejects(() => access(fixture.duplicateB), /ENOENT/, "first duplicate extra must be absent after native apply");
    await assert.rejects(() => access(fixture.duplicateC), /ENOENT/, "second duplicate extra must be absent after native apply");
    await access(fixture.discard);
    ax = await waitFor(value => value.includes("Cleanup history"), "cleanup history must render applied plan");
    assert.ok(!/(?:Cleanup incomplete|interrupted|indeterminate|failed)/i.test(ax), "partial native cleanup outcome cannot be reported as success");
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
    await click(hasButton(`Restore ${path.basename(fixture.duplicateB)}`));
    ax = await waitFor(value => value.includes("Undo complete") && value.includes("1 file restored"), "native per-item Restore must complete after relaunch");
    const restoredOneFile = await assertLiveFile(fixture.duplicateB);
    assertFingerprint(restoredOneFile, before[originalFixtureFiles.indexOf(fixture.duplicateB)], "restored first duplicate extra");
    await assert.rejects(() => access(fixture.duplicateC), /ENOENT/, "second duplicate extra must remain missing after per-item Restore");
    ax = await click(hasButton("Undo"));
    ax = await waitFor(value => value.includes("Undo complete") && value.includes("1 file restored"), "whole Undo must restore remaining item");
    const restoredTwo = await assertLiveFile(fixture.duplicateC);
    assertFingerprint(restoredTwo, before[originalFixtureFiles.indexOf(fixture.duplicateC)], "restored second duplicate extra");
    const restored = await assertLiveFile(fixture.discard);
    assertFingerprint(restored, before[originalFixtureFiles.indexOf(fixture.discard)], "retained discard fixture");
    await navigate("Cleanup");
    ax = await waitFor(value => value.includes("Cleanup history"), "cleanup history must render restored plan");
    assert.ok(!/(?:Undo incomplete|interrupted|indeterminate|failed)/i.test(ax), "partial native Undo outcome cannot be reported as success");
    await phaseDone(phase, { restored: [restoredOneFile, restoredTwo, restored] });

    phase = "activity-cleanup-filters-and-scan-history";
    await navigate("Activity");
    ax = await click(hasButton("Refresh activity"));
    ax = await waitFor(value => value.includes("refresh activity complete") && value.includes("Timeline") && value.includes("Cleanup history"),
      "durable Activity refresh must render timeline and cleanup history");
    await chooseSelectValue("Action", "Cleanup");
    await chooseSelectValue("Period", "Last 7 days");
    ax = await waitFor(value => value.includes(`Cleanup · move · moved`) && value.includes(`Cleanup · restore · restored`) && value.includes(fixture.duplicateB) && value.includes(fixture.duplicateC),
      "Cleanup and restore events must remain visible for both duplicate paths");
    assert.match(ax, /7 days: \d+ events/);
    assert.match(ax, /30 days: \d+ events/);
    assert.ok(/observed allocation Unknown/i.test(ax), "cleanup timeline must preserve unknown allocation");
    assert.ok(!/\bFreed\s+\d/i.test(ax) && !/\breclaimed\s+\d/i.test(ax), "cleanup Activity must not claim freed or reclaimed bytes");
    await chooseSelectValue("Action", "Scan");
    ax = await waitFor(value => value.includes("Scan ·") && value.includes(fixture.root) && /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(value),
      "Scan filter must render durable fixture scan evidence with known timestamp");
    await phaseDone(phase, { cleanupPaths: [fixture.duplicateB, fixture.duplicateC], scanRoot: fixture.root });

    phase = "native-compression-picker";
    const sourceBefore = await assertLiveFile(fixture.sourcePng);
    await navigate("Compress");
    await click(hasButton("Choose media & compress"));
    await pickerChoose(fixture.sourcePng);
    await pickerChoose(fixture.compressionOutput ?? fixture.root);
    ax = await waitFor(value => value.includes("Latest result") && value.includes(fixture.sourcePng) && /output/i.test(value), "native compression must report source/output");
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

    phase = "compression-quick-look-preview";
    ax = await click(hasButton("Preview output"));
    ax = await waitFor(value => value.includes(path.basename(encodedPath)) && /Quick Look|Close/i.test(value),
      "Preview output must open observed native Quick Look for exact output path");
    assert.ok(ax.includes(path.basename(encodedPath)), "Quick Look AX must identify exact compressed output");
    await checkpoint(`${phase}-open`);
    await app.pressKey("Escape");
    ax = await waitFor(value => value.includes("Latest result") && value.includes(encodedPath) && value.includes("Preview output"),
      "Escape must close Quick Look and restore compression result controls");
    await phaseDone(phase, { previewPath: encodedPath, closedWith: "Escape" });

    phase = "activity-compression-filter";
    await navigate("Activity");
    ax = await click(hasButton("Refresh activity"));
    ax = await waitFor(value => value.includes("refresh activity complete") && value.includes("Timeline"),
      "post-compression Activity refresh must render durable timeline");
    await chooseSelectValue("Action", "Compression");
    await chooseSelectValue("Period", "Last 30 days");
    ax = await waitFor(value => value.includes("Compression · compress · completed") && value.includes(encodedPath) && /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(value),
      "Compression filter must render persisted completed output event");
    assert.match(ax, /30 days: \d+ events/);
    await phaseDone(phase, { compressionOutput: encodedPath });

    for (const view of ["Apps", "Monitor", "Activity"]) {
      phase = `native-${view.toLowerCase()}`;
      await navigate(view);
      const action = view === "Apps" ? "Refresh app inventory" : view === "Monitor" ? "Refresh readings" : "Refresh activity";
      ax = await click(hasButton(action));
      const expected = view === "Apps" ? "Application inventory" : view === "Monitor" ? "Storage volumes" : "Cleanup, scan, compression";
      ax = await waitFor(value => value.includes(expected) && value.includes(`${action.replace("Refresh readings", "refresh monitor").replace("Refresh app inventory", "refresh apps").replace("Refresh activity", "refresh activity")} complete`), `${view} native reading must render after response`);
      if (view === "Monitor") {
        const nextLine = controlLines(ax).find(hasButton("Next processes"));
        assert.ok(nextLine, "Monitor must expose process pagination");
        if (!nextLine.includes("disabled")) {
          await click(hasButton("Next processes"));
          ax = await waitFor(value => /Showing 51–\d+ of/.test(value), "second process page must render distinct supplied rows");
          await checkpoint("monitor-process-page-two");
          await click(hasButton("Previous processes"));
          ax = await waitFor(value => /Showing 1–50 of/.test(value), "process pagination must restore first page");
        } else {
          assert.ok(/Showing 1–\d+ of|No processes reported/.test(ax), "short process source must explicitly report its supplied rows");
        }
      }
      await phaseDone(phase);
      if (view === "Apps") {
        const nextLine = controlLines(ax).find(hasButton("Next applications"));
        assert.ok(nextLine, "Apps must expose supplied inventory pagination");
        if (!nextLine.includes("disabled")) {
          await click(hasButton("Next applications"));
          ax = await waitFor(value => /Showing 26–\d+ of/.test(value), "second app page must render distinct supplied rows");
          await checkpoint("apps-inventory-page-two");
          await click(hasButton("Previous applications"));
          ax = await waitFor(value => /Showing 1–25 of/.test(value), "app pagination must restore first page");
        }
        phase = "native-app-details";
        const appName = path.basename(appBundle, path.extname(appBundle));
        assert.ok(ax.includes(appBundle), "Apps inventory must include current Cockpit bundle path");
        ax = await click(hasButton(`Details for ${appName}`));
        ax = await waitFor(value => value.includes(`Details · ${appName}`) && value.includes(appBundle), "app details must render current bundle identity and exact path");
        for (const related of ["Library/Preferences", "Library/Caches", "Library/Application Support", "Library/Logs", "Library/Containers"]) {
          assert.ok(ax.includes(related), `app details must render exact related path family ${related}`);
        }
        assert.ok(ax.includes("report only"), "related app paths must remain report only");
        ax = await waitFor(value => value.includes("app details complete") && value.includes("Verified app inspection") && value.includes("Resource history"), "current running app must expose verified native inspection & confirmed persisted history");
        assert.ok(/Verified process · PID \d+ · start \d+/.test(ax), "native inspection must expose verified incarnation");
        assert.ok(ax.includes("Disk I/O") && ax.includes("Open files") && ax.includes("Network endpoints"), "app inspection must render bounded native I/O observations");
        const previousSamples = Number(ax.match(/(\d+) retained samples/)?.[1] ?? 0);
        assert.ok(previousSamples > 0 && previousSamples <= 256, "confirmed native history must contain bounded samples");
        ax = await click(hasButton("Check app feed"));
        ax = await waitFor(value => value.includes("App feed result") && value.includes(appBundle) && /(?:unavailable|unsupported)/i.test(value) && /(?:Not performed|false)/i.test(value),
          "selected Cockpit feed check must report real unsupported state without network");
        assert.ok(/SUFeedURL|feed_url_not_declared|bundle_feed_url_not_declared/i.test(ax), "Cockpit feed result must explain missing SUFeedURL");
        ax = await click(hasButton("Check Homebrew"));
        ax = await waitFor(value => value.includes("Homebrew result") && value.includes(appBundle)
          && /not_managed|unavailable|partial|no-update|available/.test(value),
          "explicit Homebrew check must return correlated metadata or supported unavailable state", 50_000);
        assert.ok(!/upgrade complete|installed update/i.test(ax), "Homebrew metadata must never claim an installation");
        const cockpitBefore = await stat(appBundle);
        ax = await click(hasButton("Review app uninstall"));
        ax = await waitFor(value => /Action failed:/i.test(value) && /running|liveness|process/i.test(value),
          "running Cockpit uninstall review must fail before any effect");
        const cockpitAfter = await stat(appBundle);
        assert.equal(cockpitAfter.dev, cockpitBefore.dev, "running Cockpit uninstall failure changed volume identity");
        assert.equal(cockpitAfter.ino, cockpitBefore.ino, "running Cockpit uninstall failure changed bundle identity");
        await click(hasButton(`Details for ${appName}`));
        ax = await waitFor(value => value.includes("app details complete") && Number(value.match(/(\d+) retained samples/)?.[1]) === Math.min(256, previousSamples + 1), "second verified inspection must append bounded resource history");
        assert.ok(ax.includes("Application history") && /partial|unknown/i.test(ax), "app history must expose partial or unknown coverage");
        await phaseDone(phase);

        phase = "disposable-app-inventory-and-details";
        ax = await waitFor(value => value.includes(appFixture.path) && value.includes(appFixture.bundleID),
          "fresh app inventory must include disposable fixture bundle and bundle ID");
        ax = await click(hasButton(`Details for ${appFixture.name}`));
        ax = await waitFor(value => value.includes(`Details · ${appFixture.name}`) && value.includes(appFixture.path) && value.includes(appFixture.bundleID),
          "disposable app Details must render exact path and bundle identity");
        assert.ok(ax.includes("Unknown") && ax.includes("report only"), "disposable app metadata must preserve unknown bytes and report-only related paths");
        await phaseDone(phase);

        phase = "disposable-app-uninstall-cancel-and-apply";
        ax = await click(hasButton("Review app uninstall"));
        ax = await waitFor(value => value.includes("Move App to Trash") && value.includes("Cancel"),
          "disposable app uninstall review must show native confirmation");
        await app.click(locate(ax, hasButton("Cancel"), phase));
        ax = await waitFor(value => /Action failed:/i.test(value) && /cancel/i.test(value),
          "disposable app uninstall review cancel must return explicit cancellation");
        const canceledBundle = await bundleFingerprint(appFixture);
        assert.equal(canceledBundle.ino, beforeApp.ino, "canceled app review changed bundle identity");
        ax = await click(hasButton("Review app uninstall"));
        ax = await waitFor(value => value.includes("Move App to Trash") && value.includes("Cancel"),
          "disposable app uninstall retry must show native confirmation");
        await app.click(locate(ax, hasButton("Move App to Trash"), phase));
        ax = await waitFor(value => value.includes("Review ready · app uninstall") && value.includes("Apply app uninstall"),
          "accepted app uninstall review must expose native apply action");
        ax = await click(hasButton("Apply app uninstall"));
        ax = await waitFor(value => value.includes("App uninstall complete") && /1 app moved to Trash/.test(value),
          "app uninstall apply must report one moved bundle");
        await assert.rejects(() => access(appFixture.path), /ENOENT/, "app bundle must be absent after uninstall apply");
        await phaseDone(phase, { appPath: appFixture.path, bundleID: appFixture.bundleID });
      }
    }

    phase = "activity-app-uninstall-and-relaunch-undo";
    await navigate("Activity");
    ax = await click(hasButton("Refresh activity"));
    ax = await waitFor(value => value.includes("refresh activity complete") && value.includes("Timeline"),
      "Activity refresh must expose durable app uninstall timeline");
    await chooseSelectValue("Action", "Uninstall");
    await chooseSelectValue("Period", "Last 30 days");
    ax = await waitFor(value => value.includes("Uninstall · remove · moved") && value.includes(appFixture.path) && /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(value),
      "Uninstall filter must render durable moved bundle event");
    const appRelaunched = await onRelaunch({ app, phase, fixture, report });
    assert.ok(appRelaunched?.restarted === true, "app uninstall restart must report observed quit/relaunch completion");
    app = appRelaunched.app;
    assert.ok(app && typeof app.getAXState === "function", "app uninstall restart must return fresh CUA app");
    ax = await waitFor(value => value.includes("Storage") || value.includes("Loaded"), "app uninstall relaunch must return to native dashboard");
    await navigate("Activity");
    ax = await click(hasButton("Refresh activity"));
    ax = await waitFor(value => value.includes("Cleanup history") && value.includes(appFixture.path), "relaunch must retain app uninstall cleanup journal");
    ax = await click(hasButton(`Restore ${appFixture.name}`));
    ax = await waitFor(value => value.includes("Undo complete") && value.includes("1 file restored"), "app bundle Undo must complete after relaunch");
    const restoredApp = await bundleFingerprint(appFixture);
    assert.equal(restoredApp.dev, beforeApp.dev, "restored app bundle volume changed");
    assert.equal(restoredApp.ino, beforeApp.ino, "restored app bundle identity changed");
    for (let index = 0; index < beforeApp.files.length; index += 1) assertFingerprint(restoredApp.files[index], beforeApp.files[index], `app Undo file ${beforeApp.files[index].path}`);
    await phaseDone(phase, { appPath: appFixture.path, restored: restoredApp });

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

    const finalFixtureFiles = [...originalFixtureFiles, fixture.growthFile, fixture.replayFile];
    const after = await Promise.all([...finalFixtureFiles, ...bundleFiles].map(fingerprint));
    for (let index = 0; index < originalFixtureFiles.length; index += 1) {
      assertFingerprint(after[index], before[index], `read-only fixture integrity ${before[index].path}`);
    }
    for (let index = 0; index < bundleFiles.length; index += 1) {
      assertFingerprint(after[finalFixtureFiles.length + index], before[originalFixtureFiles.length + index], `bundle integrity ${bundleFiles[index]}`);
    }
    report.finalFingerprints = after.slice(0, finalFixtureFiles.length);
    const afterApp = await bundleFingerprint(appFixture);
    assert.equal(afterApp.dev, beforeApp.dev, "restored app bundle volume changed");
    assert.equal(afterApp.ino, beforeApp.ino, "restored app bundle identity changed");
    for (let index = 0; index < beforeApp.files.length; index += 1) assertFingerprint(afterApp.files[index], beforeApp.files[index], `restored app bundle file ${beforeApp.files[index].path}`);
    report.finalApplication = afterApp;
    report.status = report.phases.every(item => item.status === "passed") ? "passed" : "partial";
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
