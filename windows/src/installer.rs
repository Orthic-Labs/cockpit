//! Installer cards in the notch (the Windows counterpart of the Mac disk image installer,
//! mac/Notch/Sources/Features/DiskImage*.swift): a finished installer in Downloads is checked
//! and either installed on its own, with an Undo card, or asked about in the notch.
//!
//! * A watcher thread reads the Downloads folder with `ReadDirectoryChangesW` and notes
//!   `.msi`, `.msix`/`.msixbundle`/`.appx`/`.appxbundle` and setup-looking `.exe` files. A
//!   file counts as finished when its size stopped changing and nobody holds it open for
//!   writing. Files already there when the notch starts are left alone.
//! * `WinVerifyTrust` (`WINTRUST_ACTION_GENERIC_VERIFY_V2`) decides whether the signature is
//!   trusted; the signer's name is read from the certificate with PowerShell.
//! * Signed MSIX and MSI packages install on their own while `installer_auto` is on (off by default):
//!   MSIX through `Add-AppxPackage`, MSI through `msiexec /i /qb`. A four second window with a
//!   Cancel button comes first. Undo removes the package by its full name or product code.
//!   Everything else (unsigned, an installed copy to replace, a running app, a setup `.exe`,
//!   several downloads at once) asks in the notch.
//!
//! The cards reuse the notch's card renderer through `send::Panel`; a row's action is
//! `send::Action::Installer(Choice)`. Nothing here draws. `start` runs two threads (the
//! watcher and a half-second ticker) and posts the caller's window message whenever the card
//! changed. Everything else is called on the UI thread and only touches the model behind one
//! lock. Installs and PowerShell run on their own short-lived worker threads.

#![allow(dead_code)]

use crate::card::{Button, CardContent, Lead, Row, Tone};
use crate::diag;
use crate::glyphs::{Symbol, Tile};
use crate::send::{Action, Panel};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

type Handle = *mut c_void;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// A file is finished when this long passes with no change to it.
const SETTLE: Duration = Duration::from_millis(1500);
/// A file nobody can open for reading within this long is given up on.
const GIVE_UP: Duration = Duration::from_secs(300);
/// The Cancel window before an automatic install starts.
const GRACE: Duration = Duration::from_secs(4);
/// How long the Undo card stays (a hover keeps it).
const UNDO_HOLD: Duration = Duration::from_secs(30);
/// How long the other result cards stay.
const NOTE_HOLD: Duration = Duration::from_secs(10);
/// An ask nobody answered goes away after this.
const ASK_HOLD: Duration = Duration::from_secs(300);
const MAX_HANDLED: usize = 512;

// ---- the cards ------------------------------------------------------------------------------

/// What a row of an installer card asks for (`send::Action::Installer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Install (also "Install anyway" for an unsigned one).
    Install,
    /// Replace the installed copy.
    Replace,
    /// Close the running app, then update it.
    QuitAndUpdate,
    /// Remove what was just installed.
    Undo,
    /// Run a setup program.
    OpenInstaller,
    /// Show the download in Explorer.
    ShowFile,
    /// Stop before the install starts.
    Cancel,
    /// Put the card away.
    Dismiss,
}

