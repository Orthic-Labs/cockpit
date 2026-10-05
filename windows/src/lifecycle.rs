//! Pure lifecycle helpers: panel placement, monitor-set reconciliation planning, sampling
//! cadence, CPU delta math and the re-entrancy gate. No Win32 calls, so tests are exact.

use windows::Win32::Foundation::RECT;

pub const PILL_WIDTH: i32 = 132;
pub const PILL_HEIGHT: i32 = 76;
pub const PILL_MARGIN: i32 = 12;
pub const VISIBLE_INTERVAL_MS: u32 = 2_000;
pub const HIDDEN_INTERVAL_MS: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl From<RECT> for Bounds {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

impl From<Bounds> for RECT {
    fn from(b: Bounds) -> Self {
        RECT {
            left: b.left,
            top: b.top,
            right: b.right,
            bottom: b.bottom,
        }
    }
}

impl Bounds {
    pub fn width(self) -> i32 {
        self.right - self.left
    }
    pub fn height(self) -> i32 {
        self.bottom - self.top
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorSpec {
    pub id: String,
    pub bounds: Bounds,
}

/// Pill rectangle for a monitor: right edge, vertically centred.
pub fn pill_bounds(monitor: Bounds) -> Bounds {
    let left = monitor.right - PILL_WIDTH - PILL_MARGIN;
    let top = monitor.top + (monitor.height() - PILL_HEIGHT) / 2;
    Bounds {
        left,
        top,
        right: left + PILL_WIDTH,
        bottom: top + PILL_HEIGHT,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelAction {
    Destroy(String),
    Move(MonitorSpec),
    Create(MonitorSpec),
}

/// Diff existing panels against the enumerated monitor set. Destroys come first, then
/// moves/creates in monitor order. Duplicate ids in `desired` keep the first entry.
pub fn plan_panels(existing: &[MonitorSpec], desired: &[MonitorSpec]) -> Vec<PanelAction> {
    let mut unique: Vec<&MonitorSpec> = Vec::new();
    for spec in desired {
        if !unique.iter().any(|u| u.id == spec.id) {
            unique.push(spec);
        }
    }
    let mut actions = Vec::new();
    for old in existing {
        if !unique.iter().any(|u| u.id == old.id) {
            actions.push(PanelAction::Destroy(old.id.clone()));
        }
    }
    for want in unique {
        match existing.iter().find(|e| e.id == want.id) {
            None => actions.push(PanelAction::Create(want.clone())),
            Some(old) if old.bounds != want.bounds => actions.push(PanelAction::Move(want.clone())),
            Some(_) => {}
        }
    }
    actions
}

/// Hidden (or no) panels poll slowly; any visible panel keeps the normal cadence.
pub fn sampling_interval_ms(total_panels: usize, hidden_panels: usize) -> u32 {
    if total_panels == 0 || hidden_panels >= total_panels {
        HIDDEN_INTERVAL_MS
    } else {
        VISIBLE_INTERVAL_MS
    }
}

/// Busy fraction from two GetSystemTimes snapshots `(idle, kernel, user)`; kernel includes idle.
pub fn cpu_fraction(previous: Option<(u64, u64, u64)>, current: (u64, u64, u64)) -> Option<f32> {
    let (pi, pk, pu) = previous?;
    let total = current
        .1
        .saturating_sub(pk)
        .saturating_add(current.2.saturating_sub(pu));
    if total == 0 {
        return None;
    }
    let idle = current.0.saturating_sub(pi);
    let busy = total.saturating_sub(idle);
    Some((busy as f32 / total as f32).clamp(0.0, 1.0))
}

/// Serialises monitor reconciliation. A request that arrives while one is running (sent
/// messages can be delivered inside Win32 calls the reconcile makes) is coalesced into one
/// re-run instead of recursing.
#[derive(Default)]
pub struct ReconcileGate {
    running: bool,
    rerun: bool,
}

impl ReconcileGate {
    pub const fn new() -> Self {
        Self {
            running: false,
            rerun: false,
        }
    }
    /// True if the caller should run the reconcile loop now.
    pub fn request(&mut self) -> bool {
        if self.running {
            self.rerun = true;
            false
        } else {
            self.running = true;
            self.rerun = false;
            true
        }
    }
    /// Call after each pass; true means run another pass.
    pub fn finish(&mut self) -> bool {
        if self.rerun {
            self.rerun = false;
            true
        } else {
            self.running = false;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(left: i32, top: i32, right: i32, bottom: i32) -> Bounds {
        Bounds {
            left,
            top,
            right,
            bottom,
        }
    }
    fn spec(id: &str, bounds: Bounds) -> MonitorSpec {
        MonitorSpec {
            id: id.into(),
            bounds,
        }
    }
    const M1: Bounds = Bounds {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    const M2: Bounds = Bounds {
        left: 1920,
        top: 0,
        right: 3840,
        bottom: 1080,
    };
    const NEG: Bounds = Bounds {
        left: -1920,
        top: -200,
        right: 0,
        bottom: 880,
    };

    #[test]
    fn pill_is_right_edge_vertically_centred() {
        let p = pill_bounds(M1);
        assert_eq!(
            (p.right, p.left),
            (1920 - PILL_MARGIN, 1920 - PILL_MARGIN - PILL_WIDTH)
        );
        assert_eq!(p.top, (1080 - PILL_HEIGHT) / 2);
        assert_eq!((p.width(), p.height()), (PILL_WIDTH, PILL_HEIGHT));
    }

    #[test]
    fn pill_on_negative_origin_monitor() {
        let p = pill_bounds(NEG);
        assert_eq!(p.right, -PILL_MARGIN);
        assert_eq!(p.top, -200 + (1080 - PILL_HEIGHT) / 2);
    }

    #[test]
    fn unchanged_set_plans_nothing() {
        let set = [spec("A", M1), spec("B", M2)];
        assert!(plan_panels(&set, &set).is_empty());
    }

    #[test]
    fn disconnect_destroys_only_missing_monitor() {
        let existing = [spec("A", M1), spec("B", M2)];
        let desired = [spec("A", M1)];
        assert_eq!(
            plan_panels(&existing, &desired),
            vec![PanelAction::Destroy("B".into())]
        );
    }

    #[test]
    fn reconnect_creates_panel() {
        let existing = [spec("A", M1)];
        let desired = [spec("A", M1), spec("B", M2)];
        assert_eq!(
            plan_panels(&existing, &desired),
            vec![PanelAction::Create(spec("B", M2))]
        );
    }

    #[test]
    fn all_monitors_gone_then_back() {
        let both = [spec("A", M1), spec("B", M2)];
        assert_eq!(
            plan_panels(&both, &[]),
            vec![
                PanelAction::Destroy("A".into()),
                PanelAction::Destroy("B".into())
            ]
        );
        assert_eq!(
            plan_panels(&[], &both),
            vec![
                PanelAction::Create(spec("A", M1)),
                PanelAction::Create(spec("B", M2))
            ]
        );
    }

    #[test]
    fn rearranged_or_resized_monitor_moves_panel() {
        let existing = [spec("A", M1)];
        let desired = [spec("A", b(0, 0, 2560, 1440))];
        assert_eq!(
            plan_panels(&existing, &desired),
            vec![PanelAction::Move(spec("A", b(0, 0, 2560, 1440)))]
        );
    }

    #[test]
    fn destroys_precede_creates_and_duplicate_ids_collapse() {
        let existing = [spec("OLD", M1)];
        let desired = [spec("NEW", M1), spec("NEW", M2)];
        assert_eq!(
            plan_panels(&existing, &desired),
            vec![
                PanelAction::Destroy("OLD".into()),
                PanelAction::Create(spec("NEW", M1))
            ]
        );
    }

    #[test]
    fn cadence_slows_only_when_every_panel_hidden() {
        assert_eq!(sampling_interval_ms(2, 0), VISIBLE_INTERVAL_MS);
        assert_eq!(sampling_interval_ms(2, 1), VISIBLE_INTERVAL_MS);
        assert_eq!(sampling_interval_ms(2, 2), HIDDEN_INTERVAL_MS);
        assert_eq!(sampling_interval_ms(0, 0), HIDDEN_INTERVAL_MS);
    }

    #[test]
    fn cpu_fraction_cases() {
        assert_eq!(cpu_fraction(None, (1, 2, 3)), None);
        assert_eq!(cpu_fraction(Some((0, 0, 0)), (50, 100, 0)), Some(0.5));
        assert_eq!(cpu_fraction(Some((0, 0, 0)), (100, 100, 0)), Some(0.0));
        assert_eq!(cpu_fraction(Some((0, 0, 0)), (0, 60, 40)), Some(1.0));
        assert_eq!(cpu_fraction(Some((5, 5, 5)), (5, 5, 5)), None);
        // Counter regression saturates to zero total rather than wrapping.
        assert_eq!(cpu_fraction(Some((9, 9, 9)), (1, 1, 1)), None);
        // Idle larger than total clamps instead of going negative.
        assert_eq!(cpu_fraction(Some((0, 0, 0)), (500, 100, 0)), Some(0.0));
    }

    #[test]
    fn gate_coalesces_reentrant_requests() {
        let mut gate = ReconcileGate::new();
        assert!(gate.request());
        assert!(!gate.request());
        assert!(!gate.request());
        assert!(gate.finish());
        assert!(!gate.finish());
        assert!(gate.request());
    }
}
