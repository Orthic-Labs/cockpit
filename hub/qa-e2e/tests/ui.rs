//! Headless QA of the hub on `rightkit-qa`: launches the real debug build (`qa-native`),
//! hidden, with its in-app control server on, and tours every section read-only.
//! Nothing here clicks a destructive control.
//!
//! Run from the user's login session (the launcher needs a GUI session for `open`):
//!   (cd hub/src-tauri && cargo build --features qa-native,custom-protocol)
//!   (cd hub/qa-e2e && cargo test --test ui -- --nocapture)
//! The binary is `$PULSE_HUB_BIN`, else the hub's debug build under `src-tauri/target`
//! (or `$CARGO_TARGET_DIR`).
//! Screenshots go to `$PULSE_QA_SHOTS` (default: the scenario scratch dir).

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_qa::control::{self, Control, LaunchSpec, Mode};
use rightkit_qa::harness::Harness;
use rightkit_qa::workspace;
use serde_json::{json, Value};

const FIXTURE: &str = "alpha-folder";
const NO_NOTCH: &str = "notch isn't running";
const BROKEN: &str = r"/\b(undefined|NaN|Unhandled|panicked)\b|\[object Object\]/";

struct Section {
    id: &'static str,
    title: &'static str,
    settings: bool,
}

// Overview is the section the hub opens on.
const SECTIONS: [Section; 9] = [
    Section { id: "overview", title: "Overview", settings: false },
    Section { id: "storage", title: "Storage", settings: false },
    Section { id: "cleanup", title: "Cleanup", settings: false },
    Section { id: "monitor", title: "Monitor", settings: false },
    Section { id: "apps", title: "Apps", settings: false },
    Section { id: "accounts", title: "Accounts", settings: true },
    Section { id: "appearance", title: "Appearance", settings: true },
    Section { id: "notifications", title: "Notifications", settings: true },
    Section { id: "general", title: "General", settings: true },
];

fn s(v: Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn body_text(ctl: &Control) -> String {
    s(ctl.eval("return document.body.innerText").expect("eval body text"))
}

fn title(ctl: &Control) -> String {
    s(ctl
        .eval("return document.querySelector('.rk-top__title')?.textContent?.trim() ?? ''")
        .expect("eval title"))
}

fn wait_text(ctl: &Control, needle: &str, timeout: Duration, what: &str) {
    ctl.wait_for_text(None, needle, timeout).unwrap_or_else(|e| panic!("{what}: {e:?}"));
}

fn hub_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("PULSE_HUB_BIN") {
        return PathBuf::from(p);
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/target"));
    target.join("debug/pulse-hub")
}

#[test]
fn hub_sections_render_without_errors() {
    let h = Harness::new("pulse-hub", env!("CARGO_MANIFEST_DIR")).expect("rightkit-qa harness");
    h.scenario("hub sections render without errors", "fast", &["platform:darwin"], |sc| {
        let ws = workspace::create(&sc.scratch("ws"), None, "pulse-hub").expect("qa workspace");
        // The app derives HOME from RIGHTKIT_QA_DATA_DIR/home (macOS launches carry only
        // RIGHTKIT_* keys), so Storage scans this fixture and never the user's real home.
        // rightkit-qa >= 0.2.12 requires env paths to live inside the QA app home
        // (/private/tmp/rightkit-qa/…); the scanner resolves /private paths.
        let home = ws.home.join("fixture-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(FIXTURE)).expect("fixture dir");
        std::fs::write(home.join(FIXTURE).join("fixture.bin"), vec![0u8; 65536]).expect("fixture file");

        let shots: PathBuf = std::env::var_os("PULSE_QA_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| sc.scratch("screenshots"));
        std::fs::create_dir_all(&shots).expect("screenshots dir");

        let spec = LaunchSpec {
            binary: hub_binary(),
            mode: Mode::Hidden,
            env: vec![("RIGHTKIT_PULSE_QA_HOME".into(), home.to_string_lossy().into_owned())],
            startup_timeout: Duration::from_secs(90),
            label: "Pulse".into(),
        };
        let ctl = control::launch(&spec, &ws, sc.tracker()).expect("launch pulse-hub");
        ctl.wait_eval("return !!document.querySelector('nav.rk-nav')", Duration::from_secs(30))
            .expect("nav.rk-nav never appeared");

        for (index, sec) in SECTIONS.iter().enumerate() {
            if index > 0 {
                // A DOM click on the nav item (Overview is the initial section).
                let js = format!(
                    "const b=[...document.querySelectorAll('nav.rk-nav button')].find(e=>e.textContent.trim()==={}); if(!b) return false; b.click(); return true;",
                    json!(sec.title)
                );
                ctl.wait_eval(&js, Duration::from_secs(10))
                    .unwrap_or_else(|e| panic!("nav item {}: {}", sec.title, e.0));
            }
            let want = format!("return document.querySelector('.rk-top__title')?.textContent?.trim() === {}", json!(sec.title));
            if let Err(e) = ctl.wait_eval(&want, Duration::from_secs(15)) {
                let dom = s(ctl
                    .eval("return location.href + '\\n' + document.body.innerHTML.slice(0, 2500)")
                    .unwrap_or(Value::Null));
                let _ = ctl.screenshot_to(&shots.join(format!("FAIL-{}.png", sec.id)));
                panic!("title never became {} (is {:?}): {}\n{dom}", sec.title, title(&ctl), e.0);
            }
            assert_eq!(title(&ctl), sec.title);

            if sec.settings {
                // CI has no notch, so Settings must degrade to its explanatory state.
                wait_text(&ctl, NO_NOTCH, Duration::from_secs(15), &format!("{} did not show the notch-not-running state", sec.title));
            } else {
                if sec.id == "storage" {
                    // Folders is one pane of Storage's workspace; open it, then
                    // wait for the fixture row itself ("Rescan" shows before the scan starts).
                    ctl.wait_eval(
                        "const b=[...document.querySelectorAll('button,[role=radio],[role=tab]')].find(e=>e.textContent.trim()==='Folders'); if(!b) return false; b.click(); return true;",
                        Duration::from_secs(30),
                    )
                    .unwrap_or_else(|e| panic!("Storage has no Folders control: {}", e.0));
                    if let Err(e) = ctl.wait_for_text(None, FIXTURE, Duration::from_secs(60)) {
                        let text = body_text(&ctl);
                        panic!(
                            "Storage did not list fixture entry {FIXTURE} ({}); fixture home {}; page text:\n{}",
                            e.0,
                            home.display(),
                            text.chars().take(1500).collect::<String>()
                        );
                    }
                } else {
                    // Let the view's first load settle.
                    std::thread::sleep(Duration::from_millis(1500));
                }
                let errors = ctl
                    .eval("return Array.from(document.querySelectorAll('.error')).map(e=>e.textContent.trim())")
                    .expect("eval errors");
                let errors = errors.as_array().cloned().unwrap_or_default();
                assert!(errors.is_empty(), "{} shows error text: {errors:?}", sec.title);
            }

            let broken = s(ctl
                .eval(&format!("const m=document.body.innerText.match({BROKEN}); return m?m[0]:''"))
                .expect("eval broken text"));
            assert!(broken.is_empty(), "{} shows broken text \"{broken}\"", sec.title);

            let shot = shots.join(format!("{:02}-{}.png", index + 1, sec.id));
            ctl.screenshot_to(&shot).expect("screenshot");
            sc.keep(&format!("{:02}-{}", index + 1, sec.id), &shot);
        }
        let _ = std::fs::remove_dir_all(&home);
    });
}
