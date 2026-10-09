//! Headless QA of the hub on `rightkit-qa`: launches the real debug build (`qa-native`),
//! hidden, with its in-app control server on, and tours every section read-only (the only
//! controls pressed are the ones whose effect is a command file left for the notch).
//!
//! Runs on macOS and on Windows. The two differ in where the notch's published state lives and
//! in what the notch publishes: the Mac fixture is the full Mac snapshot, the Windows fixture is
//! the smaller snapshot of the Windows notch (`windows/src/bridge.rs`), so the Windows journey
//! exercises the hub's "show a control only when the notch published its key" gating.
//!
//! Run from the user's login session (macOS: the launcher needs a GUI session for `open`):
//!   (cd hub/src-tauri && cargo build --features qa-native,custom-protocol)
//!   (cd hub/qa-e2e && cargo test --test ui -- --nocapture)
//! The binary is `$PULSE_HUB_BIN`, else the hub's debug build under `src-tauri/target`
//! (or `$CARGO_TARGET_DIR`); `pulse-hub.exe` on Windows.
//! Screenshots go to `$PULSE_QA_SHOTS` (default: the scenario scratch dir).
//!
//! Windows only, second scenario: `$PULSE_QA_NOTCH_BIN` names the notch executable
//! (`windows/target/debug/pulse-windows-prototype.exe`). The step starts that real notch and mocks
//! nothing, so it runs only where the variable is set (scripts/gate.sh sets it on the CI runner).
//! A developer's machine is left alone: the notch draws its panels on the desktop, owns one
//! per-user instance mutex and removes the user's start-with-Windows registry entry at start.

#![cfg(any(target_os = "macos", windows))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_qa::control::{self, Control, LaunchSpec, Mode};
use rightkit_qa::harness::{Harness, Scenario};
use rightkit_qa::workspace::{self, QaWorkspace};
use serde_json::{json, Value};

const FIXTURE: &str = "alpha-folder";
const BROKEN: &str = r"/\b(undefined|NaN|Unhandled|panicked)\b|\[object Object\]/";
const PLATFORM: &str = if cfg!(windows) { "platform:windows" } else { "platform:darwin" };
/// macOS keeps its original behaviour: the first failed assertion panics. Windows gathers every
/// content problem of the tour and reports them together after the last screenshot, so one CI run
/// shows all of them.
const STRICT: bool = cfg!(target_os = "macos");

struct Section {
    id: &'static str,
    title: &'static str,
    settings: bool,
    /// A label the notch-state fixture guarantees this settings section renders.
    expect: &'static str,
    /// More labels this settings section must show once `expect` has appeared.
    also: &'static [&'static str],
    /// Whether the section must show at least one switch.
    switches: bool,
}

// Overview is the section the hub opens on.
#[cfg(target_os = "macos")]
static SECTIONS: [Section; 10] = [
    Section { id: "overview", title: "Overview", settings: false, expect: "", also: &[], switches: false },
    Section { id: "storage", title: "Storage", settings: false, expect: "", also: &[], switches: false },
    Section { id: "cleanup", title: "Cleanup", settings: false, expect: "", also: &[], switches: false },
    Section { id: "monitor", title: "Monitor", settings: false, expect: "", also: &[], switches: false },
    Section { id: "apps", title: "Apps", settings: false, expect: "", also: &[], switches: false },
    Section { id: "permissions", title: "Permissions", settings: true, expect: "Accessibility", also: &[], switches: false },
    Section { id: "accounts", title: "Accounts", settings: true, expect: "codex@example.test", also: &[], switches: true },
    Section { id: "appearance", title: "Appearance", settings: true, expect: "Fold for full-screen apps", also: &[], switches: true },
    Section { id: "notifications", title: "Notifications", settings: true, expect: "When a session finishes", also: &[], switches: true },
    // Nearby sharing lives inside General on the hub.
    Section { id: "general", title: "General", settings: true, expect: "Open Pulse at login", also: &["Nearby sharing", "Send and receive files nearby"], switches: true },
];

// The Windows notch publishes far fewer settings, so each section expects only what that
// snapshot (windows_notch_fixture) makes the hub render. Permissions are read from the system.
#[cfg(windows)]
static SECTIONS: [Section; 10] = [
    Section { id: "overview", title: "Overview", settings: false, expect: "", also: &[], switches: false },
    Section { id: "storage", title: "Storage", settings: false, expect: "", also: &[], switches: false },
    Section { id: "cleanup", title: "Cleanup", settings: false, expect: "", also: &[], switches: false },
    Section { id: "monitor", title: "Monitor", settings: false, expect: "", also: &[], switches: false },
    Section { id: "apps", title: "Apps", settings: false, expect: "", also: &[], switches: false },
    Section { id: "permissions", title: "Permissions", settings: true, expect: "Start with Windows", also: &[], switches: false },
    Section { id: "accounts", title: "Accounts", settings: true, expect: "Up to date", also: &[], switches: true },
    Section { id: "appearance", title: "Appearance", settings: true, expect: "Fold to a pill when the pointer leaves", also: &[], switches: true },
    Section { id: "notifications", title: "Notifications", settings: true, expect: "When a limit resets", also: &[], switches: true },
    Section { id: "general", title: "General", settings: true, expect: "Open Pulse at login", also: &["Nearby sharing", "Send and receive files nearby"], switches: true },
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
    target.join(if cfg!(windows) { "debug/pulse-hub.exe" } else { "debug/pulse-hub" })
}

/// Records a content problem: macOS stops at the first one, Windows collects them all.
fn flag(problems: &mut Vec<String>, msg: String) {
    if STRICT {
        panic!("{msg}");
    } else {
        problems.push(msg);
    }
}

/// A fresh isolated QA workspace. On Windows rightkit-qa canonicalises its paths to the
/// `\\?\C:\...` verbatim form and puts them in HOME, USERPROFILE, LOCALAPPDATA and
/// WEBVIEW2_USER_DATA_FOLDER; WebView2 and the Win32 settings code take plain drive paths, so
/// they are rewritten to that form here (they still all live inside the workspace home, which is
/// rewritten with them, so the launcher's "inside the QA app home" check holds).
fn fresh_workspace(sc: &Scenario) -> QaWorkspace {
    let ws = workspace::create(&sc.scratch("ws"), None, "pulse-hub").expect("qa workspace");
    #[cfg(windows)]
    let ws = plain_workspace(ws);
    ws
}

#[cfg(windows)]
fn plain_str(path: &str) -> Option<String> {
    path.strip_prefix(r"\\?\").filter(|rest| !rest.starts_with("UNC\\")).map(str::to_string)
}

#[cfg(windows)]
fn plain_workspace(mut ws: QaWorkspace) -> QaWorkspace {
    if let Some(p) = plain_str(&ws.home.to_string_lossy()) {
        ws.home = PathBuf::from(p);
    }
    if let Some(p) = plain_str(&ws.data_dir.to_string_lossy()) {
        ws.data_dir = PathBuf::from(p);
    }
    for value in ws.env.values_mut() {
        if let Some(p) = plain_str(value) {
            *value = p;
        }
    }
    ws
}

/// Where the notch publishes `notch-state.json` and reads `hub-commands`, and where the hub looks:
/// `~/Library/Application Support/Pulse` on the Mac; `%LOCALAPPDATA%\Pulse` on Windows, where the
/// launcher points LOCALAPPDATA at the workspace data dir (workspace.rs sets it with USERPROFILE).
fn bridge_dir(home: &Path, ws: &QaWorkspace) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let _ = ws;
        home.join("Library/Application Support/Pulse")
    }
    #[cfg(windows)]
    {
        let _ = home;
        ws.data_dir.join("Pulse")
    }
}

fn launch_hub(ws: &QaWorkspace, home: &Path, sc: &Scenario) -> Control {
    let spec = LaunchSpec {
        binary: hub_binary(),
        mode: Mode::Hidden,
        env: vec![("RIGHTKIT_PULSE_QA_HOME".into(), home.to_string_lossy().into_owned())],
        startup_timeout: Duration::from_secs(90),
        label: "Pulse".into(),
    };
    let ctl = control::launch(&spec, ws, sc.tracker()).expect("launch pulse-hub");
    ctl.wait_eval("return !!document.querySelector('nav.rk-nav')", Duration::from_secs(30))
        .expect("nav.rk-nav never appeared");
    ctl
}

/// Every command file the hub has left for the notch (nothing runs a notch in the tour, so they stay).
fn commands_seen(commands: &Path) -> Vec<Value> {
    std::fs::read_dir(commands)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .collect()
}

