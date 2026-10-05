//! Declarative, report-only cleanup rules.
//!
//! This module deliberately does not inspect the filesystem or processes.  A
//! scanner supplies `ScanMetadata`; rule evaluation turns that evidence into
//! a finding.  Missing evidence is represented as `None` and is never treated
//! as permission to clean anything.

use serde::{Deserialize, Serialize};

pub const RULE_SCHEMA_VERSION: u32 = 1;

/// Process/use state used by the report and cleanup safety boundary.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    InUse,
    NotInUse,
    #[default]
    Unknown,
}

/// Risk is intentionally conservative.  `Explanation` is informational and
/// has no cleanup action; every actionable initial rule is `Review`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    #[default]
    Review,
    Explanation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupRoute {
    Trash,
    OwnerTool,
    Simctl,
    None,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    CloneAwareUniqueBytes,
    AttributedAllocation,
    LogicalBytes,
    ExplanationOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipCheck {
    ExactPath,
    ChromeCloneDirectory,
    OwnerToolState,
    AppBundleBackup,
    InstalledAppHistory,
    AppManaged,
    ExplanationOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeScope {
    AnyMountedLocal,
    StartupVolume,
    ExternalVolume,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathState {
    Present,
    Absent,
    Inaccessible,
    #[default]
    Unknown,
}

/// Named evidence fields keep rules declarative while making every safety
/// prerequisite auditable in JSON.  A missing boolean is unknown.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKey {
    VolumeMounted,
    PathPresent,
    InspectionComplete,
    OwnershipConfirmed,
    NoProtectedDescendant,
    NoSourceRepository,
    NoUserData,
    NoCloudPlaceholder,
    LivenessNotInUse,
    AgeThresholdMet,
    ChromeFamilyStopped,
    ActiveResidentsStopped,
    OwnerStateComplete,
    SourceRootConfirmedAbsent,
    OwnerLeaseInactive,
    GeneratorStopped,
    ReplacementNewer,
    ModelCacheRedownloadable,
    AppNotInstalled,
    AppProcessStopped,
    InventoryComplete,
    SimulatorNotInUse,
}

/// The scan contract is data-only.  `Option<bool>` is important: false means
/// measured absence, while None means unavailable evidence.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScanMetadata {
    pub path: String,
    #[serde(default)]
    pub volume_id: Option<String>,
    #[serde(default)]
    pub is_startup_volume: Option<bool>,
    #[serde(default)]
    pub is_external_volume: Option<bool>,
    #[serde(default)]
    pub volume_mounted: Option<bool>,
    #[serde(default)]
    pub path_state: PathState,
    #[serde(default)]
    pub age_days: Option<u64>,
    #[serde(default)]
    pub logical_bytes: Option<u64>,
    #[serde(default)]
    pub attributed_bytes: Option<u64>,
    #[serde(default)]
    pub unique_bytes: Option<u64>,
    /// Bounds supplied by the scanner.  The rule layer never invents a
    /// reclaim value from logical or attributed bytes.
    #[serde(default)]
    pub deletion_estimate_lower: Option<u64>,
    #[serde(default)]
    pub deletion_estimate_upper: Option<u64>,
    #[serde(default)]
    pub liveness: Liveness,
    #[serde(default)]
    pub evidence: ScanEvidence,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScanEvidence {
    #[serde(default)]
    pub inspection_complete: Option<bool>,
    #[serde(default)]
    pub ownership_confirmed: Option<bool>,
    #[serde(default)]
    pub protected_descendant: Option<bool>,
    #[serde(default)]
    pub source_repository: Option<bool>,
    #[serde(default)]
    pub user_data: Option<bool>,
    #[serde(default)]
    pub cloud_placeholder: Option<bool>,
    #[serde(default)]
    pub chrome_family_running: Option<bool>,
    #[serde(default)]
    pub active_residents: Option<bool>,
    #[serde(default)]
    pub owner_state_complete: Option<bool>,
    #[serde(default)]
    pub source_root_exists: Option<bool>,
    #[serde(default)]
    pub owner_lease_active: Option<bool>,
    #[serde(default)]
    pub generating_tool_running: Option<bool>,
    #[serde(default)]
    pub replacement_newer: Option<bool>,
    #[serde(default)]
    pub model_cache_redownloadable: Option<bool>,
    #[serde(default)]
    pub app_installed: Option<bool>,
    #[serde(default)]
    pub app_process_running: Option<bool>,
    #[serde(default)]
    pub inventory_complete: Option<bool>,
    #[serde(default)]
    pub simulator_in_use: Option<bool>,
}

/// Short names used by storage adapters that keep scan rows and liveness
/// evidence in separate records.
pub type ScanReport = ScanMetadata;
pub type Evidence = ScanEvidence;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Rule {
    pub schema_version: u32,
    pub rule_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub path_patterns: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<VolumeScope>,
    pub ownership: OwnershipCheck,
    /// Liveness-specific evidence, kept separate from general eligibility.
    #[serde(alias = "liveness_check")]
    #[serde(default)]
    pub liveness: Vec<EvidenceKey>,
    #[serde(default)]
    pub eligibility: Vec<EvidenceKey>,
    #[serde(default)]
    pub age_threshold_days: Option<u64>,
    pub measurement: Measurement,
    pub route: CleanupRoute,
    #[serde(default)]
    pub route_detail: Option<String>,
    #[serde(default)]
    pub risk: Risk,
    #[serde(default = "default_report_only")]
    pub report_only: bool,
    #[serde(default)]
    pub explanation: Option<String>,
}

fn default_report_only() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RulePack {
    pub schema_version: u32,
    pub rule_pack_version: u32,
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Finding {
    pub id: String,
    pub rule_id: String,
    pub rule_version: u32,
    pub path: String,
    #[serde(default)]
    pub volume_id: Option<String>,
    pub liveness: Liveness,
    pub risk: Risk,
    pub route: CleanupRoute,
    pub eligible: bool,
    pub reasons: Vec<String>,
    /// Names of evidence signals that were unavailable (None) for this row.
    /// Non-empty means the finding is report-only because of missing data.
    #[serde(default)]
    pub unknown_signals: Vec<String>,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub route_detail: Option<String>,
    pub logical_bytes: Option<u64>,
    pub attributed_bytes: Option<u64>,
    pub unique_bytes: Option<u64>,
    /// Reported as an estimate only; apply is outside this report-only module.
    pub deletion_estimate_lower: Option<u64>,
    pub deletion_estimate_upper: Option<u64>,
}

impl RulePack {
    /// Static consistency check of a rule pack.  Returns human-readable
    /// problems; an empty list means the pack is structurally safe.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.schema_version != RULE_SCHEMA_VERSION {
            problems.push(format!("pack schema_version {}", self.schema_version));
        }
        let mut seen = std::collections::BTreeSet::new();
        for rule in &self.rules {
            if !seen.insert(rule.id.clone()) {
                problems.push(format!("{}: duplicate rule id", rule.id));
            }
            problems.extend(rule.validate());
        }
        problems
    }
}

impl Rule {
    /// A rule can only ever make a row cleanup-eligible when it has a route,
    /// a measurement and review-class risk.
    pub fn is_actionable(&self) -> bool {
        self.route != CleanupRoute::None
            && self.measurement != Measurement::ExplanationOnly
            && self.risk != Risk::Explanation
    }

    pub fn validate(&self) -> Vec<String> {
        let mut p = Vec::new();
        let id = &self.id;
        if self.schema_version != RULE_SCHEMA_VERSION {
            p.push(format!("{id}: schema_version {}", self.schema_version));
        }
        if !self.report_only {
            p.push(format!("{id}: report_only must stay true"));
        }
        if self.is_actionable() {
            if self.path_patterns.is_empty() {
                p.push(format!("{id}: actionable rule has no path patterns"));
            }
            for pattern in &self.path_patterns {
                let norm = normalize_path(pattern);
                let first = norm.split('/').next().unwrap_or("");
                if !pattern.starts_with('/') || first.is_empty() || first.contains(['*', '?']) {
                    p.push(format!("{id}: pattern must be absolute with literal root: {pattern}"));
                }
                if normalize_path(pattern).split('/').any(|s| s == "..") {
                    p.push(format!("{id}: pattern contains ..: {pattern}"));
                }
            }
            if self.ownership == OwnershipCheck::ExplanationOnly {
                p.push(format!("{id}: actionable rule needs an ownership check"));
            }
            if !self.liveness.contains(&EvidenceKey::LivenessNotInUse) {
                p.push(format!("{id}: liveness must include liveness_not_in_use"));
            }
            if self.age_threshold_days.is_none() {
                p.push(format!("{id}: actionable rule needs age_threshold_days"));
            }
            if self.risk != Risk::Review {
                p.push(format!("{id}: actionable risk must be review"));
            }
        } else {
            if self.risk != Risk::Explanation {
                p.push(format!("{id}: non-actionable rule must have risk explanation"));
            }
            if self.route != CleanupRoute::None || self.measurement != Measurement::ExplanationOnly {
                p.push(format!("{id}: explanation rule needs route none and explanation_only"));
            }
            if !self.path_patterns.is_empty() {
                p.push(format!("{id}: explanation rule must not match paths"));
            }
            if self.explanation.as_deref().is_none_or(|t| t.trim().is_empty()) {
                p.push(format!("{id}: explanation text missing"));
            }
        }
        p
    }

    /// Return a finding only when path patterns and known volume scope match.
    /// Anything unknown, contradictory, in use, incomplete, placeholder or
    /// protected stays report-only with a specific reason.
    pub fn evaluate(&self, scan: &ScanMetadata) -> Option<Finding> {
        // Explanation rules describe non-file resources (swap, snapshots,
        // app-managed media) and never attach to filesystem rows.
        if self.path_patterns.is_empty() {
            return None;
        }
        if !self
            .path_patterns
            .iter()
            .any(|pattern| path_matches(pattern, &scan.path))
        {
            return None;
        }
        if !volume_matches(&self.volumes, scan) {
            return None;
        }

        let liveness = effective_liveness(scan);
        let mut out = Outcome::default();

        if !self.is_actionable() {
            out.note("not_actionable:explanation_only".to_string());
        } else {
            // Rule-declared requirements first, then the unconditional
            // baseline; every signal is judged so all unknowns are listed.
            for key in self.liveness.iter().chain(self.eligibility.iter()) {
                out.judge(*key, self, scan, liveness);
            }
            for key in [
                EvidenceKey::VolumeMounted,
                EvidenceKey::PathPresent,
                EvidenceKey::InspectionComplete,
                EvidenceKey::OwnershipConfirmed,
                EvidenceKey::NoProtectedDescendant,
                EvidenceKey::NoSourceRepository,
                EvidenceKey::NoUserData,
                EvidenceKey::NoCloudPlaceholder,
                EvidenceKey::LivenessNotInUse,
            ] {
                out.judge(key, self, scan, liveness);
            }
            if self.age_threshold_days.is_some() {
                out.judge(EvidenceKey::AgeThresholdMet, self, scan, liveness);
            }

            // Volume scope and identity.
            match scope_state(&self.volumes, scan) {
                Some(true) => {}
                Some(false) => out.note("evidence_failed:volume_scope".to_string()),
                None => {
                    out.unknown.push("volume_scope".to_string());
                    out.note(format!("evidence_unknown:volume_scope:{}", scope_unknowns(&self.volumes, scan)));
                }
            }
            if scan.is_startup_volume == Some(true) && scan.is_external_volume == Some(true) {
                out.note("evidence_conflict:volume_startup_and_external".to_string());
            }
            if scan.volume_id.as_deref().is_none_or(|v| v.trim().is_empty()) {
                out.unknown.push("volume_id".to_string());
                out.note("evidence_unknown:volume_id".to_string());
            }

            // Path shape: relative or traversing paths are never actionable.
            if let Some(problem) = path_problem(&scan.path) {
                out.note(format!("path_not_canonical:{problem}"));
            }

            // Contradictions between the scanner's liveness and its signals.
            if scan.liveness == Liveness::NotInUse {
                let e = &scan.evidence;
                for (name, value) in [
                    ("chrome_family_running", e.chrome_family_running),
                    ("active_residents", e.active_residents),
                    ("generating_tool_running", e.generating_tool_running),
                    ("app_process_running", e.app_process_running),
                    ("simulator_in_use", e.simulator_in_use),
                    ("owner_lease_active", e.owner_lease_active),
                ] {
                    if value == Some(true) {
                        out.note(format!("evidence_conflict:not_in_use_but_{name}"));
                    }
                }
            }
            if scan.evidence.replacement_newer == Some(true)
                && scan.evidence.app_installed == Some(false)
            {
                out.note("evidence_conflict:replacement_newer_but_app_not_installed".to_string());
            }

            // The size the rule measures must be known.
            let (size, name) = match self.measurement {
                Measurement::CloneAwareUniqueBytes => (scan.unique_bytes, "unique_bytes"),
                Measurement::AttributedAllocation => (scan.attributed_bytes, "attributed_bytes"),
                Measurement::LogicalBytes => (scan.logical_bytes, "logical_bytes"),
                Measurement::ExplanationOnly => (None, "explanation_only"),
            };
            if size.is_none() {
                out.unknown.push(name.to_string());
                out.note(format!("size_unknown:{name}"));
            }
        }

        // Inverted reclaim bounds are reported as unknown, never shown.
        let (mut lower, mut upper) = (scan.deletion_estimate_lower, scan.deletion_estimate_upper);
        if let (Some(l), Some(u)) = (lower, upper)
            && l > u
        {
            lower = None;
            upper = None;
            out.note("estimate_inconsistent:lower_exceeds_upper".to_string());
        }

        let eligible = out.ok && self.is_actionable();
        let mut reasons = out.reasons;
        if !eligible && reasons.is_empty() {
            reasons.push("report_only".to_string());
        }
        let mut unknown_signals = out.unknown;
        unknown_signals.sort();
        unknown_signals.dedup();

        Some(Finding {
            id: stable_finding_id(&self.id, &scan.path),
            rule_id: self.id.clone(),
            rule_version: self.rule_version,
            path: scan.path.clone(),
            volume_id: scan.volume_id.clone(),
            liveness,
            risk: self.risk,
            route: self.route,
            eligible,
            reasons,
            unknown_signals,
            explanation: self.explanation.clone(),
            route_detail: self.route_detail.clone(),
            logical_bytes: scan.logical_bytes,
            attributed_bytes: scan.attributed_bytes,
            unique_bytes: scan.unique_bytes,
            deletion_estimate_lower: lower,
            deletion_estimate_upper: upper,
        })
    }
}

struct Outcome {
    ok: bool,
    reasons: Vec<String>,
    unknown: Vec<String>,
    evaluated: Vec<EvidenceKey>,
}

impl Default for Outcome {
    fn default() -> Self {
        Self {
            ok: true,
            reasons: Vec::new(),
            unknown: Vec::new(),
            evaluated: Vec::new(),
        }
    }
}

impl Outcome {
    fn note(&mut self, reason: String) {
        self.ok = false;
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    fn judge(&mut self, key: EvidenceKey, rule: &Rule, scan: &ScanMetadata, liveness: Liveness) {
        if self.evaluated.contains(&key) {
            return;
        }
        self.evaluated.push(key);
        let name = evidence_name(key);
        if key == EvidenceKey::AgeThresholdMet && rule.age_threshold_days.is_none() {
            self.note("rule_invalid:age_threshold_missing".to_string());
            return;
        }
        match evidence(key, rule, scan, liveness) {
            Some(true) => {}
            Some(false) => {
                if key == EvidenceKey::AgeThresholdMet {
                    self.note(format!(
                        "age_below_threshold:{}<{}",
                        scan.age_days.unwrap_or(0),
                        rule.age_threshold_days.unwrap_or(0)
                    ));
                } else if name.starts_with("no_") {
                    self.note(format!("protected:{name}"));
                } else {
                    self.note(format!("evidence_failed:{name}"));
                }
            }
            None => {
                self.unknown.push(name.to_string());
                self.note(format!("evidence_unknown:{name}{}", unknown_cause(key, scan)));
            }
        }
    }
}

fn unknown_cause(key: EvidenceKey, scan: &ScanMetadata) -> String {
    match key {
        EvidenceKey::PathPresent if scan.path_state == PathState::Inaccessible => {
            ":path_inaccessible".into()
        }
        EvidenceKey::PathPresent => ":path_state_unknown".into(),
        EvidenceKey::LivenessNotInUse => format!(":{}", liveness_cause(scan)),
        EvidenceKey::AgeThresholdMet => ":age_days".into(),
        _ => String::new(),
    }
}

fn liveness_cause(scan: &ScanMetadata) -> &'static str {
    if scan.volume_mounted == Some(false) {
        "volume_not_mounted"
    } else if scan.volume_mounted.is_none() {
        "volume_mounted_unknown"
    } else if scan.path_state == PathState::Inaccessible {
        "path_inaccessible"
    } else if scan.path_state == PathState::Absent {
        "path_absent"
    } else if scan.path_state == PathState::Unknown {
        "path_state_unknown"
    } else if scan.evidence.inspection_complete != Some(true) {
        "inspection_incomplete_or_unknown"
    } else {
        "scanner_liveness_not_reported"
    }
}

/// Evaluate every rule against one explicitly supplied scan row.
pub fn detect(rules: &[Rule], scan: &ScanMetadata) -> Vec<Finding> {
    rules
        .iter()
        .filter_map(|rule| rule.evaluate(scan))
        .collect()
}

/// Adapter entry point for storage implementations that receive evidence as
/// a separate value.  It still only evaluates supplied data.
pub fn evaluate_scan(rules: &[Rule], scan: &ScanReport, evidence: &Evidence) -> Vec<Finding> {
    let mut row = scan.clone();
    row.evidence = evidence.clone();
    detect(rules, &row)
}

/// Evaluate a batch of explicitly supplied scan rows.  Output is sorted by
/// finding id and is independent of input order.  Rows that produce the same
/// id but differ in content are merged conservatively into one ineligible
/// finding.  No filesystem or process inspection happens here.
pub fn evaluate_all(rules: &[Rule], scans: &[ScanMetadata]) -> Vec<Finding> {
    let mut groups: std::collections::BTreeMap<String, Vec<(bool, String, Finding)>> =
        std::collections::BTreeMap::new();
    for finding in scans.iter().flat_map(|scan| detect(rules, scan)) {
        let json = serde_json::to_string(&finding).unwrap_or_default();
        groups
            .entry(finding.id.clone())
            .or_default()
            .push((finding.eligible, json, finding));
    }
    groups
        .into_values()
        .map(|mut group| {
            group.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
            let differs = group.iter().any(|g| g.1 != group[0].1);
            let mut pick = group.swap_remove(0).2;
            if differs {
                pick.eligible = false;
                let reason = "evidence_conflict:duplicate_scan_rows".to_string();
                if !pick.reasons.contains(&reason) {
                    pick.reasons.push(reason);
                }
            }
            pick
        })
        .collect()
}

/// Stable across runs and independent of process order.  FNV-1a is used only
/// as a compact identifier; it is not a security or content-integrity hash.
/// Separators and `.`/empty segments are normalised; case is preserved and a
/// leading separator is kept so absolute and relative paths differ.
pub fn stable_finding_id(rule_id: &str, path: &str) -> String {
    let lead = if path.starts_with('/') || path.starts_with('\\') {
        "/"
    } else {
        ""
    };
    let key = format!("{}\0{}{}", rule_id, lead, normalize_path(path));
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("finding:{rule_id}:{hash:016x}")
}

fn effective_liveness(scan: &ScanMetadata) -> Liveness {
    // An observed in-use state is never discarded.
    if scan.liveness == Liveness::InUse {
        return Liveness::InUse;
    }
    if scan.volume_mounted != Some(true)
        || scan.path_state == PathState::Inaccessible
        || scan.path_state == PathState::Unknown
        || scan.path_state == PathState::Absent
        || scan.evidence.inspection_complete != Some(true)
    {
        Liveness::Unknown
    } else {
        scan.liveness
    }
}

/// A row is in scope unless a scope flag is known to exclude it.
fn volume_matches(scopes: &[VolumeScope], scan: &ScanMetadata) -> bool {
    scopes.is_empty() || scopes.iter().any(|s| scope_flag(*s, scan) != Some(false))
}

fn scope_flag(scope: VolumeScope, scan: &ScanMetadata) -> Option<bool> {
    match scope {
        VolumeScope::AnyMountedLocal => Some(true),
        VolumeScope::StartupVolume => scan.is_startup_volume,
        VolumeScope::ExternalVolume => scan.is_external_volume,
    }
}

/// Some(true) only when a declared scope is positively confirmed.
fn scope_state(scopes: &[VolumeScope], scan: &ScanMetadata) -> Option<bool> {
    if scopes.is_empty() {
        return Some(true);
    }
    let flags: Vec<Option<bool>> = scopes.iter().map(|s| scope_flag(*s, scan)).collect();
    if flags.contains(&Some(true)) {
        Some(true)
    } else if flags.contains(&None) {
        None
    } else {
        Some(false)
    }
}

fn scope_unknowns(scopes: &[VolumeScope], scan: &ScanMetadata) -> String {
    let mut names: Vec<&str> = scopes
        .iter()
        .filter(|s| scope_flag(**s, scan).is_none())
        .map(|s| match s {
            VolumeScope::StartupVolume => "is_startup_volume",
            VolumeScope::ExternalVolume => "is_external_volume",
            VolumeScope::AnyMountedLocal => "any_mounted_local",
        })
        .collect();
    names.sort();
    names.dedup();
    names.join(",")
}

fn path_problem(path: &str) -> Option<&'static str> {
    let bytes = path.as_bytes();
    let absolute = matches!(bytes.first(), Some(b'/') | Some(b'\\'))
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':');
    if path.trim().is_empty() {
        Some("empty")
    } else if !absolute {
        Some("relative")
    } else if path.replace('\\', "/").split('/').any(|p| p == "..") {
        Some("parent_traversal")
    } else {
        None
    }
}

