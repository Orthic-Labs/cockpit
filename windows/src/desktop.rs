//! Claude Desktop on Windows, read-only: whether it is running, which account it is signed
//! into, which organizations Desktop filed under that account, and the usage response it
//! last cached for them. Nothing here writes, refreshes or creates anything, and no token,
//! cookie or credential is read: `config.json` is only searched for the one non-secret
//! `lastKnownAccountUuid` member and its sign-in material is never parsed.
//!
//! Where it lives (`%APPDATA%\Claude`):
//! - `lockfile`: Chromium's process lock, held open for as long as Desktop runs;
//! - `config.json` `lastKnownAccountUuid`: the signed-in account;
//! - `claude-code-sessions\<account>\<org>` and `local-agent-mode-sessions\<account>\<org>`:
//!   the organizations of an account (folder names only);
//! - `Cache\Cache_Data` (`data_0` .. `data_N`, `f_XXXXXX`, `index`): Chromium's HTTP cache.
//!   On Windows that is the older block-file backend, not the Simple Cache the Mac reads.
//!   `data_1` holds 256-byte `EntryStore` records (state, key, three stream sizes and block
//!   addresses); stream 0 is the response header block (`date`, `content-encoding`) and
//!   stream 1 the zstd-encoded body of `GET /api/organizations/<org>/usage[?...]`, the same
//!   JSON the OAuth usage endpoint returns. The files are held open by Desktop and read
//!   through shared handles, never written.

use crate::json;
use crate::usage::{self, LimitWindow};
use crate::zstd;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `ERROR_SHARING_VIOLATION`: another process holds the file with an incompatible share mode.
const SHARING_VIOLATION: i32 = 32;
const CONFIG_MAX_BYTES: u64 = 512 * 1024;
/// `kBlockMagic` of Chromium's block files, and the size of their header.
const BLOCK_MAGIC: u32 = 0xC104_CAC3;
const BLOCK_HEADER_BYTES: usize = 8192;
/// `EntryStore` is exactly one 256-byte block: the key starts at byte 96 and at most 160
/// key bytes live inside it (a longer key sits in its own stream and is skipped).
const ENTRY_BYTES: usize = 256;
const ENTRY_KEY_OFFSET: usize = 96;
const MAX_INLINE_KEY: usize = 160;
const SCAN_CHUNK_ENTRIES: usize = 1024;
const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;
/// How many chained 256-byte block files (`next_file`) one scan follows.
const MAX_CHAIN: usize = 8;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;
const MAX_JSON_BYTES: usize = 256 * 1024;
/// A cached reading older than this is not shown as current (as on the Mac).
const FRESH_SECONDS: u64 = 30 * 60;
const FUTURE_SLACK_SECONDS: u64 = 5 * 60;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// One cached usage response.
pub struct Reading {
    pub windows: Vec<LimitWindow>,
    /// Unix seconds from the response's own `Date:` header: when the numbers were true.
    pub captured: u64,
}

fn data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("Claude"))
}

/// The file's bytes when it is a regular file of at most `max` bytes.
pub fn read_bounded(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > max {
        return None;
    }
    std::fs::read(path).ok()
}

// ------------------------------------------------------------------ running, account

/// Whether Claude Desktop is running. Chromium keeps `lockfile` open for writing with a
/// read-only share mode while its main process lives, so opening it for write fails with a
/// sharing violation exactly then. Nothing is written or created: an unheld or missing file
/// (a crashed Desktop leaves one behind) means not running.
pub fn running() -> bool {
    let Some(dir) = data_dir() else {
        return false;
    };
    match std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join("lockfile"))
    {
        Ok(_) => false,
        Err(error) => error.raw_os_error() == Some(SHARING_VIOLATION),
    }
}

/// The account Desktop is signed into while it runs, otherwise `None`.
pub fn signed_in_account() -> Option<String> {
    if !running() {
        return None;
    }
    let bytes = read_bounded(&data_dir()?.join("config.json"), CONFIG_MAX_BYTES)?;
    let id = string_member(&bytes, "lastKnownAccountUuid")?.to_ascii_lowercase();
    is_account_id(&id).then_some(id)
}