/// The symbol on a choice's pill, as the Mac's `DiskImageCard.symbol(for:)` picks it.
pub fn symbol(choice: Choice) -> Symbol {
    match choice {
        Choice::Install => Symbol::DownApp,
        Choice::Replace | Choice::QuitAndUpdate => Symbol::Cycle,
        Choice::Undo => Symbol::Undo,
        Choice::OpenInstaller => Symbol::Box,
        Choice::ShowFile => Symbol::Folder,
        Choice::Cancel => Symbol::Stop,
        Choice::Dismiss => Symbol::Clock,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    Ask,
    Working,
    Done,
    Problem,
}

#[derive(Clone, Debug, PartialEq)]
struct Card {
    title: String,
    detail: String,
    warning: Option<String>,
    style: Style,
    buttons: Vec<(&'static str, Choice)>,
    /// The file whose icon leads the card (the package tile when there is none).
    icon: Option<String>,
    /// How long the card stays; `None` keeps it until answered or replaced.
    hold: Option<Duration>,
    expires: Option<Instant>,
}

impl Card {
    fn new(style: Style, title: String, detail: String) -> Card {
        Card {
            title,
            detail,
            warning: None,
            style,
            buttons: Vec::new(),
            icon: None,
            hold: None,
            expires: None,
        }
    }

    fn icon(mut self, path: &Path) -> Card {
        if !path.as_os_str().is_empty() {
            self.icon = Some(path.to_string_lossy().into_owned());
        }
        self
    }

    fn with(mut self, label: &'static str, choice: Choice) -> Card {
        self.buttons.push((label, choice));
        self
    }

    fn held(mut self, hold: Duration) -> Card {
        self.hold = Some(hold);
        self
    }

    fn warned(mut self, warning: &str) -> Card {
        self.warning = Some(warning.to_string());
        self
    }

    /// The card as the Mac's `DiskImageCard` builds it: the file's icon, the title, the detail
    /// (amber on a problem) and the warning under it, then the indeterminate bar while
    /// working, then one pill per button and a round close. A working card has its buttons
    /// only while it can be cancelled.
    fn panel(&self) -> Panel {
        let content = CardContent {
            title: self.title.clone(),
            subtitle: (!self.detail.is_empty()).then(|| self.detail.clone()),
            lead: Some(match &self.icon {
                Some(path) => Lead::File(path.clone()),
                None => Lead::Tile(Tile::Package),
            }),
            wide: true,
            problem: self.style == Style::Problem,
            ..CardContent::default()
        };
        let mut panel = Panel::new(content);
        if let Some(warning) = &self.warning {
            let tinted = Row::Tinted {
                text: warning.clone(),
                tone: Tone::Warning,
            };
            panel.row(tinted, None);
        }
        if self.style == Style::Working {
            panel.row(Row::Progress(None), None);
        }
        if self.style != Style::Working || !self.buttons.is_empty() {
            let pills = self
                .buttons
                .iter()
                .map(|(label, choice)| Button::new(*label, symbol(*choice)))
                .collect();
            let mut actions: Vec<Option<Action>> = self
                .buttons
                .iter()
                .map(|(_, choice)| Some(Action::Installer(*choice)))
                .collect();
            actions.push(Some(Action::Installer(Choice::Dismiss)));
            let row = Row::Buttons {
                buttons: pills,
                close: true,
            };
            panel.buttons(row, actions);
        }
        panel
    }
}

// ---- what was downloaded --------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Msi,
    Msix,
    Exe,
}

#[derive(Clone, Debug, PartialEq)]
enum Trust {
    /// A trusted signature; the signer's name.
    Signed(String),
    Unsigned,
}

#[derive(Clone, Debug)]
struct Item {
    path: PathBuf,
    kind: Kind,
    trust: Trust,
    name: String,
    version: String,
    /// The MSI product code or the MSIX package name; empty for a setup program.
    id: String,
    /// The version already installed, if any.
    installed: Option<String>,
    /// Where the installed copy lives, to see whether it is running.
    location: Option<String>,
    running: bool,
    /// Executables that were running when the app was closed for this install; each is
    /// started again once the new copy is in place.
    relaunch: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct Undo {
    kind: Kind,
    name: String,
    /// The MSI product code or the MSIX package full name.
    target: String,
    updated: bool,
}

fn publisher(item: &Item) -> String {
    match &item.trust {
        Trust::Signed(name) => name.clone(),
        Trust::Unsigned => "an unknown publisher".to_string(),
    }
}

fn version_from(item: &Item) -> String {
    if item.version.is_empty() {
        format!("From {}.", publisher(item))
    } else {
        format!("Version {} from {}.", item.version, publisher(item))
    }
}

fn card_ask(item: &Item) -> Card {
    Card::new(
        Style::Ask,
        format!("Install {}?", item.name),
        format!("{} Windows may ask for permission.", version_from(item)),
    )
    .with("Install", Choice::Install)
    .icon(&item.path)
    .held(ASK_HOLD)
}

fn card_untrusted(item: &Item) -> Card {
    Card::new(
        Style::Ask,
        format!("Install {}?", item.name),
        version_from(item),
    )
    .warned("Its signature could not be verified.")
    .with("Install anyway", Choice::Install)
    .icon(&item.path)
    .held(ASK_HOLD)
}

fn card_replace(item: &Item) -> Card {
    let installed = item.installed.clone().unwrap_or_default();
    let detail = if item.version.is_empty() || installed.is_empty() {
        format!("{} is already installed.", item.name)
    } else {
        format!(
            "Version {installed} is installed. This one is {}.",
            item.version
        )
    };
    Card::new(Style::Ask, item.name.clone(), detail)
        .with("Replace", Choice::Replace)
        .icon(&item.path)
        .held(ASK_HOLD)
}

fn card_quit_update(item: &Item) -> Card {
    Card::new(
        Style::Ask,
        item.name.clone(),
        format!("{} is open. It will be closed, then updated.", item.name),
    )
    .with("Quit & update", Choice::QuitAndUpdate)
    .icon(&item.path)
    .held(ASK_HOLD)
}

fn card_installing(name: &str, cancel: bool) -> Card {
    let card = Card::new(
        Style::Working,
        format!("Installing {name}"),
        "Checking and installing it.".to_string(),
    );
    if cancel {
        card.with("Cancel", Choice::Cancel)
    } else {
        card
    }
}

fn card_installed(name: &str, version: &str, previous: Option<&str>) -> Card {
    let card = match previous {
        Some(old) => {
            let title = if version.is_empty() {
                format!("Updated {name}")
            } else {
                format!("Updated {name} to {version}")
            };
            let detail = if old.is_empty() {
                "Replaced the earlier version.".to_string()
            } else {
                format!("Replaced version {old}.")
            };
            Card::new(Style::Done, title, detail)
        }
        None => Card::new(
            Style::Done,
            format!("Installed {name}"),
            "The download is still in Downloads.".to_string(),
        ),
    };
    card.with("Undo", Choice::Undo).held(UNDO_HOLD)
}

fn card_undoing(name: &str) -> Card {
    Card::new(
        Style::Working,
        format!("Undoing {name}"),
        "Removing it.".to_string(),
    )
}

fn card_undone(name: &str, updated: bool) -> Card {
    let detail = if updated {
        "The earlier version is not restored."
    } else {
        "The download is still in Downloads."
    };
    Card::new(Style::Done, format!("Removed {name}"), detail.to_string()).held(NOTE_HOLD)
}

fn card_cancelled(name: &str) -> Card {
    Card::new(
        Style::Done,
        "Cancelled".to_string(),
        format!("{name} was not installed."),
    )
    .held(NOTE_HOLD)
}

fn card_failed(name: &str, why: &str) -> Card {
    Card::new(
        Style::Problem,
        format!("Could not install {name}"),
        why.to_string(),
    )
    .with("Show in Explorer", Choice::ShowFile)
    .held(ASK_HOLD)
}

fn card_still_open(name: &str) -> Card {
    Card::new(
        Style::Problem,
        format!("{name} is still open"),
        "It did not close. Save your work, then try again.".to_string(),
    )
    .with("Quit & update", Choice::QuitAndUpdate)
    .held(ASK_HOLD)
}

fn card_installer(item: &Item) -> Card {
    let detail = match &item.trust {
        Trust::Signed(name) => format!("Signed by {name}. This download is an installer."),
        Trust::Unsigned => "This download is an installer.".to_string(),
    };
    Card::new(Style::Ask, format!("{} Installer", item.name), detail)
        .with("Open installer", Choice::OpenInstaller)
        .icon(&item.path)
        .held(ASK_HOLD)
}

fn card_several(count: usize) -> Card {
    Card::new(
        Style::Ask,
        format!("{count} installers downloaded"),
        "Pick which one to open from Downloads.".to_string(),
    )
    .with("Show in Explorer", Choice::ShowFile)
    .held(ASK_HOLD)
}

fn sample_item(kind: Kind, trust: Trust, version: &str, installed: Option<&str>) -> Item {
    Item {
        path: PathBuf::new(),
        kind,
        trust,
        name: "Notes Pro".to_string(),
        version: version.to_string(),
        id: String::new(),
        installed: installed.map(str::to_string),
        location: None,
        running: false,
        relaunch: Vec::new(),
    }
}

/// The card for one of `qa/notch-views.json`'s disk image views, as Windows words it, for
/// the off-screen renderer. `None` for an id this module has no card for.
pub fn sample(id: &str) -> Option<Panel> {
    let signed = || Trust::Signed("Example Software".to_string());
    let msix = |version: &str, installed: Option<&str>| {
        sample_item(Kind::Msix, signed(), version, installed)
    };
    let unsigned = sample_item(Kind::Msi, Trust::Unsigned, "2.4.1", None);
    let setup = sample_item(Kind::Exe, signed(), "", None);
    let card = match id {
        "disk-install-ask" => card_ask(&msix("2.4.1", None)),
        "disk-install-untrusted" => card_untrusted(&unsigned),
        "disk-replace-ask" => card_replace(&msix("2.5.0", Some("2.4.1"))),
        "disk-quit-update-ask" => card_quit_update(&msix("2.5.0", Some("2.4.1"))),
        "disk-installing" => card_installing("Notes Pro", false),
        "disk-installing-cancel" => card_installing("Notes Pro", true),
        "disk-installed-undo" => card_installed("Notes Pro", "2.4.1", None),
        "disk-updated-undo" => card_installed("Notes Pro", "2.5.0", Some("2.4.1")),
        "disk-undoing" => card_undoing("Notes Pro"),
        "disk-undone" => card_undone("Notes Pro", false),
        "disk-cancelled" => card_cancelled("Notes Pro"),
        "disk-install-failed" => card_failed("Notes Pro", "Windows could not verify the package."),
        "disk-still-open" => card_still_open("Notes Pro"),
        "disk-installer-package" => card_installer(&setup),
        "disk-several-apps" => card_several(3),
        _ => return None,
    };
    Some(card.panel())
}

// ---- the model ------------------------------------------------------------------------------

#[derive(Default)]
struct Context {
    item: Option<Item>,
    undo: Option<Undo>,
    /// What "Show in Explorer" reveals.
    path: Option<PathBuf>,
    cancel: Option<Arc<AtomicBool>>,
}

struct Pending {
    first: Instant,
    last: Instant,
    len: Option<u64>,
}

struct Model {
    card: Option<Card>,
    ctx: Context,
    hovering: bool,
    auto: bool,
    /// A check or an install is running on a worker thread.
    busy: bool,
    pending: HashMap<PathBuf, Pending>,
    handled: HashSet<(PathBuf, u64)>,
    queue: VecDeque<Vec<PathBuf>>,
}

impl Model {
    fn new() -> Model {
        Model {
            card: None,
            ctx: Context::default(),
            hovering: false,
            auto: true,
            busy: false,
            pending: HashMap::new(),
            handled: HashSet::new(),
            queue: VecDeque::new(),
        }
    }
}

static MODEL: LazyLock<Mutex<Model>> = LazyLock::new(|| Mutex::new(Model::new()));
static STARTED: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);
static MESSAGE: AtomicU32 = AtomicU32::new(0);
static DIRECTORY: AtomicIsize = AtomicIsize::new(0);

fn model() -> MutexGuard<'static, Model> {
    MODEL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn notify() {
    sys::post_message(
        WINDOW.load(Ordering::Relaxed),
        MESSAGE.load(Ordering::Relaxed),
    );
}

fn show(mut card: Card) {
    card.expires = card.hold.map(|hold| Instant::now() + hold);
    let mut model = model();
    // A card that does not name a file leads with the icon of the download in hand.
    if card.icon.is_none()
        && let Some(item) = &model.ctx.item
    {
        card = card.icon(&item.path);
    }
    model.card = Some(card);
    drop(model);
    notify();
}

/// A worker is done: the next download may be looked at once the card goes.
fn finish() {
    model().busy = false;
    notify();
}

fn downloads_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("Downloads"))
        .filter(|p| p.is_dir())
}

