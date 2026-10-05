//! Pill settings: bounded hand parser/encoder for exactly the shared schema, plus the
//! on-disk store (`%LOCALAPPDATA%\Cockpit\pill-settings.json`).
//!
//! Schema (shared with the Mac pill), schema_version 1:
//! `{"schema_version":1,"visible":true,"cadence_seconds":2,
//!   "monitors":{"<monitor-key>":{"enabled":true,"anchor":"top-right"}}}`
//!
//! Policy: unknown fields are ignored; an unknown version, malformed or oversized file yields
//! defaults and the caller must not overwrite that file (`LoadOutcome::writable == false`).
//! No serde: the JSON reader below is a small bounded recursive-descent parser.

use crate::runtime::UserSecurity;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use windows::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};
use windows::core::PCWSTR;

pub const MAX_FILE_BYTES: usize = 64 * 1024;
pub const SCHEMA_VERSION: i64 = 1;
pub const CADENCE_MIN: u32 = 2;
pub const CADENCE_MAX: u32 = 10;
pub const CADENCE_DEFAULT: u32 = 2;
pub const MAX_MONITORS: usize = 64;
pub const MAX_KEY_BYTES: usize = 256;
const MAX_DEPTH: usize = 8;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const DIR_NAME: &str = "Cockpit";
const FILE_NAME: &str = "pill-settings.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Anchor {
    pub fn as_str(self) -> &'static str {
        match self {
            Anchor::TopLeft => "top-left",
            Anchor::TopRight => "top-right",
            Anchor::BottomLeft => "bottom-left",
            Anchor::BottomRight => "bottom-right",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top-left" => Some(Anchor::TopLeft),
            "top-right" => Some(Anchor::TopRight),
            "bottom-left" => Some(Anchor::BottomLeft),
            "bottom-right" => Some(Anchor::BottomRight),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MonitorSetting {
    pub enabled: bool,
    pub anchor: Anchor,
}

impl MonitorSetting {
    pub const DEFAULT: Self = Self {
        enabled: true,
        anchor: Anchor::TopRight,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PillSettings {
    pub visible: bool,
    pub cadence_seconds: u32,
    pub monitors: BTreeMap<String, MonitorSetting>,
}

impl PillSettings {
    pub const fn new() -> Self {
        Self {
            visible: true,
            cadence_seconds: CADENCE_DEFAULT,
            monitors: BTreeMap::new(),
        }
    }
    /// Setting for a monitor; unknown monitors get the defaults (enabled, top-right).
    pub fn monitor(&self, key: &str) -> MonitorSetting {
        self.monitors
            .get(key)
            .copied()
            .unwrap_or(MonitorSetting::DEFAULT)
    }
    // Mutators back the future settings-request channel (the pill is the sole writer); they
    // have no in-process caller yet but are exercised by tests.
    #[allow(dead_code)]
    pub fn set_visible(&mut self, visible: bool) -> bool {
        let changed = self.visible != visible;
        self.visible = visible;
        changed
    }
    #[allow(dead_code)]
    pub fn set_cadence(&mut self, seconds: i64) -> bool {
        let value = clamp_cadence(seconds);
        let changed = self.cadence_seconds != value;
        self.cadence_seconds = value;
        changed
    }
    #[allow(dead_code)]
    pub fn set_monitor(&mut self, key: &str, setting: MonitorSetting) -> bool {
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return false;
        }
        if !self.monitors.contains_key(key) && self.monitors.len() >= MAX_MONITORS {
            return false;
        }
        self.monitors.insert(key.to_string(), setting) != Some(setting)
    }
}

impl Default for PillSettings {
    fn default() -> Self {
        Self::new()
    }
}

pub fn clamp_cadence(seconds: i64) -> u32 {
    seconds.clamp(CADENCE_MIN as i64, CADENCE_MAX as i64) as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Oversized,
    Malformed,
    UnknownVersion,
}

impl ParseError {
    pub fn as_str(self) -> &'static str {
        match self {
            ParseError::Oversized => "oversized",
            ParseError::Malformed => "malformed",
            ParseError::UnknownVersion => "unknown_version",
        }
    }
}

// ------------------------------------------------------------------ JSON reader

enum Json {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    Array,
    Object(Vec<(String, Json)>),
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(ParseError::Malformed)
        }
    }
    fn literal(&mut self, word: &[u8]) -> Result<(), ParseError> {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(ParseError::Malformed)
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, ParseError> {
        if depth > MAX_DEPTH {
            return Err(ParseError::Malformed);
        }
        self.skip_ws();
        match self.peek().ok_or(ParseError::Malformed)? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => Ok(Json::Text(self.string()?)),
            b't' => self.literal(b"true").map(|_| Json::Bool(true)),
            b'f' => self.literal(b"false").map(|_| Json::Bool(false)),
            b'n' => self.literal(b"null").map(|_| Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(ParseError::Malformed),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.expect(b'{')?;
        let mut members: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            if members.iter().any(|(existing, _)| *existing == key) {
                return Err(ParseError::Malformed); // duplicate keys are ambiguous
            }
            self.skip_ws();
            self.expect(b':')?;
            let value = self.value(depth + 1)?;
            members.push((key, value));
            if members.len() > 4 * MAX_MONITORS {
                return Err(ParseError::Malformed);
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Json::Object(members));
                }
                _ => return Err(ParseError::Malformed),
            }
        }
    }

    /// Arrays only need to be skipped (unknown fields); elements are validated, not kept.
    fn array(&mut self, depth: usize) -> Result<Json, ParseError> {
        self.expect(b'[')?;
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array);
        }
        loop {
            self.value(depth + 1)?;
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Json::Array);
                }
                _ => return Err(ParseError::Malformed),
            }
        }
    }

    fn number(&mut self) -> Result<Json, ParseError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(ParseError::Malformed),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(ParseError::Malformed);
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(ParseError::Malformed);
            }
            self.digits();
        }
        // The slice is ASCII digits/sign/dot/exponent only.
        let text =
            std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|_| ParseError::Malformed)?;
        Ok(Json::Number(text.to_string()))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + 4)
            .ok_or(ParseError::Malformed)?;
        let mut value = 0u32;
        for byte in slice {
            let digit = (*byte as char).to_digit(16).ok_or(ParseError::Malformed)?;
            value = value * 16 + digit;
        }
        self.pos += 4;
        Ok(value)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self.peek().ok_or(ParseError::Malformed)?;
            self.pos += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = self.peek().ok_or(ParseError::Malformed)?;
                    self.pos += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&first) {
                                self.expect(b'\\')?;
                                self.expect(b'u')?;
                                let second = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&second) {
                                    return Err(ParseError::Malformed);
                                }
                                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                            } else {
                                first
                            };
                            char::from_u32(code).ok_or(ParseError::Malformed)?
                        }
                        _ => return Err(ParseError::Malformed),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0x00..=0x1f => return Err(ParseError::Malformed),
                other => out.push(other),
            }
            if out.len() > MAX_FILE_BYTES {
                return Err(ParseError::Malformed);
            }
        }
        String::from_utf8(out).map_err(|_| ParseError::Malformed)
    }
}