fn wait_command(commands: &Path, timeout: Duration, want: &dyn Fn(&Value) -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if commands_seen(commands).iter().any(|c| want(c)) {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// What the notch publishes to `~/Library/Application Support/Pulse/notch-state.json`
/// (mac/Notch/Sources/System/HubBridge.swift), so the Settings sections render real
/// controls instead of "notch isn't running". Every key the views read is present.
#[cfg(target_os = "macos")]
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
                // Three Claude accounts: the signed-in one, an older one saved with a
                // session window whose reset has passed (shown as "Reset", no percentage;
                // its folder is gone, so it can be forgotten), and one that exists only as a
                // folder on disk, never read ("No reading yet", nameable, not forgettable).
                "claudeAccounts": [
                    {
                        "id": "qa-active", "name": "Work", "email": "claude@example.test",
                        "plan": "Max 5x", "active": true, "onDisk": true, "capturedAt": now - 60.0,
                        "windows": [
                            {"label": "Current session", "usedFraction": 0.42, "resetsAt": now + 7200.0, "seconds": 18000},
                            {"label": "All models", "usedFraction": 0.3, "resetsAt": now + 3.0 * 86400.0, "seconds": 604800},
                        ],
                    },
                    {
                        "id": "qa-cached", "name": "old@example.test", "email": "old@example.test",
                        "plan": "Pro", "active": false, "onDisk": false, "capturedAt": now - 2.0 * 86400.0,
                        "windows": [
                            {"label": "Current session", "usedFraction": 0.9, "resetsAt": now - 40.0 * 3600.0, "seconds": 18000},
                            {"label": "All models", "usedFraction": 0.55, "resetsAt": now + 86400.0, "seconds": 604800},
                        ],
                    },
                    {
                        "id": "5e6f7a8b-0000-4000-8000-000000000001", "name": "Claude 5e6f7a8b",
                        "active": false, "onDisk": true, "capturedAt": null, "windows": [],
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

/// What the Windows notch publishes to `%LOCALAPPDATA%\Pulse\notch-state.json`
/// (windows/src/bridge.rs `state_json`): `product` "Pulse", `platform` "windows", the settings it
/// accepts (Mac key names where both systems have one, `pill-settings.json` names for the
/// Windows-only ones), `notchEdge`/`notchVisibility`/`notchSize` options only (size: `notchSize`, `usesCustomNotchScale`,
/// `customNotchScale`; `notchEdge` is the primary monitor's edge), accounts without Mac
/// fields (no `claudeAccounts`, no `seconds`), three permission rows with no title, and
/// `updates` without a status. No `conveniences`, `helper` or `system`. Nearby sharing is off
/// here so the hub does not open its LocalSend socket (the hub reads that switch from
/// pill-settings.json on Windows, written beside this file).
#[cfg(windows)]
fn windows_notch_fixture() -> Value {
    json!({
        "schema": 1,
        "product": "Pulse",
        "platform": "windows",
        "version": "0.2.0",
        "settings": {
            "launchAtLogin": false,
            "autoUpdateCheck": true,
            "nearbyEnabled": false,
            "nearbyAcceptKnown": false,
            "announceUsageReset": true,
            "announceSessionLimitReached": true,
            "announceWeeklyLimitReached": true,
            "mac_shortcuts": false,
            "screenshot_shortcuts": true,
            "screenshot_to_desktop": true,
            "installer_auto": false,
            "folds": true,
            "visible": true,
            "mute_claude_alerts": false,
            "mute_codex_alerts": false,
            "settingsWritable": true,
            "nearbyAlias": null,
            "nearbySaveFolder": null,
            "cadence_seconds": 2,
            "notchVisibility": "onHover",
            "notchEdge": "top",
            "notchSize": "medium",
            "usesCustomNotchScale": false,
            "customNotchScale": 1.0,
            "edges": {},
        },
        "options": {
            "notchEdge": ["top", "bottom", "left", "right"],
            "notchVisibility": ["alwaysShow", "onHover", "hidden"],
            "notchSize": ["small", "medium", "large"],
        },
        "displays": [],
        "accounts": [
            {
                "id": "claude", "name": "Claude", "connected": true, "usesKeychain": false,
                "refusedAccess": false, "needsRenewal": false,
                "signInExplanation": "Up to date", "summary": "Up to date", "label": null, "plan": "Max 5x",
                "limits": [
                    {"label": "Session", "usedFraction": 0.42},
                    {"label": "All models", "usedFraction": 0.30},
                ],
            },
            {
                "id": "codex", "name": "Codex", "connected": true, "usesKeychain": false,
                "refusedAccess": false, "needsRenewal": false,
                "signInExplanation": "Up to date", "summary": "Up to date", "label": null, "plan": "Plus",
                "limits": [
                    {"label": "5h limit", "usedFraction": 0.25},
                    {"label": "Weekly limit", "usedFraction": 0.10},
                    {"label": "Spark \u{b7} 5h limit", "usedFraction": 0.05},
                ],
            },
        ],
        "providerOrder": ["claude", "codex"],
        "permissions": [
            {"id": "startup", "status": "off", "required": false},
            {"id": "notifications", "status": "unknown", "required": false},
            {"id": "firewall", "status": "unknown", "required": false},
        ],
        "permissionErrors": {},
        "updates": {"current": "0.2.0", "autoCheck": true},
    })
}

/// Every visible switch must keep its design geometry: a 36x22 track with the knob
/// fully inside it and centred vertically. Returns how many switches were visible and, when
/// a switch is malformed (or a section that must show switches shows none), a description
/// naming the section, the switch and the numbers.
fn switch_problem(ctl: &Control, sec: &Section) -> Option<String> {
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
    if sec.switches && count == 0 {
        return Some(format!("{} shows no [role=switch] controls (fixture should render them)", sec.title));
    }
    if !problems.is_empty() {
        return Some(format!("{} has {} malformed switch(es) of {count}:\n{}", sec.title, problems.len(), problems.join("\n")));
    }
    None
}

/// Accounts: the two read Claude accounts are rows (the signed-in one with the Active badge and
/// percentages, a named one by its name), the never-read one is folded into one "never seen signed
/// in" line that expands to its name and "no reading" with rename enabled and no Forget, the cached one's past session reset reads "\u{2014}" (no percentage) while its
/// future weekly reset keeps its percentage, and renaming leaves a command for the notch.
#[cfg(target_os = "macos")]
fn claude_accounts_fail(ctl: &Control, shots: &Path, rows: &[Value], why: &str) -> ! {
    let _ = ctl.screenshot_to(&shots.join("FAIL-accounts-claude.png"));
    panic!("Claude accounts list: {why}; rows: {rows:?}");
}

#[cfg(target_os = "macos")]
fn check_claude_accounts(ctl: &Control, bridge: &Path, shots: &Path) {
    let js = r#"
        const rows = [...document.querySelectorAll('.ck-claude-account')].map((r) => ({
            name: r.querySelector('.ck-claude-name-text')?.textContent ?? '',
            active: /\bActive\b/.test(r.querySelector('.ck-account-name')?.innerText ?? ''),
            forget: !!r.querySelector('.ck-forget'),
            wins: [...r.querySelectorAll('.ck-claude-win')].map((w) => ({
                state: w.dataset.state, value: w.querySelector('.ck-claude-value')?.textContent.trim(),
                text: w.innerText.replace(/\s+/g, ' '),
            })),
            asOf: /as of /.test(r.innerText),
            noReading: /no reading/i.test(r.innerText),
            inputs: r.querySelectorAll('input').length,
            renamable: !!r.querySelector('.ck-claude-name') && !r.querySelector('.ck-claude-name').disabled,
        }));
        const line = document.querySelector('.ck-claude-unseen');
        return { rows, unseenLine: line ? line.textContent.trim() : null, unseenOpen: line?.getAttribute('aria-expanded') === 'true' };
    "#;
    let state = ctl.eval(js).expect("eval claude accounts");
    let rows = state["rows"].as_array().cloned().unwrap_or_default();
    let fail = |why: String| claude_accounts_fail(ctl, shots, &rows, &why);
    // The never-read account is folded into one line; only the two read accounts are rows.
    if rows.len() != 2 {
        fail(format!("expected 2 account rows before expanding, found {}", rows.len()));
    }
    if state["unseenLine"] != "1 account never seen signed in" || state["unseenOpen"] != false {
        fail(format!("expected the collapsed \"1 account never seen signed in\" line, found {}", state["unseenLine"]));
    }
    if rows.iter().any(|r| r["inputs"] != 0) {
        fail("no account row may show an input while not editing".into());
    }
    let (active, cached) = (&rows[0], &rows[1]);
    let is_raw_id = |n: &Value| {
        let n = n.as_str().unwrap_or("");
        n.strip_prefix("Claude ").is_some_and(|h| h.len() == 8 && h.chars().all(|c| c.is_ascii_hexdigit()))
    };
    if is_raw_id(&active["name"]) || is_raw_id(&cached["name"]) {
        fail("a named account must show its name, never \"Claude <8 hex>\"".into());
    }
    if active["name"] != "Work" || active["active"] != true || active["forget"] != false || active["asOf"] != false {
        fail("the signed-in account should be \"Work\", Active, not forgettable and without an \"as of\" line".into());
    }
    if active["wins"][0]["value"] != "42%" || active["wins"][1]["value"] != "30%" {
        fail("the active account should show 42% and 30%".into());
    }
    if cached["name"] != "old@example.test" || cached["active"] != false || cached["forget"] != true || cached["asOf"] != true {
        fail("the cached account should be named by its address, not Active, forgettable, with an \"as of\" line".into());
    }
    if cached["wins"][0]["state"] != "reset" || cached["wins"][0]["value"] != "\u{2014}" {
        fail("the cached session window is past its reset and should read \"\u{2014}\"".into());
    }
    if cached["wins"][1]["value"] != "55%" || cached["wins"][1]["state"] != "live" {
        fail("the cached weekly window has not reset and should still show 55%".into());
    }
    if cached["wins"][0]["text"].as_str().unwrap_or("").contains('%') {
        fail("a window past its reset must show no percentage".into());
    }
    // Expand the folded line: the never-read account then shows its name and "no reading".
    ctl.wait_eval(
        "const b = document.querySelector('.ck-claude-unseen'); if (!b) return false; if (b.getAttribute('aria-expanded') !== 'true') b.click(); return !!document.querySelector('.ck-claude-account[data-account=\"5e6f7a8b-0000-4000-8000-000000000001\"]');",
        Duration::from_secs(10),
    )
    .expect("expand never-seen accounts");
    let all = ctl.eval(js).expect("eval claude accounts expanded");
    let all = all["rows"].as_array().cloned().unwrap_or_default();
    let fail = |why: String| claude_accounts_fail(ctl, shots, &all, &why);
    if all.len() != 3 {
        fail(format!("expected 3 account rows after expanding, found {}", all.len()));
    }
    let folder = &all[2];
    if folder["name"] != "Claude 5e6f7a8b"
        || folder["active"] != false
        || folder["forget"] != false
        || folder["asOf"] != false
        || folder["noReading"] != true
        || folder["renamable"] != true
        || folder["wins"].as_array().is_some_and(|w| !w.is_empty())
    {
        fail("the folder-only account should read \"Claude 5e6f7a8b\", \"no reading\", have its rename enabled, no windows, no \"as of\" line and no Forget".into());
    }

    // Rename an account as a person would: click its name, type in the inline field, press away; the hub must leave the notch a
    // command (nothing runs a notch here, so it stays in hub-commands). The folder-only
    // account is named before it was ever read.
    let commands = bridge.join("hub-commands");
    for (id, name) in [("qa-cached", "Old Pro"), ("5e6f7a8b-0000-4000-8000-000000000001", "Spare")] {
        let rename = format!(
            r#"
            const row = document.querySelector('.ck-claude-account[data-account="{id}"]');
            if (!row) return false;
            const input = row.querySelector('input');
            if (!input) {{ row.querySelector('.ck-claude-name')?.click(); return false; }}
            input.focus();
            Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, '{name}');
            input.dispatchEvent(new Event('input', {{ bubbles: true }}));
            input.blur();
            return true;
        "#
        );
        ctl.wait_eval(&rename, Duration::from_secs(10)).expect("rename field");
        let found = wait_command(&commands, Duration::from_secs(10), &|c| {
            c["command"] == "renameClaudeAccount" && c["id"] == id && c["name"] == name
        });
        if !found {
            let _ = ctl.screenshot_to(&shots.join("FAIL-accounts-rename.png"));
            panic!("renaming Claude account {id} left no renameClaudeAccount command in {}", commands.display());
        }
    }
}

/// What every open section must satisfy on both systems: no error text, no "undefined/NaN/[object Object]/
/// panicked", and every switch keeps its design geometry (a 36x22 track with a centred knob).
fn generic_checks(ctl: &Control, sec: &Section, shots: &Path, problems: &mut Vec<String>) {
    let errors = ctl
        .eval("return Array.from(document.querySelectorAll('.error')).map(e=>e.textContent.trim())")
        .expect("eval errors");
    let errors = errors.as_array().cloned().unwrap_or_default();
    if !errors.is_empty() {
        flag(problems, format!("{} shows error text: {errors:?}", sec.title));
    }

    let broken = s(ctl
        .eval(&format!("const m=document.body.innerText.match({BROKEN}); return m?m[0]:''"))
        .expect("eval broken text"));
    if !broken.is_empty() {
        flag(problems, format!("{} shows broken text \"{broken}\"", sec.title));
    }

    if let Some(problem) = switch_problem(ctl, sec) {
        let _ = ctl.screenshot_to(&shots.join(format!("FAIL-{}-switches.png", sec.id)));
        flag(problems, problem);
    }
}

// ------------------------------------------------------------------------------ Windows checks

/// The shell's caption buttons and platform marking: frameless window, three buttons the shell draws.
#[cfg(windows)]
const WINDOWS_CHROME_JS: &str = r#"
    const caps = [...document.querySelectorAll('.rk-caption__btn')].map((b) => {
        const r = b.getBoundingClientRect();
        return { label: b.getAttribute('aria-label'), x: r.x, y: r.y, w: r.width, h: r.height, right: r.right };
    });
    const shell = document.querySelector('.rk-shell');
    return { caps, platform: shell ? shell.getAttribute('data-platform') : null,
             windowsUa: /Windows/.test(navigator.userAgent), width: window.innerWidth };
"#;

/// Mac wording that must never reach the Windows hub (Overview's "Your Mac is running well" is the
/// kind of leftover this catches). Case-sensitive: a lower-case "finder" is ordinary English.
#[cfg(windows)]
const MAC_WORDING_JS: &str = r#"
    const m = document.body.innerText.match(/\b(this Mac|your Mac|System Settings|Finder|Codenotch|macOS|Trash)\b/g);
    return m ? [...new Set(m)] : [];
"#;

/// A picker with no options is a control that does nothing: every <select> and radio group must
/// have labelled options; in Settings (where the fixture decides the selection) exactly one radio is on.
#[cfg(windows)]
const PICKERS_JS: &str = r#"
    const out = [];
    const settings = __SETTINGS__;
    for (const sel of document.querySelectorAll('select')) {
        const name = sel.getAttribute('aria-label') || sel.className || 'select';
        const options = [...sel.options];
        if (options.length === 0) { out.push(`select "${name}" has no options`); continue; }
        if (options.some((o) => !o.textContent.trim())) out.push(`select "${name}" has an option with no text`);
        if (settings && (sel.selectedIndex < 0 || !sel.value)) out.push(`select "${name}" has no selected option`);
    }
    for (const group of document.querySelectorAll('[role=radiogroup]')) {
        const name = group.getAttribute('aria-label') || 'radio group';
        const items = [...group.querySelectorAll('[role=radio]')];
        if (items.length === 0) { out.push(`radio group "${name}" has no options`); continue; }
        if (items.some((i) => !i.textContent.trim() && !i.getAttribute('aria-label'))) out.push(`radio group "${name}" has an unlabelled option`);
        const on = items.filter((i) => i.getAttribute('aria-checked') === 'true').length;
        if (settings && on !== 1) out.push(`radio group "${name}" has ${on} selected options, want 1`);
    }
    for (const range of document.querySelectorAll('input[type=range]')) {
        if (!Number.isFinite(Number(range.value))) out.push(`slider "${range.getAttribute('aria-label') || ''}" has a non-numeric value`);
    }
    return out;
"#;

/// Text each Windows settings section must and must not show with the Windows notch's snapshot.
#[cfg(windows)]
fn windows_expectations(id: &str) -> (&'static [&'static str], &'static [&'static str]) {
    const NONE: &[&str] = &[];
    match id {
        "permissions" => {
            const HAVE: &[&str] = &[
                "Notifications", "Start with Windows", "Windows Firewall", "Open Settings", "Open Firewall",
                "Every required permission is granted.",
            ];
            const LACK: &[&str] = &["Full Disk Access", "Accessibility", "Input Monitoring"];
            (HAVE, LACK)
        }
        "accounts" => {
            const HAVE: &[&str] = &["Logins Pulse reads", "Claude", "Codex", "Shown", "Up to date"];
            const LACK: &[&str] = &["Forget reading", "Allow access", "never seen signed in", "Hidden"];
            (HAVE, LACK)
        }
        "appearance" => {
            const HAVE: &[&str] = &[
                "Placement", "Edge", "Show", "Alt-drag the notch to slide it along its edge",
                "Fold to a pill when the pointer leaves", "Reset position", "Size and surface", "Size", "Custom size",
            ];
            // Scale appears only while Custom size is on; Surface (glass/solid) is not published.
            const LACK: &[&str] = &[
                "Option-drag", "Rings", "Colour", "Language", "Accent", "Surface", "Scale", "Fold for full-screen apps",
            ];
            (HAVE, LACK)
        }
        "notifications" => {
            const HAVE: &[&str] = &[
                "When a limit resets", "When the session limit is reached", "When the weekly limit is reached",
                "Mute alerts", "Mute Claude alerts", "Mute Codex alerts",
            ];
            const LACK: &[&str] = &[
                "Send a test", "Preview reset", "Preview session limit", "Preview weekly limit", "Play a sound",
                "When a session finishes", "Channel",
            ];
            (HAVE, LACK)
        }
        "general" => {
            const HAVE: &[&str] = &[
                "Startup", "Open Pulse at login", "Nearby sharing", "Send and receive files nearby", "Off.",
                "Keyboard shortcuts", "Mac-style editing", "Screenshot shortcuts", "Save screenshots to the Desktop",
                "Install signed installers automatically", "Updates", "Version 0.2.0", "Check now",
                "Automatically check", "Refresh now", "Pulse notch 0.2.0",
            ];
            const LACK: &[&str] = &[
                "Uninstall without password", "Enable the launcher", "Fn works as Command", "Cut and paste in Finder",
                "Dock click", "built on Codenotch", "Ask the provider every time you look", "Apple",
            ];
            (HAVE, LACK)
        }
        _ => (NONE, NONE),
    }
}

/// Windows content assertions that hold whatever the notch publishes: the shell's caption buttons, no Mac
/// wording, no empty picker. Every problem is recorded, none stops the tour.
#[cfg(windows)]
fn windows_common(ctl: &Control, sec: &Section, problems: &mut Vec<String>) {
    let name = sec.title;
    // Frameless window: the shell's three caption buttons, in order, in the top right.
    let chrome = ctl.eval(WINDOWS_CHROME_JS).expect("eval windows chrome");
    let caps = chrome["caps"].as_array().cloned().unwrap_or_default();
    let labels: Vec<&str> = caps.iter().map(|c| c["label"].as_str().unwrap_or("")).collect();
    if labels != ["Minimize window", "Maximize or restore window", "Close window"] {
        problems.push(format!("{name}: caption buttons are {labels:?}, want Minimize, Maximize or restore, Close window"));
    } else {
        let width = chrome["width"].as_f64().unwrap_or(0.0);
        let boxes_ok = caps.iter().all(|c| c["w"].as_f64().unwrap_or(0.0) > 0.0 && c["h"].as_f64().unwrap_or(0.0) > 0.0);
        let ordered = caps.windows(2).all(|p| p[0]["x"].as_f64() < p[1]["x"].as_f64());
        let at_top_right = caps.last().and_then(|c| c["right"].as_f64()).is_some_and(|r| r <= width + 1.0 && r >= width - 40.0)
            && caps.iter().all(|c| c["y"].as_f64().is_some_and(|y| y < 60.0));
        if !boxes_ok || !ordered || !at_top_right {
            problems.push(format!("{name}: caption buttons are not three visible buttons in the top right (width {width}): {caps:?}"));
        }
    }
    match chrome["platform"].as_str() {
        Some("windows") | None => {}
        Some(other) => problems.push(format!("{name}: the shell is marked platform \"{other}\", want windows")),
    }
    if chrome["windowsUa"] != true {
        problems.push(format!("{name}: navigator.userAgent does not say Windows, so the hub's Windows wording is off"));
    }

    // No Mac wording anywhere in the page.
    let mac = ctl.eval(MAC_WORDING_JS).expect("eval mac wording");
    let mac: Vec<String> = mac.as_array().map(|a| a.iter().map(|v| s(v.clone())).collect()).unwrap_or_default();
    if !mac.is_empty() {
        problems.push(format!("{name}: Mac wording on Windows: {mac:?}"));
    }

    // Pickers are never empty, and no number is NaN (the BROKEN scan also covers "NaN%" in text).
    let pickers = ctl
        .eval(&PICKERS_JS.replace("__SETTINGS__", if sec.settings { "true" } else { "false" }))
        .expect("eval pickers");
    for issue in pickers.as_array().cloned().unwrap_or_default() {
        problems.push(format!("{name}: {}", s(issue)));
    }
}

/// What the fixture (windows_notch_fixture) must make the open section show or leave out.
#[cfg(windows)]
fn windows_fixture_checks(ctl: &Control, sec: &Section, problems: &mut Vec<String>) {
    let name = sec.title;
    let (have, lack) = windows_expectations(sec.id);
    if !have.is_empty() || !lack.is_empty() {
        let text = body_text(ctl);
        for want in have {
            if !text.contains(want) {
                problems.push(format!("{name}: expected {want:?} on the page"));
            }
        }
        for unwanted in lack {
            if text.contains(unwanted) {
                problems.push(format!("{name}: {unwanted:?} must not appear with the Windows notch's snapshot"));
            }
        }
    }

    // Per-section structure.
    let probe = |js: &str| ctl.eval(js).expect("eval section structure");
    match sec.id {
        "appearance" => {
            let radios = probe(
                r#"const group = (n) => [...document.querySelectorAll(`[role=radiogroup][aria-label="${n}"] [role=radio]`)]
                       .map((b) => ({ text: b.textContent.trim(), on: b.getAttribute('aria-checked') === 'true' }));
                   const custom = document.querySelector('[role=switch][aria-label="Custom size"]');
                   return { edge: group('Edge'), show: group('Show'), size: group('Size'), selects: document.querySelectorAll('select').length,
                            ranges: document.querySelectorAll('input[type=range]').length,
                            custom: custom ? custom.getAttribute('aria-checked') : null,
                            groups: [...document.querySelectorAll('.ck-sgroup h2')].map((h) => h.textContent.trim()) };"#,
            );
            let texts = |key: &str| -> Vec<String> {
                radios[key].as_array().map(|a| a.iter().map(|o| s(o["text"].clone())).collect()).unwrap_or_default()
            };
            let on = |key: &str| -> Vec<String> {
                radios[key]
                    .as_array()
                    .map(|a| a.iter().filter(|o| o["on"] == true).map(|o| s(o["text"].clone())).collect())
                    .unwrap_or_default()
            };
            if texts("edge") != ["Top", "Bottom", "Left", "Right"] || on("edge") != ["Top"] {
                problems.push(format!("{name}: Edge should offer Top, Bottom, Left, Right with Top on; has {:?}, on {:?}", texts("edge"), on("edge")));
            }
            if texts("show") != ["Always show", "On hover", "Hidden"] || on("show") != ["On hover"] {
                problems.push(format!("{name}: Show should offer Always show, On hover, Hidden with On hover on; has {:?}, on {:?}", texts("show"), on("show")));
            }
            if texts("size") != ["Small", "Medium", "Large"] || on("size") != ["Medium"] {
                problems.push(format!("{name}: Size should offer Small, Medium, Large with Medium on; has {:?}, on {:?}", texts("size"), on("size")));
            }
            if radios["custom"] != "false" {
                problems.push(format!("{name}: the Custom size switch should be present and off (usesCustomNotchScale false), found {}", radios["custom"]));
            }
            let groups: Vec<String> = radios["groups"].as_array().map(|a| a.iter().map(|g| s(g.clone())).collect()).unwrap_or_default();
            if groups != ["Placement", "Size and surface"] {
                problems.push(format!("{name}: Appearance cards are {groups:?}; the Windows snapshot should leave Placement and Size and surface (no Rings, Colour, Language)"));
            }
            if radios["selects"] != 0 || radios["ranges"] != 0 {
                problems.push(format!("{name}: the Windows notch publishes no select lists, and no Scale slider while Custom size is off, but the page has {} select(s) and {} slider(s)", radios["selects"], radios["ranges"]));
            }
        }
        "accounts" => {
            let rows = probe(
                r#"return [...document.querySelectorAll('.ck-account')].map((r) => ({
                       name: r.querySelector('.ck-account-name strong')?.textContent ?? '',
                       shown: r.querySelector('.ck-account-name .ck-status')?.textContent.trim() ?? '',
                       sub: r.querySelector('.ck-text > .ck-sub')?.textContent.trim() ?? '',
                       up: r.querySelector('button[aria-label^="Move"][aria-label$="up"]')?.disabled,
                       down: r.querySelector('button[aria-label^="Move"][aria-label$="down"]')?.disabled,
                       on: r.querySelector('[role=switch]')?.getAttribute('aria-checked'),
                       toggle: r.querySelector('[role=switch]')?.getAttribute('aria-label') ?? '',
                       claudeRows: r.querySelectorAll('.ck-claude-account').length,
                       buttons: [...r.querySelectorAll('button')].map((b) => b.textContent.trim()).filter(Boolean),
                   }));"#,
            );
            let rows = rows.as_array().cloned().unwrap_or_default();
            let names: Vec<String> = rows.iter().map(|r| s(r["name"].clone())).collect();
            if names != ["Claude", "Codex"] {
                problems.push(format!("{name}: accounts should list Claude then Codex (providerOrder), found {names:?}"));
            } else {
                let (claude, codex) = (&rows[0], &rows[1]);
                for (row, who) in [(claude, "Claude"), (codex, "Codex")] {
                    if row["shown"] != "Shown" || row["sub"] != "Up to date" || row["on"] != "true" || row["toggle"] != format!("Show {who}") {
                        problems.push(format!("{name}: {who} should be Shown, read \"Up to date\" and have its \"Show {who}\" switch on: {row}"));
                    }
                    if row["claudeRows"] != 0 || row["buttons"].as_array().is_some_and(|b| b.iter().any(|t| t == "Forget reading" || t == "Allow access\u{2026}")) {
                        problems.push(format!("{name}: {who} shows Mac-only account parts (per-account Claude rows, Forget reading, Allow access): {row}"));
                    }
                }
                if claude["up"] != true || claude["down"] != false || codex["up"] != false || codex["down"] != true {
                    problems.push(format!("{name}: only the first row's Move up and the last row's Move down are disabled; found Claude up={} down={}, Codex up={} down={}", claude["up"], claude["down"], codex["up"], codex["down"]));
                }
            }
        }
        "general" => {
            let nearby = probe(
                r#"const sw = document.querySelector('[role=switch][aria-label="Nearby sharing"]');
                   return { present: !!sw, on: sw?.getAttribute('aria-checked'), deviceName: !!document.querySelector('input[aria-label="Device name"]'),
                            groups: [...document.querySelectorAll('.ck-sgroup h2')].map((h) => h.textContent.trim()) };"#,
            );
            if nearby["present"] != true || nearby["on"] != "false" || nearby["deviceName"] != false {
                problems.push(format!("{name}: the Nearby sharing card should show its switch off and no Device name field (nearbyEnabled false): {nearby}"));
            }
            let groups: Vec<String> = nearby["groups"].as_array().map(|a| a.iter().map(|g| s(g.clone())).collect()).unwrap_or_default();
            if groups != ["Startup", "Nearby sharing", "Keyboard shortcuts", "Installers", "Updates", "Readings"] {
                problems.push(format!("{name}: General's cards are {groups:?}; the Windows snapshot should leave Startup, Nearby sharing, Keyboard shortcuts, Installers, Updates, Readings"));
            }
        }
        _ => {}
    }
}

