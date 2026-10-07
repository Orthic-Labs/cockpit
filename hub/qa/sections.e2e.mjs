import fs from "node:fs";
import path from "node:path";

// Read-only tour of every hub section. Nothing here clicks a destructive control:
// it navigates, reads text and saves a screenshot per section as evidence.
const shots = path.resolve(
  process.env.COCKPIT_QA_SHOTS ?? path.join(process.env.RIGHTKIT_QA_EVIDENCE_DIR ?? ".cache/rightkit-qa", "screenshots"),
);
fs.mkdirSync(shots, { recursive: true });

const sections = [
  { id: "storage", title: "Storage" },
  { id: "cleanup", title: "Cleanup" },
  { id: "monitor", title: "Monitor" },
  { id: "apps", title: "Apps" },
  { id: "accounts", title: "Accounts", settings: true },
  { id: "appearance", title: "Appearance", settings: true },
  { id: "notifications", title: "Notifications", settings: true },
  { id: "general", title: "General", settings: true },
];

const BROKEN = /\b(undefined|NaN|Unhandled|panicked)\b|\[object Object\]/;
const NO_NOTCH = "notch isn't running";

const pageTitle = () => browser.execute(() => document.querySelector(".rk-page__head h1")?.textContent?.trim() ?? "");
const bodyText = () => browser.execute(() => document.body.innerText);

describe("Cockpit hub sections", () => {
  before(async () => {
    await $("nav.rk-nav").waitForExist({ timeout: 30_000 });
  });

  for (const [index, section] of sections.entries()) {
    it(`${section.title} renders its title without errors`, async () => {
      if (index > 0 || section.id !== "storage") {
        const item = await $(`//nav[contains(@class,'rk-nav')]//button[normalize-space(.)='${section.title}']`);
        await item.waitForExist({ timeout: 10_000 });
        await item.click();
      }
      await browser.waitUntil(async () => (await pageTitle()) === section.title, {
        timeout: 15_000,
        timeoutMsg: `title never became ${section.title}`,
      });

      if (section.settings) {
        // CI has no notch, so Settings must degrade to its explanatory state.
        await browser.waitUntil(async () => (await bodyText()).includes(NO_NOTCH), {
          timeout: 15_000,
          timeoutMsg: `${section.title} did not show the notch-not-running state`,
        });
      } else {
        // Let the view's first load settle (HOME is a tiny fixture folder in CI).
        if (section.id === "storage") {
          await browser.waitUntil(async () => (await bodyText()).includes("Rescan"), {
            timeout: 30_000,
            timeoutMsg: "Storage scan of the fixture folder never finished",
          });
          if (process.env.COCKPIT_QA_FIXTURE_NAME) {
            const text = await bodyText();
            if (!text.includes(process.env.COCKPIT_QA_FIXTURE_NAME)) {
              throw new Error(`Storage did not list fixture entry ${process.env.COCKPIT_QA_FIXTURE_NAME}`);
            }
          }
        } else {
          await browser.pause(1500);
        }
        const errors = await browser.execute(() =>
          Array.from(document.querySelectorAll(".error")).map((e) => e.textContent.trim()),
        );
        if (errors.length) throw new Error(`${section.title} shows error text: ${errors.join(" | ")}`);
      }

      const text = await bodyText();
      const broken = text.match(BROKEN);
      if (broken) throw new Error(`${section.title} shows broken text "${broken[0]}"`);
      await browser.saveScreenshot(path.join(shots, `${String(index + 1).padStart(2, "0")}-${section.id}.png`));
    });
  }
});
