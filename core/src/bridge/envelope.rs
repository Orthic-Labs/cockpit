//! The message every bridge delivery carries: who wrote it, which chat it is
//! for, and the text. It travels between computers over ssh (see `links`), so
//! it is not signed; ssh already authenticates both ends.

use crate::localsend::proto;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: u32 = 2;
/// Largest `body`, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported envelope version")]
    Version,
    #[error("message body is larger than 64 KiB")]
    Oversize,
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
    pub fn new(from: Sender, to: Target, body: impl Into<String>) -> Result<Envelope, EnvelopeError> {
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
            body,
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parse and check version and size.
    pub fn from_json(text: &str) -> Result<Envelope, EnvelopeError> {
        if text.len() > MAX_BODY_BYTES * 2 {
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