/// JS expression for one option of a segmented control.
#[cfg(windows)]
fn radio_js(group: &str, text: &str) -> String {
    format!("[...document.querySelectorAll('[role=radiogroup][aria-label=\"{group}\"] [role=radio]')].find((b) => b.textContent.trim() === '{text}')")
}

/// Presses a control with a pointer click (scrolled into view first) and waits for the command the
/// hub must leave for the notch. When the pointer click is not delivered, a DOM click stands in and a
/// note says so: the control still has to work, but the evidence shows which kind of click did it.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn press(
    ctl: &Control,
    sc: &Scenario,
    shots: &Path,
    problems: &mut Vec<String>,
    commands: &Path,
    what: &str,
    find: &str,
    want: &dyn Fn(&Value) -> bool,
) {
    let probe = format!(
        "const el = ({find}); if (!el) return null; el.scrollIntoView({{block: 'center'}}); const r = el.getBoundingClientRect(); return {{x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width, h: r.height, disabled: !!el.disabled}};"
    );
    let rect = match ctl.wait_eval(&probe, Duration::from_secs(10)) {
        Ok(rect) => rect,
        Err(e) => {
            let _ = ctl.screenshot_to(&shots.join("FAIL-press.png"));
            problems.push(format!("{what}: control not found ({})", e.0));
            return;
        }
    };
    if rect["disabled"] == true || rect["w"].as_f64().unwrap_or(0.0) <= 0.0 {
        problems.push(format!("{what}: control is disabled or has no box: {rect}"));
        return;
    }
    let (x, y) = (rect["x"].as_f64().unwrap_or(0.0), rect["y"].as_f64().unwrap_or(0.0));
    if ctl.click(x, y, "left", 1).is_ok() && wait_command(commands, Duration::from_secs(4), want) {
        return;
    }
    let dom = format!("const el = ({find}); if (!el) return false; el.click(); return true;");
    let _ = ctl.eval(&dom);
    if wait_command(commands, Duration::from_secs(10), want) {
        sc.note(format!("{what}: the pointer click left no command; a DOM click did"));
        return;
    }
    let _ = ctl.screenshot_to(&shots.join("FAIL-press.png"));
    problems.push(format!("{what}: pressing it left no matching command in {}", commands.display()));
}