fn member<'a>(members: &'a [(String, Json)], name: &str) -> Option<&'a Json> {
    members.iter().find(|(key, _)| key == name).map(|(_, v)| v)
}

fn integer(value: &Json) -> Result<i64, ParseError> {
    match value {
        Json::Number(text) => text.parse::<i64>().map_err(|_| ParseError::Malformed),
        _ => Err(ParseError::Malformed),
    }
}

/// Parse the settings file bytes. Never panics; any failure maps to a `ParseError`.
pub fn parse_settings(bytes: &[u8]) -> Result<PillSettings, ParseError> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ParseError::Oversized);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut reader = Reader { bytes, pos: 0 };
    let root = reader.value(0)?;
    reader.skip_ws();
    if reader.pos != bytes.len() {
        return Err(ParseError::Malformed);
    }
    let Json::Object(root) = root else {
        return Err(ParseError::Malformed);
    };
    let version = integer(member(&root, "schema_version").ok_or(ParseError::Malformed)?)?;
    if version != SCHEMA_VERSION {
        return Err(ParseError::UnknownVersion);
    }
    let mut settings = PillSettings::new();
    match member(&root, "visible") {
        None | Some(Json::Null) => {}
        Some(Json::Bool(value)) => settings.visible = *value,
        Some(_) => return Err(ParseError::Malformed),
    }
    match member(&root, "cadence_seconds") {
        None | Some(Json::Null) => {}
        Some(value) => {
            let cadence = integer(value)?;
            if !(CADENCE_MIN as i64..=CADENCE_MAX as i64).contains(&cadence) {
                return Err(ParseError::Malformed);
            }
            settings.cadence_seconds = cadence as u32;
        }
    }
    match member(&root, "monitors") {
        None | Some(Json::Null) => {}
        Some(Json::Object(entries)) => {
            if entries.len() > MAX_MONITORS {
                return Err(ParseError::Malformed);
            }
            for (key, entry) in entries {
                if key.is_empty() || key.len() > MAX_KEY_BYTES {
                    return Err(ParseError::Malformed);
                }
                let Json::Object(fields) = entry else {
                    return Err(ParseError::Malformed);
                };
                let mut setting = MonitorSetting::DEFAULT;
                match member(fields, "enabled") {
                    None | Some(Json::Null) => {}
                    Some(Json::Bool(value)) => setting.enabled = *value,
                    Some(_) => return Err(ParseError::Malformed),
                }
                match member(fields, "anchor") {
                    None | Some(Json::Null) => {}
                    Some(Json::Text(text)) => {
                        setting.anchor = Anchor::parse(text).ok_or(ParseError::Malformed)?;
                    }
                    Some(_) => return Err(ParseError::Malformed),
                }
                settings.monitors.insert(key.clone(), setting);
            }
        }
        Some(_) => return Err(ParseError::Malformed),
    }
    Ok(settings)
}

