//! Idempotent cleanup activity accounting.
//!
//! Timestamps are Unix seconds, hence UTC. Observed volume deltas are kept
//! separate from bytes moved/logical bytes: moving an item to Trash is never
//! reported as freed space, and restoring an item contributes no reclaimed
//! bytes.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, path::Path};

use crate::store::{StateStore, VersionedRecord};

pub const MAX_ACTIVITY_EVENTS: usize = 100_000;
pub const MAX_ACTIVITY_ID_BYTES: usize = 128;
pub const MAX_UNIX_SECONDS: u64 = 253_402_300_799;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivityKind {
    CleanupMoved {
        moved_bytes: u64,
        logical_bytes: u64,
    },
    CleanupRestored {
        logical_bytes: u64,
    },
    Scan {
        logical_bytes: u64,
        attributed_bytes: u64,
    },
    Uninstall {
        moved_bytes: u64,
        logical_bytes: u64,
    },
    Compression {
        #[serde(with = "i128_string")]
        logical_delta_bytes: i128,
    },
    VolumeObservation {
        #[serde(with = "i128_string")]
        used_delta_bytes: i128,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: String,
    pub occurred_at: u64,
    pub kind: ActivityKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityError {
    EmptyId,
    DuplicateConflict,
    InvalidTime,
    Capacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordStatus {
    Inserted,
    DuplicateIgnored,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActivityLedger {
    events: BTreeMap<String, ActivityEvent>,
}

impl ActivityLedger {
    pub fn record(&mut self, event: ActivityEvent) -> Result<RecordStatus, ActivityError> {
        if event.id.trim().is_empty() || event.id.len() > MAX_ACTIVITY_ID_BYTES {
            return Err(ActivityError::EmptyId);
        }
        if event.occurred_at > MAX_UNIX_SECONDS {
            return Err(ActivityError::InvalidTime);
        }
        if let Some(existing) = self.events.get(&event.id) {
            return if existing == &event {
                Ok(RecordStatus::DuplicateIgnored)
            } else {
                Err(ActivityError::DuplicateConflict)
            };
        }
        if self.events.len() >= MAX_ACTIVITY_EVENTS {
            return Err(ActivityError::Capacity);
        }
        self.events.insert(event.id.clone(), event);
        Ok(RecordStatus::Inserted)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn events(&self) -> impl Iterator<Item = &ActivityEvent> {
        self.events.values()
    }

    pub fn totals(&self, window: UtcWindow) -> ActivityTotals {
        let mut totals = ActivityTotals {
            window,
            ..ActivityTotals::default()
        };
        for event in self
            .events
            .values()
            .filter(|e| window.contains(e.occurred_at))
        {
            match &event.kind {
                ActivityKind::CleanupMoved {
                    moved_bytes,
                    logical_bytes,
                } => {
                    totals.overflowed |= add_u64(&mut totals.moved_bytes, *moved_bytes);
                    totals.overflowed |= add_u64(&mut totals.logical_bytes, *logical_bytes);
                }
                // Restored bytes intentionally do not subtract from moved or
                // reclaimed totals: restoration is its own observed outcome.
                ActivityKind::CleanupRestored { .. } => {
                    totals.overflowed |= add_u64(&mut totals.restored_events, 1);
                }
                ActivityKind::Scan {
                    logical_bytes,
                    attributed_bytes,
                } => {
                    totals.overflowed |= add_u64(&mut totals.scanned_logical_bytes, *logical_bytes);
                    totals.overflowed |=
                        add_u64(&mut totals.scanned_attributed_bytes, *attributed_bytes);
                    totals.overflowed |= add_u64(&mut totals.scan_events, 1);
                }
                ActivityKind::Uninstall {
                    moved_bytes,
                    logical_bytes,
                } => {
                    totals.overflowed |= add_u64(&mut totals.uninstalled_moved_bytes, *moved_bytes);
                    totals.overflowed |=
                        add_u64(&mut totals.uninstalled_logical_bytes, *logical_bytes);
                    totals.overflowed |= add_u64(&mut totals.uninstall_events, 1);
                }
                ActivityKind::Compression {
                    logical_delta_bytes,
                } => {
                    totals.overflowed |= add_i128(
                        &mut totals.compression_logical_delta_bytes,
                        *logical_delta_bytes,
                    );
                    totals.overflowed |= add_u64(&mut totals.compression_events, 1);
                }
                ActivityKind::VolumeObservation { used_delta_bytes } => {
                    totals.overflowed |=
                        add_i128(&mut totals.observed_volume_delta_bytes, *used_delta_bytes);
                    if *used_delta_bytes < 0 {
                        let absolute = used_delta_bytes.unsigned_abs();
                        if absolute > u128::from(u64::MAX) {
                            totals.overflowed = true;
                        }
                        totals.overflowed |= add_u64(
                            &mut totals.observed_reclaimed_bytes,
                            absolute.min(u128::from(u64::MAX)) as u64,
                        );
                    }
                }
            }
            totals.overflowed |= add_u64(&mut totals.events, 1);
        }
        totals
    }

    pub fn weekly(&self, timestamp: u64) -> ActivityTotals {
        self.totals(week_window_utc(timestamp))
    }
    pub fn monthly(&self, timestamp: u64) -> ActivityTotals {
        self.totals(month_window_utc(timestamp))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UtcWindow {
    pub start: u64,
    pub end: u64,
}

impl UtcWindow {
    pub fn new(start: u64, end: u64) -> Option<Self> {
        (start < end).then_some(Self { start, end })
    }
    pub fn contains(self, timestamp: u64) -> bool {
        timestamp >= self.start && timestamp < self.end
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivityTotals {
    pub window: UtcWindow,
    pub events: u64,
    pub moved_bytes: u64,
    pub logical_bytes: u64,
    pub observed_volume_delta_bytes: i128,
    pub observed_reclaimed_bytes: u64,
    pub restored_events: u64,
    pub scanned_logical_bytes: u64,
    pub scanned_attributed_bytes: u64,
    pub scan_events: u64,
    pub uninstalled_moved_bytes: u64,
    pub uninstalled_logical_bytes: u64,
    pub uninstall_events: u64,
    #[serde(with = "i128_string")]
    pub compression_logical_delta_bytes: i128,
    pub compression_events: u64,
    pub overflowed: bool,
}

fn add_u64(target: &mut u64, value: u64) -> bool {
    match target.checked_add(value) {
        Some(sum) => {
            *target = sum;
            false
        }
        None => {
            *target = u64::MAX;
            true
        }
    }
}
fn add_i128(target: &mut i128, value: i128) -> bool {
    match target.checked_add(value) {
        Some(sum) => {
            *target = sum;
            false
        }
        None => {
            *target = if value.is_negative() {
                i128::MIN
            } else {
                i128::MAX
            };
            true
        }
    }
}

mod i128_string {
    use serde::{Deserialize, Deserializer, Serializer};
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wide {
        String(String),
        Number(i128),
    }
    pub fn serialize<S: Serializer>(value: &i128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i128, D::Error> {
        match Wide::deserialize(deserializer)? {
            Wide::String(value) => value.parse().map_err(serde::de::Error::custom),
            Wide::Number(value) => Ok(value),
        }
    }
}

pub fn week_window_utc(timestamp: u64) -> UtcWindow {
    let days = (timestamp / 86_400) as i64;
    let monday = days - (days + 3).rem_euclid(7);
    UtcWindow {
        start: (monday.max(0) as u64).saturating_mul(86_400),
        end: ((monday + 7).max(0) as u64).saturating_mul(86_400),
    }
}

pub fn month_window_utc(timestamp: u64) -> UtcWindow {
    let (year, month, _) = civil_from_days((timestamp / 86_400) as i64);
    let start_days = days_from_civil(year, month, 1);
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end_days = days_from_civil(next_year, next_month, 1);
    UtcWindow {
        start: (start_days.max(0) as u64).saturating_mul(86_400),
        end: (end_days.max(0) as u64).saturating_mul(86_400),
    }
}

/// Errors from the durable activity projection. The in-memory ledger remains
/// available for callers that do not opt into persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityPersistenceError {
    Activity(ActivityError),
    Storage(String),
}

impl std::fmt::Display for ActivityPersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Activity(error) => write!(f, "activity: {error:?}"),
            Self::Storage(error) => write!(f, "activity storage: {error}"),
        }
    }
}
impl std::error::Error for ActivityPersistenceError {}
impl From<ActivityError> for ActivityPersistenceError {
    fn from(error: ActivityError) -> Self {
        Self::Activity(error)
    }
}
impl From<io::Error> for ActivityPersistenceError {
    fn from(error: io::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

/// Activity ledger backed by immutable version-one records. Loading verifies
/// every record before exposing the projection; recording publishes first, so
/// a failed write cannot make memory claim an event that will be absent after
/// restart.
#[derive(Clone, Debug)]
pub struct DurableActivityLedger {
    ledger: ActivityLedger,
    store: StateStore,
}

impl DurableActivityLedger {
    pub fn open(directory: &Path) -> io::Result<Self> {
        let store = StateStore::open(directory, "activity")?;
        let mut ledger = ActivityLedger::default();
        for VersionedRecord {
            schema_version: _,
            id,
            version,
            state,
        } in store.records::<ActivityEvent>()?
        {
            if version != 1 || id != state.id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "activity record identity/version mismatch",
                ));
            }
            ledger.record(state).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid activity record: {error:?}"),
                )
            })?;
        }
        Ok(Self { ledger, store })
    }

    pub fn record(
        &mut self,
        event: ActivityEvent,
    ) -> Result<RecordStatus, ActivityPersistenceError> {
        let mut candidate = self.ledger.clone();
        let status = candidate.record(event.clone())?;
        if status == RecordStatus::DuplicateIgnored {
            return Ok(status);
        }
        match self.store.publish(&event.id, 1, &event) {
            Ok(_) => {
                self.ledger = candidate;
                Ok(RecordStatus::Inserted)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                match self.store.load::<ActivityEvent>(&event.id)? {
                    Some(existing) if existing.state == event => Ok({
                        self.ledger = candidate;
                        RecordStatus::DuplicateIgnored
                    }),
                    _ => Err(ActivityPersistenceError::Activity(
                        ActivityError::DuplicateConflict,
                    )),
                }
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn ledger(&self) -> &ActivityLedger {
        &self.ledger
    }
    pub fn len(&self) -> usize {
        self.ledger.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ledger.is_empty()
    }
    pub fn events(&self) -> impl Iterator<Item = &ActivityEvent> {
        self.ledger.events()
    }
    pub fn totals(&self, window: UtcWindow) -> ActivityTotals {
        self.ledger.totals(window)
    }
    pub fn weekly(&self, timestamp: u64) -> ActivityTotals {
        self.ledger.weekly(timestamp)
    }
    pub fn monthly(&self, timestamp: u64) -> ActivityTotals {
        self.ledger.monthly(timestamp)
    }
}

// Howard Hinnant's civil calendar conversion, expressed without a clock or
// timezone dependency. `days` is days since 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }).div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let d = doy - (153 * mp + 2).div_euclid(5) + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y as i32, m as u32, d as u32)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = (if y >= 0 { y } else { y - 399 }).div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from(month) + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2).div_euclid(5) + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
