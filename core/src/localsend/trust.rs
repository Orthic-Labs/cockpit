//! Per-device trust for nearby sharing: every device seen on the network or in
//! a request has one record, and the owner sets it to Allow, Ask or Deny.
//!
//! * Allow: a request is accepted without a card, files and text alike. A
//!   sender that proves its key (it presents the certificate its fingerprint
//!   names in the TLS handshake) is always honoured; one that cannot (other
//!   LocalSend apps) only while it is announced from the address it calls from,
//!   and never when this device has proved its key before.
//! * Ask: every request raises a card, text included.
//! * Deny: a request is answered 403 at once and counted in `refused`.
//!
//! The records live in `nearby_devices.json` in the service's state directory.
//! Fingerprints are the policy key; the alias, model and kind are only what the
//! device claimed, and the owner's own name for it is `label`.

use super::{now_ms, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Records kept; past this the oldest unlabelled non-Allow one makes room.
pub const MAX_DEVICES: usize = 200;
const FILE: &str = "nearby_devices.json";
/// How often an unchanged device's `last_seen_ms` is written to disk.
const SAVE_EVERY_MS: u64 = 5 * 60 * 1000;
/// How often a refusal count is written to disk.
const SAVE_REFUSED_MS: u64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustState {
    Allow,
    Ask,
    Deny,
}

impl TrustState {
    pub fn parse(text: &str) -> Option<TrustState> {
        match text {
            "allow" => Some(TrustState::Allow),
            "ask" => Some(TrustState::Ask),
            "deny" => Some(TrustState::Deny),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TrustState::Allow => "allow",
            TrustState::Ask => "ask",
            TrustState::Deny => "deny",
        }
    }
}

/// What the owner decided about one device, and what it last claimed to be.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustEntry {
    pub fingerprint: String,
    /// The owner's name for the device; empty until they give one.
    #[serde(default)]
    pub label: String,
    /// The alias the device announced itself with (a claim, not an identity).
    #[serde(default)]
    pub alias_seen: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub first_seen_ms: u64,
    #[serde(default)]
    pub last_seen_ms: u64,
    pub state: TrustState,
    /// The device has at least once proved it holds the key behind its
    /// fingerprint (a client certificate in the handshake that matched).
    #[serde(default)]
    pub verified: bool,
    /// Requests answered 403 because of Deny.
    #[serde(default)]
    pub refused: u64,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default = "one")]
    version: u32,
    #[serde(default = "ask")]
    default_state: TrustState,
    #[serde(default)]
    devices: Vec<TrustEntry>,
}

fn one() -> u32 {
    1
}

fn ask() -> TrustState {
    TrustState::Ask
}

/// A claimed text cut to `max` characters with control characters removed.
fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    !fingerprint.is_empty() && fingerprint.len() <= 128
}

pub(crate) struct TrustStore {
    path: PathBuf,
    default_state: TrustState,
    devices: Vec<TrustEntry>,
    saved_ms: u64,
}