/// The commands the Windows hub leaves for the notch when a person uses each section's controls.
#[cfg(windows)]
fn windows_interactions(ctl: &Control, sec: &Section, bridge: &Path, sc: &Scenario, shots: &Path, problems: &mut Vec<String>) {
    let commands = bridge.join("hub-commands");
    let radio = radio_js;
    let switch = |label: &str| format!("document.querySelector('[role=switch][aria-label=\"{label}\"]')");
    match sec.id {
        "appearance" => {
            press(ctl, sc, shots, problems, &commands, "Edge: Bottom", &radio("Edge", "Bottom"), &|c| {
                c["command"] == "set" && c["key"] == "notchEdge" && c["value"] == "bottom"
            });
            press(ctl, sc, shots, problems, &commands, "Size: Large", &radio("Size", "Large"), &|c| {
                c["command"] == "set" && c["key"] == "notchSize" && c["value"] == "large"
            });
            press(ctl, sc, shots, problems, &commands, "Custom size", &switch("Custom size"), &|c| {
                c["command"] == "set" && c["key"] == "usesCustomNotchScale" && c["value"] == true
            });
            press(ctl, sc, shots, problems, &commands, "Fold to a pill", &switch("Fold to a pill when the pointer leaves"), &|c| {
                c["command"] == "set" && c["key"] == "folds" && c["value"] == false
            });
        }
        "accounts" => {
            press(ctl, sc, shots, problems, &commands, "Move Claude down", "document.querySelector('button[aria-label=\"Move Claude down\"]')", &|c| {
                c["command"] == "order" && c["value"] == json!(["codex", "claude"])
            });
            press(ctl, sc, shots, problems, &commands, "Show Codex", &switch("Show Codex"), &|c| {
                c["command"] == "connect" && c["provider"] == "codex" && c["value"] == false
            });
        }
        "notifications" => {
            press(ctl, sc, shots, problems, &commands, "Mute Claude alerts", &switch("Mute Claude alerts"), &|c| {
                c["command"] == "set" && c["key"] == "mute_claude_alerts" && c["value"] == true
            });
        }
        "general" => {
            press(ctl, sc, shots, problems, &commands, "Nearby sharing", &switch("Nearby sharing"), &|c| {
                c["command"] == "set" && c["key"] == "nearbyEnabled" && c["value"] == true
            });
            press(ctl, sc, shots, problems, &commands, "Automatically check", &switch("Automatically check"), &|c| {
                c["command"] == "set" && c["key"] == "autoUpdateCheck" && c["value"] == false
            });
        }
        _ => {}
    }
}

// ------------------------------------------------------------------- the control crawl (macOS)

