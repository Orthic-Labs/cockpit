//! `pulse send <file…> --to <alias>` and `pulse send --list`: send files to a
//! nearby device over the open LocalSend protocol, without the hub running.
//! The device must accept on its own screen.

use pulse_core::localsend::discovery;
use pulse_core::localsend::proto::{self, DeviceInfo};
use pulse_core::localsend::send::{self, Outcome, Peer, Phase, SendItem};
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// This command's identity on the network. It announces a port nothing
/// listens on, so devices answer by multicast instead of registering with a
/// server that is not there (and never confuse it with the hub's service).
fn identity() -> DeviceInfo {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "Mac".to_string());
    DeviceInfo {
        alias: format!("{host} (Pulse CLI)"),
        version: proto::VERSION.to_string(),
        device_model: Some("Mac".to_string()),
        device_type: Some("headless".to_string()),
        fingerprint: proto::random_hex(16),
        port: proto::PORT + 1,
        protocol: "https".to_string(),
        download: false,
        announce: None,
    }
}

/// The fingerprint of this computer's own sharing certificate (the one the hub
/// announces with), read from the state folder the hub keeps it in. None when
/// the hub has never run here, in which case nothing of ours is on the network.
fn own_fingerprint() -> Option<String> {
    #[cfg(windows)]
    let state = std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Pulse"));
    #[cfg(not(windows))]
    let state = std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join("Library/Application Support/Pulse"));
    let cert = std::fs::read(state?.join("localsend").join("cert.der")).ok()?;
    Some(pulse_core::localsend::net::sha256_hex(&cert))
}

fn nearby(me: &DeviceInfo) -> Result<Vec<(Peer, DeviceInfo)>, String> {
    // This computer's own hub answers the announcement like any device. It is
    // dropped by identity (certificate fingerprint), not by address, so a
    // separate LocalSend app on the same computer still shows.
    let own = own_fingerprint();
    let others = |h: &discovery::Heard| {
        !own.as_deref()
            .is_some_and(|own| own.eq_ignore_ascii_case(&h.info.fingerprint))
    };
    let mut heard = discovery::scan(me, Duration::from_millis(2500))
        .map_err(|e| format!("Couldn't look for nearby devices: {e}"))?;
    heard.retain(others);
    if heard.is_empty() {
        // Multicast can be lost on busy networks; ask the local hosts directly.
        let found = std::sync::Mutex::new(Vec::new());
        discovery::sweep(me, &std::sync::atomic::AtomicBool::new(false), &|device| {
            if others(&device)
                && let Ok(mut list) = found.lock()
            {
                list.push(device);
            }
        });
        heard = found.into_inner().unwrap_or_default();
    }
    Ok(heard
        .into_iter()
        .map(|h| {
            (
                Peer {
                    ip: h.ip,
                    port: h.info.port,
                    https: h.info.protocol == "https",
                    fingerprint: h.info.fingerprint.clone(),
                    alias: h.info.alias.clone(),
                },
                h.info,
            )
        })
        .collect())
}

pub fn run(mut args: Vec<String>, machine: bool) -> Result<(), String> {
    let to = crate::take_option(&mut args, "--to")?;
    let list = crate::take_flag(&mut args, "--list");
    let me = identity();
    let devices = nearby(&me)?;

    if list {
        if !args.is_empty() {
            return Err("send --list takes no files".into());
        }
        if machine {
            let rows: Vec<Value> = devices
                .iter()
                .map(|(peer, info)| {
                    json!({"alias": info.alias, "deviceType": info.device_type,
                           "deviceModel": info.device_model, "ip": peer.ip.to_string(),
                           "fingerprint": info.fingerprint})
                })
                .collect();
            println!("{}", Value::Array(rows));
        } else if devices.is_empty() {
            println!("No nearby devices. Open LocalSend on the other device and try again.");
        } else {
            for (peer, info) in &devices {
                println!(
                    "{}\t{}\t{}",
                    info.alias,
                    info.device_type.as_deref().unwrap_or("device"),
                    peer.ip
                );
            }
        }
        return Ok(());
    }

    let to = to.ok_or("send needs --to <alias> (see `pulse send --list`)")?;
    if args.is_empty() {
        return Err("send needs at least one file or folder".into());
    }
    let items: Vec<SendItem> = args
        .iter()
        .map(|a| {
            let path = PathBuf::from(a);
            SendItem::Path(if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .map(|d| d.join(&path))
                    .unwrap_or(path)
            })
        })
        .collect();
    let entries = send::expand(&items)?;

    let wanted = to.to_lowercase();
    let exact: Vec<&(Peer, DeviceInfo)> = devices
        .iter()
        .filter(|(_, info)| info.alias.to_lowercase() == wanted || info.fingerprint == to)
        .collect();
    let chosen = if exact.len() == 1 {
        exact[0]
    } else {
        let partial: Vec<&(Peer, DeviceInfo)> = devices
            .iter()
            .filter(|(_, info)| info.alias.to_lowercase().contains(&wanted))
            .collect();
        match (exact.len(), partial.len()) {
            (0, 1) => partial[0],
            (0, 0) => {
                let names: Vec<&str> = devices.iter().map(|(_, i)| i.alias.as_str()).collect();
                return Err(if names.is_empty() {
                    "No nearby devices. Open LocalSend on the other device and try again.".into()
                } else {
                    format!(
                        "No nearby device called \"{to}\". Nearby: {}",
                        names.join(", ")
                    )
                });
            }
            _ => {
                let names: Vec<&str> = devices
                    .iter()
                    .filter(|(_, i)| i.alias.to_lowercase().contains(&wanted))
                    .map(|(_, i)| i.alias.as_str())
                    .collect();
                return Err(format!(
                    "\"{to}\" matches more than one device: {}",
                    names.join(", ")
                ));
            }
        }
    };
    let peer = &chosen.0;
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let files = entries.len();

    if machine {
        println!(
            "{}",
            json!({"event": "waiting", "to": peer.alias, "files": files, "bytes": total})
        );
    } else {
        eprintln!(
            "Waiting for {} to accept {files} file(s), {}…",
            peer.alias,
            human(total)
        );
    }

    let cancel = AtomicBool::new(false);
    let interactive = std::io::stderr().is_terminal();
    let mut last = Instant::now() - Duration::from_secs(1);
    let outcome = send::deliver(&me, peer, &entries, &cancel, &|_| {}, &mut |p| {
        if p.phase != Phase::Sending || last.elapsed() < Duration::from_millis(250) {
            return;
        }
        last = Instant::now();
        if machine {
            println!(
                "{}",
                json!({"event": "progress", "bytes": p.done, "total": p.total,
                       "filesDone": p.files_done, "files": p.files_total, "file": p.current})
            );
        } else if interactive {
            let percent = if p.total == 0 {
                100
            } else {
                p.done * 100 / p.total
            };
            eprint!(
                "\rSending {percent}% ({} of {})   ",
                human(p.done),
                human(p.total)
            );
            let _ = std::io::stderr().flush();
        }
    })?;
    if !machine && interactive {
        eprintln!();
    }
    match outcome {
        Outcome::Done => {
            if machine {
                println!(
                    "{}",
                    json!({"event": "done", "to": peer.alias, "files": files, "bytes": total})
                );
            } else {
                println!("Sent {files} file(s), {}, to {}.", human(total), peer.alias);
            }
            Ok(())
        }
        Outcome::Declined => Err(format!("{} declined.", peer.alias)),
        Outcome::Cancelled => Err("Cancelled.".into()),
    }
}