fn kind_of(name: &str) -> Option<Kind> {
    let lower = name.to_ascii_lowercase();
    let (stem, extension) = lower.rsplit_once('.')?;
    match extension {
        "msi" => Some(Kind::Msi),
        "msix" | "msixbundle" | "appx" | "appxbundle" => Some(Kind::Msix),
        "exe" => {
            let looks_like_setup = stem.contains("setup") || stem.contains("install");
            (looks_like_setup && !stem.contains("uninstall")).then_some(Kind::Exe)
        }
        _ => None,
    }
}

/// "NotesPro-Setup-2.4.1" reads "NotesPro": words up to the first one that names the kind of
/// download, a platform or a version.
fn name_from_file(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let words: Vec<&str> = stem
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .take_while(|w| {
            let lower = w.to_ascii_lowercase();
            let numeric = lower
                .trim_start_matches('v')
                .starts_with(|c: char| c.is_ascii_digit());
            let platform = matches!(
                lower.as_str(),
                "setup"
                    | "install"
                    | "installer"
                    | "x64"
                    | "x86"
                    | "arm64"
                    | "win"
                    | "win32"
                    | "win64"
                    | "windows"
                    | "msi"
                    | "msix"
            );
            !numeric && !platform
        })
        .collect();
    if words.is_empty() {
        stem
    } else {
        words.join(" ")
    }
}

/// The first line of `text`, cut to `most` characters.
fn short(text: &str, most: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= most {
        line.to_string()
    } else {
        let head: String = line.chars().take(most).collect();
        format!("{head}\u{2026}")
    }
}

// ---- public face ----------------------------------------------------------------------------