fn evidence(
    key: EvidenceKey,
    rule: &Rule,
    scan: &ScanMetadata,
    liveness: Liveness,
) -> Option<bool> {
    Some(match key {
        EvidenceKey::VolumeMounted => scan.volume_mounted?,
        EvidenceKey::PathPresent => match scan.path_state {
            PathState::Present => true,
            PathState::Absent => false,
            PathState::Inaccessible | PathState::Unknown => return None,
        },
        EvidenceKey::InspectionComplete => scan.evidence.inspection_complete?,
        EvidenceKey::OwnershipConfirmed => scan.evidence.ownership_confirmed?,
        EvidenceKey::NoProtectedDescendant => !scan.evidence.protected_descendant?,
        EvidenceKey::NoSourceRepository => !scan.evidence.source_repository?,
        EvidenceKey::NoUserData => !scan.evidence.user_data?,
        EvidenceKey::NoCloudPlaceholder => !scan.evidence.cloud_placeholder?,
        EvidenceKey::LivenessNotInUse => match liveness {
            Liveness::NotInUse => true,
            Liveness::InUse => false,
            Liveness::Unknown => return None,
        },
        EvidenceKey::AgeThresholdMet => scan.age_days? >= rule.age_threshold_days?,
        EvidenceKey::ChromeFamilyStopped => !scan.evidence.chrome_family_running?,
        EvidenceKey::ActiveResidentsStopped => !scan.evidence.active_residents?,
        EvidenceKey::OwnerStateComplete => scan.evidence.owner_state_complete?,
        EvidenceKey::SourceRootConfirmedAbsent => !scan.evidence.source_root_exists?,
        EvidenceKey::OwnerLeaseInactive => !scan.evidence.owner_lease_active?,
        EvidenceKey::GeneratorStopped => !scan.evidence.generating_tool_running?,
        EvidenceKey::ReplacementNewer => scan.evidence.replacement_newer?,
        EvidenceKey::ModelCacheRedownloadable => scan.evidence.model_cache_redownloadable?,
        EvidenceKey::AppNotInstalled => !scan.evidence.app_installed?,
        EvidenceKey::AppProcessStopped => !scan.evidence.app_process_running?,
        EvidenceKey::InventoryComplete => scan.evidence.inventory_complete?,
        EvidenceKey::SimulatorNotInUse => !scan.evidence.simulator_in_use?,
    })
}