/// A canonical 8-4-4-4-12 hex id; anything else is not an account or organization folder.
pub fn is_account_id(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && parts.iter().zip([8usize, 4, 4, 4, 12]).all(|(part, length)| {
            part.len() == length && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// The organizations Desktop filed under an account: the folder names under its session
/// directories. Nothing inside them is read.
pub fn organizations(account: &str) -> Vec<String> {
    let Some(root) = data_dir() else {
        return Vec::new();
    };
    let mut found: Vec<String> = Vec::new();
    for parent in ["claude-code-sessions", "local-agent-mode-sessions"] {
        let Ok(entries) = std::fs::read_dir(root.join(parent).join(account)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if is_account_id(&name) && !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

/// Account id to the newest modification time (unix seconds) of its folder, from the union of
/// `claude-code-sessions` and `local-agent-mode-sessions`. Names only; nothing inside is read.
/// Empty when Claude Desktop is not installed.
pub fn account_folders() -> Vec<(String, u64)> {
    let Some(root) = data_dir() else {
        return Vec::new();
    };
    let mut found: Vec<(String, u64)> = Vec::new();
    for parent in ["claude-code-sessions", "local-agent-mode-sessions"] {
        let Ok(entries) = std::fs::read_dir(root.join(parent)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !is_account_id(&name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs());
            match found.iter_mut().find(|(id, _)| *id == name) {
                Some(slot) => slot.1 = slot.1.max(modified),
                None => found.push((name, modified)),
            }
        }
    }
    found
}

/// Whether Claude Desktop keeps running in the system tray when its window is closed
/// (`preferences.menuBarEnabled` in `claude_desktop_config.json`, on unless set to false).
/// With the tray off, closing the main window quits Desktop; with it on, closing only hides
/// the window. Only that one member is scanned for; the rest of the file is never parsed.
pub fn tray_enabled() -> bool {
    let Some(dir) = data_dir() else {
        return true;
    };
    let Some(bytes) = read_bounded(&dir.join("claude_desktop_config.json"), CONFIG_MAX_BYTES)
    else {
        return true;
    };
    let Some(at) = member_value(&bytes, "menuBarEnabled") else {
        return true;
    };
    !bytes[at..].starts_with(b"false")
}

// ------------------------------------------------------------------ member scanning

fn skip_space(text: &[u8], mut at: usize) -> usize {
    while text.get(at).is_some_and(|byte| byte.is_ascii_whitespace()) {
        at += 1;
    }
    at
}

/// Index of the value of the first `"key":` in `text`. A scan, not a parse: it is used on
/// files that hold sign-in material (never parsed) or that are too large and too varied for
/// the bounded JSON reader. An escaped copy of the key inside a string does not match.
fn member_value(text: &[u8], key: &str) -> Option<usize> {
    let needle = format!("\"{key}\"");
    let needle = needle.as_bytes();
    let mut from = 0usize;
    while let Some(found) = text
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let at = skip_space(text, from + found + needle.len());
        if text.get(at) == Some(&b':') {
            return Some(skip_space(text, at + 1));
        }
        from += found + 1;
    }
    None
}

/// A plain (escape-free) string member.
pub fn string_member(text: &[u8], key: &str) -> Option<String> {
    let start = member_value(text, key)?;
    let rest = text.get(start..)?.strip_prefix(b"\"")?;
    let end = rest.iter().position(|byte| *byte == b'"')?;
    let value = rest.get(..end)?;
    if value.contains(&b'\\') {
        return None;
    }
    String::from_utf8(value.to_vec()).ok()
}

/// The bytes of an object member, braces included.
pub fn object_member<'a>(text: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let start = member_value(text, key)?;
    let rest = text.get(start..)?;
    if rest.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in rest.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return rest.get(..=index);
                }
            }
            _ => {}
        }
    }
    None
}

// ------------------------------------------------------------------ the usage cache