/// Starts the Downloads watcher. `window` is the key of a window of the UI thread that gets
/// `message` (a `WM_APP + n`) whenever the card changed; the caller then re-reads
/// `popup_panel()`. `auto` is the `installer_auto` setting. Calling it twice does nothing.
pub fn start(window: isize, message: u32, auto: bool) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    STOP.store(false, Ordering::SeqCst);
    WINDOW.store(window, Ordering::SeqCst);
    MESSAGE.store(message, Ordering::SeqCst);
    model().auto = auto;
    let Some(dir) = downloads_dir() else {
        diag::info("installer_no_downloads", &[("action", "not_watching")]);
        return;
    };
    let watching = std::thread::Builder::new()
        .name("pulse-installer-watch".into())
        .spawn(move || watcher(&dir));
    if let Err(error) = watching {
        diag::info(
            "installer_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
        return;
    }
    let ticking = std::thread::Builder::new()
        .name("pulse-installer-tick".into())
        .spawn(ticker);
    if let Err(error) = ticking {
        diag::info(
            "installer_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

/// Stops both threads. Call when the notch quits.
pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    STARTED.store(false, Ordering::SeqCst);
    let handle = DIRECTORY.swap(0, Ordering::SeqCst);
    if handle != 0 {
        sys::cancel_watch(handle);
    }
}

/// The `installer_auto` setting: off means every installer is asked about.
pub fn set_auto(on: bool) {
    model().auto = on;
}

/// The card that hangs from the notch while there is news, if any.
pub fn popup_panel() -> Option<Panel> {
    model().card.as_ref().map(Card::panel)
}

/// The pointer is over or left the card: hovering keeps it up.
pub fn popup_hover(on: bool) {
    let mut model = model();
    model.hovering = on;
    if !on {
        let now = Instant::now();
        if let Some(card) = model.card.as_mut() {
            card.expires = card.hold.map(|hold| now + hold);
        }
    }
}

/// A click on a row of `popup_panel()`.
pub fn perform(choice: Choice) {
    let (item, undo, path, cancel, was_working) = {
        let mut model = model();
        let snapshot = (
            model.ctx.item.clone(),
            model.ctx.undo.clone(),
            model.ctx.path.clone(),
            model.ctx.cancel.clone(),
            model
                .card
                .as_ref()
                .is_some_and(|c| c.style == Style::Working),
        );
        // Cancel keeps the card up until the worker shows its answer.
        if choice != Choice::Cancel {
            model.card = None;
        }
        if choice == Choice::Dismiss {
            model.ctx = Context::default();
        }
        snapshot
    };
    match choice {
        Choice::Dismiss => {
            // A close on the cancellable card cancels.
            if was_working && let Some(flag) = &cancel {
                flag.store(true, Ordering::SeqCst);
            }
        }
        Choice::Cancel => {
            if let Some(flag) = &cancel {
                flag.store(true, Ordering::SeqCst);
            }
        }
        Choice::ShowFile => {
            if let Some(path) = path.or_else(|| item.as_ref().map(|i| i.path.clone())) {
                reveal(&path);
            }
        }
        Choice::Install | Choice::Replace => {
            if let Some(item) = item {
                if item.kind == Kind::Exe {
                    open_file(&item.path);
                } else {
                    spawn_job(move |cancel| install_job(item, false, cancel));
                }
            }
        }
        Choice::QuitAndUpdate => {
            if let Some(item) = item {
                spawn_job(move |cancel| quit_job(item, cancel));
            }
        }
        Choice::OpenInstaller => {
            if let Some(item) = item {
                open_file(&item.path);
            }
        }
        Choice::Undo => {
            if let Some(undo) = undo {
                spawn_job(move |_| undo_job(&undo));
            }
        }
    }
    notify();
}

fn spawn_job<F>(job: F)
where
    F: FnOnce(Arc<AtomicBool>) + Send + 'static,
{
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut model = model();
        model.busy = true;
        model.ctx.cancel = Some(cancel.clone());
    }
    let spawned = std::thread::Builder::new()
        .name("pulse-installer-job".into())
        .spawn(move || job(cancel));
    if let Err(error) = spawned {
        diag::info(
            "installer_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
        finish();
    }
}

// ---- watching Downloads ---------------------------------------------------------------------

fn watcher(dir: &Path) {
    let Some(handle) = sys::open_directory(dir) else {
        diag::info("installer_watch_failed", &[("action", "not_watching")]);
        return;
    };
    DIRECTORY.store(handle, Ordering::SeqCst);
    while !STOP.load(Ordering::Relaxed) {
        let Some(names) = sys::next_changes(handle) else {
            break;
        };
        let now = Instant::now();
        let mut model = model();
        for name in names {
            if kind_of(&name).is_none() {
                continue;
            }
            model.pending.insert(
                dir.join(&name),
                Pending {
                    first: now,
                    last: now,
                    len: None,
                },
            );
        }
    }
    DIRECTORY.store(0, Ordering::SeqCst);
    sys::close_directory(handle);
}

fn ticker() {
    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        let batch = settled();
        let next = {
            let mut model = model();
            if let Some(batch) = batch {
                model.queue.push_back(batch);
            }
            // The file behind a question is gone from Downloads: the question is moot.
            let moot = !model.busy
                && model.card.as_ref().is_some_and(|c| c.style == Style::Ask)
                && model
                    .ctx
                    .item
                    .as_ref()
                    .is_some_and(|item| matches!(item.path.try_exists(), Ok(false)));
            if moot {
                model.card = None;
                model.ctx = Context::default();
                notify();
            }
            let expired = model
                .card
                .as_ref()
                .and_then(|c| c.expires)
                .is_some_and(|at| Instant::now() >= at);
            if expired && !model.hovering && !model.busy {
                model.card = None;
                model.ctx = Context::default();
                notify();
            }
            if model.card.is_none() && !model.busy {
                let next = model.queue.pop_front();
                if next.is_some() {
                    model.busy = true;
                }
                next
            } else {
                None
            }
        };
        if let Some(paths) = next {
            let spawned = std::thread::Builder::new()
                .name("pulse-installer-check".into())
                .spawn(move || check_job(&paths));
            if spawned.is_err() {
                finish();
            }
        }
    }
}

/// The files that finished downloading since the last look, as one batch.
fn settled() -> Option<Vec<PathBuf>> {
    let mut model = model();
    let keys: Vec<PathBuf> = model.pending.keys().cloned().collect();
    let mut ready = Vec::new();
    let mut gone = Vec::new();
    for key in keys {
        let Some(entry) = model.pending.get_mut(&key) else {
            continue;
        };
        if entry.last.elapsed() < SETTLE {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&key) else {
            gone.push(key);
            continue;
        };
        let len = meta.len();
        if entry.len != Some(len) {
            entry.len = Some(len);
            entry.last = Instant::now();
            continue;
        }
        let open = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&key)
            .is_ok();
        if open && len > 0 {
            ready.push((key, len));
        } else if entry.first.elapsed() > GIVE_UP {
            gone.push(key);
        }
    }
    for key in gone {
        model.pending.remove(&key);
    }
    let mut batch = Vec::new();
    for (key, len) in ready {
        model.pending.remove(&key);
        if model.handled.len() >= MAX_HANDLED {
            model.handled.clear();
        }
        if model.handled.insert((key.clone(), len)) {
            batch.push(key);
        }
    }
    (!batch.is_empty()).then_some(batch)
}