/// Page-side helpers shared by every crawl probe (prepended to each snippet; `eval` runs a
/// function body). `enumerate()` lists the visible controls outside the nav, in document order:
/// native and ARIA controls plus rows that merely have a click handler (pointer cursor on the
/// outermost such element), keeping the innermost when one control holds another.
#[cfg(target_os = "macos")]
const CRAWL_JS: &str = r#"
const SEL = 'button,[role=button],[role=switch],[role=radio],[role=tab],[role=checkbox],[role=link],[role=menuitem],[role=slider],[role=combobox],[role=option],a[href],select,input,textarea,summary,[tabindex="0"]';
const clean = (t) => (t || '').replace(/\s+/g, ' ').trim().slice(0, 80);
const labelOf = (el) => {
  let t = clean(el.getAttribute('aria-label'));
  if (!t && el.getAttribute('aria-labelledby')) {
    t = clean(el.getAttribute('aria-labelledby').split(' ').map((id) => document.getElementById(id)?.textContent || '').join(' '));
  }
  if (!t) t = clean(el.innerText || el.textContent);
  if (!t) t = clean(el.getAttribute('title') || el.getAttribute('placeholder') || el.getAttribute('name') || (el.tagName === 'INPUT' ? el.value : ''));
  if (!t) t = el.tagName.toLowerCase() + '.' + clean(String(el.className && el.className.baseVal !== undefined ? el.className.baseVal : el.className)).split(' ')[0];
  return t;
};
const roleOf = (el) => {
  const r = el.getAttribute('role');
  if (r) return r;
  const tag = el.tagName.toLowerCase();
  if (tag === 'a') return 'link';
  if (tag === 'select') return 'select';
  if (tag === 'textarea') return 'textbox';
  if (tag === 'input') {
    const t = (el.type || 'text').toLowerCase();
    return t === 'checkbox' ? 'checkbox' : t === 'radio' ? 'radio' : t === 'range' ? 'slider' : 'textbox';
  }
  if (el.classList.contains('rk-toggle')) return 'switch';
  return tag === 'button' || tag === 'summary' ? 'button' : 'clickable';
};
const enumerate = () => {
  const found = new Set();
  const visible = (el) => {
    if (el.closest('nav.rk-nav') || el.closest('.rk-caption')) return false;
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const cs = getComputedStyle(el);
    return cs.visibility !== 'hidden' && cs.display !== 'none';
  };
  document.querySelectorAll(SEL).forEach((el) => { if (visible(el)) found.add(el); });
  document.querySelectorAll('div,li,tr,span,section,article,label,p').forEach((el) => {
    if (found.has(el) || el.closest(SEL) || !visible(el)) return;
    if (getComputedStyle(el).cursor !== 'pointer') return;
    const parent = el.parentElement;
    if (parent && getComputedStyle(parent).cursor === 'pointer' && !parent.closest(SEL)) return;
    found.add(el);
  });
  const all = [...found];
  const leaves = all.filter((el) => !all.some((o) => o !== el && el.contains(o)));
  leaves.sort((a, b) => (a.compareDocumentPosition(b) & 4 ? -1 : 1));
  return leaves;
};
const MARK_SEL = '[role=switch],[role=radio],[role=tab],[role=checkbox],input[type=checkbox],input[type=radio],[aria-expanded],[aria-pressed],[aria-selected]';
const markOf = (el) => [roleOf(el), labelOf(el), el.getAttribute('aria-checked') ?? (el.checked !== undefined ? String(el.checked) : ''), el.getAttribute('aria-pressed') ?? '', el.getAttribute('aria-selected') ?? '', el.getAttribute('aria-expanded') ?? ''].join('|');
const marks = () => [...document.querySelectorAll(MARK_SEL)].filter((el) => !el.closest('nav.rk-nav')).map(markOf);
const styleSig = (el) => {
  const one = (e) => { const c = getComputedStyle(e); return [c.backgroundColor, c.color, c.boxShadow, c.transform, c.opacity, c.filter, c.borderColor, c.outlineStyle, c.textDecorationLine].join(';'); };
  return one(el) + '#' + (el.firstElementChild ? one(el.firstElementChild) : '');
};
const dialogs = () => document.querySelectorAll('[role=dialog],[role=alertdialog],dialog[open],.rk-modal,.rk-sheet,.modal').length;
"#;

#[cfg(target_os = "macos")]
fn crawl_js(body: &str, subs: &[(&str, String)]) -> String {
    let mut b = body.to_string();
    for (key, value) in subs {
        b = b.replace(key, value);
    }
    format!("{CRAWL_JS}\n{b}")
}

#[cfg(target_os = "macos")]
const DESTRUCTIVE_WORDS: &[&str] = &[
    "forget", "uninstall", "delete", "remove", "restart", "quit", "erase", "trash", "clean", "cleanup", "reclaim", "reset",
    "wipe", "disconnect", "clear", "terminate", "kill", "stop", "apply", "purge", "revoke", "empty",
];
#[cfg(target_os = "macos")]
const DESTRUCTIVE_PHRASES: &[&str] = &["sign out", "log out", "move to"];
#[cfg(target_os = "macos")]
const EXTERNAL_WORDS: &[&str] = &["reveal", "grant", "allow", "download", "install", "feedback", "report"];
#[cfg(target_os = "macos")]
const EXTERNAL_PHRASES: &[&str] = &[
    "system settings", "open settings", "open in finder", "show in finder", "request access", "check for update", "sign in",
    "log in", "learn more", "release notes", "documentation", "send test",
];

#[cfg(target_os = "macos")]
struct Found {
    idx: usize,
    label: String,
    role: String,
    disabled: bool,
    href: String,
}

/// Why a control is hovered but never clicked, or None when pressing it is safe in the fixture home.
#[cfg(target_os = "macos")]
fn skip_reason(sec: &Section, f: &Found) -> Option<&'static str> {
    let l = f.label.to_lowercase();
    let words: Vec<&str> = l.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let has_word = |list: &[&str]| words.iter().any(|w| list.contains(w));
    let has_phrase = |list: &[&str]| list.iter().any(|p| l.contains(*p));
    let setting = matches!(f.role.as_str(), "switch" | "radio" | "checkbox" | "tab" | "textbox");
    if f.disabled {
        return Some("not clicked: disabled");
    }
    if has_word(DESTRUCTIVE_WORDS) || has_phrase(DESTRUCTIVE_PHRASES) {
        return Some("not clicked: destructive");
    }
    if matches!(f.role.as_str(), "slider" | "select" | "combobox") {
        return Some("not clicked: native value control (popup or drag)");
    }
    if f.href.starts_with("http") || f.href.starts_with("mailto") {
        return Some("not clicked: external link");
    }
    if !setting && (has_word(EXTERNAL_WORDS) || has_phrase(EXTERNAL_PHRASES) || sec.id == "permissions") {
        return Some("not clicked: opens an external app or system prompt");
    }
    None
}

#[cfg(target_os = "macos")]
fn slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    let out: String = out.chars().take(48).collect();
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "control".to_string()
    } else {
        out
    }
}

#[cfg(target_os = "macos")]
fn list_controls(ctl: &Control) -> Vec<Found> {
    let js = crawl_js(
        "return enumerate().map((el, i) => ({i, label: labelOf(el), role: roleOf(el), disabled: !!el.disabled || el.getAttribute('aria-disabled') === 'true', href: el.getAttribute('href') || ''}));",
        &[],
    );
    ctl.eval(&js)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|v| Found {
            idx: v["i"].as_u64().unwrap_or(0) as usize,
            label: s(v["label"].clone()),
            role: s(v["role"].clone()),
            disabled: v["disabled"] == true,
            href: s(v["href"].clone()),
        })
        .collect()
}

/// Brings the section back after a control navigated away or changed a pane.
#[cfg(target_os = "macos")]
fn reopen(ctl: &Control, sec: &Section) {
    let want = format!("return document.querySelector('.rk-top__title')?.textContent?.trim() === {}", json!(sec.title));
    if ctl.eval(&want).ok() != Some(json!(true)) {
        let nav = format!(
            "const b=[...document.querySelectorAll('nav.rk-nav button')].find(e=>e.textContent.trim()==={}); if(!b) return false; b.click(); return true;",
            json!(sec.title)
        );
        let _ = ctl.wait_eval(&nav, Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(500));
    }
    if sec.id == "storage" {
        let _ = ctl.eval("const b=[...document.querySelectorAll('button,[role=radio],[role=tab]')].find(e=>e.textContent.trim()==='Folders'); if(b && b.getAttribute('aria-checked') !== 'true' && b.getAttribute('aria-selected') !== 'true') b.click(); return true;");
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(target_os = "macos")]
fn page_state(ctl: &Control) -> Value {
    let js = crawl_js(
        "return {title: document.querySelector('.rk-top__title')?.textContent?.trim() ?? '', text: document.body.innerText, marks: marks(), dialogs: dialogs()};",
        &[],
    );
    ctl.eval(&js).unwrap_or(Value::Null)
}

/// What changed between two page states, in words, plus the commands the hub left for the notch.
#[cfg(target_os = "macos")]
fn describe(before: &Value, after: &Value, commands: &[Value]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if before["title"] != after["title"] {
        parts.push(format!("page: {} -> {}", s(before["title"].clone()), s(after["title"].clone())));
    }
    let (mb, ma) = (before["marks"].as_array().cloned().unwrap_or_default(), after["marks"].as_array().cloned().unwrap_or_default());
    for (b, a) in mb.iter().zip(ma.iter()) {
        if b != a && parts.len() < 6 {
            parts.push(format!("state: {} -> {}", s(b.clone()).replace('|', " "), s(a.clone()).replace('|', " ")));
        }
    }
    if before["dialogs"] != after["dialogs"] {
        parts.push(format!("dialogs: {} -> {}", before["dialogs"], after["dialogs"]));
    }
    let lines = |v: &Value| -> std::collections::HashSet<String> {
        s(v["text"].clone()).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
    };
    let (lb, la) = (lines(before), lines(after));
    let added: Vec<&String> = la.difference(&lb).collect();
    let removed = lb.difference(&la).count();
    if !added.is_empty() || removed > 0 {
        let mut sample: Vec<String> = added.iter().map(|l| l.chars().take(50).collect::<String>()).collect();
        sample.sort();
        sample.truncate(2);
        parts.push(format!("text: +{} -{} lines (e.g. {})", added.len(), removed, sample.join(" / ")));
    }
    for c in commands.iter().take(2) {
        let t = c.to_string();
        parts.push(format!("command: {}", t.chars().take(120).collect::<String>()));
    }
    if parts.is_empty() {
        "no observable change".to_string()
    } else {
        parts.join("; ")
    }
}

/// Undoes a control's effect so the next control meets the page as it was: toggles and radios
/// go back to their earlier state, dialogs close, focus leaves any field, the section reopens.
#[cfg(target_os = "macos")]
fn restore(ctl: &Control, sec: &Section, before_marks: &Value) {
    let undo = crawl_js(
        "const prev = __PREV__; const els = [...document.querySelectorAll(MARK_SEL)].filter((el) => !el.closest('nav.rk-nav')); let n = 0; if (els.length === prev.length) { els.forEach((el, j) => { const now = markOf(el); if (prev[j] === now) return; const role = roleOf(el); const was = prev[j].split('|'); if ((role === 'radio' || role === 'tab') && was[2] !== 'true' && was[4] !== 'true') return; el.click(); n++; }); } if (document.activeElement && document.activeElement.blur) document.activeElement.blur(); return n;",
        &[("__PREV__", before_marks.to_string())],
    );
    let _ = ctl.eval(&undo);
    if page_state(ctl)["dialogs"].as_u64().unwrap_or(0) > 0 {
        let _ = ctl.key("Escape");
        std::thread::sleep(Duration::from_millis(250));
    }
    reopen(ctl, sec);
}

#[cfg(target_os = "macos")]
fn file_bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_default()
}

