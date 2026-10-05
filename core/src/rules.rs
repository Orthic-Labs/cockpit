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
    pub logical_bytes: Option<u64>,
    pub attributed_bytes: Option<u64>,
    pub unique_bytes: Option<u64>,
    /// Reported as an estimate only; apply is outside this report-only module.
    pub deletion_estimate_lower: Option<u64>,
    pub deletion_estimate_upper: Option<u64>,
}

impl Rule {
    /// Return a finding only when path patterns and known volume scope match.
    /// Unknown volume identity is retained as a finding but cannot be eligible.
    pub fn evaluate(&self, scan: &ScanMetadata) -> Option<Finding> {
        // Explanation rules describe non-file resources (swap, snapshots,
        // app-managed media).  They are shown by their rule metadata and do
        // not attach themselves to every filesystem row.
        if self.path_patterns.is_empty() {
            return None;
        }
        if !self.path_patterns.is_empty()
            && !self
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
        let mut reasons = Vec::new();
        let mut eligible = self.route != CleanupRoute::None
            && self.measurement != Measurement::ExplanationOnly
            && self.risk != Risk::Explanation;

        for requirement in self.liveness.iter().chain(self.eligibility.iter()) {
            match evidence(*requirement, self, scan, liveness) {
                Some(true) => {}
                Some(false) => {
                    eligible = false;
                    reasons.push(format!("evidence_failed:{}", evidence_name(*requirement)));
                }
                None => {
                    eligible = false;
                    reasons.push(format!("evidence_unknown:{}", evidence_name(*requirement)));
                }
            }
        }

        // These protections are unconditional for actionable rules.  They
        // prevent broad path patterns from ever selecting source/user data.
        for (key, value) in [
            (EvidenceKey::VolumeMounted, scan.volume_mounted),
            (
                EvidenceKey::PathPresent,
                Some(scan.path_state == PathState::Present),
            ),
            (
                EvidenceKey::InspectionComplete,
                scan.evidence.inspection_complete,
            ),
            (
                EvidenceKey::OwnershipConfirmed,
                scan.evidence.ownership_confirmed,
            ),
            (
                EvidenceKey::NoProtectedDescendant,
                scan.evidence.protected_descendant.map(|v| !v),
            ),
            (
                EvidenceKey::NoSourceRepository,
                scan.evidence.source_repository.map(|v| !v),
            ),
            (EvidenceKey::NoUserData, scan.evidence.user_data.map(|v| !v)),
            (
                EvidenceKey::NoCloudPlaceholder,
                scan.evidence.cloud_placeholder.map(|v| !v),
            ),
        ] {
            if eligible && self.route != CleanupRoute::None {
                match value {
                    Some(true) => {}
                    Some(false) => {
                        eligible = false;
                        reasons.push(format!("protected:{}", evidence_name(key)));
                    }
                    None => {
                        eligible = false;
                        reasons.push(format!("evidence_unknown:{}", evidence_name(key)));
                    }
                }
            }
        }

        if let Some(threshold) = self.age_threshold_days {
            match scan.age_days {
                Some(days) if days >= threshold => {}
                Some(_) => {
                    eligible = false;
                    reasons.push("age_below_threshold".to_string());
                }
                None => {
                    eligible = false;
                    reasons.push("evidence_unknown:age_threshold_met".to_string());
                }
            }
        }

        if eligible && liveness != Liveness::NotInUse {
            eligible = false;
            reasons.push(format!("liveness:{:?}", liveness).to_lowercase());
        }

        if !eligible && reasons.is_empty() {
            reasons.push("report_only".to_string());
        }

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
            logical_bytes: scan.logical_bytes,
            attributed_bytes: scan.attributed_bytes,
            unique_bytes: scan.unique_bytes,
            deletion_estimate_lower: scan.deletion_estimate_lower,
            deletion_estimate_upper: scan.deletion_estimate_upper,
        })
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

/// Evaluate a batch of explicitly supplied scan rows.  No filesystem or
/// process inspection happens here, and no finding is mutated or approved.
pub fn evaluate_all(rules: &[Rule], scans: &[ScanMetadata]) -> Vec<Finding> {
    scans.iter().flat_map(|scan| detect(rules, scan)).collect()
}

/// Stable across runs and independent of process order.  FNV-1a is used only
/// as a compact identifier; it is not a security or content-integrity hash.
pub fn stable_finding_id(rule_id: &str, path: &str) -> String {
    let key = format!("{}\0{}", rule_id, normalize_path(path));
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("finding:{}:{hash:016x}", rule_id)
}

fn effective_liveness(scan: &ScanMetadata) -> Liveness {
    if scan.volume_mounted != Some(true)
        || scan.path_state == PathState::Inaccessible
        || scan.path_state == PathState::Unknown
        || scan.evidence.inspection_complete != Some(true)
    {
        Liveness::Unknown
    } else {
        scan.liveness
    }
}

fn volume_matches(scopes: &[VolumeScope], scan: &ScanMetadata) -> bool {
    if scopes.is_empty() {
        return true;
    }
    scopes.iter().any(|scope| match scope {
        VolumeScope::AnyMountedLocal => true,
        VolumeScope::StartupVolume => scan.is_startup_volume != Some(false),
        VolumeScope::ExternalVolume => scan.is_external_volume != Some(false),
    })
}

fn evidence(
    key: EvidenceKey,
    rule: &Rule,
    scan: &ScanMetadata,
    liveness: Liveness,
) -> Option<bool> {
    Some(match key {
        EvidenceKey::VolumeMounted => scan.volume_mounted?,
        EvidenceKey::PathPresent => scan.path_state == PathState::Present,
        EvidenceKey::InspectionComplete => scan.evidence.inspection_complete?,
        EvidenceKey::OwnershipConfirmed => scan.evidence.ownership_confirmed?,
        EvidenceKey::NoProtectedDescendant => !scan.evidence.protected_descendant?,
        EvidenceKey::NoSourceRepository => !scan.evidence.source_repository?,
        EvidenceKey::NoUserData => !scan.evidence.user_data?,
        EvidenceKey::NoCloudPlaceholder => !scan.evidence.cloud_placeholder?,
        EvidenceKey::LivenessNotInUse => liveness == Liveness::NotInUse,
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

fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern = normalize_path(pattern);
    let path = normalize_path(path);
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();
    match_parts(&pattern_parts, &path_parts)
}

fn match_parts(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
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
            age_threshold_days: None,
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
            age_threshold_days: None,
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