impl TrustStore {
    /// The records in `directory`. With no file yet, the fingerprints of the old
    /// `known-devices.json` (devices accepted before) become Allow. A file that
    /// cannot be read is set aside as `.bad` and the store starts empty, so
    /// nothing is allowed by accident.
    pub fn load(directory: &Path) -> TrustStore {
        let path = directory.join(FILE);
        let mut store = TrustStore {
            path: path.clone(),
            default_state: TrustState::Ask,
            devices: Vec::new(),
            saved_ms: now_ms(),
        };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(stored) => {
                    store.default_state = stored.default_state;
                    let mut seen = HashSet::new();
                    store.devices = stored
                        .devices
                        .into_iter()
                        .filter(|d| valid_fingerprint(&d.fingerprint))
                        .filter(|d| seen.insert(d.fingerprint.clone()))
                        .take(MAX_DEVICES)
                        .collect();
                }
                Err(_) => {
                    let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                }
            },
            Err(_) => {
                let now = now_ms();
                let mut migrated: Vec<String> =
                    super::load_known(&directory.join("known-devices.json"))
                        .into_iter()
                        .filter(|f| valid_fingerprint(f))
                        .collect();
                migrated.sort();
                store.devices = migrated
                    .into_iter()
                    .take(MAX_DEVICES)
                    .map(|fingerprint| TrustEntry {
                        fingerprint,
                        label: String::new(),
                        alias_seen: String::new(),
                        model: None,
                        kind: None,
                        first_seen_ms: now,
                        last_seen_ms: now,
                        state: TrustState::Allow,
                        refused: 0,
                        verified: false,
                    })
                    .collect();
                if !store.devices.is_empty() {
                    let _ = store.save();
                }
            }
        }
        store
    }

    fn save(&mut self) -> std::io::Result<()> {
        let stored = Stored {
            version: 1,
            default_state: self.default_state,
            devices: self.devices.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&stored).map_err(std::io::Error::other)?;
        write_atomic(&self.path, &bytes)?;
        self.saved_ms = now_ms();
        Ok(())
    }

    pub fn default_state(&self) -> TrustState {
        self.default_state
    }

    /// What a request or send from `fingerprint` is subject to. A device with
    /// no usable fingerprint can never be allowed.
    pub fn state_of(&self, fingerprint: &str) -> TrustState {
        if !valid_fingerprint(fingerprint) {
            return match self.default_state {
                TrustState::Deny => TrustState::Deny,
                _ => TrustState::Ask,
            };
        }
        self.devices
            .iter()
            .find(|d| d.fingerprint == fingerprint)
            .map(|d| d.state)
            .unwrap_or(self.default_state)
    }

    /// Whether this device has proved its key before.
    pub fn has_proven(&self, fingerprint: &str) -> bool {
        self.devices
            .iter()
            .any(|d| d.fingerprint == fingerprint && d.verified)
    }

    /// Record a sighting. A first sighting takes the default state.
    pub fn observe(
        &mut self,
        fingerprint: &str,
        alias: &str,
        model: Option<&str>,
        kind: Option<&str>,
        verified: bool,
    ) {
        if !valid_fingerprint(fingerprint) {
            return;
        }
        let now = now_ms();
        let alias = clean(alias, 80);
        let model = model.map(|m| clean(m, 60)).filter(|m| !m.is_empty());
        let kind = kind.map(|k| clean(k, 20)).filter(|k| !k.is_empty());
        let save = match self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
        {
            Some(entry) => {
                let changed = entry.alias_seen != alias
                    || entry.model != model
                    || entry.kind != kind
                    || (verified && !entry.verified);
                let stale = now.saturating_sub(self.saved_ms) >= SAVE_EVERY_MS;
                entry.alias_seen = alias;
                entry.model = model;
                entry.kind = kind;
                entry.verified |= verified;
                entry.last_seen_ms = now;
                changed || stale
            }
            None => {
                if !self.make_room() {
                    return;
                }
                self.devices.push(TrustEntry {
                    fingerprint: fingerprint.to_string(),
                    label: String::new(),
                    alias_seen: alias,
                    model,
                    kind,
                    first_seen_ms: now,
                    last_seen_ms: now,
                    state: self.default_state,
                    refused: 0,
                    verified,
                });
                true
            }
        };
        if save {
            let _ = self.save();
        }
    }

    /// Room for one more record: drop the oldest unlabelled one that is not
    /// Allow, else the oldest that is not Allow. False when all are Allow.
    fn make_room(&mut self) -> bool {
        if self.devices.len() < MAX_DEVICES {
            return true;
        }
        let pick = |only_unlabelled: bool| {
            self.devices
                .iter()
                .enumerate()
                .filter(|(_, d)| d.state != TrustState::Allow)
                .filter(|(_, d)| !only_unlabelled || d.label.is_empty())
                .min_by_key(|(_, d)| d.last_seen_ms)
                .map(|(index, _)| index)
        };
        match pick(true).or_else(|| pick(false)) {
            Some(index) => {
                self.devices.remove(index);
                true
            }
            None => false,
        }
    }

    /// Count a request answered 403.
    pub fn refuse(&mut self, fingerprint: &str) {
        let now = now_ms();
        let due = now.saturating_sub(self.saved_ms) >= SAVE_REFUSED_MS;
        if let Some(entry) = self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
        {
            entry.refused = entry.refused.saturating_add(1);
            if due {
                let _ = self.save();
            }
        }
    }

    pub fn set_state(&mut self, fingerprint: &str, state: TrustState) -> Result<(), String> {
        if !valid_fingerprint(fingerprint) {
            return Err("That device has no fingerprint.".to_string());
        }
        let index = match self.devices.iter().position(|d| d.fingerprint == fingerprint) {
            Some(index) => index,
            None => {
                if !self.make_room() {
                    return Err("Too many devices are allowed; forget one first.".to_string());
                }
                let now = now_ms();
                self.devices.push(TrustEntry {
                    fingerprint: fingerprint.to_string(),
                    label: String::new(),
                    alias_seen: String::new(),
                    model: None,
                    kind: None,
                    first_seen_ms: now,
                    last_seen_ms: now,
                    state,
                    refused: 0,
                    verified: false,
                });
                self.devices.len() - 1
            }
        };
        self.devices[index].state = state;
        self.save()
            .map_err(|e| format!("Couldn't save the device list: {e}"))
    }

    pub fn set_label(&mut self, fingerprint: &str, label: &str) -> Result<(), String> {
        let entry = self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
            .ok_or_else(|| "That device isn't in the list.".to_string())?;
        entry.label = clean(label, 80).trim().to_string();
        self.save()
            .map_err(|e| format!("Couldn't save the device list: {e}"))
    }

    pub fn forget(&mut self, fingerprint: &str) -> Result<(), String> {
        let before = self.devices.len();
        self.devices.retain(|d| d.fingerprint != fingerprint);
        if self.devices.len() == before {
            return Ok(());
        }
        self.save()
            .map_err(|e| format!("Couldn't save the device list: {e}"))
    }

    /// New devices start as Ask or Deny; Allow is only ever set per device.
    pub fn set_default(&mut self, state: TrustState) -> Result<(), String> {
        if state == TrustState::Allow {
            return Err("New devices can be asked about or denied, not allowed.".to_string());
        }
        self.default_state = state;
        self.save()
            .map_err(|e| format!("Couldn't save the device list: {e}"))
    }

    /// Newest sighting first.
    pub fn list(&self) -> Vec<TrustEntry> {
        let mut list = self.devices.clone();
        list.sort_by(|a, b| b.last_seen_ms.cmp(&a.last_seen_ms));
        list
    }
}