/// Hovers, presses and clicks every control of the open section, saving
/// `<surface>__<control>__hover|pressed|after.png` and one inventory entry each.
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn crawl_section(ctl: &Control, sec: &Section, surface: &str, shots: &Path, commands: &Path, deadline: std::time::Instant, per_section: usize, inventory: &mut Vec<Value>) {
    let controls = list_controls(ctl);
    let mut used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (n, f) in controls.iter().enumerate() {
        let mut slug_name = slug(&f.label);
        let seen = used.entry(slug_name.clone()).or_insert(0);
        *seen += 1;
        if *seen > 1 {
            slug_name = format!("{slug_name}-{seen}");
        }
        let mut entry = json!({
            "surface": surface, "label": f.label, "role": f.role, "slug": slug_name,
            "hover": null, "pressed": null, "after": null,
            "hover_style_changed": null, "press_pixels_changed": null,
            "clicked": false, "action": "", "disabled": f.disabled,
        });
        if n >= per_section || std::time::Instant::now() > deadline {
            entry["action"] = json!("not captured: time or per-section budget");
            inventory.push(entry);
            continue;
        }
        // The control may have moved or vanished since the list was taken.
        let probe_js = crawl_js(
            "const el = enumerate()[__IDX__]; if (!el) return null; if (labelOf(el) !== __LABEL__) return {moved: true}; el.scrollIntoView({block: 'center'}); const r = el.getBoundingClientRect(); return {x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width, h: r.height};",
            &[("__IDX__", f.idx.to_string()), ("__LABEL__", json!(f.label).to_string())],
        );
        let probe = ctl.eval(&probe_js).unwrap_or(Value::Null);
        let (Some(x), Some(y)) = (probe["x"].as_f64(), probe["y"].as_f64()) else {
            entry["action"] = json!("not captured: the control moved or disappeared");
            inventory.push(entry);
            continue;
        };
        let sig_js = crawl_js("const el = enumerate()[__IDX__]; return el ? styleSig(el) : '';", &[("__IDX__", f.idx.to_string())]);
        let name = |state: &str| format!("{surface}__{slug_name}__{state}.png");

        // Hover: the pointer rests on the control; compare its computed look with the pointer elsewhere.
        let _ = ctl.move_to(2.0, 2.0);
        std::thread::sleep(Duration::from_millis(80));
        let rest_sig = ctl.eval(&sig_js).unwrap_or(Value::Null);
        let _ = ctl.move_to(x, y);
        std::thread::sleep(Duration::from_millis(120));
        let hover_sig = ctl.eval(&sig_js).unwrap_or(Value::Null);
        let hover_png = shots.join(name("hover"));
        if ctl.screenshot_to(&hover_png).is_ok() {
            entry["hover"] = json!(name("hover"));
        }
        entry["hover_style_changed"] = json!(rest_sig != hover_sig);

        if let Some(why) = skip_reason(sec, f) {
            entry["action"] = json!(why);
            let _ = ctl.move_to(2.0, 2.0);
            inventory.push(entry);
            continue;
        }

        // Pressed: mouse down and look; mouse up completes the click.
        let before = page_state(ctl);
        let commands_before = commands_seen(commands);
        let _ = ctl.pointer("down", x, y, "left", &[]);
        std::thread::sleep(Duration::from_millis(100));
        let pressed_png = shots.join(name("pressed"));
        if ctl.screenshot_to(&pressed_png).is_ok() {
            entry["pressed"] = json!(name("pressed"));
        }
        let _ = ctl.pointer("up", x, y, "left", &[]);
        std::thread::sleep(Duration::from_millis(400));
        let after_png = shots.join(name("after"));
        if ctl.screenshot_to(&after_png).is_ok() {
            entry["after"] = json!(name("after"));
        }
        entry["press_pixels_changed"] = json!(file_bytes(&hover_png) != file_bytes(&pressed_png));
        let after = page_state(ctl);
        let new_commands: Vec<Value> = commands_seen(commands).into_iter().filter(|c| !commands_before.contains(c)).collect();
        entry["clicked"] = json!(true);
        entry["action"] = json!(describe(&before, &after, &new_commands));
        inventory.push(entry);

        restore(ctl, sec, &before["marks"]);
    }
    let _ = ctl.move_to(2.0, 2.0);
}

#[cfg(target_os = "macos")]
fn write_inventory(shots: &Path, inventory: &[Value]) {
    let _ = std::fs::write(shots.join("inventory.json"), serde_json::to_vec_pretty(&json!({"platform": "mac", "controls": inventory})).unwrap_or_default());
    let yn = |v: &Value| match v.as_bool() {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    };
    let cell = |t: String| t.replace('|', "/").replace('\n', " ");
    let captured = inventory.iter().filter(|e| !e["hover"].is_null()).count();
    let clicked = inventory.iter().filter(|e| e["clicked"] == true).count();
    let mut md = String::from("# Mac hub controls\n\n");
    md.push_str(&format!("{} controls listed, {} with a hover shot, {} clicked.\n\n", inventory.len(), captured, clicked));
    md.push_str("| Surface | Control | Role | Hover shot | Hover look changed | Pressed pixels changed | Action observed |\n|---|---|---|---|---|---|---|\n");
    for e in inventory {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            cell(s(e["surface"].clone())),
            cell(s(e["label"].clone())),
            cell(s(e["role"].clone())),
            if e["hover"].is_null() { "no" } else { "yes" },
            yn(&e["hover_style_changed"]),
            yn(&e["press_pixels_changed"]),
            cell(s(e["action"].clone())),
        ));
    }
    let _ = std::fs::write(shots.join("inventory.md"), md);
}

/// The sidebar's own items: hover, press and click each (the click opens that section).
#[cfg(target_os = "macos")]
fn crawl_nav(ctl: &Control, shots: &Path, inventory: &mut Vec<Value>) {
    for sec in SECTIONS.iter() {
        let probe = format!(
            "const b=[...document.querySelectorAll('nav.rk-nav button')].find(e=>e.textContent.trim()==={}); if(!b) return null; const r=b.getBoundingClientRect(); return {{x: r.x + r.width / 2, y: r.y + r.height / 2}};",
            json!(sec.title)
        );
        let at = ctl.eval(&probe).unwrap_or(Value::Null);
        let (Some(x), Some(y)) = (at["x"].as_f64(), at["y"].as_f64()) else { continue };
        let name = |state: &str| format!("nav__{}__{state}.png", sec.id);
        let mut entry = json!({
            "surface": "nav", "label": sec.title, "role": "button", "slug": sec.id,
            "hover": null, "pressed": null, "after": null, "hover_style_changed": null,
            "press_pixels_changed": null, "clicked": true, "action": "", "disabled": false,
        });
        let sig_js = crawl_js(
            "const b=[...document.querySelectorAll('nav.rk-nav button')].find(e=>e.textContent.trim()===__TITLE__); return b ? styleSig(b) : '';",
            &[("__TITLE__", json!(sec.title).to_string())],
        );
        let _ = ctl.move_to(2.0, 2.0);
        std::thread::sleep(Duration::from_millis(80));
        let rest_sig = ctl.eval(&sig_js).unwrap_or(Value::Null);
        let _ = ctl.move_to(x, y);
        std::thread::sleep(Duration::from_millis(120));
        let hover_sig = ctl.eval(&sig_js).unwrap_or(Value::Null);
        entry["hover_style_changed"] = json!(rest_sig != hover_sig);
        let hover_png = shots.join(name("hover"));
        if ctl.screenshot_to(&hover_png).is_ok() {
            entry["hover"] = json!(name("hover"));
        }
        let before = page_state(ctl);
        let _ = ctl.pointer("down", x, y, "left", &[]);
        std::thread::sleep(Duration::from_millis(100));
        let pressed_png = shots.join(name("pressed"));
        if ctl.screenshot_to(&pressed_png).is_ok() {
            entry["pressed"] = json!(name("pressed"));
        }
        let _ = ctl.pointer("up", x, y, "left", &[]);
        std::thread::sleep(Duration::from_millis(500));
        let after_png = shots.join(name("after"));
        if ctl.screenshot_to(&after_png).is_ok() {
            entry["after"] = json!(name("after"));
        }
        entry["press_pixels_changed"] = json!(file_bytes(&hover_png) != file_bytes(&pressed_png));
        entry["action"] = json!(describe(&before, &page_state(ctl), &[]));
        inventory.push(entry);
    }
    let _ = ctl.move_to(2.0, 2.0);
}

/// The interaction standard, enforced on the crawl inventory: every control shows a hover state
/// and every clicked control shows a pressed state. Allowlist: text inputs and native value
/// controls (selects, sliders) show focus or a native popup instead, and disabled controls are inert.
#[cfg(target_os = "macos")]
const STATE_ALLOWLIST_ROLES: &[&str] = &["textbox", "searchbox", "select", "combobox", "slider", "spinbutton"];

