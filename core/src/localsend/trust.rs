//! Per-device trust for nearby sharing: every device seen on the network or in
//! a request has one record, and the owner sets it to Allow, Ask or Deny.
//!
//! * Allow: a request is accepted without a card, files and text alike, but
//!   only from a sender that proves a key (`receive::prepare_upload`): by the
//!   client certificate it presents (`proof = "certificate"`, identity), or, for
//!   senders that present none, by answering a connect-back handshake at the
//!   address it calls from (`proof = "callback"`). A callback is relayable by
//!   anyone on the network while the real device is online, so it is a
//!   convenience tier only: it skips the card, but the sender stays unproven
//!   (files kept apart, no clipboard, size caps). Without proof the request is
//!   asked about.
//! * Ask: every request raises a card, text included.
//! * Deny: a request is answered 403 at once and counted in `refused`.
//!
//! The records live in `nearby_devices.json` in the service's state directory.
//! Fingerprints (always lowercase, see `normalize`) are the policy key; the
//! alias, model and kind are only what the device claimed, and the owner's own
//! name for it is `label`.

use super::{now_ms, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Unlabelled records the owner never decided on, kept at most; past this the
/// oldest makes room. Allow, Deny and labelled records are never dropped.
pub const MAX_UNDECIDED: usize = 200;
const FILE: &str = "nearby_devices.json";
/// How often an unchanged device's `last_seen_ms` is written to disk.
const SAVE_EVERY_MS: u64 = 5 * 60 * 1000;
/// How often a refusal count is written to disk.
const SAVE_REFUSED_MS: u64 = 2000;

pub const PROOF_CERTIFICATE: &str = "certificate";
pub const PROOF_CALLBACK: &str = "callback";

/// The one spelling of a fingerprint used as a key everywhere.
pub fn normalize(fingerprint: &str) -> String {
    fingerprint.trim().to_ascii_lowercase()
}

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
    /// What an unproven request claimed on a record that has proof; shown on
    /// the card, never copied into `alias_seen`/`model`/`kind`.
    #[serde(default)]
    pub alias_claimed: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub first_seen_ms: u64,
    #[serde(default)]
    pub last_seen_ms: u64,
    pub state: TrustState,
    /// How the device last proved the key behind its fingerprint:
    /// "certificate" or "callback"; None when it never has.
    #[serde(default)]
    pub proof: Option<String>,
    /// The owner set the state on purpose (so the record is never dropped).
    #[serde(default)]
    pub decided: bool,
    /// Requests answered 403 because of Deny.
    #[serde(default)]
    pub refused: u64,
}

impl TrustEntry {
    fn blank(fingerprint: String, state: TrustState, now: u64) -> TrustEntry {
        TrustEntry {
            fingerprint,
            label: String::new(),
            alias_seen: String::new(),
            alias_claimed: String::new(),
            model: None,
            kind: None,
            first_seen_ms: now,
            last_seen_ms: now,
            state,
            proof: None,
            decided: false,
            refused: 0,
        }
    }

    /// Whether the record may be dropped to make room.
    fn evictable(&self) -> bool {
        // A device that has proved its key keeps that memory: forgetting it would let a
        // later request for the same fingerprint fall back to the weaker callback.
        !self.decided
            && self.label.is_empty()
            && self.state != TrustState::Allow
            && self.proof.is_none()
    }
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
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}

fn valid_fingerprint(fingerprint: &str) -> bool {
    !fingerprint.is_empty() && fingerprint.len() <= 128
}

pub(crate) struct TrustStore {
    path: PathBuf,
    default_state: TrustState,
    devices: Vec<TrustEntry>,
    saved_ms: u64,
    /// The file on disk could not be read: it is left alone until an explicit
    /// change succeeds, so a read error never overwrites the owner's records.
    hold: bool,
}

