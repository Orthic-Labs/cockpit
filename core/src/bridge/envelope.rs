//! The signed envelope every bridge message travels in, and the checks a
//! receiver applies. Signing is HMAC-SHA256 (RFC 2104, built on `sha2`) with a
//! 32-byte pair key two devices exchanged when the user accepted the pairing.

use crate::localsend::proto;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: u32 = 1;
/// Largest `body`, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Largest clock difference between sender and receiver.
pub const MAX_SKEW_MS: u64 = 5 * 60 * 1000;
/// How many nonces a receiver remembers.
pub const REPLAY_WINDOW: usize = 1000;
/// LocalSend upload `fileType` that carries an envelope in `preview`.
pub const BRIDGE_FILE_TYPE: &str = "application/x-pulse-bridge+json";
/// LocalSend upload `fileType` that carries a pairing offer in `preview`.
pub const PAIR_FILE_TYPE: &str = "application/x-pulse-pair+json";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported envelope version")]
    Version,
    #[error("message body is larger than 64 KiB")]
    Oversize,
    #[error("sender and receiver clocks differ by more than 5 minutes")]
    Skew,
    #[error("signature does not match")]
    BadSignature,
    #[error("message was already received")]
    Replay,
    #[error("unknown device")]
    UnknownDevice,
    #[error("malformed envelope: {0}")]
    Malformed(String),
}

/// Who sent it. `device` is the sending computer's certificate fingerprint,
/// `session` the chat, `name` how the chat is shown ("title on Device").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    pub device: String,
    pub session: String,
    pub name: String,
}

/// Who it is for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub device: String,
    pub session: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Message,
    Roster,
    Receipt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub id: String,
    /// Milliseconds since the Unix epoch, sender's clock.
    pub ts: u64,
    pub from: Sender,
    pub to: Target,
    pub kind: Kind,
    pub body: String,
    pub nonce: String,
    /// Lowercase hex HMAC-SHA256; empty for an unsigned (local) envelope.
    #[serde(default)]
    pub sig: String,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// A random version 4 UUID.
pub fn new_uuid() -> String {
    let mut bytes = from_hex(&proto::random_hex(16)).unwrap_or_default();
    bytes.resize(16, 0);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = to_hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A fresh 32-byte pair key, as 64 hex characters.
pub fn new_pair_key() -> String {
    proto::random_hex(32)
}

/// HMAC-SHA256 (RFC 2104).
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        let digest = Sha256::digest(key);
        block[..32].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for ((i, o), k) in inner_pad
        .iter_mut()
        .zip(outer_pad.iter_mut())
        .zip(block.iter())
    {
        *i ^= k;
        *o ^= k;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    let result = outer.finalize();
    let mut mac = [0u8; 32];
    mac.copy_from_slice(&result);
    mac
}

fn same_text(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

impl Envelope {
    /// A new unsigned envelope with a fresh id, time and nonce.
    pub fn new(
        from: Sender,
        to: Target,
        kind: Kind,
        body: impl Into<String>,
    ) -> Result<Envelope, EnvelopeError> {
        let body = body.into();
        if body.len() > MAX_BODY_BYTES {
            return Err(EnvelopeError::Oversize);
        }
        Ok(Envelope {
            v: VERSION,
            id: new_uuid(),
            ts: now_ms(),
            from,
            to,
            kind,
            body,
            nonce: proto::random_hex(16),
            sig: String::new(),
        })
    }

    /// Canonical JSON of every field except `sig`: keys sorted, no spaces.
    pub fn canonical(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.remove("sig");
        }
        canonical_json(&value)
    }

    pub fn sign(&mut self, key: &[u8]) {
        self.sig = to_hex(&hmac_sha256(key, self.canonical().as_bytes()));
    }

    pub fn is_signed(&self) -> bool {
        !self.sig.is_empty()
    }

    /// Version, size, signature and clock skew. Replays are `ReplayGuard`'s job.
    pub fn verify(&self, key: &[u8], now: u64) -> Result<(), EnvelopeError> {
        if self.v != VERSION {
            return Err(EnvelopeError::Version);
        }
        if self.body.len() > MAX_BODY_BYTES {
            return Err(EnvelopeError::Oversize);
        }
        let expected = to_hex(&hmac_sha256(key, self.canonical().as_bytes()));
        if !same_text(&expected, &self.sig) {
            return Err(EnvelopeError::BadSignature);
        }
        if self.ts.abs_diff(now) > MAX_SKEW_MS {
            return Err(EnvelopeError::Skew);
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<Envelope, EnvelopeError> {
        if text.len() > MAX_BODY_BYTES * 2 {
            return Err(EnvelopeError::Oversize);
        }
        serde_json::from_str(text).map_err(|e| EnvelopeError::Malformed(e.to_string()))
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Remembers the last `REPLAY_WINDOW` nonces.
#[derive(Debug)]
pub struct ReplayGuard {
    order: VecDeque<String>,
    seen: HashSet<String>,
    capacity: usize,
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new(REPLAY_WINDOW)
    }
}

impl ReplayGuard {
    pub fn new(capacity: usize) -> ReplayGuard {
        ReplayGuard {
            order: VecDeque::new(),
            seen: HashSet::new(),
            capacity: capacity.max(1),
        }
    }

    /// Record `device` + `nonce`; false when it was already recorded.
    pub fn insert(&mut self, device: &str, nonce: &str) -> bool {
        let key = format!("{device}:{nonce}");
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// Everything a receiver checks, in order: the pair key exists, the envelope
/// verifies, and its nonce is new. The nonce is recorded only for a valid one.
pub fn accept(
    envelope: &Envelope,
    key: Option<&[u8]>,
    now: u64,
    replay: &mut ReplayGuard,
) -> Result<(), EnvelopeError> {
    let key = key.ok_or(EnvelopeError::UnknownDevice)?;
    envelope.verify(key, now)?;
    if !replay.insert(&envelope.from.device, &envelope.nonce) {
        return Err(EnvelopeError::Replay);
    }
    Ok(())
}