/// The newest fresh usage response Desktop cached for any of `organizations`, or why there
/// is none. `not_before` (unix seconds) rejects a response from before the account was last
/// seen to change, because organizations can be shared between accounts. A response with a
/// window already past its reset is a description of a finished period and is rejected too.
pub fn cached_usage(
    organizations: &[String],
    now: u64,
    not_before: u64,
) -> Result<Reading, &'static str> {
    if organizations.is_empty() {
        return Err("no organization is filed under the account");
    }
    let dir = data_dir()
        .map(|dir| dir.join("Cache").join("Cache_Data"))
        .filter(|dir| dir.is_dir())
        .ok_or("no Claude Desktop cache folder")?;
    let candidates = scan(&dir, organizations).ok_or("the cache index could not be read")?;
    let mut newest: Option<Reading> = None;
    for candidate in &candidates {
        let Some(reading) = read_entry(&dir, candidate) else {
            continue;
        };
        if newest
            .as_ref()
            .is_none_or(|best| reading.captured > best.captured)
        {
            newest = Some(reading);
        }
    }
    let reading = newest.ok_or("no usage response is cached for the account")?;
    if reading.captured > now + FUTURE_SLACK_SECONDS {
        return Err("the cached reading is dated in the future");
    }
    if now.saturating_sub(reading.captured) > FRESH_SECONDS {
        return Err("the cached reading is older than 30 minutes");
    }
    if reading.captured < not_before {
        return Err("the cached reading predates the account change");
    }
    if reading
        .windows
        .iter()
        .any(|window| window.resets_at.is_some_and(|reset| reset <= now))
    {
        return Err("a cached window has already reset");
    }
    Ok(reading)
}