#[cfg(target_os = "macos")]
fn check_interaction_states(inventory: &[Value]) -> Vec<String> {
    let mut problems = Vec::new();
    // The pressed check is only meaningful when the harness's mouse-down reaches the page at
    // all: if not one clicked control changed between its hover and pressed shots, the
    // harness could not press, and that is reported once rather than per control.
    let harness_presses = inventory
        .iter()
        .any(|e| e["clicked"] == true && e["press_pixels_changed"] == true);
    if !harness_presses && inventory.iter().any(|e| e["clicked"] == true) {
        eprintln!("interaction states: the harness produced no pressed state on any control; pressed checks skipped");
    }
    for e in inventory {
        let role = e["role"].as_str().unwrap_or("");
        if e["disabled"] == true || STATE_ALLOWLIST_ROLES.contains(&role) {
            continue;
        }
        let who = format!("{} / {} ({role})", s(e["surface"].clone()), s(e["label"].clone()));
        if !e["hover"].is_null() && e["hover_style_changed"] != true {
            problems.push(format!("no hover state: {who}"));
        }
        // Advisory until rightkit-control's mouse-down is shown to reach `:active` in the
        // page (2 of 121 clicked controls changed on 2026-10-10, before any CSS change).
        if harness_presses && e["clicked"] == true && e["press_pixels_changed"] != true {
            eprintln!("interaction states (advisory): no pressed state: {who}");
        }
    }
    problems
}

// ------------------------------------------------------------------------------------- the tour

