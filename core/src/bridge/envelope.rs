//! The message every bridge delivery carries: who wrote it, which chat it is
//! for, and the text. It travels between computers over ssh (see `links`), so
//! it is not signed; ssh already authenticates both ends.

use crate::localsend::proto;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: u32 = 2;
/// Largest `body`, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Largest encoded envelope: a body of control characters grows sixfold as JSON.
pub const MAX_ENCODED_BYTES: usize = MAX_BODY_BYTES * 6 + 2048;
/// Largest `device`, `session`, `name` or `id`, in bytes.
pub const MAX_META_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported envelope version")]
    Version,
    #[error("message body is larger than 64 KiB")]
    Oversize,
    #[error("an envelope field is longer than 256 bytes")]
    MetaOversize,
    #[error("malformed envelope: {0}")]
    Malformed(String),
}

/// Who sent it. `device` is the sending computer's name (its link name on the
/// receiving side), `session` the chat, `name` how the chat is shown
/// ("title on Device").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    pub device: String,
    pub session: String,
    pub name: String,
}

/// Who it is for: a computer name and a chat id there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub device: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub id: String,
    /// When it was written (ms since the epoch).
    pub ts: u64,
    pub from: Sender,
    pub to: Target,
    pub body: String,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `text` cut to at most `max` bytes at a character boundary.
fn truncate_bytes(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// A label made safe to print in guidance for a chat: letters, digits, spaces and
/// `. , _ -` only (anything else becomes a space), whitespace squashed, at most `max`
/// characters. It cannot hold quotes, shell syntax or markup.
pub fn plain_label(text: &str, max: usize) -> String {
    let mapped: String = text
        .chars()
        .map(|c| match c {
            c if c.is_alphanumeric() => c,
            '.' | ',' | '_' | '-' => c,
            _ => ' ',
        })
        .collect();
    mapped
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// A random version 4 UUID.
pub fn new_uuid() -> String {
    let hex = proto::random_hex(16);
    let mut bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    bytes.resize(16, 0);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

impl Envelope {
    /// A new envelope with a fresh id and time.
    pub fn new(
        from: Sender,
        to: Target,
        body: impl Into<String>,
    ) -> Result<Envelope, EnvelopeError> {
        let body = body.into();
        if body.len() > MAX_BODY_BYTES {
            return Err(EnvelopeError::Oversize);
        }
        // The sender's own labels are cut to size; the receiver rejects anything longer.
        let from = Sender {
            device: truncate_bytes(&from.device, MAX_META_BYTES),
            session: truncate_bytes(&from.session, MAX_META_BYTES),
            name: truncate_bytes(&from.name, MAX_META_BYTES),
        };
        let envelope = Envelope {
            v: VERSION,
            id: new_uuid(),
            ts: now_ms(),
            from,
            to,
            body,
        };
        envelope.check_meta()?;
        Ok(envelope)
    }

    /// Every metadata field is at most 256 bytes.
    pub fn check_meta(&self) -> Result<(), EnvelopeError> {
        let fields = [
            &self.id,
            &self.from.device,
            &self.from.session,
            &self.from.name,
            &self.to.device,
            &self.to.session,
        ];
        if fields.iter().any(|f| f.len() > MAX_META_BYTES) {
            return Err(EnvelopeError::MetaOversize);
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse and check version and size.
    pub fn from_json(text: &str) -> Result<Envelope, EnvelopeError> {
        if text.len() > MAX_ENCODED_BYTES {
            return Err(EnvelopeError::Oversize);
        }
        let env: Envelope =
            serde_json::from_str(text).map_err(|e| EnvelopeError::Malformed(e.to_string()))?;
        if env.v != VERSION {
            return Err(EnvelopeError::Version);
        }
        if env.body.len() > MAX_BODY_BYTES {
            return Err(EnvelopeError::Oversize);
        }
        env.check_meta()?;
        if env.id.is_empty() || env.to.session.is_empty() {
            return Err(EnvelopeError::Malformed("missing id or target".to_string()));
        }
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_bad_input() {
        let env = Envelope::new(
            Sender {
                device: "Mac".into(),
                session: "a".into(),
                name: "Planner on Mac".into(),
            },
            Target {
                device: "Dell".into(),
                session: "b".into(),
            },
            "hello",
        )
        .unwrap();
        let back = Envelope::from_json(&env.to_json()).unwrap();
        assert_eq!(back, env);
        assert_eq!(new_uuid().len(), 36);
        assert!(matches!(
            Envelope::from_json("{}"),
            Err(EnvelopeError::Malformed(_))
        ));
        let big = "x".repeat(MAX_BODY_BYTES + 1);
        assert_eq!(
            Envelope::new(Sender::default(), Target::default(), big).unwrap_err(),
            EnvelopeError::Oversize
        );
    }
}