fn evidence_name(key: EvidenceKey) -> &'static str {
    match key {
        EvidenceKey::VolumeMounted => "volume_mounted",
        EvidenceKey::PathPresent => "path_present",
        EvidenceKey::InspectionComplete => "inspection_complete",
        EvidenceKey::OwnershipConfirmed => "ownership_confirmed",
        EvidenceKey::NoProtectedDescendant => "no_protected_descendant",
        EvidenceKey::NoSourceRepository => "no_source_repository",
        EvidenceKey::NoUserData => "no_user_data",
        EvidenceKey::NoCloudPlaceholder => "no_cloud_placeholder",
        EvidenceKey::LivenessNotInUse => "liveness_not_in_use",
        EvidenceKey::AgeThresholdMet => "age_threshold_met",
        EvidenceKey::ChromeFamilyStopped => "chrome_family_stopped",
        EvidenceKey::ActiveResidentsStopped => "active_residents_stopped",
        EvidenceKey::OwnerStateComplete => "owner_state_complete",
        EvidenceKey::SourceRootConfirmedAbsent => "source_root_confirmed_absent",
        EvidenceKey::OwnerLeaseInactive => "owner_lease_inactive",
        EvidenceKey::GeneratorStopped => "generator_stopped",
        EvidenceKey::ReplacementNewer => "replacement_newer",
        EvidenceKey::ModelCacheRedownloadable => "model_cache_redownloadable",
        EvidenceKey::AppNotInstalled => "app_not_installed",
        EvidenceKey::AppProcessStopped => "app_process_stopped",
        EvidenceKey::InventoryComplete => "inventory_complete",
        EvidenceKey::SimulatorNotInUse => "simulator_not_in_use",
    }
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// Segment-wise, case-insensitive glob match (macOS and Windows volumes are
/// case-insensitive by default; over-matching only produces a gated report
/// row).  `*`/`?` never cross a separator, so `/a/b*` does not match
/// `/a/bc/d`, and a trailing `**` requires at least one more segment so the
/// rule root itself is never selected.
fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(pattern).to_lowercase();
    let path = normalize_path(path).to_lowercase();
    if pattern.is_empty() || path.is_empty() {
        return false;
    }
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    match_parts(&pattern_parts, &path_parts)
}