fn hub_tour(h: &Harness) {
    h.scenario("hub sections render without errors", "fast", &[PLATFORM], |sc| {
        let ws = fresh_workspace(sc);
        // The app derives HOME (USERPROFILE on Windows) from RIGHTKIT_PULSE_QA_HOME, so Storage
        // scans this fixture and never the user's real home. rightkit-qa >= 0.2.12 requires env
        // paths to live inside the QA app home (/private/tmp/rightkit-qa/… on the Mac; the system
        // temp dir on Windows); the scanner resolves /private paths.
        let home = ws.home.join("fixture-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(FIXTURE)).expect("fixture dir");
        std::fs::write(home.join(FIXTURE).join("fixture.bin"), vec![0u8; 65536]).expect("fixture file");

        // The notch's published state, so Settings renders real controls (CI has no notch).
        let bridge = bridge_dir(&home, &ws);
        std::fs::create_dir_all(&bridge).expect("bridge dir");
        #[cfg(target_os = "macos")]
        let published = notch_fixture();
        #[cfg(windows)]
        let published = windows_notch_fixture();
        std::fs::write(bridge.join("notch-state.json"), serde_json::to_vec(&published).expect("notch fixture json"))
            .expect("write notch-state.json");
        // The Windows hub reads the sharing switches from the notch's own settings file, not from
        // notch-state.json; keep both saying nearby sharing is off.
        #[cfg(windows)]
        std::fs::write(bridge.join("pill-settings.json"), br#"{"schema_version":1,"nearby_enabled":false}"#)
            .expect("write pill-settings.json");

        let shots: PathBuf = std::env::var_os("PULSE_QA_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| sc.scratch("screenshots"));
        std::fs::create_dir_all(&shots).expect("screenshots dir");

        let ctl = launch_hub(&ws, &home, sc);
        let mut problems: Vec<String> = Vec::new();
        // The control crawl (macOS): every control hovered, pressed and clicked per section, within one time budget.
        #[cfg(target_os = "macos")]
        let mut inventory: Vec<Value> = Vec::new();
        #[cfg(target_os = "macos")]
        let crawl_deadline = std::time::Instant::now() + Duration::from_secs(270);

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
                for label in std::iter::once(&sec.expect).chain(sec.also.iter()) {
                    if let Err(e) = ctl.wait_for_text(None, label, Duration::from_secs(20)) {
                        let _ = ctl.screenshot_to(&shots.join(format!("FAIL-{}.png", sec.id)));
                        panic!(
                            "{} never showed fixture label {:?} ({}); page text:\n{}",
                            sec.title,
                            label,
                            e.0,
                            body_text(&ctl).chars().take(1500).collect::<String>()
                        );
                    }
                }
            } else if sec.id == "storage" {
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
            // Geometry, on every section: a switch that is not 36x22 with a centred knob fails.
            generic_checks(&ctl, sec, &shots, &mut problems);
            #[cfg(target_os = "macos")]
            if sec.id == "accounts" {
                check_claude_accounts(&ctl, &bridge, &shots);
            }
            #[cfg(windows)]
            {
                windows_common(&ctl, sec, &mut problems);
                windows_fixture_checks(&ctl, sec, &mut problems);
            }

            let shot = shots.join(format!("{:02}-{}.png", index + 1, sec.id));
            ctl.screenshot_to(&shot).expect("screenshot");
            sc.keep(&format!("{:02}-{}", index + 1, sec.id), &shot);

            // Every control of this section: hover, pressed and after-click shots plus an inventory entry each.
            #[cfg(target_os = "macos")]
            {
                crawl_section(&ctl, sec, sec.id, &shots, &bridge.join("hub-commands"), crawl_deadline, 40, &mut inventory);
                write_inventory(&shots, &inventory);
            }

            // After the screenshot (pressing scrolls the page): use the section's controls as a person would.
            #[cfg(windows)]
            windows_interactions(&ctl, sec, &bridge, sc, &shots, &mut problems);
        }
        #[cfg(target_os = "macos")]
        {
            crawl_nav(&ctl, &shots, &mut inventory);
            write_inventory(&shots, &inventory);
            problems.extend(check_interaction_states(&inventory));
        }
        let _ = std::fs::remove_dir_all(&home);
        if !problems.is_empty() {
            panic!("{} problem(s) in the tour:\n- {}", problems.len(), problems.join("\n- "));
        }
    });
}

// ------------------------------------------------------------------------ the real Windows notch

/// The real notch process, killed (with anything it started) when this goes out of scope.
#[cfg(windows)]
struct NotchProcess {
    child: std::process::Child,
    tracker: rightkit_qa::process::Tracker,
}

#[cfg(windows)]
impl Drop for NotchProcess {
    fn drop(&mut self) {
        let pid = self.child.id();
        rightkit_qa::process::kill_tree(pid);
        let _ = self.child.wait();
        self.tracker.forget(pid);
    }
}

#[cfg(windows)]
fn start_notch(notch: &Path, ws: &QaWorkspace, sc: &Scenario) -> NotchProcess {
    use std::os::windows::process::CommandExt;
    let out = std::fs::File::create(ws.home.join("notch.out.log")).expect("notch output log");
    let err = out.try_clone().expect("notch output log handle");
    let mut cmd = std::process::Command::new(notch);
    cmd.current_dir(&ws.home)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console for the debug build
    for (key, value) in &ws.env {
        cmd.env(key, value);
    }
    let child = cmd.spawn().unwrap_or_else(|e| panic!("cannot start the notch {}: {e}", notch.display()));
    sc.tracker().register(child.id(), "pulse notch");
    NotchProcess { child, tracker: sc.tracker().clone() }
}

#[cfg(windows)]
fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

#[cfg(windows)]
fn wait_for<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(found) = probe() {
            return Some(found);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Presses a control with a pointer click and waits for `done`; when the pointer click changes nothing, a
/// DOM click stands in and a note says so.
#[cfg(windows)]
fn click_until(ctl: &Control, sc: &Scenario, what: &str, find: &str, done: &dyn Fn() -> bool) -> bool {
    let probe = format!("const el = ({find}); if (!el) return null; el.scrollIntoView({{block: 'center'}}); const r = el.getBoundingClientRect(); return {{x: r.x + r.width / 2, y: r.y + r.height / 2}};");
    let rect = ctl.wait_eval(&probe, Duration::from_secs(10)).unwrap_or_else(|e| panic!("{what}: control not found ({})", e.0));
    let clicked = ctl.click(rect["x"].as_f64().unwrap_or(0.0), rect["y"].as_f64().unwrap_or(0.0), "left", 1);
    if wait_for(Duration::from_secs(12), || done().then_some(())).is_some() {
        return true;
    }
    sc.note(format!("{what}: the pointer click changed nothing ({clicked:?}); a DOM click was used instead"));
    let _ = ctl.eval(&format!("const el = ({find}); if (!el) return false; el.click(); return true;"));
    wait_for(Duration::from_secs(30), || done().then_some(())).is_some()
}

/// What the notch and the hub left behind, for a failure message.
#[cfg(windows)]
fn notch_diagnostics(ws: &QaWorkspace, bridge: &Path) -> String {
    let tail = |path: PathBuf| {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| format!("(unreadable: {e})"));
        let start = text.len().saturating_sub(1500);
        let start = (start..=text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(text.len());
        format!("--- {} ---\n{}", path.display(), &text[start..])
    };
    let left: Vec<String> = std::fs::read_dir(bridge.join("hub-commands"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    format!(
        "{}\n{}\nfiles in {}: {:?}\nhub-commands left unread: {left:?}",
        tail(bridge.join("notch.log")),
        tail(ws.home.join("notch.out.log")),
        bridge.display(),
        std::fs::read_dir(bridge).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>(),
    )
}

/// Creates `path` owned by the current user with a user-and-SYSTEM-only DACL, as the notch's own
/// settings store does. The notch refuses to save into a Pulse folder that a process holding an
/// administrator token created the plain way (its owner is then the Administrators group, not the
/// user), and the CI runner's token is one.
#[cfg(windows)]
fn create_trusted_dir(path: &Path, scratch: &Path) -> Result<(), String> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$sec = New-Object System.Security.AccessControl.DirectorySecurity
$sec.SetSecurityDescriptorSddlForm("O:${sid}D:P(A;;FA;;;${sid})(A;;FA;;;SY)")
[void][System.IO.Directory]::CreateDirectory($env:PULSE_QA_DIR, $sec)
"#;
    use std::os::windows::process::CommandExt;
    let script = scratch.join("create-trusted-dir.ps1");
    std::fs::write(&script, SCRIPT).map_err(|e| e.to_string())?;
    let out = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .env("PULSE_QA_DIR", path)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() && path.is_dir() {
        Ok(())
    } else {
        Err(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

/// A person picks Bottom for the Edge in the hub; the real notch picks the command up, saves
/// `pill-settings.json` and publishes the new edge, and the hub shows it. The only step where a notch exists.
#[cfg(windows)]
fn notch_edge_journey(h: &Harness) {
    h.scenario("notch applies the hub's edge change", "fast", &[PLATFORM], |sc| {
        let Some(notch) = std::env::var_os("PULSE_QA_NOTCH_BIN").map(PathBuf::from) else {
            sc.skip("PULSE_QA_NOTCH_BIN is not set: the real notch runs only where scripts/gate.sh sets it (CI)");
            return;
        };
        assert!(notch.is_file(), "PULSE_QA_NOTCH_BIN {} is not a file", notch.display());
        let ws = fresh_workspace(sc);
        let home = ws.home.join("fixture-home");
        std::fs::create_dir_all(&home).expect("fixture home");
        let bridge = bridge_dir(&home, &ws);
        match create_trusted_dir(&bridge, &sc.scratch("ps")) {
            Ok(()) => {}
            Err(why) => {
                sc.note(format!("could not create the Pulse folder user-owned ({why}); the notch may refuse to save into it"));
                std::fs::create_dir_all(&bridge).expect("bridge dir");
            }
        }
        let shots: PathBuf = std::env::var_os("PULSE_QA_SHOTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| sc.scratch("screenshots"));
        std::fs::create_dir_all(&shots).expect("screenshots dir");

        // 1. The real notch publishes its own state: nothing is written for it.
        let mut running = start_notch(&notch, &ws, sc);
        let state_path = bridge.join("notch-state.json");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let state = loop {
            if let Ok(Some(status)) = running.child.try_wait() {
                panic!("the notch exited ({status}) before it published notch-state.json\n{}", notch_diagnostics(&ws, &bridge));
            }
            if let Some(v) = read_json(&state_path) {
                if v["product"] == "Pulse" && v["platform"] == "windows" {
                    break v;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("the notch published no Windows notch-state.json within 60 s\n{}", notch_diagnostics(&ws, &bridge));
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        assert_eq!(
            state["settings"]["settingsWritable"], true,
            "the notch refuses to write its settings (untrusted Pulse folder?), so no edge change can be saved\n{}",
            notch_diagnostics(&ws, &bridge)
        );
        assert_eq!(state["settings"]["notchEdge"], "top", "a fresh notch docks to the top edge");
        let mut problems: Vec<String> = Vec::new();
        // The tour's fixture must be what this notch really publishes, or its gating checks prove nothing:
        // the same keys at the top level, in `settings` and in `options`.
        let keys = |v: &Value, at: &str| -> Vec<String> {
            let node = if at.is_empty() { v } else { &v[at] };
            let mut keys: Vec<String> = node.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
            keys.sort();
            keys
        };
        let fixture = windows_notch_fixture();
        for at in ["", "settings", "options"] {
            let (real, modelled) = (keys(&state, at), keys(&fixture, at));
            if real != modelled {
                problems.push(format!(
                    "windows_notch_fixture has drifted from the real notch at {:?}: the notch publishes {real:?}, the fixture {modelled:?}; update the fixture in hub/qa-e2e/tests/ui.rs",
                    if at.is_empty() { "top level" } else { at }
                ));
            }
        }

        // 2. The hub, reading that real state, offers the notch's four edges with Top on.
        let ctl = launch_hub(&ws, &home, sc);
        ctl.wait_eval(
            "const b=[...document.querySelectorAll('nav.rk-nav button')].find(e=>e.textContent.trim()==='Appearance'); if(!b) return false; b.click(); return true;",
            Duration::from_secs(10),
        )
        .expect("nav item Appearance");
        // The Edge options in order, the selected one starred.
        const EDGE_STATE: &str = "[...document.querySelectorAll('[role=radiogroup][aria-label=\"Edge\"] [role=radio]')].map((b) => b.textContent.trim() + (b.getAttribute('aria-checked') === 'true' ? '*' : '')).join(',')";
        let edge_now = || s(ctl.eval(&format!("return {EDGE_STATE}")).unwrap_or(Value::Null));
        if let Err(e) = ctl.wait_eval(&format!("return ({EDGE_STATE}) === 'Top*,Bottom,Left,Right'"), Duration::from_secs(30)) {
            let _ = ctl.screenshot_to(&shots.join("FAIL-notch-edge.png"));
            panic!("the hub's Edge control is not Top*,Bottom,Left,Right ({}); it reads {}", e.0, edge_now());
        }
        let before = shots.join("11-notch-edge-before.png");
        ctl.screenshot_to(&before).expect("screenshot");
        sc.keep("11-notch-edge-before", &before);

        // 3. Press Bottom as a person would. The notch deletes the hub's command once applied, so the
        //    evidence of a press is the saved settings file, not the command file.
        let commands = bridge.join("hub-commands");
        let pill = bridge.join("pill-settings.json");
        let edge_is = |v: &Value, want: &str| {
            v["edge"].as_str() == Some(want)
                || v["edges"].as_object().is_some_and(|m| !m.is_empty() && m.values().all(|e| e.as_str() == Some(want)))
        };
        let saved = click_until(&ctl, sc, "Edge: Bottom", &radio_js("Edge", "Bottom"), &|| {
            read_json(&pill).is_some_and(|v| edge_is(&v, "bottom"))
        });
        if !saved {
            let _ = ctl.screenshot_to(&shots.join("FAIL-notch-edge.png"));
            problems.push(format!(
                "pressing Bottom left no \"edge\":\"bottom\" in {} (commands seen by the hub: {:?})\n{}",
                pill.display(),
                commands_seen(&commands),
                notch_diagnostics(&ws, &bridge)
            ));
        }

        // 4. The notch republishes the edge and the hub shows it. The saved file alone is not enough:
        //    the hub's control must follow, or the next poll snaps it back to Top.
        let published = wait_for(Duration::from_secs(20), || {
            read_json(&state_path).filter(|v| v["settings"]["notchEdge"] == "bottom")
        });
        if published.is_none() {
            problems.push(format!(
                "the notch saved edge=bottom but still publishes notchEdge {} in notch-state.json (windows/src/bridge.rs published_edge should follow the default edge the Edge control sets)",
                read_json(&state_path).map(|v| v["settings"]["notchEdge"].to_string()).unwrap_or_default()
            ));
        }
        let shown = ctl.wait_eval(
            "return [...document.querySelectorAll('[role=radiogroup][aria-label=\"Edge\"] [role=radio]')].some((b) => b.textContent.trim() === 'Bottom' && b.getAttribute('aria-checked') === 'true')",
            Duration::from_secs(15),
        );
        if shown.is_err() {
            problems.push(format!("the hub's Edge control never showed Bottom as selected after the notch applied it (it reads {})", edge_now()));
        }
        let after = shots.join("12-notch-edge-after.png");
        ctl.screenshot_to(&after).expect("screenshot");
        sc.keep("12-notch-edge-after", &after);

        // 4b. Size > Large, then Custom size: saved, republished, shown (the Scale slider appears, finite).
        let size_saved = click_until(&ctl, sc, "Size: Large", &radio_js("Size", "Large"), &|| {
            read_json(&pill).is_some_and(|v| v["notch_size"].as_str() == Some("large"))
        });
        if !size_saved {
            let _ = ctl.screenshot_to(&shots.join("FAIL-notch-size.png"));
            problems.push(format!("pressing Large left no \"notch_size\":\"large\" in {}\n{}", pill.display(), notch_diagnostics(&ws, &bridge)));
        }
        if wait_for(Duration::from_secs(20), || read_json(&state_path).filter(|v| v["settings"]["notchSize"] == "large")).is_none() {
            problems.push(format!(
                "the notch saved notch_size=large but publishes notchSize {} in notch-state.json",
                read_json(&state_path).map(|v| v["settings"]["notchSize"].to_string()).unwrap_or_default()
            ));
        }
        if ctl
            .wait_eval(
                "return [...document.querySelectorAll('[role=radiogroup][aria-label=\"Size\"] [role=radio]')].some((b) => b.textContent.trim() === 'Large' && b.getAttribute('aria-checked') === 'true')",
                Duration::from_secs(15),
            )
            .is_err()
        {
            problems.push("the hub's Size control never showed Large as selected after the notch applied it".into());
        }
        let custom_find = "document.querySelector('[role=switch][aria-label=\"Custom size\"]')";
        let custom_saved = click_until(&ctl, sc, "Custom size", custom_find, &|| {
            read_json(&pill).is_some_and(|v| v["uses_custom_notch_scale"] == true)
        });
        if !custom_saved {
            let _ = ctl.screenshot_to(&shots.join("FAIL-notch-size.png"));
            problems.push(format!("pressing Custom size left no \"uses_custom_notch_scale\":true in {}\n{}", pill.display(), notch_diagnostics(&ws, &bridge)));
        }
        // The Scale slider shows only while Custom size is on; its value and reading must be numbers.
        if ctl
            .wait_eval(
                "const r = document.querySelector('input[type=range][aria-label=\"Scale\"]'); return !!r && Number.isFinite(Number(r.value)) && !/NaN/.test(r.parentElement.textContent)",
                Duration::from_secs(20),
            )
            .is_err()
        {
            let _ = ctl.screenshot_to(&shots.join("FAIL-notch-size.png"));
            problems.push(format!(
                "with Custom size on, the hub should show a Scale slider with a finite value; the page shows {}",
                s(ctl.eval("const r = document.querySelector('input[type=range]'); return r ? r.value + ' / ' + r.parentElement.textContent.trim() : 'no slider'").unwrap_or(Value::Null))
            ));
        }
        let sized = shots.join("13-notch-size.png");
        ctl.screenshot_to(&sized).expect("screenshot");
        sc.keep("13-notch-size", &sized);

        // 5. The same hub against the notch's real snapshot (not the fixture): the settings sections it
        //    feeds must render without empty pickers, NaN, Mac wording or malformed switches.
        for id in ["appearance", "notifications", "general", "accounts"] {
            let sec = SECTIONS.iter().find(|sec| sec.id == id).expect("section");
            let js = format!(
                "const b=[...document.querySelectorAll('nav.rk-nav button')].find(e=>e.textContent.trim()==={}); if(!b) return false; b.click(); return true;",
                json!(sec.title)
            );
            ctl.wait_eval(&js, Duration::from_secs(10)).unwrap_or_else(|e| panic!("nav item {}: {}", sec.title, e.0));
            let want = format!("return document.querySelector('.rk-top__title')?.textContent?.trim() === {}", json!(sec.title));
            ctl.wait_eval(&want, Duration::from_secs(15)).unwrap_or_else(|e| panic!("title never became {}: {}", sec.title, e.0));
            std::thread::sleep(Duration::from_millis(1500));
            generic_checks(&ctl, sec, &shots, &mut problems);
            windows_common(&ctl, sec, &mut problems);
            let shot = shots.join(format!("13-notch-real-{}.png", sec.id));
            ctl.screenshot_to(&shot).expect("screenshot");
            sc.keep(&format!("13-notch-real-{}", sec.id), &shot);
        }
        if !problems.is_empty() {
            panic!("{} problem(s) in the notch step:\n- {}", problems.len(), problems.join("\n- "));
        }
        drop(ctl);
        drop(running);
    });
}

#[test]
fn hub_sections_render_without_errors() {
    let h = Harness::new("pulse-hub", env!("CARGO_MANIFEST_DIR")).expect("rightkit-qa harness");
    hub_tour(&h);
    // One test, not two: both launch the hub, and a second hub on Windows would hand off to the
    // first through the per-user instance mutex and exit.
    #[cfg(windows)]
    notch_edge_journey(&h);
}