/// Where one entry's two streams are: sizes and Chromium block-file addresses.
struct Candidate {
    sizes: [usize; 2],
    addresses: [u32; 2],
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Which organization a cache key's usage URL names, or `None` when the key is not a usage
/// request. Keys carry a partition prefix (`1/0/https://claude.ai/...`), so the URL inside is
/// matched; the host check keeps other sites' look-alike paths out.
fn usage_organization(key: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(key).ok()?;
    if !(text.contains("claude.ai") || text.contains("anthropic.com")) {
        return None;
    }
    let marker = "/api/organizations/";
    let rest = &text[text.find(marker)? + marker.len()..];
    let path = rest.split(['?', '#']).next()?;
    let mut segments = path.split('/');
    let id = segments.next()?;
    if segments.next()? != "usage" || segments.next().is_some() || !is_account_id(id) {
        return None;
    }
    Some(id.to_ascii_lowercase())
}

/// An `EntryStore` block as a candidate when it is a live usage entry of one of the
/// organizations.
fn candidate_of(entry: &[u8], organizations: &[String]) -> Option<Candidate> {
    let key_length = le32(entry, 32) as usize;
    // State 0 is a normal entry (1 is evicted, 2 doomed); a non-zero long-key address means
    // the key is stored elsewhere.
    if le32(entry, 20) != 0 || key_length == 0 || key_length > MAX_INLINE_KEY || le32(entry, 36) != 0
    {
        return None;
    }
    let key = entry.get(ENTRY_KEY_OFFSET..ENTRY_KEY_OFFSET + key_length)?;
    let organization = usage_organization(key)?;
    if !organizations.contains(&organization) {
        return None;
    }
    let sizes = [le32(entry, 40) as i32, le32(entry, 44) as i32];
    let addresses = [le32(entry, 56), le32(entry, 60)];
    Some(Candidate {
        sizes: [usize::try_from(sizes[0]).ok()?, usize::try_from(sizes[1]).ok()?],
        addresses,
    })
}

fn fill(file: &mut File, buffer: &mut [u8]) -> usize {
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    filled
}

/// Adds the usage entries of one 256-byte block file to `found`; the number of the next
/// file in the chain (zero for none), or `None` when this one cannot be read as such a file.
fn scan_file(
    dir: &Path,
    number: u32,
    organizations: &[String],
    found: &mut Vec<Candidate>,
) -> Option<u32> {
    let mut file = File::open(dir.join(format!("data_{number}"))).ok()?;
    let mut header = [0u8; BLOCK_HEADER_BYTES];
    file.read_exact(&mut header).ok()?;
    // Magic, major version 2, and 256-byte blocks: anything else is not a layout this reads.
    if le32(&header, 0) != BLOCK_MAGIC
        || le32(&header, 4) >> 16 != 2
        || le32(&header, 12) != ENTRY_BYTES as u32
    {
        return None;
    }
    let next = i16::from_le_bytes([header[10], header[11]]);
    let mut chunk = vec![0u8; SCAN_CHUNK_ENTRIES * ENTRY_BYTES];
    let mut scanned = 0u64;
    loop {
        let filled = fill(&mut file, &mut chunk);
        for entry in chunk[..filled].chunks_exact(ENTRY_BYTES) {
            if let Some(candidate) = candidate_of(entry, organizations) {
                found.push(candidate);
            }
        }
        scanned += filled as u64;
        if filled < chunk.len() || scanned > MAX_SCAN_BYTES {
            break;
        }
    }
    Some(u32::try_from(next).unwrap_or(0))
}

/// Every live usage entry in the cache for one of the organizations. `data_1` holds the
/// entry records; a full block file chains to the next. `None` when the first is unreadable.
fn scan(dir: &Path, organizations: &[String]) -> Option<Vec<Candidate>> {
    let mut found = Vec::new();
    let mut number = 1u32;
    for round in 0..MAX_CHAIN {
        match scan_file(dir, number, organizations, &mut found) {
            Some(0) => break,
            Some(next) => number = next,
            None if round == 0 => return None,
            None => break,
        }
    }
    Some(found)
}

/// One stream's bytes from its block-file address. Bit 31 marks the address as set, bits
/// 28..30 are the file type (0 an external `f_XXXXXX` file, 2/3/4 the 256, 1024 and 4096-byte
/// block files), bits 24..25 the block count less one, 16..23 the block file's number and
/// 0..15 the first block.
fn read_stream(dir: &Path, address: u32, size: usize, max: usize) -> Option<Vec<u8>> {
    if size == 0 || size > max || address & 0x8000_0000 == 0 {
        return None;
    }
    let kind = (address >> 28) & 7;
    let (path, offset) = if kind == 0 {
        (dir.join(format!("f_{:06x}", address & 0x0FFF_FFFF)), 0u64)
    } else {
        let block = match kind {
            2 => 256u64,
            3 => 1024,
            4 => 4096,
            _ => return None,
        };
        let blocks = u64::from(((address >> 24) & 3) + 1);
        if size as u64 > blocks * block {
            return None;
        }
        let file = (address >> 16) & 0xFF;
        let start = u64::from(address & 0xFFFF);
        (
            dir.join(format!("data_{file}")),
            BLOCK_HEADER_BYTES as u64 + start * block,
        )
    };
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = vec![0u8; size];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// `Fri, 09 Oct 2026 17:34:35 GMT` as unix seconds.
fn http_date(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    let _weekday = parts.next()?;
    let day: u32 = parts.next()?.parse().ok()?;
    let month = parts.next()?;
    let year: u32 = parts.next()?.parse().ok()?;
    let clock = parts.next()?;
    if parts.next()? != "GMT" || parts.next().is_some() {
        return None;
    }
    let month = MONTHS
        .iter()
        .position(|name| name.eq_ignore_ascii_case(month))?
        + 1;
    usage::parse_iso8601(&format!("{year:04}-{month:02}-{day:02}T{clock}Z"))
}

/// The response's own `Date:` out of the header block, a run of NUL-separated `name:value`
/// strings (lower-case names over HTTP/2 and 3, original case over HTTP/1.1).
fn header_date(block: &[u8]) -> Option<u64> {
    for needle in [b"\0date:", b"\0Date:"] {
        let Some(found) = block
            .windows(needle.len())
            .position(|window| window == needle)
        else {
            continue;
        };
        let rest = block.get(found + needle.len()..)?;
        let end = rest.iter().position(|byte| *byte == 0)?;
        if let Some(date) = http_date(std::str::from_utf8(&rest[..end]).ok()?.trim()) {
            return Some(date);
        }
    }
    None
}

/// One entry's usage windows and date, or `None` when anything about it cannot be read.
fn read_entry(dir: &Path, candidate: &Candidate) -> Option<Reading> {
    let headers = read_stream(
        dir,
        candidate.addresses[0],
        candidate.sizes[0],
        MAX_HEADER_BYTES,
    )?;
    let captured = header_date(&headers)?;
    let body = read_stream(
        dir,
        candidate.addresses[1],
        candidate.sizes[1],
        MAX_BODY_BYTES,
    )?;
    let text = if body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        zstd::decompress(&body, MAX_JSON_BYTES)?
    } else {
        body
    };
    let root = json::parse(&text, MAX_JSON_BYTES)?;
    let windows = usage::claude_windows(&root);
    if windows.is_empty() {
        return None;
    }
    Some(Reading { windows, captured })
}
