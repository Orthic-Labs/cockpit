//! Hover card content: the key numbers behind each ring (same information as the Mac
//! hover cards). Pure data; `render.rs` draws it. Unknown values read `--`.

use crate::layout::Cell;
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

pub fn content_for(
    cell: Cell,
    machine: Option<&Machine>,
    usage: &[Usage; 2],
    now: u64,
) -> CardContent {
    match cell {
        Cell::Cpu => cpu(machine),
        Cell::Memory => memory(machine),
        Cell::Disk => disks(machine),
        Cell::Claude => provider("Claude", &usage[0], now),
        Cell::Codex => provider("Codex", &usage[1], now),
    }
}

fn cpu(machine: Option<&Machine>) -> CardContent {
    let mut rows = Vec::new();
    match machine {
        Some(m) => {
            rows.push(bar("Busy", percent_text(m.cpu), m.cpu));
            rows.push(Row::Pair {
                label: "Logical processors".into(),
                value: if m.cores > 0 {
                    m.cores.to_string()
                } else {
                    "--".into()
                },
            });
        }
        None => rows.push(Row::Note("Waiting for the first sample".into())),
    }
    CardContent {
        title: "CPU".into(),
        accessory: None,
        rows,
    }
}

fn memory(machine: Option<&Machine>) -> CardContent {
    let mut rows = Vec::new();
    match machine.and_then(|m| m.memory) {
        Some(m) => {
            rows.push(bar(
                "In use",
                format!("{} of {}", size_text(m.used()), size_text(m.total)),
                Some(m.used_fraction()),
            ));
            rows.push(Row::Pair {
                label: "Available".into(),
                value: size_text(m.available),
            });
            let commit = (m.commit_limit > 0)
                .then(|| (m.commit_used as f64 / m.commit_limit as f64).clamp(0.0, 1.0) as f32);
            rows.push(bar(
                "Commit",
                format!(
                    "{} of {}",
                    size_text(m.commit_used),
                    size_text(m.commit_limit)
                ),
                commit,
            ));
        }
        None => rows.push(Row::Note("Memory readings unavailable".into())),
    }
    CardContent {
        title: "Memory".into(),
        accessory: None,
        rows,
    }
}

fn disks(machine: Option<&Machine>) -> CardContent {
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
    CardContent {
        title: "Disks".into(),
        accessory: None,
        rows,
    }
}

fn provider(name: &str, usage: &Usage, now: u64) -> CardContent {
    let mut rows = Vec::new();
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
    let status = match (usage.status, usage.updated) {
        (Status::Ok, Some(updated)) => format!("Updated {}", age_text(updated, now)),
        (status, Some(updated)) => {
            format!("{} - last reading {}", status.text(), age_text(updated, now))
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
