import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";

// CUA captures are JPEG on Mac; PNG is accepted for providers using that format.
// Read actual image dimensions so a no-op window transition cannot pass on text alone.
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

// Run inside cua_repl with a real cua.getApp binding. No mocked DOM, injected
// state, shell UI control, credentials, live filesystem scan or cleanup actions.
export async function runInstalledJourney(app, options) {
  const { appBundle, validScan, invalidScan, output, entryName, entryCount, volumes,
    notchMaxWidthPixels = 40, onCheckpoint = () => {} } = options;
  assert.ok(path.isAbsolute(appBundle) && path.isAbsolute(output));
  assert.ok(entryCount > 1 && volumes.length > 1, "Use a multi-entry, multi-volume journey");
  await mkdir(output, { recursive: true });
  const report = { status: "running", appBundle, startedAt: new Date().toISOString(), checkpoints: [] };
  const files = [validScan, invalidScan, ...[
    "Contents/MacOS/Cockpit", "Contents/Helpers/cockpit",
    "Contents/Resources/dashboard/app.js", "Contents/Resources/dashboard/style.css",
  ].map(file => path.join(appBundle, file))];
  const fingerprint = async () => Promise.all(files.map(async file =>
    createHash("sha256").update(await readFile(file)).digest("hex")));
  const before = await fingerprint();
  report.inputs = files.map((file, index) => ({ file, sha256: before[index] }));
  let phase = "open-dashboard";
  const state = () => app.getAXState({ emit: false, disableDiffing: true });
  const waitFor = async (predicate, description) => {
    const deadline = Date.now() + 5_000;
    do {
      const ax = await state();
      if (predicate(ax)) return ax;
    } while (Date.now() < deadline);
    throw Error(`${phase}: ${description}`);
  };
  const locate = (ax, predicate) => {
    const matches = ax.split("\n").filter(line => /^\s*\d+ /.test(line) && predicate(line));
    assert.equal(matches.length, 1, `Expected one native control in ${phase}, found ${matches.length}`);
    return Number(matches[0].trim().split(" ")[0]);
  };
  const click = async predicate => {
    await app.click(locate(await state(), predicate));
    return state();
  };
  const button = name => line => new RegExp(`^\\s*\\d+ button ${name}(?:,|$)`).test(line);
  const expectText = (ax, value) => assert.ok(ax.includes(value), `${phase}: missing rendered ${value}`);
  const loaded = ax => expectText(ax, `text Loaded ${entryCount} entries`);
  const checkpoint = async name => {
    const ax = await state();
    await writeFile(path.join(output, `${name}.txt`), ax);
    const capture = await app.getScreenshot({ emit: false });
    const frame = imageSize(capture);
    await writeFile(path.join(output, `${name}.jpg`), capture);
    report.checkpoints.push({ name, status: "observed", frame, at: new Date().toISOString() });
    await writeFile(path.join(output, "result.json"), JSON.stringify(report, null, 2) + "\n");
    await onCheckpoint(name);
    return frame;
  };
  const importFile = async file => {
    await click(button("Import scan JSON"));
    // Exercise actual AppKit open-panel keyboard routing, not input.files injection.
    await app.pressKey("super+shift+g");
    await state();
    await app.typeText(file);
    await app.pressKey("Return");
    await state();
    const picker = await state();
    expectText(picker, path.basename(file));
    await app.click(locate(picker, button("Open")));
    return state();
  };
  const search = async (query, submit) => {
    await click(line => line.includes("search text field") && line.includes("Name or path"));
    await app.pressKey("super+a");
    await app.typeText(query);
    await waitFor(ax => ax.split("\n").some(line => /^\s*\d+ search text field/.test(line)
      && line.includes("Name or path") && line.includes(`Value: ${query},`)),
      "keyboard replacement must equal requested query");
    if (submit === "keyboard") {
      await app.pressKey("Return");
      return state();
    }
    return click(button("Search"));
  };
  try {
    let ax = await state();
    if (ax.includes("system dialog Cockpit — Notch")) {
      ax = await click(line => line.includes("button Description: Open Cockpit dashboard"));
    }
    expectText(ax, `URL: file://${appBundle}/Contents/Resources/dashboard/index.html`);

    phase = "native-import";
    ax = await importFile(validScan);
    loaded(ax);
    expectText(ax, "heading Where space lives");
    await checkpoint(phase);

    phase = "repeat-native-import";
    await click(button("Find"));
    await importFile(validScan);
    ax = await waitFor(ax => ax.includes("heading Where space lives")
      && ax.includes(`text Loaded ${entryCount} entries`), "Repeated import must return to loaded Storage view");
    await checkpoint(phase);

    phase = "cancel-retains-scan";
    await click(button("Import scan JSON"));
    ax = await click(button("Cancel"));
    loaded(ax);

    phase = "keyboard-search-one";
    await click(button("Find"));
    ax = await search(entryName, "keyboard");
    expectText(ax, "text 1 matches");
    expectText(ax, `/${entryName}`);
    expectText(ax, "cell (selectable) Complete");
    await checkpoint(phase);

    phase = "button-search-zero-and-clear";
    ax = await search("cockpit-e2e-impossible-result-92481", "button");
    expectText(ax, "text 0 matches");
    assert.ok(!ax.includes(`cell (selectable) Inspect `), "Zero results must remove old rows");
    ax = await click(button("Clear"));
    expectText(ax, `text ${entryCount} matches`);
    await checkpoint(phase);

    phase = "invalid-import-recovers";
    ax = await importFile(invalidScan);
    expectText(ax, "Scan JSON was not accepted");
    expectText(ax, "Could not load scan JSON");
    await checkpoint("invalid-import-visible-error");
    phase = "invalid-import-recovers";
    ax = await click(button("Find"));
    loaded(ax);
    expectText(ax, `text ${entryCount} matches`);
    ax = await search(entryName, "keyboard");
    expectText(ax, "text 1 matches");

    phase = "notch-full-instruments";
    ax = await click(line => /^\s*\d+ close button/.test(line));
    expectText(ax, "system dialog Cockpit — Notch");
    const control = ax.split("\n").find(line => line.includes("button Description: Open Cockpit dashboard"));
    assert.ok(control?.includes("Value:"), "Notch needs observable instrument readings");
    const readings = control.slice(control.indexOf("Value:") + 6).split(", ");
    assert.equal(readings.length, 4 + volumes.length, "No missing provider/resource/volume clusters");
    for (const instrument of ["Claude", "ChatGPT", "CPU", "Memory pressure"]) {
      assert.equal(readings.filter(reading => reading.includes(instrument)).length, 1,
        `Exactly one reading required for ${instrument}`);
    }
    for (const volume of volumes) {
      assert.equal(readings.filter(reading => reading.startsWith(`${volume} · free `)).length, 1,
        `Exactly one free-space reading required for ${volume}`);
    }
    const notchFrame = await checkpoint(phase);
    assert.ok(notchFrame.width <= notchMaxWidthPixels, "Resting notch exceeds approved compact width");

    phase = "notch-reopen-retains-filter";
    ax = await click(line => line.includes("button Description: Open Cockpit dashboard"));
    loaded(ax);
    expectText(ax, "heading Search supplied entries");
    expectText(ax, "text 1 matches");
    expectText(ax, `Value: ${entryName}`);
    const windowFrame = await checkpoint(phase);

    phase = "fullscreen-retains-results";
    await click(line => /^\s*\d+ full screen button/.test(line));
    expectText(await state(), "text 1 matches");
    const fullscreenFrame = await checkpoint("fullscreen-results");
    assert.ok(fullscreenFrame.width > windowFrame.width && fullscreenFrame.height > windowFrame.height,
      "Fullscreen action must actually enlarge rendered window");
    await app.pressKey("Escape");
    ax = await state();
    expectText(ax, "text 1 matches");
    const restoredFrame = await checkpoint(phase);
    assert.deepEqual(restoredFrame, windowFrame, "Escape must restore original rendered window dimensions");

    phase = "fixture-integrity";
    assert.deepEqual(await fingerprint(), before, "Read-only import changed fixture files");
    report.status = "passed";
    for (const checkpoint of report.checkpoints) checkpoint.status = "passed";
    report.finishedAt = new Date().toISOString();
    await writeFile(path.join(output, "result.json"), JSON.stringify(report, null, 2) + "\n");
    return report;
  } catch (error) {
    report.status = "failed";
    report.failedPhase = phase;
    report.error = String(error);
    report.finishedAt = new Date().toISOString();
    // Preserve actual failing UI & completed checkpoints; never erase a partial run.
    await writeFile(path.join(output, "failure.txt"), await state()).catch(() => {});
    await app.getScreenshot({ emit: false }).then(bytes => writeFile(path.join(output, "failure.jpg"), bytes)).catch(() => {});
    await writeFile(path.join(output, "result.json"), JSON.stringify(report, null, 2) + "\n");
    throw error;
  }
}
