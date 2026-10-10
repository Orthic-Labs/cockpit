//! Alert sounds and system toasts (the Mac's `SessionChime` and `ChannelNotifications`).
//!
//! Sounds are the stock files in `%SystemRoot%\Media` played with `PlaySoundW` (asynchronous,
//! so nothing waits), falling back to the system "asterisk" beep when a file is missing.
//! A toast goes through the WinRT `ToastNotificationManager` from a hidden PowerShell: the
//! toast API only shows a notification for an application id (AUMID) that has a Start-menu
//! shortcut carrying it, and Pulse installs none, so the toast is raised under Windows
//! PowerShell's own registered AUMID and reads as coming from "Windows PowerShell". Giving
//! it Pulse's name needs the installer to create a Start-menu shortcut with
//! `System.AppUserModel.ID` set (then `TOAST_AUMID` below becomes that id). If PowerShell or
//! the toast fails the alert has already played its sound and is simply not shown as a toast.

use crate::diag;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

/// Windows PowerShell's registered AUMID: the only one guaranteed to exist on every PC.
const TOAST_AUMID: &str =
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const SND_ASYNC: u32 = 0x0001;
const SND_NODEFAULT: u32 = 0x0002;
const SND_FILENAME: u32 = 0x0002_0000;

#[link(name = "winmm")]
unsafe extern "system" {
    fn PlaySoundW(name: *const u16, module: *mut c_void, flags: u32) -> i32;
}

/// The three tones, as the Mac's finished / info / blocked sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// A turn ended or a limit refreshed: short and unremarkable.
    Finished,
    /// A usage threshold or reset.
    Notice,
    /// Something wants attention: a limit is spent, a drive is nearly full or failing.
    Attention,
}

impl Sound {
    fn file(self) -> &'static str {
        match self {
            Sound::Finished => "Windows Notify System Generic.wav",
            Sound::Notice => "Windows Notify Messaging.wav",
            Sound::Attention => "Windows Exclamation.wav",
        }
    }
}

/// Plays the sound without waiting for it. False when neither the file nor the fallback beep
/// started.
pub fn play(sound: Sound) -> bool {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let path = std::path::Path::new(&root).join("Media").join(sound.file());
    if path.is_file() {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call; with SND_FILENAME the
        // module argument is unused.
        let started = unsafe {
            PlaySoundW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
            )
        };
        if started != 0 {
            return true;
        }
    }
    // winuser's MessageBeep; declared here because the windows crate's module layout for
    // it differs between generations.
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBeep(utype: u32) -> i32;
    }
    const MB_ICONASTERISK: u32 = 0x40;
    // SAFETY: MessageBeep has no preconditions.
    let beeped = unsafe { MessageBeep(MB_ICONASTERISK) } != 0;
    if !beeped {
        diag::info("alert_sound_failed", &[("sound", sound.file())]);
    }
    beeped
}

/// A system toast with `title` and `body`. Returns at once; the hidden PowerShell runs on its
/// own thread. The text travels in environment variables, never in the script, so it cannot
/// change the script.
pub fn toast(title: &str, body: &str) {
    const SCRIPT: &str = "$ErrorActionPreference='Stop'; \
        [void][Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]; \
        [void][Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime]; \
        $t=[Security.SecurityElement]::Escape($env:PULSE_TOAST_TITLE); \
        $b=[Security.SecurityElement]::Escape($env:PULSE_TOAST_BODY); \
        $x=New-Object Windows.Data.Xml.Dom.XmlDocument; \
        $x.LoadXml(\"<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual><audio silent='true'/></toast>\"); \
        [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($env:PULSE_TOAST_AUMID).Show([Windows.UI.Notifications.ToastNotification]::new($x))";
    let (title, body) = (title.to_string(), body.to_string());
    let _ = std::thread::Builder::new()
        .name("pulse-toast".into())
        .spawn(move || {
            let status = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
                .env("PULSE_TOAST_TITLE", title)
                .env("PULSE_TOAST_BODY", body)
                .env("PULSE_TOAST_AUMID", TOAST_AUMID)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();
            if !matches!(status, Ok(s) if s.success()) {
                diag::info("toast_failed", &[("aumid", TOAST_AUMID)]);
            }
        });
}
