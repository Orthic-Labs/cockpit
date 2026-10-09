//! Hover card content: the key numbers behind each ring (same information as the Mac
//! hover cards). Pure data; `render.rs` draws it. Unknown values read `--`.

use crate::layout::Cell;
use crate::send::{self, Panel};
use crate::drive_health::{self, Report};
use crate::sensors::{Machine, size_text};
use crate::usage::{Status, Usage, age_text, reset_in};

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// Label on the left, value on the right.
    Pair { label: String, value: String },
    /// Pair plus a progress bar for a used share (`None` draws an empty track).
    Bar {
        label: String,
        value: String,
        fraction: Option<f32>,
    },
    /// Secondary-ink line.
    Note(String),
    /// Critical-ink line that says the account is stopped (a spent limit).
    Alert(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CardContent {
    pub title: String,
    pub accessory: Option<String>,
    pub rows: Vec<Row>,
}

fn percent_text(fraction: Option<f32>) -> String {
    fraction.map_or_else(
        || "--".to_string(),
        |f| format!("{}%", (f.clamp(0.0, 1.0) * 100.0).round() as u32),
    )
}

fn bar(label: &str, value: String, fraction: Option<f32>) -> Row {
    Row::Bar {
        label: label.to_string(),
        value,
        fraction,
    }
}

/// The card for `cell` and whether it is a nearby-sharing popup (news, shown without hover).
/// Every row of a non-Send card is plain text; the Send cell shows `popup` when there is one,
/// else its hover panel, whose rows with an action are clickable.
pub fn panel_for(
    cell: Cell,
    machine: Option<&Machine>,
    usage: &[Usage; 2],
    now: u64,
    popup: Option<Panel>,
) -> (Panel, bool) {
    if cell == Cell::Send {
        return match popup {
            Some(panel) => (panel, true),
            None => (send::hover_panel(), false),
        };
    }
    let content = content_for(cell, machine, usage, now);
    let actions = vec![None; content.rows.len()];
    (Panel { content, actions }, false)
}

fn content_for(cell: Cell, machine: Option<&Machine>, usage: &[Usage; 2], now: u64) -> CardContent {
    match cell {
        // One System card whichever of its two rings is hovered (the Mac has one cell).
        Cell::Cpu | Cell::Memory => system(machine),
        Cell::Disk => disks(machine, &drive_health::current()),
        Cell::Claude => provider("Claude", &usage[0], now),
        Cell::Codex => provider("Codex", &usage[1], now),
        Cell::Send => send::hover_panel().content,
    }
}

/// The System card: CPU, then memory, each a bar with its detail line.
pub fn system(machine: Option<&Machine>) -> CardContent {
    let mut rows = Vec::new();
    match machine {
        Some(m) => {
            rows.push(bar("CPU", percent_text(m.cpu), m.cpu));
            let busy = if m.cpu.is_some() {
                format!("{} busy", percent_text(m.cpu))
            } else {
                "Usage unavailable".to_string()
            };
            rows.push(Row::Note(if m.cores > 0 {
                format!("{busy} \u{b7} {} cores", m.cores)
            } else {
                busy
            }));
            match m.memory {
                Some(mem) => {
                    rows.push(bar(
                        "Memory",
                        percent_text(Some(mem.used_fraction())),
                        Some(mem.used_fraction()),
                    ));
                    rows.push(Row::Note(format!(
                        "{} of {} used",
                        size_text(mem.used()),
                        size_text(mem.total)
                    )));
                }
                None => {
                    rows.push(bar("Memory", "--".into(), None));
                    rows.push(Row::Note("Memory readings unavailable".into()));
                }
            }
        }
        None => rows.push(Row::Note("Waiting for the first sample".into())),
    }
    CardContent {
        title: "System".into(),
        accessory: None,
        rows,
    }
}

/// The Disks card: each volume's free space, then the drive-health lines.
pub fn disks(machine: Option<&Machine>, health: &Report) -> CardContent {
    let mut rows = Vec::new();
    match machine {
        Some(m) if !m.drives.is_empty() => {
            // System drive first, as the ring shows it.
            let mut ordered: Vec<_> = m.drives.iter().collect();
            ordered.sort_by_key(|d| !d.system);
            for drive in ordered {
                let name = drive.root.trim_end_matches('\\');
                let label = if drive.system {
                    format!("{name} (system)")
                } else {
                    name.to_string()
                };
                rows.push(Row::Bar {
                    label,
                    value: format!(
                        "{} free of {}",
                        size_text(drive.free),
                        size_text(drive.total)
                    ),
                    fraction: Some(drive.used_fraction()),
                });
            }
        }
        Some(_) => rows.push(Row::Note("No drive readings".into())),
        None => rows.push(Row::Note("Waiting for the first sample".into())),
    }
    match health {
        Report::Pending => {}
        Report::Missing => rows.push(Row::Pair {
            label: "Drive health".into(),
            value: "Install smartmontools".into(),
        }),
        Report::Drives(drives) => {
            rows.extend(drive_health::lines(drives).into_iter().map(Row::Note));
        }
    }
    CardContent {
        title: "Disks".into(),
        accessory: None,
        rows,
    }
}

fn provider(name: &str, usage: &Usage, now: u64) -> CardContent {
    let mut rows = Vec::new();
    if let Some(block) = &usage.block {
        rows.push(Row::Alert(match block.resets_at {
            Some(reset) if reset > now => {
                format!("{} \u{b7} resets in {}", block.reason, reset_in(reset, now))
            }
            _ => block.reason.clone(),
        }));
    }
    for window in &usage.windows {
        rows.push(Row::Bar {
            label: window.label.clone(),
            value: percent_text(Some(window.fraction)),
            fraction: Some(window.fraction),
        });
        if let Some(reset) = window.resets_at {
            rows.push(Row::Note(format!("Resets in {}", reset_in(reset, now))));
        }
    }
    if usage.status == Status::AccessDenied && usage.windows.is_empty() {
        // Says what happened and what fixes it, not "sign in" (the login is there).
        rows.push(Row::Note(format!("Windows refused access to {name}'s saved login.")));
        rows.push(Row::Note("Fix the file's permissions to read usage.".into()));
        return CardContent {
            title: format!("{name} Usage"),
            accessory: usage.plan.clone(),
            rows,
        };
    }
    let status = match (usage.status, usage.updated) {
        (Status::Ok, Some(updated)) => format!("Updated {}", age_text(updated, now)),
        (status, Some(updated)) => {
            format!(
                "{} - last reading {}",
                status.text(),
                age_text(updated, now)
            )
        }
        (status, None) => status.text().to_string(),
    };
    rows.push(Row::Note(status));
    CardContent {
        title: name.to_string(),
        accessory: usage.plan.clone(),
        rows,
    }
}
