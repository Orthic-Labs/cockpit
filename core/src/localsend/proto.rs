//! LocalSend protocol v2 wire types and the file-name rules for received files.
//!
//! Implemented independently from the open specification
//! (github.com/localsend/protocol, README.md, v2.2); no LocalSend code is used.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

pub const PORT: u16 = 53317;
pub const MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const API: &str = "/api/localsend/v2";
pub const VERSION: &str = "2.0";

fn default_version() -> String {
    VERSION.to_string()
}
fn default_port() -> u16 {
    PORT
}
fn default_protocol() -> String {
    "https".to_string()
}

/// What a device says about itself: the multicast announcement, `/register`
/// and `/info` bodies, and `info` inside `prepare-upload`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub alias: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub device_model: Option<String>,
    #[serde(default)]
    pub device_type: Option<String>,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub download: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announce: Option<bool>,
}

/// One file in a `prepare-upload` request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub id: String,
    pub file_name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub file_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrepareUploadRequest {
    pub info: DeviceInfo,
    pub files: BTreeMap<String, FileMeta>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareUploadResponse {
    pub session_id: String,
    pub files: BTreeMap<String, String>,
}

/// A text sent as a message is one `text/plain` file with its text in `preview`.
pub fn is_message(files: &BTreeMap<String, FileMeta>) -> bool {
    files.len() == 1
        && files.values().all(|f| {
            f.file_type == "text/plain" && f.preview.as_ref().is_some_and(|p| !p.is_empty())
        })
}

// ---- file names ------------------------------------------------------------

/// One path component made safe: no separators, no control characters, no
/// leading or trailing dots or spaces (so never `.` or `..`), at most 200 bytes.
pub fn sanitize_component(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = replaced
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string();
    if trimmed.is_empty() {
        return "file".to_string();
    }
    if trimmed.len() <= 200 {
        return trimmed;
    }
    let (stem, ext) = split_extension(&trimmed);
    let ext = if ext.len() <= 16 { ext } else { "" };
    let mut end = 200usize.saturating_sub(ext.len()).min(stem.len());
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    let cut = stem[..end].trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    if cut.is_empty() {
        "file".to_string()
    } else {
        format!("{cut}{ext}")
    }
}

/// A sender's `fileName` as safe components below the save folder. `..`, `.`,
/// empty parts and both separators never survive; at most the last 8 parts.
pub fn sanitize_relative(raw: &str) -> Vec<String> {
    let mut parts: Vec<String> = raw
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .map(sanitize_component)
        .collect();
    if parts.len() > 8 {
        parts = parts.split_off(parts.len() - 8);
    }
    if parts.is_empty() {
        parts.push("file".to_string());
    }
    parts
}

fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// Create (empty) the file `parts` names below `base`, choosing "name (1).ext",
/// "name (2).ext", … when it exists. `create_new` makes the choice atomic and
/// never follows a symlink, so an existing file is never overwritten.
pub fn reserve_unique(base: &Path, parts: &[String]) -> io::Result<PathBuf> {
    let (dirs, last) = parts
        .split_last()
        .map(|(last, dirs)| (dirs, last))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty file name"))?;
    let mut dir = base.to_path_buf();
    for part in dirs {
        dir.push(sanitize_component(part));
    }
    std::fs::create_dir_all(&dir)?;
    let last = sanitize_component(last);
    let (stem, ext) = split_extension(&last);
    for n in 0..10_000u32 {
        let candidate = if n == 0 {
            last.clone()
        } else {
            format!("{stem} ({n}){ext}")
        };
        let path = dir.join(candidate);
        if !path.starts_with(base) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path escapes the save folder",
            ));
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files with that name",
    ))
}

pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

/// `n` random bytes as lowercase hex (session ids, tokens, file ids).
/// Unix reads `/dev/urandom`; elsewhere the standard library's per-process
/// random hasher keys are mixed with the clock.
pub fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    if !fill_random(&mut bytes) {
        use std::hash::{BuildHasher, Hasher};
        for chunk in bytes.chunks_mut(8) {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            let value = hasher.finish().to_le_bytes();
            chunk.copy_from_slice(&value[..chunk.len()]);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn fill_random(buffer: &mut [u8]) -> bool {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(buffer))
        .is_ok()
}

#[cfg(not(unix))]
fn fill_random(_buffer: &mut [u8]) -> bool {
    false
}

/// Percent-encode a query value.
pub fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_removed() {
        assert_eq!(sanitize_relative("../../etc/passwd"), vec!["etc", "passwd"]);
        assert_eq!(sanitize_relative("..\\..\\x.txt"), vec!["x.txt"]);
        assert_eq!(sanitize_relative("/abs/olute"), vec!["abs", "olute"]);
        assert_eq!(sanitize_relative(".."), vec!["file"]);
        assert_eq!(sanitize_component("a/b"), "a_b");
        assert_eq!(sanitize_component(" ... "), "file");
        assert_eq!(sanitize_component("C:evil"), "C_evil");
    }

    #[test]
    fn long_names_keep_their_extension() {
        let name = format!("{}.jpeg", "a".repeat(400));
        let clean = sanitize_component(&name);
        assert!(clean.len() <= 200 && clean.ends_with(".jpeg"));
    }

    #[test]
    fn decode_round_trip() {
        assert_eq!(url_decode(&url_encode("a b/c%d")), "a b/c%d");
        assert_eq!(url_decode("100%"), "100%");
    }
}