fn push_json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Encode to the schema. Fails (never truncates) if the result would exceed the size cap.
pub fn encode_settings(settings: &PillSettings) -> Result<String, ParseError> {
    let mut out = format!(
        "{{\"schema_version\":{SCHEMA_VERSION},\"visible\":{},\"cadence_seconds\":{},\"monitors\":{{",
        settings.visible,
        clamp_cadence(settings.cadence_seconds as i64)
    );
    for (index, (key, setting)) in settings.monitors.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_json_string(&mut out, key);
        out.push_str(&format!(
            ":{{\"enabled\":{},\"anchor\":\"{}\"}}",
            setting.enabled,
            setting.anchor.as_str()
        ));
    }
    out.push_str("}}\n");
    if out.len() > MAX_FILE_BYTES || settings.monitors.len() > MAX_MONITORS {
        return Err(ParseError::Oversized);
    }
    Ok(out)
}

// ------------------------------------------------------------------ store

#[derive(Debug)]
pub enum StoreError {
    NoBaseDirectory,
    ReparsePoint(&'static str),
    NotRegular(&'static str),
    /// The existing directory/file (or the caller's identity) failed the ownership /
    /// DACL trust check; carries a stable `what:reason` detail. Refused, never repaired.
    Untrusted(String),
    Encode(ParseError),
    Io(&'static str, std::io::Error),
}

impl StoreError {
    pub fn describe(&self) -> String {
        match self {
            StoreError::NoBaseDirectory => "no_base_directory".into(),
            StoreError::ReparsePoint(what) => format!("reparse_point:{what}"),
            StoreError::NotRegular(what) => format!("not_regular:{what}"),
            StoreError::Untrusted(detail) => format!("untrusted:{detail}"),
            StoreError::Encode(e) => format!("encode:{}", e.as_str()),
            StoreError::Io(op, e) => format!("io:{op}:{e}"),
        }
    }
}

pub struct SettingsPaths {
    pub dir: PathBuf,
    pub file: PathBuf,
}

/// `%LOCALAPPDATA%\Cockpit\pill-settings.json`; requires an absolute LOCALAPPDATA.
pub fn settings_paths() -> Result<SettingsPaths, StoreError> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or(StoreError::NoBaseDirectory)?;
    let base = PathBuf::from(base);
    if !base.is_absolute() {
        return Err(StoreError::NoBaseDirectory);
    }
    Ok(paths_under(&base))
}

pub fn paths_under(base: &Path) -> SettingsPaths {
    let dir = base.join(DIR_NAME);
    let file = dir.join(FILE_NAME);
    SettingsPaths { dir, file }
}

pub fn is_reparse_attributes(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[derive(Debug)]
pub struct LoadOutcome {
    pub settings: PillSettings,
    /// False when the on-disk file was unusable: it must not be overwritten.
    pub writable: bool,
    /// Structured event reason when defaults were used for a reason other than "no file".
    pub problem: Option<String>,
    pub file_found: bool,
}

fn defaults(writable: bool, problem: Option<String>, file_found: bool) -> LoadOutcome {
    LoadOutcome {
        settings: PillSettings::new(),
        writable,
        problem,
        file_found,
    }
}

/// Resolve the current user's token identity for trust checks. Failure means we can
/// prove nothing about on-disk ownership: nothing is read and nothing is writable.
fn identity() -> Result<UserSecurity, String> {
    UserSecurity::current().map_err(|e| format!("identity:0x{:08X}", e.code().0 as u32))
}

/// Verify `path` passes the ownership/DACL trust check; maps failures to the
/// `what_untrusted:<reason>` problem tag used by `LoadOutcome::problem`.
fn require_trusted(path: &Path, ctx: &UserSecurity, what: &str) -> Option<String> {
    crate::runtime::verify_restricted(path, ctx)
        .err()
        .map(|t| format!("{what}_untrusted:{}", t.describe()))
}

/// Load from `paths`. Missing directory/file is a normal first run (writable defaults).
/// An existing directory or file that fails the ownership/DACL trust check is refused
/// (defaults, `writable == false`): it is neither read nor ever overwritten, so a
/// foreign or world-writable settings file is preserved byte-for-byte.
pub fn load(paths: &SettingsPaths) -> LoadOutcome {
    let ctx = match fs::symlink_metadata(&paths.dir) {
        Ok(meta) => {
            if is_reparse_attributes(meta.file_attributes()) || !meta.is_dir() {
                return defaults(false, Some("directory_reparse_or_not_dir".into()), false);
            }
            match identity() {
                Ok(ctx) => ctx,
                Err(problem) => return defaults(false, Some(problem), false),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return defaults(true, None, false),
        Err(e) => return defaults(false, Some(format!("directory_stat:{e}")), false),
    };
    if let Some(problem) = require_trusted(&paths.dir, &ctx, "directory") {
        return defaults(false, Some(problem), false);
    }
    let meta = match fs::symlink_metadata(&paths.file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return defaults(true, None, false),
        Err(e) => return defaults(false, Some(format!("file_stat:{e}")), false),
    };
    if is_reparse_attributes(meta.file_attributes()) || !meta.is_file() {
        return defaults(false, Some("file_reparse_or_not_regular".into()), true);
    }
    if let Some(problem) = require_trusted(&paths.file, &ctx, "file") {
        return defaults(false, Some(problem), true);
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return defaults(false, Some(ParseError::Oversized.as_str().into()), true);
    }
    let mut bytes = Vec::new();
    let read = fs::File::open(&paths.file)
        .and_then(|file| file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(e) = read {
        return defaults(false, Some(format!("read:{e}")), true);
    }
    match parse_settings(&bytes) {
        Ok(settings) => LoadOutcome {
            settings,
            writable: true,
            problem: None,
            file_found: true,
        },
        Err(e) => defaults(false, Some(e.as_str().into()), true),
    }
}

pub(crate) fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Atomically publish `settings`. Trust policy (mirrors `load`): the existing Cockpit
/// directory or settings file must pass the ownership/DACL check — anything owned by
/// another user or more broadly ACL'd is refused, never rewritten. Anything we create
/// (directory, temp file, and therefore the published file, which keeps the temp file's
/// descriptor through the rename) gets an explicit restrictive SECURITY_DESCRIPTOR
/// (current user + SYSTEM only); broad ACLs are never silently inherited.
/// Reparse-point directory/file are refused; only the settings file is replaced.
pub fn save(paths: &SettingsPaths, settings: &PillSettings) -> Result<(), StoreError> {
    let text = encode_settings(settings).map_err(StoreError::Encode)?;
    let ctx = UserSecurity::current()
        .map_err(|e| StoreError::Untrusted(format!("identity:0x{:08X}", e.code().0 as u32)))?;
    let descriptor = ctx.restrictive_descriptor().map_err(|e| {
        StoreError::Untrusted(format!("security_descriptor:0x{:08X}", e.code().0 as u32))
    })?;
    match fs::symlink_metadata(&paths.dir) {
        Ok(meta) => {
            if is_reparse_attributes(meta.file_attributes()) {
                return Err(StoreError::ReparsePoint("directory"));
            }
            if !meta.is_dir() {
                return Err(StoreError::NotRegular("directory"));
            }
            crate::runtime::verify_restricted(&paths.dir, &ctx)
                .map_err(|t| StoreError::Untrusted(format!("directory:{}", t.describe())))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match crate::runtime::create_dir_restricted(&paths.dir, &descriptor) {
                Ok(()) => {}
                // Possibly lost a race with another creator: apply the same checks an
                // already-present directory would get; if it is still missing, report
                // the original create failure.
                Err(e) => {
                    let create_err = std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF);
                    let meta = match fs::symlink_metadata(&paths.dir) {
                        Ok(meta) => meta,
                        Err(_) => return Err(StoreError::Io("create_dir", create_err)),
                    };
                    if is_reparse_attributes(meta.file_attributes()) {
                        return Err(StoreError::ReparsePoint("directory"));
                    }
                    if !meta.is_dir() {
                        return Err(StoreError::NotRegular("directory"));
                    }
                    crate::runtime::verify_restricted(&paths.dir, &ctx).map_err(|t| {
                        StoreError::Untrusted(format!("directory:{}", t.describe()))
                    })?;
                }
            }
        }
        Err(e) => return Err(StoreError::Io("stat_dir", e)),
    }
    match fs::symlink_metadata(&paths.file) {
        Ok(meta) => {
            if is_reparse_attributes(meta.file_attributes()) {
                return Err(StoreError::ReparsePoint("file"));
            }
            if !meta.is_file() {
                return Err(StoreError::NotRegular("file"));
            }
            crate::runtime::verify_restricted(&paths.file, &ctx)
                .map_err(|t| StoreError::Untrusted(format!("file:{}", t.describe())))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(StoreError::Io("stat_file", e)),
    }

    let (temp_path, mut temp) = create_temp(paths, &descriptor)?;
    let written = temp
        .write_all(text.as_bytes())
        .and_then(|_| temp.sync_all())
        .map_err(|e| StoreError::Io("write_temp", e));
    drop(temp);
    if let Err(e) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }
    let from = wide_path(&temp_path);
    let to = wide_path(&paths.file);
    // Both buffers are NUL-terminated and outlive the synchronous call.
    let moved = unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(error) = moved {
        let _ = fs::remove_file(&temp_path);
        return Err(StoreError::Io(
            "MoveFileExW",
            std::io::Error::from_raw_os_error(error.code().0 & 0xFFFF),
        ));
    }
    Ok(())
}

fn create_temp(
    paths: &SettingsPaths,
    descriptor: &crate::runtime::RestrictiveSecurity,
) -> Result<(PathBuf, fs::File), StoreError> {
    let pid = std::process::id();
    let mut last = None;
    for attempt in 0..8u32 {
        let path = paths.dir.join(format!("{FILE_NAME}.{pid}.{attempt}.tmp"));
        // CREATE_NEW with an explicit restrictive DACL: never inherits a broad ACL.
        match crate::runtime::create_file_restricted(&path, descriptor) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.code().0 & 0xFFFF == crate::runtime::winsec::ERROR_FILE_EXISTS as i32 => {
                last = Some(e)
            }
            Err(e) => {
                return Err(StoreError::Io(
                    "create_temp",
                    std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF),
                ));
            }
        }
    }
    Err(StoreError::Io(
        "create_temp",
        std::io::Error::from_raw_os_error(
            last.map(|e| e.code().0 & 0xFFFF)
                .unwrap_or(crate::runtime::winsec::ERROR_FILE_EXISTS as i32),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"schema_version":1,"visible":false,"cadence_seconds":5,
        "monitors":{"\\\\.\\DISPLAY1":{"enabled":false,"anchor":"bottom-left"}}}"#;

    #[test]
    fn parses_full_schema() {
        let s = parse_settings(FULL.as_bytes()).unwrap();
        assert!(!s.visible);
        assert_eq!(s.cadence_seconds, 5);
        let m = s.monitor("\\\\.\\DISPLAY1");
        assert!(!m.enabled);
        assert_eq!(m.anchor, Anchor::BottomLeft);
    }

    #[test]
    fn defaults_when_optional_fields_missing() {
        let s = parse_settings(br#"{"schema_version":1}"#).unwrap();
        assert_eq!(s, PillSettings::new());
        assert_eq!(s.monitor("x"), MonitorSetting::DEFAULT);
    }

    #[test]
    fn unknown_fields_are_ignored_at_every_level() {
        let s = parse_settings(
            br#"{"schema_version":1,"extra":[1,{"a":null},"s"],"cadence_seconds":3,
            "monitors":{"M":{"anchor":"top-left","future":{"x":1.5e3}}}}"#,
        )
        .unwrap();
        assert_eq!(s.cadence_seconds, 3);
        assert_eq!(s.monitor("M").anchor, Anchor::TopLeft);
        assert!(s.monitor("M").enabled);
    }

    #[test]
    fn cadence_mutators_are_clamped_but_file_values_are_strict() {
        assert_eq!(clamp_cadence(-5), 2);
        assert_eq!(clamp_cadence(0), 2);
        assert_eq!(clamp_cadence(2), 2);
        assert_eq!(clamp_cadence(7), 7);
        assert_eq!(clamp_cadence(10), 10);
        assert_eq!(clamp_cadence(11), 10);
        assert_eq!(clamp_cadence(i64::MAX), 10);
        assert_eq!(
            parse_settings(br#"{"schema_version":1,"cadence_seconds":99}"#),
            Err(ParseError::Malformed)
        );
        assert_eq!(
            parse_settings(br#"{"schema_version":1,"cadence_seconds":1}"#),
            Err(ParseError::Malformed)
        );
    }

    #[test]
    fn unknown_version_and_missing_version() {
        assert_eq!(
            parse_settings(br#"{"schema_version":2}"#),
            Err(ParseError::UnknownVersion)
        );
        assert_eq!(parse_settings(b"{}"), Err(ParseError::Malformed));
        assert_eq!(
            parse_settings(br#"{"schema_version":"1"}"#),
            Err(ParseError::Malformed)
        );
    }

    #[test]
    fn malformed_inputs_are_rejected() {
        let cases: &[&[u8]] = &[
            b"",
            b"[]",
            b"{",
            b"{\"schema_version\":1,}",
            b"{\"schema_version\":1} trailing",
            b"{\"schema_version\":1,\"schema_version\":1}",
            b"{\"schema_version\":1,\"visible\":1}",
            b"{\"schema_version\":1,\"cadence_seconds\":2.5}",
            b"{\"schema_version\":1,\"cadence_seconds\":\"2\"}",
            b"{\"schema_version\":1,\"monitors\":[]}",
            b"{\"schema_version\":1,\"monitors\":{\"M\":{\"anchor\":\"center\"}}}",
            b"{\"schema_version\":1,\"monitors\":{\"M\":{\"enabled\":\"yes\"}}}",
            b"{\"schema_version\":1,\"monitors\":{\"\":{}}}",
            b"{\"schema_version\":1,\"x\":\"bad\\q\"}",
            b"{\"schema_version\":1,\"x\":\"\x01\"}",
            b"{\"schema_version\":1,\"x\":\"\\ud800\"}",
            b"{\"schema_version\":1,\"x\":01}",
            b"\xff\xfe",
        ];
        for bad in cases {
            assert_eq!(parse_settings(bad), Err(ParseError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn depth_is_bounded() {
        let mut text = String::from("{\"schema_version\":1,\"x\":");
        text.push_str(&"[".repeat(200));
        text.push_str(&"]".repeat(200));
        text.push('}');
        assert_eq!(parse_settings(text.as_bytes()), Err(ParseError::Malformed));
    }

    #[test]
    fn oversized_input_is_rejected() {
        let mut text = String::from("{\"schema_version\":1,\"pad\":\"");
        text.push_str(&"a".repeat(MAX_FILE_BYTES));
        text.push_str("\"}");
        assert_eq!(parse_settings(text.as_bytes()), Err(ParseError::Oversized));
    }

    #[test]
    fn too_many_monitors_rejected() {
        let mut text = String::from("{\"schema_version\":1,\"monitors\":{");
        for i in 0..=MAX_MONITORS {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&format!("\"M{i}\":{{}}"));
        }
        text.push_str("}}");
        assert_eq!(parse_settings(text.as_bytes()), Err(ParseError::Malformed));
    }

    #[test]
    fn string_escapes_and_surrogates_decode() {
        let s = parse_settings(br#"{"schema_version":1,"monitors":{"a\u0041\ud83d\ude00\/":{}}}"#)
            .unwrap();
        assert!(s.monitors.contains_key("aA\u{1F600}/"));
    }

    #[test]
    fn bom_is_tolerated() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"schema_version":1}"#);
        assert!(parse_settings(&bytes).is_ok());
    }

    #[test]
    fn encode_roundtrips_and_is_exact() {
        let mut s = PillSettings::new();
        s.visible = false;
        s.cadence_seconds = 4;
        s.monitors.insert(
            "\\\\.\\DISPLAY2".into(),
            MonitorSetting {
                enabled: false,
                anchor: Anchor::BottomRight,
            },
        );
        let text = encode_settings(&s).unwrap();
        assert_eq!(
            text,
            "{\"schema_version\":1,\"visible\":false,\"cadence_seconds\":4,\"monitors\":{\"\\\\\\\\.\\\\DISPLAY2\":{\"enabled\":false,\"anchor\":\"bottom-right\"}}}\n"
        );
        assert_eq!(parse_settings(text.as_bytes()).unwrap(), s);
        assert_eq!(
            encode_settings(&PillSettings::new()).unwrap(),
            "{\"schema_version\":1,\"visible\":true,\"cadence_seconds\":2,\"monitors\":{}}\n"
        );
    }

    #[test]
    fn encode_escapes_control_characters_and_roundtrips() {
        let mut s = PillSettings::new();
        s.monitors
            .insert("a\"b\\c\n\u{1}".into(), MonitorSetting::DEFAULT);
        let text = encode_settings(&s).unwrap();
        assert_eq!(parse_settings(text.as_bytes()).unwrap(), s);
    }

    #[test]
    fn encode_clamps_cadence() {
        let mut s = PillSettings::new();
        s.cadence_seconds = 500;
        assert!(
            encode_settings(&s)
                .unwrap()
                .contains("\"cadence_seconds\":10")
        );
    }

    #[test]
    fn encode_refuses_oversize_instead_of_truncating() {
        let mut s = PillSettings::new();
        for i in 0..MAX_MONITORS {
            s.monitors.insert(
                format!("{i:02}{}", "\u{1}".repeat(MAX_KEY_BYTES - 2)),
                MonitorSetting::DEFAULT,
            );
        }
        assert_eq!(encode_settings(&s), Err(ParseError::Oversized));
    }

    #[test]
    fn mutators_report_change_and_enforce_bounds() {
        let mut s = PillSettings::new();
        assert!(!s.set_visible(true));
        assert!(s.set_visible(false));
        assert!(!s.set_cadence(2));
        assert!(s.set_cadence(100));
        assert_eq!(s.cadence_seconds, 10);
        let custom = MonitorSetting {
            enabled: true,
            anchor: Anchor::TopLeft,
        };
        assert!(s.set_monitor("M", custom));
        assert!(!s.set_monitor("M", custom));
        assert!(!s.set_monitor("", custom));
        assert!(!s.set_monitor(&"k".repeat(MAX_KEY_BYTES + 1), custom));
    }

    #[test]
    fn anchor_names_roundtrip() {
        for a in [
            Anchor::TopLeft,
            Anchor::TopRight,
            Anchor::BottomLeft,
            Anchor::BottomRight,
        ] {
            assert_eq!(Anchor::parse(a.as_str()), Some(a));
        }
        assert_eq!(Anchor::parse("Top-Right"), None);
    }

    #[test]
    fn reparse_attribute_detection_and_paths() {
        assert!(is_reparse_attributes(0x400 | 0x10));
        assert!(!is_reparse_attributes(0x10));
        let p = paths_under(Path::new("C:\\Users\\u\\AppData\\Local"));
        assert!(p.file.ends_with("Cockpit\\pill-settings.json"));
        assert_eq!(p.file.parent().unwrap(), p.dir);
    }

    #[test]
    fn load_missing_is_writable_default() {
        let paths = paths_under(Path::new("C:\\cockpit-test-nonexistent-base-dir"));
        let out = load(&paths);
        assert!(out.writable && out.problem.is_none() && !out.file_found);
        assert_eq!(out.settings, PillSettings::new());
    }

    // The tests below exercise the real ownership/DACL checks; they run in hosted
    // Windows CI, create fixtures under %TEMP%, and need no elevation.

    fn live_paths(tag: &str) -> (PathBuf, SettingsPaths) {
        let base =
            std::env::temp_dir().join(format!("cockpit-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        (base.clone(), paths_under(&base))
    }

    #[test]
    fn live_save_then_load_roundtrips_in_restricted_tree() {
        let ctx = UserSecurity::current().unwrap();
        let sd = ctx.restrictive_descriptor().unwrap();
        let (base, paths) = live_paths("roundtrip");
        crate::runtime::create_dir_restricted(&paths.dir, &sd).unwrap();
        let mut s = PillSettings::new();
        s.set_monitor(
            "M",
            MonitorSetting {
                enabled: false,
                anchor: Anchor::BottomLeft,
            },
        );
        save(&paths, &s).unwrap();
        let out = load(&paths);
        assert_eq!(out.settings, s);
        assert!(out.writable && out.problem.is_none() && out.file_found);
        // The published file carries the temp file's restrictive descriptor.
        crate::runtime::verify_restricted(&paths.file, &ctx).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn live_malformed_file_is_never_truncated() {
        let ctx = UserSecurity::current().unwrap();
        let sd = ctx.restrictive_descriptor().unwrap();
        let (base, paths) = live_paths("malformed");
        crate::runtime::create_dir_restricted(&paths.dir, &sd).unwrap();
        let garbage = b"{not json\x00\xff trailing".to_vec();
        let mut file = crate::runtime::create_file_restricted(&paths.file, &sd).unwrap();
        file.write_all(&garbage).unwrap();
        drop(file);
        let out = load(&paths);
        assert!(out.file_found && !out.writable && out.problem.is_some());
        assert_eq!(
            fs::read(&paths.file).unwrap(),
            garbage,
            "a failed load must preserve the file byte-for-byte"
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn live_broad_file_acl_is_refused_and_preserved() {
        let ctx = UserSecurity::current().unwrap();
        let sd = ctx.restrictive_descriptor().unwrap();
        let (base, paths) = live_paths("broad");
        crate::runtime::create_dir_restricted(&paths.dir, &sd).unwrap();
        // File owned by us but DACL also grants Everyone: refused, never rewritten.
        let world = crate::runtime::RestrictiveSecurity::from_sddl(&format!(
            "D:P(A;;GA;;;{})(A;;GA;;;WD)",
            ctx.sid_string()
        ))
        .unwrap();
        let payload = br#"{"schema_version":1,"visible":false}"#.to_vec();
        let mut file = crate::runtime::create_file_restricted(&paths.file, &world).unwrap();
        file.write_all(&payload).unwrap();
        drop(file);
        let out = load(&paths);
        assert!(!out.writable && out.file_found);
        assert_eq!(
            out.problem.as_deref(),
            Some("file_untrusted:broad_dacl"),
            "{:?}",
            out.problem
        );
        let err = save(&paths, &PillSettings::new()).unwrap_err();
        assert!(
            matches!(err, StoreError::Untrusted(_)),
            "{}",
            err.describe()
        );
        assert_eq!(
            fs::read(&paths.file).unwrap(),
            payload,
            "refused objects are preserved byte-for-byte"
        );
        let _ = fs::remove_dir_all(&base);
    }
}