// ---- looking at a download ------------------------------------------------------------------

fn check_job(paths: &[PathBuf]) {
    if paths.len() > 1 {
        let dir = paths[0].parent().map(Path::to_path_buf);
        model().ctx = Context {
            path: dir,
            ..Context::default()
        };
        show(card_several(paths.len()));
        finish();
        return;
    }
    let Some(item) = paths.first().and_then(|p| analyze(p)) else {
        finish();
        return;
    };
    let auto = {
        let mut model = model();
        model.ctx = Context {
            item: Some(item.clone()),
            undo: None,
            path: Some(item.path.clone()),
            cancel: None,
        };
        model.auto
    };
    let signed = matches!(item.trust, Trust::Signed(_));
    match item.kind {
        Kind::Exe if signed => show(card_installer(&item)),
        _ if !signed => show(card_untrusted(&item)),
        _ if item.installed.is_some() && item.running => show(card_quit_update(&item)),
        _ if item.installed.is_some() => show(card_replace(&item)),
        _ if auto => {
            // The install runs on this thread, after a window to cancel in.
            let cancel = Arc::new(AtomicBool::new(false));
            model().ctx.cancel = Some(cancel.clone());
            install_job(item, true, cancel);
            return;
        }
        _ => show(card_ask(&item)),
    }
    finish();
}

fn analyze(path: &Path) -> Option<Item> {
    let file = path.file_name()?.to_string_lossy().into_owned();
    let kind = kind_of(&file)?;
    let trust = if sys::verify_signature(path) {
        let signer = signer_name(path).unwrap_or_else(|| "a verified publisher".to_string());
        Trust::Signed(signer)
    } else {
        Trust::Unsigned
    };
    let mut item = Item {
        path: path.to_path_buf(),
        kind,
        trust,
        name: name_from_file(path),
        version: String::new(),
        id: String::new(),
        installed: None,
        location: None,
        running: false,
        relaunch: Vec::new(),
    };
    match kind {
        Kind::Exe => {}
        Kind::Msi => msi_details(&mut item),
        Kind::Msix => msix_details(&mut item),
    }
    if let Some(location) = item.location.clone()
        && item.installed.is_some()
    {
        item.running = running_count(&location, false) > 0;
    }
    Some(item)
}

fn msi_details(item: &mut Item) {
    let Some(meta) = sys::msi_package(&item.path) else {
        return;
    };
    if !meta.name.is_empty() {
        item.name = meta.name;
    }
    item.version = meta.version;
    item.id = meta.product_code.clone();
    // Installed already: this very product, else an older one sharing its upgrade code.
    let mut installed = meta.product_code;
    let mut version = sys::msi_product_info(&installed, "VersionString");
    if version.is_none()
        && let Some(related) = sys::msi_related_product(&meta.upgrade_code)
    {
        version = sys::msi_product_info(&related, "VersionString");
        installed = related;
    }
    if let Some(version) = version {
        item.location = sys::msi_product_info(&installed, "InstallLocation");
        item.installed = Some(version);
    }
}

fn msix_details(item: &mut Item) {
    const BODY: &str = "Add-Type -AssemblyName System.IO.Compression.FileSystem;\
        $z=[IO.Compression.ZipFile]::OpenRead($env:PULSE_PKG);\
        try{$e=$z.Entries|Where-Object{$_.FullName -eq 'AppxManifest.xml' -or \
        $_.FullName -eq 'AppxMetadata/AppxBundleManifest.xml'}|Select-Object -First 1;\
        $r=New-Object IO.StreamReader($e.Open());$x=[xml]$r.ReadToEnd();$r.Dispose()}\
        finally{$z.Dispose()};\
        $id=$x.DocumentElement.Identity;$dn='';if($x.Package){$dn=$x.Package.Properties.DisplayName};\
        $old=Get-AppxPackage -Name $id.Name|Select-Object -First 1;\
        [Console]::Out.Write(('{0}|{1}|{2}|{3}|{4}' -f $id.Name,$id.Version,$dn,$old.Version,\
        $old.InstallLocation))";
    let path = item.path.to_string_lossy().into_owned();
    let Some(out) = powershell(BODY, &[("PULSE_PKG", path.as_str())]).filter(|o| o.code == 0)
    else {
        return;
    };
    let parts: Vec<&str> = out.out.trim().split('|').collect();
    if let [identity, version, display, old, location] = parts.as_slice() {
        item.id = (*identity).to_string();
        item.version = (*version).to_string();
        // A display name that is a resource reference ("ms-resource:...") is not a name.
        if !display.is_empty() && !display.starts_with("ms-resource:") {
            item.name = (*display).to_string();
        }
        if !old.is_empty() {
            item.installed = Some((*old).to_string());
            item.location = Some((*location).to_string()).filter(|l| !l.is_empty());
        }
    }
}

pub(crate) fn signer_name(path: &Path) -> Option<String> {
    const BODY: &str = "$s=(Get-AuthenticodeSignature -LiteralPath \
        $env:PULSE_PKG).SignerCertificate;if($s){[Console]::Out.Write($s.GetNameInfo(\
        [Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false))}";
    let path = path.to_string_lossy().into_owned();
    let out = powershell(BODY, &[("PULSE_PKG", path.as_str())])?;
    let name = short(&out.out, 80);
    (out.code == 0 && !name.is_empty()).then_some(name)
}

/// How many processes run from under `location`; with `quit`, asks them to close and waits
/// up to eight seconds first.
fn running_count(location: &str, quit: bool) -> u32 {
    running_processes(location, quit).0
}

/// Like `running_count`, and also the distinct executables that were running before any
/// were asked to close, so they can be started again after an install.
fn running_processes(location: &str, quit: bool) -> (u32, Vec<PathBuf>) {
    const BODY: &str = "$l=$env:PULSE_LOC;if(-not $l){[Console]::Out.Write('0')}else{\
        $p=@(Get-Process -ErrorAction SilentlyContinue|Where-Object{$_.Path -and \
        $_.Path.StartsWith($l,[StringComparison]::OrdinalIgnoreCase)});\
        $x=@($p|ForEach-Object{$_.Path}|Select-Object -Unique);\
        if($env:PULSE_MODE -eq 'quit'){foreach($q in $p){[void]$q.CloseMainWindow()};\
        for($i=0;$i -lt 16;$i++){Start-Sleep -Milliseconds 500;\
        $p=@($p|Where-Object{-not $_.HasExited});if($p.Count -eq 0){break}}};\
        [Console]::Out.Write($p.Count);foreach($e in $x){[Console]::Out.Write(\"`n$e\")}}";
    let mode = if quit { "quit" } else { "check" };
    let Some(output) =
        powershell(BODY, &[("PULSE_LOC", location), ("PULSE_MODE", mode)]).filter(|o| o.code == 0)
    else {
        return (0, Vec::new());
    };
    let mut lines = output.out.lines();
    let count = lines
        .next()
        .and_then(|line| line.trim().parse().ok())
        .unwrap_or(0);
    let exes = lines
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    (count, exes)
}

