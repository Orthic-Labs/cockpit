//! Report-only cleanup plans, journal state & undo contracts.
//!
//! This module intentionally contains no filesystem, Trash or process code.
//! Platform adapters implement [`Revalidator`], [`CleanupExecutor`],
//! [`UndoRevalidator`] & [`UndoExecutor`] before a plan can have an effect.

use crate::store::{StateStore, VersionedRecord};
use crate::{
    model::{EntryKind, FileIdentity, VolumeIdentity},
    rules::CleanupRoute,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Component, Path, PathBuf},
};

pub const CLEANUP_SCHEMA_VERSION: u32 = 1;
pub const MAX_CLEANUP_ITEMS: usize = 10_000;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_UNIX_SECONDS: u64 = 253_402_300_799; // 9999-12-31T23:59:59Z

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CleanupAction {
    Trash,
    OwnerTool {
        executor: String,
        arguments: Vec<String>,
        preview: String,
    },
    Simctl {
        executor: String,
        arguments: Vec<String>,
        preview: String,
    },
}

impl CleanupAction {
    pub fn route(&self) -> CleanupRoute {
        match self {
            Self::Trash => CleanupRoute::Trash,
            Self::OwnerTool { .. } => CleanupRoute::OwnerTool,
            Self::Simctl { .. } => CleanupRoute::Simctl,
        }
    }
    pub fn reversible(&self) -> bool {
        matches!(self, Self::Trash)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EffectBinding {
    pub action: CleanupAction,
    pub rule_id: String,
    pub rule_version: u32,
    pub reversible: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupItem {
    pub path: PathBuf,
    pub volume: VolumeIdentity,
    pub identity: FileIdentity,
    pub kind: EntryKind,
    pub inspection_complete: bool,
    pub protected_descendant: bool,
    pub logical_bytes: Option<u64>,
    pub moved_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CleanupPlan {
    pub schema_version: u32,
    pub id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub effect: EffectBinding,
    pub items: Vec<CleanupItem>,
    /// Plans are immutable for execution only after review/seal.
    pub reviewed: bool,
    #[serde(skip)]
    sealed_binding: Option<PlanSeal>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlanSeal {
    id: String,
    created_at: u64,
    expires_at: u64,
    effect: EffectBinding,
    items: Vec<CleanupItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanError {
    InvalidId,
    InvalidExpiry,
    EmptyItems,
    MissingIdentity,
    IncompleteInspection(PathBuf),
    ProtectedDescendant(PathBuf),
    UnsupportedItem(PathBuf),
    OverlappingTargets(PathBuf, PathBuf),
    Unreviewed,
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId => f.write_str("invalid cleanup plan id"),
            Self::InvalidExpiry => f.write_str("cleanup plan expiry must follow creation"),
            Self::EmptyItems => f.write_str("cleanup plan has no items"),
            Self::MissingIdentity => f.write_str("cleanup item is missing volume or file identity"),
            Self::IncompleteInspection(p) => write!(f, "incomplete inspection: {}", p.display()),
            Self::ProtectedDescendant(p) => write!(f, "protected descendant: {}", p.display()),
            Self::UnsupportedItem(p) => write!(f, "unsupported item: {}", p.display()),
            Self::OverlappingTargets(a, b) => write!(
                f,
                "overlapping targets: {} and {}",
                a.display(),
                b.display()
            ),
            Self::Unreviewed => f.write_str("cleanup plan has not been reviewed"),
        }
    }
}
impl std::error::Error for PlanError {}

impl CleanupPlan {
    pub fn draft(
        id: impl Into<String>,
        created_at: u64,
        expires_at: u64,
        effect: EffectBinding,
        items: Vec<CleanupItem>,
    ) -> Result<Self, PlanError> {
        let id = id.into();
        if id.trim().is_empty() || id.len() > MAX_ID_BYTES || id.contains(['/', '\\']) {
            return Err(PlanError::InvalidId);
        }
        if expires_at <= created_at {
            return Err(PlanError::InvalidExpiry);
        }
        let items = normalize_items(items)?;
        Ok(Self {
            schema_version: CLEANUP_SCHEMA_VERSION,
            id,
            created_at,
            expires_at,
            effect,
            items,
            reviewed: false,
            sealed_binding: None,
        })
    }

    pub fn seal(mut self) -> Result<Self, PlanError> {
        self.validate()?;
        self.reviewed = true;
        self.sealed_binding = Some(self.seal_value());
        Ok(self)
    }

    fn seal_value(&self) -> PlanSeal {
        PlanSeal {
            id: self.id.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            effect: self.effect.clone(),
            items: self.items.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), PlanError> {
        if self.schema_version != CLEANUP_SCHEMA_VERSION {
            return Err(PlanError::InvalidId);
        }
        if self.id.trim().is_empty()
            || self.id.len() > MAX_ID_BYTES
            || self.expires_at <= self.created_at
            || self.created_at > MAX_UNIX_SECONDS
            || self.expires_at > MAX_UNIX_SECONDS
        {
            return Err(PlanError::InvalidExpiry);
        }
        if self.items.is_empty() || self.items.len() > MAX_CLEANUP_ITEMS {
            return Err(PlanError::EmptyItems);
        }
        if self.effect.reversible != self.effect.action.reversible()
            || self.effect.rule_id.trim().is_empty()
            || self.effect.rule_id.len() > MAX_ID_BYTES
        {
            return Err(PlanError::InvalidId);
        }
        match &self.effect.action {
            CleanupAction::Trash => {}
            CleanupAction::OwnerTool {
                executor,
                preview,
                arguments,
            }
            | CleanupAction::Simctl {
                executor,
                preview,
                arguments,
            } => {
                if executor.trim().is_empty()
                    || preview.trim().is_empty()
                    || executor.len() > MAX_ID_BYTES
                    || preview.len() > 64 * 1024
                    || arguments.len() > 256
                    || arguments.iter().any(|a| a.len() > 4096)
                {
                    return Err(PlanError::InvalidId);
                }
            }
        }
        let normalized = normalize_items(self.items.clone())?;
        if normalized != self.items {
            return Err(PlanError::InvalidId);
        }
        Ok(())
    }

    pub fn ensure_executable(&self, now: u64) -> Result<(), ClaimError> {
        self.validate().map_err(ClaimError::InvalidPlan)?;
        if !self.reviewed || self.sealed_binding.as_ref() != Some(&self.seal_value()) {
            return Err(ClaimError::Unreviewed);
        }
        if now >= self.expires_at {
            return Err(ClaimError::Expired);
        }
        Ok(())
    }
}

fn normalize_items(mut items: Vec<CleanupItem>) -> Result<Vec<CleanupItem>, PlanError> {
    if items.is_empty() || items.len() > MAX_CLEANUP_ITEMS {
        return Err(PlanError::EmptyItems);
    }
    for item in &items {
        if !durable_id(&item.volume.id)
            || !durable_id(&item.identity.id)
            || item.identity.volume != item.volume
        {
            return Err(PlanError::MissingIdentity);
        }
        if !item.inspection_complete {
            return Err(PlanError::IncompleteInspection(item.path.clone()));
        }
        if item.protected_descendant {
            return Err(PlanError::ProtectedDescendant(item.path.clone()));
        }
        if !item.path.is_absolute()
            || item.path.components().count() <= 1
            || item
                .path
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || matches!(item.kind, EntryKind::Symlink)
        {
            return Err(PlanError::UnsupportedItem(item.path.clone()));
        }
    }
    items.sort_by(|a, b| a.path.cmp(&b.path).then(a.identity.id.cmp(&b.identity.id)));
    items.dedup_by(|a, b| a.path == b.path && a.identity == b.identity);
    let mut kept: Vec<CleanupItem> = Vec::with_capacity(items.len());
    for item in items {
        if let Some(parent) = kept
            .iter()
            .find(|p| p.volume == item.volume && is_ancestor(&p.path, &item.path))
        {
            // Parent cleanup already covers child. A protected child is rejected
            // before this point, so silently dropping safe duplicate descendants
            // is deterministic and avoids double claims.
            if parent.identity == item.identity {
                continue;
            }
            continue;
        }
        if let Some(other) = kept
            .iter()
            .find(|p| p.volume == item.volume && p.path == item.path && p.identity != item.identity)
        {
            return Err(PlanError::OverlappingTargets(
                other.path.clone(),
                item.path.clone(),
            ));
        }
        if let Some(child) = kept
            .iter()
            .find(|p| p.volume == item.volume && is_ancestor(&item.path, &p.path))
        {
            return Err(PlanError::OverlappingTargets(item.path, child.path.clone()));
        }
        kept.push(item);
    }
    Ok(kept)
}

fn durable_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_ID_BYTES
        && !matches!(value.to_ascii_lowercase().as_str(), "unknown" | "ephemeral")
        && !value.starts_with("mount:")
}

fn is_ancestor(parent: &Path, child: &Path) -> bool {
    parent != child && child.starts_with(parent)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RevalidationError {
    Missing,
    IdentityChanged,
    VolumeChanged,
    KindChanged,
    ProtectedDescendant,
    IncompleteInspection,
    AncestorReplaced,
    Other(String),
}

pub trait Revalidator {
    fn revalidate(
        &mut self,
        plan: &CleanupPlan,
        item: &CleanupItem,
    ) -> Result<(), RevalidationError>;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ItemOutcome {
    MovedToTrash {
        trash: TrashIdentity,
        moved_bytes: u64,
    },
    OwnerToolCompleted {
        moved_bytes: u64,
    },
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
    },
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrashIdentity {
    pub volume: VolumeIdentity,
    pub identity: FileIdentity,
    pub path: PathBuf,
}

pub trait CleanupExecutor {
    fn execute(&mut self, effect: &EffectBinding, item: &CleanupItem) -> ItemOutcome;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum JournalState {
    Claimed,
    Completed,
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JournalItem {
    pub path: PathBuf,
    pub state: ItemJournalState,
    pub outcome: Option<ItemOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ItemJournalState {
    Planned,
    Started,
    Completed,
    Indeterminate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub plan_id: String,
    pub effect: EffectBinding,
    pub state: JournalState,
    pub items: Vec<JournalItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimError {
    Expired,
    AlreadyClaimed,
    Interrupted,
    EffectChanged,
    Unreviewed,
    InvalidPlan(PlanError),
    Persistence(String),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Journal {
    entries: BTreeMap<String, JournalEntry>,
}

impl Journal {
    pub fn claim(&mut self, plan: &CleanupPlan, now: u64) -> Result<(), ClaimError> {
        plan.ensure_executable(now)?;
        if let Some(existing) = self.entries.get(&plan.id) {
            if existing.effect != plan.effect {
                return Err(ClaimError::EffectChanged);
            }
            return match existing.state {
                JournalState::Interrupted => Err(ClaimError::Interrupted),
                JournalState::Claimed | JournalState::Completed => Err(ClaimError::AlreadyClaimed),
            };
        }
        self.entries.insert(
            plan.id.clone(),
            JournalEntry {
                plan_id: plan.id.clone(),
                effect: plan.effect.clone(),
                state: JournalState::Claimed,
                items: plan
                    .items
                    .iter()
                    .map(|i| JournalItem {
                        path: i.path.clone(),
                        state: ItemJournalState::Planned,
                        outcome: None,
                    })
                    .collect(),
            },
        );
        Ok(())
    }
    pub fn entry(&self, id: &str) -> Option<&JournalEntry> {
        self.entries.get(id)
    }
    fn mark_started(&mut self, id: &str, index: usize) {
        if let Some(e) = self.entries.get_mut(id) {
            if let Some(i) = e.items.get_mut(index) {
                i.state = ItemJournalState::Started;
            }
        }
    }
    fn mark_item(&mut self, id: &str, index: usize, outcome: ItemOutcome) {
        if let Some(e) = self.entries.get_mut(id) {
            if let Some(i) = e.items.get_mut(index) {
                i.state = if matches!(&outcome, ItemOutcome::Interrupted) {
                    ItemJournalState::Indeterminate
                } else {
                    ItemJournalState::Completed
                };
                i.outcome = Some(outcome);
            }
        }
    }
    fn finish(&mut self, id: &str, interrupted: bool) {
        if let Some(e) = self.entries.get_mut(id) {
            e.state = if interrupted {
                JournalState::Interrupted
            } else {
                JournalState::Completed
            };
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub plan_id: String,
    pub state: JournalState,
    pub items: Vec<ItemResult>,
    pub moved_bytes: u64,
    pub logical_bytes: u64,
    pub freed_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ItemResult {
    pub path: PathBuf,
    pub identity: FileIdentity,
    pub outcome: ItemOutcome,
}

pub fn apply<R: Revalidator, E: CleanupExecutor>(
    plan: &CleanupPlan,
    now: u64,
    journal: &mut Journal,
    revalidator: &mut R,
    executor: &mut E,
) -> Result<ApplyReport, ClaimError> {
    journal.claim(plan, now)?;
    let mut results = Vec::new();
    let mut moved = 0;
    let mut logical = 0;
    for (index, item) in plan.items.iter().enumerate() {
        journal.mark_started(&plan.id, index);
        if let Err(error) = revalidator.revalidate(plan, item) {
            let outcome = ItemOutcome::Failed {
                reason: format!("revalidation:{error:?}"),
            };
            journal.mark_item(&plan.id, index, outcome.clone());
            results.push(ItemResult {
                path: item.path.clone(),
                identity: item.identity.clone(),
                outcome,
            });
            continue;
        }
        let outcome = executor.execute(&plan.effect, item);
        if let ItemOutcome::MovedToTrash { moved_bytes, .. }
        | ItemOutcome::OwnerToolCompleted { moved_bytes } = &outcome
        {
            moved += *moved_bytes;
            logical += item.logical_bytes.unwrap_or(0);
        }
        let interrupted = matches!(&outcome, ItemOutcome::Interrupted);
        journal.mark_item(&plan.id, index, outcome.clone());
        results.push(ItemResult {
            path: item.path.clone(),
            identity: item.identity.clone(),
            outcome,
        });
        if interrupted {
            journal.finish(&plan.id, true);
            return Ok(ApplyReport {
                plan_id: plan.id.clone(),
                state: JournalState::Interrupted,
                items: results,
                moved_bytes: moved,
                logical_bytes: logical,
                freed_bytes: None,
            });
        }
    }
    journal.finish(&plan.id, false);
    Ok(ApplyReport {
        plan_id: plan.id.clone(),
        state: JournalState::Completed,
        items: results,
        moved_bytes: moved,
        logical_bytes: logical,
        freed_bytes: None,
    })
}

/// A journal whose claim and per-item transitions are immutable, durable
/// version records. Persisted entries contain effect/item facts only; the
/// in-memory sealed plan remains the source of authorization on every apply.
#[derive(Clone, Debug)]
pub struct DurableJournal {
    journal: Journal,
    store: StateStore,
    versions: BTreeMap<String, u64>,
}

impl DurableJournal {
    pub fn open(directory: &Path) -> io::Result<Self> {
        let store = StateStore::open(directory, "cleanup")?;
        let mut journal = Journal::default();
        let mut versions = BTreeMap::new();
        for VersionedRecord {
            schema_version: _,
            id,
            version,
            state,
        } in store.records::<JournalEntry>()?
        {
            if id != state.plan_id || version == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cleanup journal record identity/version mismatch",
                ));
            }
            if state.plan_id.trim().is_empty()
                || journal.entries.insert(id.clone(), state).is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate cleanup journal plan",
                ));
            }
            versions.insert(id, version);
        }
        Ok(Self {
            journal,
            store,
            versions,
        })
    }

    pub fn entry(&self, id: &str) -> Option<&JournalEntry> {
        self.journal.entry(id)
    }
    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    fn persist(&mut self, entry: &JournalEntry, version: u64) -> Result<(), ClaimError> {
        self.store
            .publish(&entry.plan_id, version, entry)
            .map(|_| ())
            .map_err(|error| ClaimError::Persistence(error.to_string()))?;
        self.versions.insert(entry.plan_id.clone(), version);
        Ok(())
    }

    fn next_version(&self, id: &str) -> u64 {
        self.versions
            .get(id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Claim is published before any revalidation or executor callback.
    pub fn claim(&mut self, plan: &CleanupPlan, now: u64) -> Result<(), ClaimError> {
        let previous = self.journal.clone();
        self.journal.claim(plan, now)?;
        let entry = self
            .journal
            .entry(&plan.id)
            .expect("claim inserted journal entry")
            .clone();
        if let Err(error) = self.persist(&entry, 1) {
            self.journal = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Durable equivalent of [`apply`]. A Started transition is committed
    /// before each item can reach user-owned revalidation/execution, and its
    /// outcome is committed immediately after that callback returns. Any
    /// interruption therefore stays indeterminate across restart.
    pub fn apply<R: Revalidator, E: CleanupExecutor>(
        &mut self,
        plan: &CleanupPlan,
        now: u64,
        revalidator: &mut R,
        executor: &mut E,
    ) -> Result<ApplyReport, ClaimError> {
        self.claim(plan, now)?;
        let mut results = Vec::new();
        let mut moved = 0;
        let mut logical = 0;
        for (index, item) in plan.items.iter().enumerate() {
            let previous = self.journal.clone();
            self.journal.mark_started(&plan.id, index);
            let started = self.journal.entry(&plan.id).expect("claimed entry").clone();
            let started_version = self.next_version(&plan.id);
            if let Err(error) = self.persist(&started, started_version) {
                self.journal = previous;
                return Err(error);
            }
            if let Err(error) = revalidator.revalidate(plan, item) {
                let outcome = ItemOutcome::Failed {
                    reason: format!("revalidation:{error:?}"),
                };
                self.journal.mark_item(&plan.id, index, outcome.clone());
                let entry = self.journal.entry(&plan.id).expect("claimed entry").clone();
                let version = self.next_version(&plan.id);
                self.persist(&entry, version)?;
                results.push(ItemResult {
                    path: item.path.clone(),
                    identity: item.identity.clone(),
                    outcome,
                });
                continue;
            }
            let outcome = executor.execute(&plan.effect, item);
            if let ItemOutcome::MovedToTrash { moved_bytes, .. }
            | ItemOutcome::OwnerToolCompleted { moved_bytes } = &outcome
            {
                moved += *moved_bytes;
                logical += item.logical_bytes.unwrap_or(0);
            }
            let interrupted = matches!(&outcome, ItemOutcome::Interrupted);
            self.journal.mark_item(&plan.id, index, outcome.clone());
            let entry = self.journal.entry(&plan.id).expect("claimed entry").clone();
            let version = self.next_version(&plan.id);
            self.persist(&entry, version)?;
            results.push(ItemResult {
                path: item.path.clone(),
                identity: item.identity.clone(),
                outcome,
            });
            if interrupted {
                self.journal.finish(&plan.id, true);
                let entry = self.journal.entry(&plan.id).expect("claimed entry").clone();
                let version = self.next_version(&plan.id);
                self.persist(&entry, version)?;
                return Ok(ApplyReport {
                    plan_id: plan.id.clone(),
                    state: JournalState::Interrupted,
                    items: results,
                    moved_bytes: moved,
                    logical_bytes: logical,
                    freed_bytes: None,
                });
            }
        }
        self.journal.finish(&plan.id, false);
        let entry = self.journal.entry(&plan.id).expect("claimed entry").clone();
        let version = self.next_version(&plan.id);
        self.persist(&entry, version)?;
        Ok(ApplyReport {
            plan_id: plan.id.clone(),
            state: JournalState::Completed,
            items: results,
            moved_bytes: moved,
            logical_bytes: logical,
            freed_bytes: None,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UndoItem {
    pub original_path: PathBuf,
    pub original_identity: FileIdentity,
    pub trash: TrashIdentity,
    pub logical_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UndoPlan {
    pub id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub items: Vec<UndoItem>,
    pub reviewed: bool,
    #[serde(skip)]
    sealed_binding: Option<UndoSeal>,
    #[serde(skip)]
    claimed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UndoSeal {
    id: String,
    created_at: u64,
    expires_at: u64,
    items: Vec<UndoItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UndoError {
    NoRestorableItems,
    InvalidExpiry,
    Unreviewed,
    Expired,
    Conflict,
    Error(String),
}

impl UndoPlan {
    pub fn from_apply(
        id: impl Into<String>,
        created_at: u64,
        expires_at: u64,
        report: &ApplyReport,
    ) -> Result<Self, UndoError> {
        if expires_at <= created_at
            || created_at > MAX_UNIX_SECONDS
            || expires_at > MAX_UNIX_SECONDS
        {
            return Err(UndoError::InvalidExpiry);
        }
        let mut items = Vec::new();
        for result in &report.items {
            if let ItemOutcome::MovedToTrash { trash, .. } = &result.outcome {
                if !valid_target_path(&result.path)
                    || !durable_id(&result.identity.id)
                    || !valid_target_path(&trash.path)
                    || trash.volume != trash.identity.volume
                    || !durable_id(&trash.identity.id)
                    || !durable_id(&trash.volume.id)
                {
                    return Err(UndoError::Error(
                        "invalid retained Trash identity or original binding".into(),
                    ));
                }
                items.push(UndoItem {
                    original_path: result.path.clone(),
                    original_identity: result.identity.clone(),
                    trash: trash.clone(),
                    logical_bytes: None,
                });
            }
        }
        if items.is_empty() {
            return Err(UndoError::NoRestorableItems);
        }
        let id = id.into();
        if id.trim().is_empty() || id.len() > MAX_ID_BYTES {
            return Err(UndoError::Error("invalid undo plan id".into()));
        }
        Ok(Self {
            id,
            created_at,
            expires_at,
            items,
            reviewed: false,
            sealed_binding: None,
            claimed: false,
        })
    }
    pub fn seal(mut self) -> Self {
        self.reviewed = true;
        self.sealed_binding = Some(self.seal_value());
        self
    }
    fn seal_value(&self) -> UndoSeal {
        UndoSeal {
            id: self.id.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            items: self.items.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UndoOutcome {
    Restored,
    ConflictOriginalOccupied,
    MissingTrash,
    IdentityChanged,
    Failed(String),
}

pub trait UndoRevalidator {
    fn revalidate(&mut self, item: &UndoItem) -> Result<(), UndoOutcome>;
}
pub trait UndoExecutor {
    fn restore(&mut self, item: &UndoItem) -> UndoOutcome;
}

pub fn apply_undo<R: UndoRevalidator, E: UndoExecutor>(
    plan: &mut UndoPlan,
    now: u64,
    revalidator: &mut R,
    executor: &mut E,
) -> Result<Vec<(PathBuf, UndoOutcome)>, UndoError> {
    if !plan.reviewed
        || plan.sealed_binding.as_ref() != Some(&plan.seal_value())
        || plan.items.is_empty()
        || plan.items.iter().any(|item| {
            !valid_target_path(&item.original_path)
                || !durable_id(&item.original_identity.id)
                || item.original_identity.volume != item.trash.identity.volume
                || !valid_target_path(&item.trash.path)
        })
    {
        return Err(UndoError::Unreviewed);
    }
    if plan.claimed {
        return Err(UndoError::Conflict);
    }
    if now >= plan.expires_at {
        return Err(UndoError::Expired);
    }
    plan.claimed = true;
    let mut out = Vec::new();
    for item in &plan.items {
        let outcome = match revalidator.revalidate(item) {
            Ok(()) => executor.restore(item),
            Err(outcome) => outcome,
        };
        out.push((item.original_path.clone(), outcome));
    }
    Ok(out)
}

/// Persisted undo facts intentionally exclude `UndoPlan`'s sealed binding.
/// A caller must present a freshly reviewed/sealed plan to claim an undo; a
/// restart can only recover outcomes and can never turn stored bytes into
/// authorization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UndoJournalEntry {
    pub plan_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub items: Vec<UndoItem>,
    pub claimed: bool,
    pub results: Vec<(PathBuf, UndoOutcome)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UndoRecoveryReport {
    pub plan_id: String,
    pub claimed: bool,
    pub results: Vec<(PathBuf, UndoOutcome)>,
}

#[derive(Clone, Debug)]
pub struct DurableUndoJournal {
    store: StateStore,
    entries: BTreeMap<String, (u64, UndoJournalEntry)>,
}

impl DurableUndoJournal {
    pub fn open(directory: &Path) -> io::Result<Self> {
        let store = StateStore::open(directory, "undo")?;
        let mut entries = BTreeMap::new();
        for VersionedRecord {
            schema_version: _,
            id,
            version,
            state,
        } in store.records::<UndoJournalEntry>()?
        {
            if id != state.plan_id || version == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "undo journal record identity/version mismatch",
                ));
            }
            if entries.insert(id, (version, state)).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate undo journal plan",
                ));
            }
        }
        Ok(Self { store, entries })
    }

    pub fn recovery(&self, id: &str) -> Option<UndoRecoveryReport> {
        self.entries.get(id).map(|(_, entry)| UndoRecoveryReport {
            plan_id: entry.plan_id.clone(),
            claimed: entry.claimed,
            results: entry.results.clone(),
        })
    }

    /// Save an undo candidate before any restore effect. Only facts needed for
    /// later comparison are stored; seal state is intentionally discarded.
    pub fn save_plan(&mut self, plan: &UndoPlan) -> Result<(), UndoError> {
        if !plan.reviewed || plan.sealed_binding.as_ref() != Some(&plan.seal_value()) {
            return Err(UndoError::Unreviewed);
        }
        let entry = UndoJournalEntry {
            plan_id: plan.id.clone(),
            created_at: plan.created_at,
            expires_at: plan.expires_at,
            items: plan.items.clone(),
            claimed: false,
            results: Vec::new(),
        };
        if self.entries.contains_key(&plan.id) {
            return Err(UndoError::Conflict);
        }
        self.store
            .publish(&plan.id, 1, &entry)
            .map_err(|error| UndoError::Error(error.to_string()))?;
        self.entries.insert(plan.id.clone(), (1, entry));
        Ok(())
    }

    fn publish(&mut self, entry: &UndoJournalEntry, version: u64) -> Result<(), UndoError> {
        self.store
            .publish(&entry.plan_id, version, entry)
            .map_err(|error| UndoError::Error(error.to_string()))?;
        self.entries
            .insert(entry.plan_id.clone(), (version, entry.clone()));
        Ok(())
    }

    /// Claim is persisted before the first revalidation/executor call. Every
    /// result is appended afterward, and loaded claimed entries are reported
    /// rather than replayed.
    pub fn apply<R: UndoRevalidator, E: UndoExecutor>(
        &mut self,
        plan: &UndoPlan,
        now: u64,
        revalidator: &mut R,
        executor: &mut E,
    ) -> Result<Vec<(PathBuf, UndoOutcome)>, UndoError> {
        if !plan.reviewed
            || plan.sealed_binding.as_ref() != Some(&plan.seal_value())
            || plan.items.is_empty()
        {
            return Err(UndoError::Unreviewed);
        }
        if now >= plan.expires_at {
            return Err(UndoError::Expired);
        }
        let (version, mut entry) = match self.entries.get(&plan.id).cloned() {
            Some(existing) => existing,
            None => {
                let candidate = UndoJournalEntry {
                    plan_id: plan.id.clone(),
                    created_at: plan.created_at,
                    expires_at: plan.expires_at,
                    items: plan.items.clone(),
                    claimed: false,
                    results: Vec::new(),
                };
                self.store
                    .publish(&plan.id, 1, &candidate)
                    .map_err(|error| UndoError::Error(error.to_string()))?;
                (1, candidate)
            }
        };
        if entry.created_at != plan.created_at
            || entry.expires_at != plan.expires_at
            || entry.items != plan.items
        {
            return Err(UndoError::Conflict);
        }
        if entry.claimed {
            return Err(UndoError::Conflict);
        }
        entry.claimed = true;
        self.publish(&entry, version.saturating_add(1))?;
        let mut out = Vec::new();
        for item in &plan.items {
            let outcome = match revalidator.revalidate(item) {
                Ok(()) => executor.restore(item),
                Err(outcome) => outcome,
            };
            out.push((item.original_path.clone(), outcome.clone()));
            entry.results.push((item.original_path.clone(), outcome));
            let next = self
                .entries
                .get(&plan.id)
                .map(|(current, _)| current.saturating_add(1))
                .unwrap_or(1);
            self.publish(&entry, next)?;
        }
        Ok(out)
    }
}

fn valid_target_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().count() > 1
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}