fn match_parts(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) if pattern.len() == 1 => !path.is_empty(),
        (Some(&"**"), _) => {
            match_parts(&pattern[1..], path)
                || (!path.is_empty() && match_parts(pattern, &path[1..]))
        }
        (Some(part), Some(value)) => {
            segment_matches(part, value) && match_parts(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn segment_matches(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let mut table = vec![vec![false; v.len() + 1]; p.len() + 1];
    table[0][0] = true;
    for i in 0..p.len() {
        for j in 0..=v.len() {
            if !table[i][j] {
                continue;
            }
            if p[i] == '*' {
                table[i + 1][j] = true;
                if j < v.len() {
                    table[i][j + 1] = true;
                }
            } else if p[i] == '?' {
                if j < v.len() {
                    table[i + 1][j + 1] = true;
                }
            } else if j < v.len() && p[i] == v[j] {
                table[i + 1][j + 1] = true;
            }
        }
    }
    table[p.len()][v.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome_rule() -> Rule {
        Rule {
            schema_version: 1,
            rule_version: 1,
            id: "chrome-signing-copies".into(),
            name: "Chrome signing copies".into(),
            path_patterns: vec!["/private/var/folders/**/code_sign_clone.*".into()],
            volumes: vec![VolumeScope::AnyMountedLocal],
            ownership: OwnershipCheck::ChromeCloneDirectory,
            liveness: vec![
                EvidenceKey::ChromeFamilyStopped,
                EvidenceKey::LivenessNotInUse,
            ],
            eligibility: vec![],
            age_threshold_days: Some(1),
            measurement: Measurement::CloneAwareUniqueBytes,
            route: CleanupRoute::Trash,
            route_detail: None,
            risk: Risk::Review,
            report_only: true,
            explanation: None,
        }
    }

    fn scan(path: &str) -> ScanMetadata {
        ScanMetadata {
            path: path.into(),
            volume_id: Some("volume".into()),
            volume_mounted: Some(true),
            path_state: PathState::Present,
            liveness: Liveness::NotInUse,
            age_days: Some(100),
            unique_bytes: Some(1),
            attributed_bytes: Some(1),
            logical_bytes: Some(1),
            evidence: ScanEvidence {
                inspection_complete: Some(true),
                ownership_confirmed: Some(true),
                protected_descendant: Some(false),
                source_repository: Some(false),
                user_data: Some(false),
                cloud_placeholder: Some(false),
                chrome_family_running: Some(false),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn running_item_is_reported_but_ineligible() {
        let mut item = scan("/private/var/folders/x/code_sign_clone.1");
        item.evidence.chrome_family_running = Some(true);
        item.liveness = Liveness::InUse;
        let finding = chrome_rule().evaluate(&item).expect("pattern matches");
        assert!(!finding.eligible);
    }

    #[test]
    fn unknown_item_is_reported_but_ineligible() {
        let mut item = scan("/private/var/folders/x/code_sign_clone.1");
        item.evidence.inspection_complete = None;
        let finding = chrome_rule().evaluate(&item).expect("pattern matches");
        assert_eq!(finding.liveness, Liveness::Unknown);
        assert!(!finding.eligible);
    }

    #[test]
    fn app_backup_requires_newer_replacement() {
        let rule = Rule {
            id: "obsolete-app-backups".into(),
            name: "Obsolete app backups".into(),
            schema_version: 1,
            rule_version: 1,
            path_patterns: vec!["/Applications/*.app.prev-*".into()],
            volumes: vec![VolumeScope::StartupVolume],
            ownership: OwnershipCheck::AppBundleBackup,
            liveness: vec![
                EvidenceKey::AppProcessStopped,
                EvidenceKey::LivenessNotInUse,
            ],
            eligibility: vec![EvidenceKey::ReplacementNewer],
            age_threshold_days: Some(1),
            measurement: Measurement::AttributedAllocation,
            route: CleanupRoute::Trash,
            route_detail: None,
            risk: Risk::Review,
            report_only: true,
            explanation: None,
        };
        let mut item = scan("/Applications/Tool.app.prev-1");
        item.is_startup_volume = Some(true);
        item.evidence.app_process_running = Some(false);
        item.evidence.replacement_newer = Some(false);
        assert!(!rule.evaluate(&item).unwrap().eligible);
        item.evidence.replacement_newer = Some(true);
        assert!(rule.evaluate(&item).unwrap().eligible);
    }
}