/// Starts each executable again, detached, so it outlives the notch's own process.
fn relaunch_apps(exes: &[PathBuf]) {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    for exe in exes.iter().filter(|exe| exe.is_file()) {
        let _ = Command::new(exe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn();
    }
}

// ---- installing and undoing -----------------------------------------------------------------

enum Fail {
    Cancelled,
    Failed(String),
}

fn install_job(item: Item, grace: bool, cancel: Arc<AtomicBool>) {
    if grace {
        show(card_installing(&item.name, true));
        let steps = GRACE.as_millis() / 100;
        for _ in 0..steps {
            std::thread::sleep(Duration::from_millis(100));
            if cancel.load(Ordering::SeqCst) {
                model().ctx = Context::default();
                show(card_cancelled(&item.name));
                finish();
                return;
            }
        }
    }
    show(card_installing(&item.name, false));
    match do_install(&item) {
        Ok(undo) => {
            // An app that was running before the install runs again afterwards.
            relaunch_apps(&item.relaunch);
            let card = card_installed(&item.name, &item.version, item.installed.as_deref());
            model().ctx.undo = Some(undo);
            show(card);
        }
        Err(Fail::Cancelled) => show(card_cancelled(&item.name)),
        Err(Fail::Failed(why)) => {
            diag::info("installer_failed", &[("kind", kind_name(item.kind))]);
            show(card_failed(&item.name, &why));
        }
    }
    finish();
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Msi => "msi",
        Kind::Msix => "msix",
        Kind::Exe => "exe",
    }
}

fn do_install(item: &Item) -> Result<Undo, Fail> {
    let updated = item.installed.is_some();
    match item.kind {
        Kind::Msi => {
            let status = Command::new(system_file("System32\\msiexec.exe"))
                .arg("/i")
                .arg(&item.path)
                .args(["/qb", "/norestart"])
                .stdin(Stdio::null())
                .status()
                .map_err(|e| Fail::Failed(short(&e.to_string(), 100)))?;
            match status.code() {
                Some(0 | 1641 | 3010) => {}
                Some(1602 | 1223) => return Err(Fail::Cancelled),
                Some(code) => {
                    return Err(Fail::Failed(format!(
                        "Windows Installer stopped with code {code}."
                    )));
                }
                None => return Err(Fail::Failed("Windows Installer did not finish.".into())),
            }
            // Evidence, not the exit code alone: the product must now be registered.
            if sys::msi_product_info(&item.id, "VersionString").is_none() {
                return Err(Fail::Failed("It did not register with Windows.".into()));
            }
            Ok(Undo {
                kind: Kind::Msi,
                name: item.name.clone(),
                target: item.id.clone(),
                updated,
            })
        }
        Kind::Msix => {
            const BODY: &str = "Add-AppxPackage -LiteralPath $env:PULSE_PKG;\
                $p=Get-AppxPackage -Name $env:PULSE_NAME|Sort-Object Version -Descending|\
                Select-Object -First 1;\
                if(-not $p){throw 'The package did not register.'};\
                [Console]::Out.Write($p.PackageFullName)";
            let path = item.path.to_string_lossy().into_owned();
            let env = [
                ("PULSE_PKG", path.as_str()),
                ("PULSE_NAME", item.id.as_str()),
            ];
            let out = powershell(BODY, &env)
                .ok_or_else(|| Fail::Failed("Windows PowerShell did not start.".into()))?;
            let full = out.out.trim().to_string();
            if out.code != 0 || full.is_empty() {
                let why = short(&out.err, 110);
                return Err(Fail::Failed(if why.is_empty() {
                    "Windows could not install it.".into()
                } else {
                    why
                }));
            }
            Ok(Undo {
                kind: Kind::Msix,
                name: item.name.clone(),
                target: full,
                updated,
            })
        }
        Kind::Exe => Err(Fail::Failed(
            "A setup program is run by its own window.".into(),
        )),
    }
}

fn quit_job(mut item: Item, cancel: Arc<AtomicBool>) {
    show(Card::new(
        Style::Working,
        format!("Closing {}", item.name),
        "Waiting for it to quit.".to_string(),
    ));
    let (left, exes) = item
        .location
        .as_deref()
        .map_or((0, Vec::new()), |location| {
            running_processes(location, true)
        });
    item.relaunch = exes;
    if left > 0 {
        model().ctx.item = Some(item.clone());
        show(card_still_open(&item.name));
        finish();
        return;
    }
    install_job(item, false, cancel);
}

fn undo_job(undo: &Undo) {
    show(card_undoing(&undo.name));
    let removed = match undo.kind {
        Kind::Msi if sys::is_product_code(&undo.target) => {
            let status = Command::new(system_file("System32\\msiexec.exe"))
                .arg("/x")
                .arg(&undo.target)
                .args(["/qb", "/norestart"])
                .stdin(Stdio::null())
                .status();
            status.is_ok_and(|s| matches!(s.code(), Some(0 | 1641 | 3010)))
        }
        Kind::Msix => {
            const BODY: &str = "Remove-AppxPackage -Package $env:PULSE_FULL";
            let env = [("PULSE_FULL", undo.target.as_str())];
            powershell(BODY, &env).is_some_and(|o| o.code == 0)
        }
        _ => false,
    };
    if removed {
        show(card_undone(&undo.name, undo.updated));
    } else {
        show(card_failed(
            &undo.name,
            "It could not be removed. Use Settings, Apps.",
        ));
    }
    finish();
}

// ---- processes ------------------------------------------------------------------------------

fn system_file(relative: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    root.join(relative)
}

struct Output {
    code: i32,
    out: String,
    err: String,
}

const PS_HEAD: &str =
    "$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.Encoding]::UTF8;try{";
