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
const BROKEN: &str = r"/\b(undefined|NaN|Unhandled|panicked)\b|\[object Object\]/";

struct Section {
    id: &'static str,
    title: &'static str,
    settings: bool,
    /// A label the notch-state fixture guarantees this settings section renders.
    expect: &'static str,
    /// Whether the section must show at least one switch.
    switches: bool,
}

// Overview is the section the hub opens on.
const SECTIONS: [Section; 10] = [
    Section { id: "overview", title: "Overview", settings: false, expect: "", switches: false },
    Section { id: "storage", title: "Storage", settings: false, expect: "", switches: false },
    Section { id: "cleanup", title: "Cleanup", settings: false, expect: "", switches: false },
    Section { id: "monitor", title: "Monitor", settings: false, expect: "", switches: false },
    Section { id: "apps", title: "Apps", settings: false, expect: "", switches: false },
    Section { id: "permissions", title: "Permissions", settings: true, expect: "Accessibility", switches: false },
    Section { id: "accounts", title: "Accounts", settings: true, expect: "codex@example.test", switches: true },
    Section { id: "appearance", title: "Appearance", settings: true, expect: "Fold for full-screen apps", switches: true },
    Section { id: "notifications", title: "Notifications", settings: true, expect: "When a session finishes", switches: true },
    Section { id: "general", title: "General", settings: true, expect: "Open Pulse at login", switches: true },
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

fn hub_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("PULSE_HUB_BIN") {
        return PathBuf::from(p);
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/target"));
    target.join("debug/pulse-hub")
}

/// What the notch publishes to `~/Library/Application Support/Pulse/notch-state.json`
/// (mac/Notch/Sources/System/HubBridge.swift), so the Settings sections render real
/// controls instead of "notch isn't running". Every key the views read is present.
fn notch_fixture() -> Value {
    let flags = [
        "foldsForFullScreen", "usesCustomNotchScale", "showsNotchReadings", "weeklyRingDashed", "weeklyHeadline",
        "weeklyReading", "claudeDailyPaceRing", "showUsagePace", "showCodexExtraLimits", "announceSessionEnd",
        "sessionEndSound", "announceUsageReset", "usageResetSound", "announceSessionLimitReached",
        "announceWeeklyLimitReached", "limitReachedSound", "launchAtLogin", "nearbyEnabled", "nearbyAcceptKnown",
        "autoUpdateCheck", "launcherEnabled", "convFnCommand", "convFinderCutPaste", "convWindowMaximizer",
        "convDockClickMinimize", "convDiskImageInstaller", "convAutoQuit", "windowManagementEnabled",
        "asksProviderOnLook",
    ];
    let mut settings = serde_json::Map::new();
    for (i, key) in flags.iter().enumerate() {
        settings.insert((*key).into(), json!(i % 2 == 0));
    }
    for (key, value) in [
        ("notchEdge", json!("top")),
        ("notchScope", json!("allDisplays")),
        ("displayPreference", json!("followActiveWindow")),
        ("notchVisibility", json!("always")),
        ("notchSize", json!("medium")),
        ("notchSurfaceStyle", json!("glass")),
        ("weeklyRing", json!("weekly")),
        ("resetTimeFormat", json!("relative")),
        ("accentColor", json!("blue")),
        ("colorTransitionStyle", json!("smooth")),
        ("language", json!("system")),
        ("notificationChannel", json!("notch")),
        ("peekDuration", json!("normal")),
        ("customNotchScale", json!(1.0)),
        ("watchLimit", json!(0.6)),
        ("criticalLimit", json!(0.85)),
        ("nearbyAlias", json!("")),
        ("nearbySaveFolder", json!("")),
    ] {
        settings.insert(key.into(), value);
    }
    let options = json!({
        "notchEdge": ["top", "bottom"],
        "notchScope": ["allDisplays", "activeDisplay"],
        "notchVisibility": ["always", "onHover"],
        "notchSize": ["small", "medium", "large"],
        "notchSurfaceStyle": ["glass", "solid"],
        "weeklyRing": ["off", "weekly"],
        "resetTimeFormat": ["relative", "clock"],
        "accentColor": ["blue", "green", "orange", "pink", "purple"],
        "colorTransitionStyle": ["smooth", "stepped"],
        "language": ["system", "english"],
        "notificationChannel": ["notch", "system"],
        "peekDuration": ["short", "normal", "long"],
        "launcherHotkey": ["optionSpace", "commandSpace"],
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default();
    json!({
        "schema": 1,
        "product": "Pulse",
        "version": "0.0.0-qa",
        "settings": settings,
        "options": options,
        "displays": [{"id": "qa-display", "name": "Built-in Display"}],
        "accounts": [
            {
                "id": "codex", "name": "Codex", "connected": true, "usesKeychain": false,
                "refusedAccess": false, "needsRenewal": false,
                "summary": "codex@example.test", "label": "codex@example.test", "plan": "Plus",
                "signInExplanation": "Pulse reads the Codex login on this Mac.",
                "limits": [{"label": "Session", "usedFraction": 0.25, "seconds": 18000}],
            },
            {
                "id": "claude", "name": "Claude", "connected": false, "usesKeychain": true,
                "refusedAccess": false, "needsRenewal": false,
                "summary": "claude@example.test", "label": "claude@example.test", "plan": "Pro",
                "signInExplanation": "Pulse reads the Claude login on this Mac.",
                "limits": [{"label": "Session", "usedFraction": 0.5, "seconds": 18000}],
                // Two Claude accounts: the signed-in one, and an older one saved with a
                // session window whose reset has passed (shown as "Reset", no percentage).
                "claudeAccounts": [
                    {
                        "id": "qa-active", "name": "Work", "email": "claude@example.test",
                        "plan": "Max 5x", "active": true, "capturedAt": now - 60.0,
                        "windows": [
                            {"label": "Current session", "usedFraction": 0.42, "resetsAt": now + 7200.0, "seconds": 18000},
                            {"label": "All models", "usedFraction": 0.3, "resetsAt": now + 3.0 * 86400.0, "seconds": 604800},
                        ],
                    },
                    {
                        "id": "qa-cached", "name": "old@example.test", "email": "old@example.test",
                        "plan": "Pro", "active": false, "capturedAt": now - 2.0 * 86400.0,
                        "windows": [
                            {"label": "Current session", "usedFraction": 0.9, "resetsAt": now - 40.0 * 3600.0, "seconds": 18000},
                            {"label": "All models", "usedFraction": 0.55, "resetsAt": now + 86400.0, "seconds": 604800},
                        ],
                    },
                ],
            },
        ],
        "providerOrder": ["codex", "claude"],
        "conveniences": {
            "accessibility": true, "inputMonitoring": true, "wanted": false, "fnStatus": "off",
            "fnDetail": "", "active": false, "runningApps": [], "autoQuitApps": [], "cutPasteResults": [],
            "windowManagement": {"enabled": false, "actions": []},
        },
        "launcherStatus": null,
        "helper": "notRegistered",
        "helperError": null,
        "permissions": [
            {"id": "accessibility", "title": "Accessibility", "why": "Lets Pulse move windows and read shortcuts.", "status": "granted", "required": true},
            {"id": "automation", "title": "Automation", "why": "Lets Pulse ask Finder to move files.", "status": "needsApproval", "required": false},
            {"id": "login", "title": "Open at login", "why": "Starts Pulse when you sign in.", "status": "off", "required": false},
        ],
        "permissionErrors": {},
        "driveAlertID": null,
        "updates": {"current": "0.0.0-qa", "available": null, "lastChecked": null, "status": "idle"},
        "system": {},
    })
}

/// Every visible switch must keep its design geometry: a 36x22 track with the knob
/// fully inside it and centred vertically. Returns how many switches were visible;
/// panics (after a FAIL screenshot) naming the section, the switch and the numbers.
fn check_switches(ctl: &Control, sec: &Section, shots: &Path) -> usize {
    let js = r#"
        const out = []; let count = 0; const seen = new Set();
        for (const t of document.querySelectorAll('[role=switch],.rk-toggle')) {
            if (seen.has(t)) continue; seen.add(t);
            const r = t.getBoundingClientRect();
            if (r.width === 0 && r.height === 0) continue;
            count++;
            const name = t.getAttribute('aria-label') || t.textContent.trim() || t.className;
            const f = (n) => Math.round(n * 100) / 100;
            if (Math.abs(r.height - 22) > 1 || Math.abs(r.width - 36) > 1) {
                out.push(`${name}: track is ${f(r.width)}x${f(r.height)}, want 36x22`);
            }
            const knob = t.querySelector('.rk-toggle__knob');
            if (!knob) { out.push(`${name}: no .rk-toggle__knob inside the track`); continue; }
            const k = knob.getBoundingClientRect();
            const inside = k.top >= r.top - 1 && k.bottom <= r.bottom + 1 && k.left >= r.left - 1 && k.right <= r.right + 1;
            const dy = (k.top + k.bottom) / 2 - (r.top + r.bottom) / 2;
            if (!inside || Math.abs(dy) > 1) {
                out.push(`${name}: knob y ${f(k.top)}..${f(k.bottom)} x ${f(k.left)}..${f(k.right)} vs track y ${f(r.top)}..${f(r.bottom)} x ${f(r.left)}..${f(r.right)}; centre offset ${f(dy)}px, inside=${inside}`);
            }
        }
        return {count, out};
    "#;
    let result = ctl.eval(js).expect("eval switch geometry");
    let count = result["count"].as_u64().unwrap_or(0) as usize;
    let problems: Vec<String> = result["out"]
        .as_array()
        .map(|a| a.iter().map(|v| s(v.clone())).collect())
        .unwrap_or_default();
    let missing = sec.switches && count == 0;
    if !problems.is_empty() || missing {
        let _ = ctl.screenshot_to(&shots.join(format!("FAIL-{}-switches.png", sec.id)));
        if missing {
            panic!("{} shows no [role=switch] controls (fixture should render them)", sec.title);
        }
        panic!("{} has {} malformed switch(es) of {count}:\n{}", sec.title, problems.len(), problems.join("\n"));
    }
    count
}

/// Accounts: both Claude accounts are listed by name, the signed-in one carries the Active
/// badge, the cached one's past session reset reads "Reset" (no percentage) while its
/// future weekly reset keeps its percentage, and renaming leaves a command for the notch.
fn claude_accounts_fail(ctl: &Control, shots: &Path, rows: &[Value], why: &str) -> ! {
    let _ = ctl.screenshot_to(&shots.join("FAIL-accounts-claude.png"));
    panic!("Claude accounts list: {why}; rows: {rows:?}");
}

fn check_claude_accounts(ctl: &Control, bridge: &Path, shots: &Path) {
    let js = r#"
        const rows = [...document.querySelectorAll('.ck-claude-account')].map((r) => ({
            name: r.querySelector('input')?.value ?? '',
            active: /\bActive\b/.test(r.querySelector('.ck-account-name')?.innerText ?? ''),
            forget: !!r.querySelector('.ck-account-name button'),
            wins: [...r.querySelectorAll('.ck-claude-win')].map((w) => ({
                state: w.dataset.state, value: w.querySelector('.ck-claude-value')?.textContent.trim(),
                text: w.innerText.replace(/\s+/g, ' '),
            })),
            asOf: /as of /.test(r.innerText),
        }));
        return rows;
    "#;
    let rows = ctl.eval(js).expect("eval claude accounts");
    let rows = rows.as_array().cloned().unwrap_or_default();
    let fail = |why: String| claude_accounts_fail(ctl, shots, &rows, &why);
    if rows.len() != 2 {
        fail(format!("expected 2 accounts, found {}", rows.len()));
    }
    let (active, cached) = (&rows[0], &rows[1]);
    if active["name"] != "Work" || active["active"] != true || active["forget"] != false || active["asOf"] != false {
        fail("the signed-in account should be \"Work\", Active, not forgettable and without an \"as of\" line".into());
    }
    if active["wins"][0]["value"] != "42%" || active["wins"][1]["value"] != "30%" {
        fail("the active account should show 42% and 30%".into());
    }
    if cached["name"] != "old@example.test" || cached["active"] != false || cached["forget"] != true || cached["asOf"] != true {
        fail("the cached account should be named by its address, not Active, forgettable, with an \"as of\" line".into());
    }
    if cached["wins"][0]["state"] != "reset" || cached["wins"][0]["value"] != "Reset" {
        fail("the cached session window is past its reset and should read \"Reset\"".into());
    }
    if cached["wins"][1]["value"] != "55%" || cached["wins"][1]["state"] != "live" {
        fail("the cached weekly window has not reset and should still show 55%".into());
    }
    if cached["wins"][0]["text"].as_str().unwrap_or("").contains('%') {
        fail("a window past its reset must show no percentage".into());
    }

    // Rename the cached account in the field, as a person would; the hub must leave the
    // notch a command (nothing runs a notch here, so it stays in hub-commands).
    let rename = r#"
        const input = document.querySelector('.ck-claude-account[data-account="qa-cached"] input');
        if (!input) return false;
        input.focus();
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, 'Old Pro');
        input.dispatchEvent(new Event('input', { bubbles: true }));
        input.blur();
        return true;
    "#;
    ctl.wait_eval(rename, Duration::from_secs(10)).expect("rename field");
    let commands = bridge.join("hub-commands");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let found = std::fs::read_dir(&commands)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
            .any(|c| c["command"] == "renameClaudeAccount" && c["id"] == "qa-cached" && c["name"] == "Old Pro");
        if found {
            break;
        }
        if std::time::Instant::now() > deadline {
            let _ = ctl.screenshot_to(&shots.join("FAIL-accounts-rename.png"));
            panic!("renaming a Claude account left no renameClaudeAccount command in {}", commands.display());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
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

        // The notch's published state, so Settings renders real controls (CI has no notch).
        let bridge = home.join("Library/Application Support/Pulse");
        std::fs::create_dir_all(&bridge).expect("bridge dir");
        std::fs::write(
            bridge.join("notch-state.json"),
            serde_json::to_vec(&notch_fixture()).expect("notch fixture json"),
        )
        .expect("write notch-state.json");

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
                // The fixture notch state must render this section's real controls.
                if let Err(e) = ctl.wait_for_text(None, sec.expect, Duration::from_secs(20)) {
                    let _ = ctl.screenshot_to(&shots.join(format!("FAIL-{}.png", sec.id)));
                    panic!(
                        "{} never showed fixture label {:?} ({}); page text:\n{}",
                        sec.title,
                        sec.expect,
                        e.0,
                        body_text(&ctl).chars().take(1500).collect::<String>()
                    );
                }
                let errors = ctl
                    .eval("return Array.from(document.querySelectorAll('.error')).map(e=>e.textContent.trim())")
                    .expect("eval errors");
                let errors = errors.as_array().cloned().unwrap_or_default();
                assert!(errors.is_empty(), "{} shows error text: {errors:?}", sec.title);
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

            // Geometry, on every section: a switch that is not 36x22 with a centred knob fails.
            check_switches(&ctl, sec, &shots);
            if sec.id == "accounts" {
                check_claude_accounts(&ctl, &bridge, &shots);
            }

            let shot = shots.join(format!("{:02}-{}.png", index + 1, sec.id));
            ctl.screenshot_to(&shot).expect("screenshot");
            sc.keep(&format!("{:02}-{}", index + 1, sec.id), &shot);
        }
        let _ = std::fs::remove_dir_all(&home);
    });
}