impl TrustStore {
    /// The records in `directory`.
    ///
    /// * A readable file is used (a stored default of Allow becomes Ask).
    /// * No file, and no `.bad` set-aside next to it: the fingerprints of the old
    ///   `known-devices.json` become **Ask** records, keeping any label (the old
    ///   switch was off by default and a device was remembered after one accept,
    ///   so that is no grant to skip the card).
    /// * An unparsable file is set aside as `.bad` and an empty store is written
    ///   at once, so a restart cannot migrate again and bring back old entries.
    /// * Any other read error: an empty in-memory store, the file untouched.
    pub fn load(directory: &Path) -> TrustStore {
        let path = directory.join(FILE);
        let bad = path.with_extension("json.bad");
        let mut store = TrustStore {
            path: path.clone(),
            default_state: TrustState::Ask,
            devices: Vec::new(),
            saved_ms: now_ms(),
            hold: false,
        };
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Stored>(&bytes) {
                Ok(stored) => {
                    store.default_state = match stored.default_state {
                        TrustState::Deny => TrustState::Deny,
                        _ => TrustState::Ask,
                    };
                    let mut seen = HashSet::new();
                    store.devices = stored
                        .devices
                        .into_iter()
                        .map(|mut d| {
                            d.fingerprint = normalize(&d.fingerprint);
                            d
                        })
                        .filter(|d| valid_fingerprint(&d.fingerprint))
                        .filter(|d| seen.insert(d.fingerprint.clone()))
                        .collect();
                }
                Err(_) => {
                    let _ = std::fs::rename(&path, &bad);
                    let _ = store.save();
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !bad.exists() => {
                let now = now_ms();
                let mut seen = HashSet::new();
                for (fingerprint, label) in super::load_known(&directory.join("known-devices.json"))
                {
                    let fingerprint = normalize(&fingerprint);
                    if !valid_fingerprint(&fingerprint) || !seen.insert(fingerprint.clone()) {
                        continue;
                    }
                    let mut entry = TrustEntry::blank(fingerprint, TrustState::Ask, now);
                    entry.label = clean(&label, 80).trim().to_string();
                    store.devices.push(entry);
                }
                if !store.devices.is_empty() {
                    let _ = store.save();
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => store.hold = true,
        }
        store
    }

    /// Write everything out. An explicit change that gets here lifts the hold.
    fn save(&mut self) -> std::io::Result<()> {
        let stored = Stored {
            version: 1,
            default_state: self.default_state,
            devices: self.devices.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&stored).map_err(std::io::Error::other)?;
        write_atomic(&self.path, &bytes)?;
        self.saved_ms = now_ms();
        self.hold = false;
        Ok(())
    }

    /// A save the owner did not ask for (a sighting, a refusal count).
    fn autosave(&mut self) {
        if !self.hold {
            let _ = self.save();
        }
    }

    fn explicit_save(&mut self) -> Result<(), String> {
        self.save()
            .map_err(|e| format!("Couldn't save the device list: {e}"))
    }

    pub fn default_state(&self) -> TrustState {
        self.default_state
    }

    /// What a request or send from `fingerprint` is subject to. A device with
    /// no usable fingerprint can never be allowed.
    pub fn state_of(&self, fingerprint: &str) -> TrustState {
        let fingerprint = normalize(fingerprint);
        if !valid_fingerprint(&fingerprint) {
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

    /// Record a sighting. A first sighting takes the default state. `proof`
    /// ("certificate" or "callback") is set only by a check that just passed;
    /// without it, a record that already has proof keeps its alias, model and
    /// kind and the claimed alias goes to `alias_claimed`.
    pub fn observe(
        &mut self,
        fingerprint: &str,
        alias: &str,
        model: Option<&str>,
        kind: Option<&str>,
        proof: Option<&str>,
    ) {
        let fingerprint = normalize(fingerprint);
        if !valid_fingerprint(&fingerprint) {
            return;
        }
        let now = now_ms();
        let alias = clean(alias, 80);
        let model = model.map(|m| clean(m, 60)).filter(|m| !m.is_empty());
        let kind = kind.map(|k| clean(k, 20)).filter(|k| !k.is_empty());
        let stale = now.saturating_sub(self.saved_ms) >= SAVE_EVERY_MS;
        let save = match self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
        {
            Some(entry) => {
                entry.last_seen_ms = now;
                let mut changed = false;
                if entry.proof.is_some() && proof.is_none() {
                    changed |= entry.alias_claimed != alias;
                    entry.alias_claimed = alias;
                } else {
                    changed |=
                        entry.alias_seen != alias || entry.model != model || entry.kind != kind;
                    entry.alias_seen = alias;
                    entry.model = model;
                    entry.kind = kind;
                }
                if let Some(proof) = proof {
                    // A certificate outranks a callback.
                    let keep = entry.proof.as_deref() == Some(PROOF_CERTIFICATE);
                    if !keep && entry.proof.as_deref() != Some(proof) {
                        entry.proof = Some(proof.to_string());
                        changed = true;
                    }
                }
                changed || stale
            }
            None => {
                self.make_room();
                let mut entry = TrustEntry::blank(fingerprint, self.default_state, now);
                entry.alias_seen = alias;
                entry.model = model;
                entry.kind = kind;
                entry.proof = proof.map(str::to_string);
                self.devices.push(entry);
                true
            }
        };
        if save {
            self.autosave();
        }
    }

    /// Whether this device has ever proved its key by certificate. Such a record
    /// never accepts the weaker callback proof.
    pub fn certificate_proven(&self, fingerprint: &str) -> bool {
        let fingerprint = normalize(fingerprint);
        self.devices
            .iter()
            .any(|d| d.fingerprint == fingerprint && d.proof.as_deref() == Some(PROOF_CERTIFICATE))
    }

    /// The device just answered a connect-back handshake for its fingerprint.
    pub fn mark_callback(&mut self, fingerprint: &str) {
        let fingerprint = normalize(fingerprint);
        if let Some(entry) = self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
            && entry.proof.is_none()
        {
            entry.proof = Some(PROOF_CALLBACK.to_string());
            self.autosave();
        }
    }

    /// Keep at most `MAX_UNDECIDED` droppable records, oldest sighting first out,
    /// leaving room for one more.
    fn make_room(&mut self) {
        while self.devices.iter().filter(|d| d.evictable()).count() >= MAX_UNDECIDED {
            let oldest = self
                .devices
                .iter()
                .enumerate()
                .filter(|(_, d)| d.evictable())
                .min_by_key(|(_, d)| d.last_seen_ms)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.devices.remove(index);
                }
                None => break,
            }
        }
    }

    /// Count a request answered 403.
    pub fn refuse(&mut self, fingerprint: &str) {
        let fingerprint = normalize(fingerprint);
        let due = now_ms().saturating_sub(self.saved_ms) >= SAVE_REFUSED_MS;
        if let Some(entry) = self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
        {
            entry.refused = entry.refused.saturating_add(1);
            if due {
                self.autosave();
            }
        }
    }

    pub fn set_state(&mut self, fingerprint: &str, state: TrustState) -> Result<(), String> {
        let fingerprint = normalize(fingerprint);
        if !valid_fingerprint(&fingerprint) {
            return Err("That device has no fingerprint.".to_string());
        }
        let index = match self
            .devices
            .iter()
            .position(|d| d.fingerprint == fingerprint)
        {
            Some(index) => index,
            None => {
                self.devices
                    .push(TrustEntry::blank(fingerprint, state, now_ms()));
                self.devices.len() - 1
            }
        };
        self.devices[index].state = state;
        self.devices[index].decided = true;
        self.explicit_save()
    }

    pub fn set_label(&mut self, fingerprint: &str, label: &str) -> Result<(), String> {
        let fingerprint = normalize(fingerprint);
        let entry = self
            .devices
            .iter_mut()
            .find(|d| d.fingerprint == fingerprint)
            .ok_or_else(|| "That device isn't in the list.".to_string())?;
        entry.label = clean(label, 80).trim().to_string();
        self.explicit_save()
    }

    pub fn forget(&mut self, fingerprint: &str) -> Result<(), String> {
        let fingerprint = normalize(fingerprint);
        let before = self.devices.len();
        self.devices.retain(|d| d.fingerprint != fingerprint);
        if self.devices.len() == before {
            return Ok(());
        }
        self.explicit_save()
    }

    /// New devices start as Ask or Deny; Allow is only ever set per device.
    pub fn set_default(&mut self, state: TrustState) -> Result<(), String> {
        if state == TrustState::Allow {
            return Err("New devices can be asked about or denied, not allowed.".to_string());
        }
        self.default_state = state;
        self.explicit_save()
    }

    /// Newest sighting first.
    pub fn list(&self) -> Vec<TrustEntry> {
        let mut list = self.devices.clone();
        list.sort_by(|a, b| b.last_seen_ms.cmp(&a.last_seen_ms));
        list
    }
}