const PS_TAIL: &str = "}catch{[Console]::Error.Write($_.Exception.Message);exit 1}";

/// Runs a fixed script in Windows PowerShell with no window. Values reach it only as
/// environment variables, never inside the script text.
fn powershell(body: &str, env: &[(&str, &str)]) -> Option<Output> {
    let script = format!("{PS_HEAD}{body}{PS_TAIL}");
    let mut command = Command::new(system_file(
        "System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    ));
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().ok()?;
    Some(Output {
        code: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Selects the download in Explorer, or opens the folder.
fn reveal(path: &Path) {
    let mut command = Command::new(system_file("explorer.exe"));
    if path.is_dir() {
        command.arg(path);
    } else if path.exists() && !path.to_string_lossy().contains('"') {
        command.raw_arg(format!("/select,\"{}\"", path.display()));
    } else {
        return;
    }
    let _ = command.creation_flags(CREATE_NO_WINDOW).spawn();
}

/// Runs a setup program through Explorer, so SmartScreen and the "open" verb apply.
fn open_file(path: &Path) {
    if !path.exists() {
        return;
    }
    let _ = Command::new(system_file("explorer.exe"))
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

// ---- Win32 ----------------------------------------------------------------------------------

/// Raw declarations (kernel32, user32, wintrust, msi) so no `windows` crate feature has to
/// change; each `unsafe` block says what it relies on.
mod sys {
    use super::{Handle, Path, c_void};
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const SHARE_ALL: u32 = 0x0000_0007;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    /// FILE_NOTIFY_CHANGE_FILE_NAME | SIZE | LAST_WRITE.
    const CHANGE_FILTER: u32 = 0x0000_0001 | 0x0000_0008 | 0x0000_0010;
    const WTD_UI_NONE: u32 = 2;
    const WTD_CHOICE_FILE: u32 = 1;
    const WTD_STATEACTION_VERIFY: u32 = 1;
    const WTD_STATEACTION_CLOSE: u32 = 2;
    const WTD_REVOCATION_CHECK_NONE: u32 = 0x10;
    const ERROR_SUCCESS: u32 = 0;
    const OPEN_PACKAGE_IGNORE_MACHINE_STATE: u32 = 1;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// WINTRUST_ACTION_GENERIC_VERIFY_V2.
    const GENERIC_VERIFY_V2: Guid = Guid {
        data1: 0x00AA_C56B,
        data2: 0xCD44,
        data3: 0x11D0,
        data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };

    #[repr(C)]
    struct WintrustFileInfo {
        size: u32,
        path: *const u16,
        file: Handle,
        known_subject: *const Guid,
    }

    #[repr(C)]
    struct WintrustData {
        size: u32,
        policy_data: *mut c_void,
        sip_data: *mut c_void,
        ui_choice: u32,
        revocation_checks: u32,
        union_choice: u32,
        file: *mut WintrustFileInfo,
        state_action: u32,
        state_data: Handle,
        url_reference: *const u16,
        prov_flags: u32,
        ui_context: u32,
        signature_settings: *mut c_void,
    }

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const c_void,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        fn ReadDirectoryChangesW(
            directory: Handle,
            buffer: *mut c_void,
            length: u32,
            subtree: i32,
            filter: u32,
            returned: *mut u32,
            overlapped: *mut c_void,
            completion: *const c_void,
        ) -> i32;
        fn CancelIoEx(file: Handle, overlapped: *const c_void) -> i32;
        fn CloseHandle(object: Handle) -> i32;
    }

    #[allow(non_snake_case)]
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PostMessageW(window: Handle, message: u32, wparam: usize, lparam: isize) -> i32;
    }

    #[allow(non_snake_case)]
    #[link(name = "wintrust")]
    unsafe extern "system" {
        fn WinVerifyTrust(window: Handle, action: *const Guid, data: *mut c_void) -> i32;
    }

    #[allow(non_snake_case)]
    #[link(name = "msi")]
    unsafe extern "system" {
        fn MsiOpenPackageExW(path: *const u16, options: u32, package: *mut u32) -> u32;
        fn MsiGetPropertyW(package: u32, name: *const u16, value: *mut u16, size: *mut u32) -> u32;
        fn MsiCloseHandle(handle: u32) -> u32;
        fn MsiGetProductInfoW(
            product: *const u16,
            property: *const u16,
            value: *mut u16,
            size: *mut u32,
        ) -> u32;
        fn MsiEnumRelatedProductsW(
            upgrade_code: *const u16,
            reserved: u32,
            index: u32,
            product: *mut u16,
        ) -> u32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn post_message(window: isize, message: u32) {
        if window != 0 {
            // SAFETY: PostMessageW tolerates a stale handle (it just fails).
            unsafe { PostMessageW(window as Handle, message, 0, 0) };
        }
    }

    // ---- directory changes ----

    /// Opens `dir` for change notifications; the handle as an integer.
    pub fn open_directory(dir: &Path) -> Option<isize> {
        let name = wide_path(dir);
        // SAFETY: a NUL-terminated name; the handle is closed by `close_directory`.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY,
                SHARE_ALL,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
        let key = handle as isize;
        (key != 0 && key != -1).then_some(key)
    }

    pub fn close_directory(handle: isize) {
        // SAFETY: a handle from `open_directory`, closed once by the watcher thread.
        unsafe { CloseHandle(handle as Handle) };
    }

    /// Ends a `next_changes` wait from another thread.
    pub fn cancel_watch(handle: isize) {
        // SAFETY: cancels pending I/O on a live directory handle; a stale one just fails.
        unsafe { CancelIoEx(handle as Handle, std::ptr::null()) };
    }

    /// Blocks until something in the folder (not its subfolders) was added, written or
    /// renamed to; the names, relative to the folder. `None` when the watch ended or failed.
    pub fn next_changes(handle: isize) -> Option<Vec<String>> {
        const ADDED: u32 = 1;
        const MODIFIED: u32 = 3;
        const RENAMED_NEW: u32 = 5;
        let mut buffer = vec![0u32; 16 * 1024];
        let mut returned = 0u32;
        // SAFETY: the buffer is DWORD aligned and `length` is its size in bytes; synchronous
        // call (no overlapped, no completion routine).
        let ok = unsafe {
            ReadDirectoryChangesW(
                handle as Handle,
                buffer.as_mut_ptr().cast::<c_void>(),
                (buffer.len() * 4) as u32,
                0,
                CHANGE_FILTER,
                &mut returned,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return None;
        }
        let used = (returned as usize).min(buffer.len() * 4);
        // SAFETY: `used` bytes of the buffer were just written by the call.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), used) };
        let word = |at: usize| -> Option<u32> {
            let chunk = bytes.get(at..at + 4)?;
            Some(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        };
        let mut names = Vec::new();
        let mut at = 0usize;
        while let (Some(next), Some(action), Some(length)) = (word(at), word(at + 4), word(at + 8))
        {
            let from = at + 12;
            let Some(raw) = bytes.get(from..from + length as usize) else {
                break;
            };
            if matches!(action, ADDED | MODIFIED | RENAMED_NEW) {
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect();
                names.push(OsString::from_wide(&units).to_string_lossy().into_owned());
            }
            if next == 0 {
                break;
            }
            at += next as usize;
        }
        Some(names)
    }

    // ---- Authenticode ----

    /// The file's signature is trusted (`WinVerifyTrust`, generic verify, no UI, no network
    /// revocation lookup).
    pub fn verify_signature(path: &Path) -> bool {
        let name = wide_path(path);
        let mut file = WintrustFileInfo {
            size: size_of::<WintrustFileInfo>() as u32,
            path: name.as_ptr(),
            file: std::ptr::null_mut(),
            known_subject: std::ptr::null(),
        };
        let mut data = WintrustData {
            size: size_of::<WintrustData>() as u32,
            policy_data: std::ptr::null_mut(),
            sip_data: std::ptr::null_mut(),
            ui_choice: WTD_UI_NONE,
            revocation_checks: 0,
            union_choice: WTD_CHOICE_FILE,
            file: &mut file,
            state_action: WTD_STATEACTION_VERIFY,
            state_data: std::ptr::null_mut(),
            url_reference: std::ptr::null(),
            prov_flags: WTD_REVOCATION_CHECK_NONE,
            ui_context: 0,
            signature_settings: std::ptr::null_mut(),
        };
        let invalid = usize::MAX as Handle;
        // SAFETY: `data` and `file` outlive both calls and have the documented layouts; the
        // second call releases the state the first one kept.
        let status = unsafe {
            WinVerifyTrust(
                invalid,
                &GENERIC_VERIFY_V2,
                (&raw mut data).cast::<c_void>(),
            )
        };
        data.state_action = WTD_STATEACTION_CLOSE;
        // SAFETY: see above.
        unsafe {
            WinVerifyTrust(
                invalid,
                &GENERIC_VERIFY_V2,
                (&raw mut data).cast::<c_void>(),
            );
        }
        status == 0
    }

    // ---- Windows Installer ----

    pub struct MsiPackage {
        pub product_code: String,
        pub upgrade_code: String,
        pub name: String,
        pub version: String,
    }

    fn from_buffer(buffer: &[u16], size: u32) -> Option<String> {
        let length = (size as usize).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..length]);
        (!text.is_empty()).then_some(text)
    }

    fn package_property(package: u32, name: &str) -> Option<String> {
        let key = wide(name);
        let mut buffer = vec![0u16; 512];
        let mut size = buffer.len() as u32;
        // SAFETY: NUL-terminated name; `size` is the buffer's length in characters.
        let result =
            unsafe { MsiGetPropertyW(package, key.as_ptr(), buffer.as_mut_ptr(), &mut size) };
        if result == ERROR_SUCCESS {
            from_buffer(&buffer, size)
        } else {
            None
        }
    }

    /// Reads the product code, upgrade code, name and version from an MSI file without
    /// installing it. `None` when it is not a readable package or has no product code.
    pub fn msi_package(path: &Path) -> Option<MsiPackage> {
        let name = wide_path(path);
        let mut package = 0u32;
        // SAFETY: NUL-terminated path; the handle is closed below.
        let result = unsafe {
            MsiOpenPackageExW(
                name.as_ptr(),
                OPEN_PACKAGE_IGNORE_MACHINE_STATE,
                &mut package,
            )
        };
        if result != ERROR_SUCCESS {
            return None;
        }
        let read = MsiPackage {
            product_code: package_property(package, "ProductCode").unwrap_or_default(),
            upgrade_code: package_property(package, "UpgradeCode").unwrap_or_default(),
            name: package_property(package, "ProductName").unwrap_or_default(),
            version: package_property(package, "ProductVersion").unwrap_or_default(),
        };
        // SAFETY: a handle from MsiOpenPackageExW, closed once.
        unsafe { MsiCloseHandle(package) };
        is_product_code(&read.product_code).then_some(read)
    }

    /// A property of an installed product ("VersionString", "InstallLocation"); `None` when
    /// the product is not installed or has no such value.
    pub fn msi_product_info(product_code: &str, property: &str) -> Option<String> {
        if !is_product_code(product_code) {
            return None;
        }
        let code = wide(product_code);
        let key = wide(property);
        let mut buffer = vec![0u16; 512];
        let mut size = buffer.len() as u32;
        // SAFETY: NUL-terminated strings; `size` is the buffer's length in characters.
        let result = unsafe {
            MsiGetProductInfoW(code.as_ptr(), key.as_ptr(), buffer.as_mut_ptr(), &mut size)
        };
        if result == ERROR_SUCCESS {
            from_buffer(&buffer, size)
        } else {
            None
        }
    }

    /// The first installed product that shares `upgrade_code`.
    pub fn msi_related_product(upgrade_code: &str) -> Option<String> {
        if !is_product_code(upgrade_code) {
            return None;
        }
        let code = wide(upgrade_code);
        let mut buffer = [0u16; 39];
        // SAFETY: NUL-terminated code; the product buffer holds the documented 39 characters.
        let result = unsafe { MsiEnumRelatedProductsW(code.as_ptr(), 0, 0, buffer.as_mut_ptr()) };
        if result == ERROR_SUCCESS {
            from_buffer(&buffer, 38)
        } else {
            None
        }
    }

    /// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`: the only shape handed to msiexec or Windows
    /// Installer.
    pub fn is_product_code(text: &str) -> bool {
        let bytes = text.as_bytes();
        bytes.len() == 38
            && bytes[0] == b'{'
            && bytes[37] == b'}'
            && bytes[1..37].iter().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }
}
