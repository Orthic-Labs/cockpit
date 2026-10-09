//! Nearby sharing over the open LocalSend protocol v2 (github.com/localsend/protocol).
//!
//! `Service` is a long-running object a host (the hub) owns: it announces this
//! computer on the network, keeps a list of nearby devices, receives files over
//! HTTPS after the user accepts, and sends files to a chosen device. Hosts hear
//! about changes through the `Event` callback and read `Service::snapshot`.

pub mod discovery;
pub mod net;
pub mod proto;
mod receive;
pub mod send;

pub use send::{Entry, Peer, SendItem};

use crate::bridge::envelope::{self, Envelope, ReplayGuard};
use proto::DeviceInfo;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Config {
    /// How other devices list this Mac.
    pub alias: String,
    pub port: u16,
    /// Where received files go.
    pub save_dir: PathBuf,
    /// Accept without asking from a device the user accepted before.
    pub accept_known: bool,
    /// Holds the certificate and the list of known devices.
    pub state_dir: PathBuf,
    pub device_model: String,
}

/// A device on the network.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub fingerprint: String,
    pub alias: String,
    pub device_model: Option<String>,
    pub device_type: Option<String>,
    pub ip: String,
    pub port: u16,
    pub protocol: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct IncomingFile {
    pub name: String,
    pub size: u64,
}

/// A request waiting for the user's answer.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    pub id: String,
    pub from: String,
    pub device_model: Option<String>,
    pub fingerprint: String,
    pub ip: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub is_message: bool,
    pub preview: Option<String>,
    /// The first few files, for display.
    pub files: Vec<IncomingFile>,
    pub known: bool,
}

/// One send or receive, from the first request to the last byte.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: String,
    /// "send" or "receive".
    pub direction: String,
    pub peer: String,
    pub peer_fingerprint: String,
    /// waiting, active, done, failed, cancelled or declined.
    pub state: String,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub files_total: usize,
    pub files_done: usize,
    pub current: Option<String>,
    pub saved_to: Option<String>,
    pub saved_files: Vec<String>,
    pub error: Option<String>,
    /// A received text message, shown with Copy rather than saved as a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub started: u64,
    pub finished: Option<u64>,
}

impl Transfer {
    pub fn is_open(&self) -> bool {
        self.state == "waiting" || self.state == "active"
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub alias: String,
    pub fingerprint: String,
    pub port: u16,
    pub save_dir: String,
    pub devices: Vec<Device>,
    pub incoming: Vec<Incoming>,
    pub transfers: Vec<Transfer>,
    /// Things that don't work, in words (discovery socket, …).
    pub warnings: Vec<String>,
    /// "unknown", "granted" or "blocked" (macOS Local Network access).
    pub local_network: String,
    /// A sweep of the local networks is running.
    pub scanning: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The device list changed.
    Devices,
    /// A device wants to send; the user must answer (`Service::respond`).
    Incoming(Incoming),
    /// A request was answered, withdrawn or timed out.
    IncomingResolved(String),
    /// Bytes moved (rate-limited).
    Progress(Transfer),
    /// A transfer reached an end state.
    Finished(Transfer),
    /// A signed Pulse bridge message from a paired device arrived and verified.
    /// Hosts pass it to `bridge::relay::incoming`; nothing is saved or shown.
    Bridge(Envelope),
    /// Anything else (a transfer dismissed, a device paired).
    Changed,
}

/// A device that completed bridge pairing: both sides keep the same 32-byte key.
#[derive(Clone, Debug)]
pub(crate) struct Pair {
    pub alias: String,
    /// 64 hex characters.
    pub key: String,
}

/// One entry of `known-devices.json`: a bare fingerprint (accepted before) or
/// an object that also holds the pair key.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum KnownEntry {
    Plain(String),
    Paired {
        fingerprint: String,
        #[serde(default)]
        alias: String,
        key: String,
    },
}

fn load_known(path: &std::path::Path) -> (HashSet<String>, HashMap<String, Pair>) {
    let mut trusted = HashSet::new();
    let mut pairs = HashMap::new();
    let entries = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<KnownEntry>>(&bytes).ok())
        .unwrap_or_default();
    for entry in entries {
        match entry {
            KnownEntry::Plain(fingerprint) => {
                trusted.insert(fingerprint);
            }
            KnownEntry::Paired {
                fingerprint,
                alias,
                key,
            } => {
                if envelope::from_hex(&key).is_some_and(|k| k.len() == 32) {
                    pairs.insert(fingerprint.clone(), Pair { alias, key });
                }
                trusted.insert(fingerprint);
            }
        }
    }
    (trusted, pairs)
}

/// Write `contents` next to `path` and rename it into place; the key file is
/// private on Unix.
fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or(path);
    std::fs::create_dir_all(parent)?;
    let temp = path.with_extension(format!("tmp-{}", proto::random_hex(4)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options.open(&temp).and_then(|mut file| {
        file.write_all(contents)?;
        file.sync_all()
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

pub(crate) struct Seen {
    pub device: Device,
    pub last_seen: Instant,
    pub misses: u32,
}

pub(crate) struct Pending {
    pub incoming: Incoming,
    pub decision: Mutex<Option<bool>>,
    pub changed: Condvar,
}

pub(crate) struct CancelHandle {
    pub flag: AtomicBool,
    pub stream: Mutex<Option<TcpStream>>,
}

pub(crate) struct SessionFile {
    pub meta: proto::FileMeta,
    pub token: String,
    pub done: bool,
}

pub(crate) struct Session {
    pub transfer_id: String,
    pub peer_ip: IpAddr,
    pub peer_fingerprint: String,
    pub files: HashMap<String, SessionFile>,
    pub cancel: Arc<AtomicBool>,
    pub activity: Instant,
    pub remaining: usize,
    pub saved: Vec<PathBuf>,
    pub save_dir: PathBuf,
}

pub(crate) struct Inner {
    pub cfg: Mutex<Config>,
    pub me: DeviceInfo,
    pub tls: Arc<rustls::ServerConfig>,
    pub stop: AtomicBool,
    pub devices: Mutex<Vec<Seen>>,
    pub transfers: Mutex<Vec<Transfer>>,
    pub pending: Mutex<HashMap<String, Arc<Pending>>>,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub cancels: Mutex<HashMap<String, Arc<CancelHandle>>>,
    pub trusted: Mutex<HashSet<String>>,
    /// Pulse bridge pair keys by device fingerprint (also in `known-devices.json`).
    pub pairs: Mutex<HashMap<String, Pair>>,
    /// Nonces of bridge messages already accepted.
    pub replay: Mutex<ReplayGuard>,
    pub on_event: Arc<dyn Fn(Event) + Send + Sync>,
    pub last_progress: Mutex<Instant>,
    pub connections: AtomicUsize,
    pub warnings: Mutex<Vec<String>>,
    /// 0 unknown, 1 reachable, 2 macOS is refusing local network access.
    pub local_network: std::sync::atomic::AtomicU8,
    /// A sweep of the local networks is running.
    pub scanning: AtomicBool,
    /// Someone asked for a sweep; the scanner thread takes it.
    pub scan_requested: AtomicBool,
    /// This Mac's own addresses and when they were read.
    pub own_ips: Mutex<(Instant, Vec<Ipv4Addr>)>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Inner {
    pub fn emit(&self, event: Event) {
        (self.on_event)(event);
    }

    fn config(&self) -> Config {
        lock(&self.cfg).clone()
    }

    /// Whether `ip` is one of this Mac's own addresses.
    fn is_own_ip(&self, ip: IpAddr) -> bool {
        let IpAddr::V4(v4) = ip else {
            return ip.is_loopback();
        };
        if v4.is_loopback() {
            return true;
        }
        let mut own = lock(&self.own_ips);
        if own.0.elapsed() > Duration::from_secs(30) {
            *own = (Instant::now(), discovery::local_ipv4s());
        }
        own.1.contains(&v4)
    }

    /// Record a device heard from `ip`, unless it is this Mac itself. Emits
    /// `Devices` when the list changed. Returns whether the device was taken.
    pub fn upsert(&self, info: &DeviceInfo, ip: IpAddr) -> bool {
        if info.fingerprint.is_empty()
            || info.fingerprint == self.me.fingerprint
            || self.is_own_ip(ip)
        {
            return false;
        }
        let device = Device {
            fingerprint: info.fingerprint.clone(),
            alias: info.alias.clone(),
            device_model: info.device_model.clone(),
            device_type: info.device_type.clone(),
            ip: ip.to_string(),
            port: info.port,
            protocol: info.protocol.clone(),
        };
        let changed = {
            let mut devices = lock(&self.devices);
            match devices
                .iter_mut()
                .find(|s| s.device.fingerprint == device.fingerprint)
            {
                Some(seen) => {
                    let changed = seen.device.alias != device.alias
                        || seen.device.ip != device.ip
                        || seen.device.port != device.port
                        || seen.device.protocol != device.protocol;
                    seen.device = device;
                    seen.last_seen = Instant::now();
                    seen.misses = 0;
                    changed
                }
                None => {
                    devices.push(Seen {
                        device,
                        last_seen: Instant::now(),
                        misses: 0,
                    });
                    true
                }
            }
        };
        if changed {
            self.emit(Event::Devices);
        }
        true
    }

    /// Sweep the local networks for devices multicast didn't find.
    fn scan(&self) {
        self.scanning.store(true, Ordering::Relaxed);
        self.emit(Event::Changed);
        discovery::sweep(&self.me, &self.stop, &|heard| {
            self.upsert(&heard.info, heard.ip);
        });
        self.scanning.store(false, Ordering::Relaxed);
        self.emit(Event::Changed);
    }

    pub fn device_list(&self) -> Vec<Device> {
        let mut list: Vec<Device> = lock(&self.devices)
            .iter()
            .map(|s| s.device.clone())
            .collect();
        list.sort_by(|a, b| a.alias.to_lowercase().cmp(&b.alias.to_lowercase()));
        list
    }

    /// A device the user accepted before, still at the address it was heard at.
    pub fn is_known(&self, fingerprint: &str, ip: IpAddr) -> bool {
        if fingerprint.is_empty() || !lock(&self.trusted).contains(fingerprint) {
            return false;
        }
        lock(&self.devices)
            .iter()
            .any(|s| s.device.fingerprint == fingerprint && s.device.ip == ip.to_string())
    }

    pub fn trust(&self, fingerprint: &str) {
        if fingerprint.is_empty() {
            return;
        }
        let added = lock(&self.trusted).insert(fingerprint.to_string());
        if added {
            self.save_known();
        }
    }

    /// Persist the accepted devices and their pair keys.
    fn save_known(&self) {
        let mut fingerprints: Vec<String> = lock(&self.trusted).iter().cloned().collect();
        fingerprints.sort();
        let pairs = lock(&self.pairs).clone();
        let list: Vec<KnownEntry> = fingerprints
            .into_iter()
            .map(|fingerprint| match pairs.get(&fingerprint) {
                Some(pair) => KnownEntry::Paired {
                    alias: pair.alias.clone(),
                    key: pair.key.clone(),
                    fingerprint,
                },
                None => KnownEntry::Plain(fingerprint),
            })
            .collect();
        let path = self.config().state_dir.join("known-devices.json");
        if let Ok(text) = serde_json::to_vec(&list) {
            let _ = write_atomic(&path, &text);
        }
    }

    /// Remember the pair key for a device the user accepted, and trust it.
    pub fn store_pair(&self, fingerprint: &str, alias: &str, key: &str) {
        if fingerprint.is_empty() {
            return;
        }
        lock(&self.pairs).insert(
            fingerprint.to_string(),
            Pair {
                alias: alias.to_string(),
                key: key.to_string(),
            },
        );
        lock(&self.trusted).insert(fingerprint.to_string());
        self.save_known();
    }

    /// The pair key for a device, decoded.
    pub fn pair_key(&self, fingerprint: &str) -> Option<Vec<u8>> {
        lock(&self.pairs)
            .get(fingerprint)
            .and_then(|p| envelope::from_hex(&p.key))
    }

    pub fn add_transfer(&self, transfer: Transfer) {
        let mut transfers = lock(&self.transfers);
        transfers.push(transfer);
        while transfers.len() > 12 {
            match transfers.iter().position(|t| !t.is_open()) {
                Some(index) => {
                    transfers.remove(index);
                }
                None => break,
            }
        }
    }

    /// Change one transfer. A progress event goes out at most ten times a
    /// second unless `force` is set.
    pub fn update_transfer(&self, id: &str, force: bool, change: impl FnOnce(&mut Transfer)) {
        let updated = {
            let mut transfers = lock(&self.transfers);
            transfers.iter_mut().find(|t| t.id == id).map(|t| {
                change(t);
                t.clone()
            })
        };
        let Some(transfer) = updated else { return };
        let due = {
            let mut last = lock(&self.last_progress);
            if force || last.elapsed() >= Duration::from_millis(100) {
                *last = Instant::now();
                true
            } else {
                false
            }
        };
        if due {
            self.emit(Event::Progress(transfer));
        }
    }

    /// Put a transfer in an end state, once.
    pub fn end_transfer(&self, id: &str, state: &str, error: Option<String>) {
        let ended = {
            let mut transfers = lock(&self.transfers);
            transfers
                .iter_mut()
                .find(|t| t.id == id && t.is_open())
                .map(|t| {
                    t.state = state.to_string();
                    t.error = error;
                    t.finished = Some(now_ms());
                    if state == "done" {
                        t.done_bytes = t.total_bytes;
                        t.files_done = t.files_total;
                    }
                    t.current = None;
                    t.clone()
                })
        };
        if let Some(transfer) = ended {
            self.emit(Event::Finished(transfer));
        }
    }
}

/// One `prepare-upload` that carries its whole payload in `preview`; returns
/// the HTTP status. Used by bridge messages and pairing offers.
fn post_preview(
    me: &DeviceInfo,
    device: &Device,
    file_type: &str,
    file_name: &str,
    preview: String,
    timeout: Duration,
) -> Result<u16, String> {
    let ip: IpAddr = device
        .ip
        .parse()
        .map_err(|_| "That device has no usable address.".to_string())?;
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "b0".to_string(),
        proto::FileMeta {
            id: "b0".to_string(),
            file_name: file_name.to_string(),
            size: preview.len() as u64,
            file_type: file_type.to_string(),
            sha256: None,
            preview: Some(preview),
            metadata: None,
        },
    );
    let mut info = me.clone();
    info.announce = None;
    let body = serde_json::to_vec(&proto::PrepareUploadRequest { info, files })
        .map_err(|e| e.to_string())?;
    net::request_json(
        ip,
        device.port,
        device.protocol == "https",
        &device.fingerprint,
        "POST",
        &format!("{}/prepare-upload", proto::API),
        Some(&body),
        timeout,
    )
    .map(|reply| reply.status)
    .map_err(|e| format!("Couldn't reach {}: {e}", device.alias))
}

/// The words for a taken port. Hosts recognise it by `PORT_IN_USE`.
pub const PORT_IN_USE: &str = "already in use";

fn port_in_use(port: u16) -> String {
    format!(
        "Port {port} is {PORT_IN_USE}. Quit the LocalSend app (or anything else using it) and try again."
    )
}

/// A running sharing service. Dropping it stops it.
pub struct Service {
    inner: Arc<Inner>,
}

impl Service {
    pub fn start(
        config: Config,
        on_event: Arc<dyn Fn(Event) + Send + Sync>,
    ) -> Result<Service, String> {
        let identity = net::Identity::load_or_create(&config.state_dir)
            .map_err(|e| format!("Couldn't create this Mac's sharing certificate: {e}"))?;
        let tls = net::server_config(&identity)
            .map_err(|e| format!("Couldn't set up secure sharing: {e}"))?;
        // Windows lets a second program bind a port another one holds in some
        // cases (a dual-stack listener); a connection that succeeds first says
        // somebody is already listening, so say so instead of failing silently.
        #[cfg(windows)]
        {
            let local = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
            if TcpStream::connect_timeout(&local, Duration::from_millis(300)).is_ok() {
                return Err(port_in_use(config.port));
            }
        }
        let listener = TcpListener::bind(("0.0.0.0", config.port)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse
                || (cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied)
            {
                port_in_use(config.port)
            } else {
                format!("Couldn't listen on port {}: {e}", config.port)
            }
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("Couldn't listen on port {}: {e}", config.port))?;

        let (trusted, pairs) = load_known(&config.state_dir.join("known-devices.json"));
        let me = DeviceInfo {
            alias: config.alias.clone(),
            version: proto::VERSION.to_string(),
            device_model: Some(config.device_model.clone()),
            device_type: Some("desktop".to_string()),
            fingerprint: identity.fingerprint.clone(),
            port: config.port,
            protocol: "https".to_string(),
            download: false,
            announce: None,
        };
        let inner = Arc::new(Inner {
            cfg: Mutex::new(config.clone()),
            me,
            tls,
            stop: AtomicBool::new(false),
            devices: Mutex::new(Vec::new()),
            transfers: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            trusted: Mutex::new(trusted),
            pairs: Mutex::new(pairs),
            replay: Mutex::new(ReplayGuard::default()),
            on_event,
            last_progress: Mutex::new(Instant::now()),
            connections: AtomicUsize::new(0),
            warnings: Mutex::new(Vec::new()),
            local_network: std::sync::atomic::AtomicU8::new(0),
            scanning: AtomicBool::new(false),
            scan_requested: AtomicBool::new(false),
            own_ips: Mutex::new((Instant::now(), discovery::local_ipv4s())),
        });

        spawn_acceptor(inner.clone(), listener);
        match discovery::listen(config.port) {
            Ok(socket) => spawn_discovery(inner.clone(), socket),
            Err(e) => lock(&inner.warnings).push(format!(
                "Nearby devices can't be found automatically ({e}). Devices that find this Mac first still work."
            )),
        }
        spawn_announcer(inner.clone());
        spawn_scanner(inner.clone());
        spawn_liveness(inner.clone());
        Ok(Service { inner })
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = &self.inner;
        let cfg = inner.config();
        let mut incoming: Vec<Incoming> = lock(&inner.pending)
            .values()
            .map(|p| p.incoming.clone())
            .collect();
        incoming.sort_by(|a, b| a.id.cmp(&b.id));
        Snapshot {
            alias: cfg.alias,
            fingerprint: inner.me.fingerprint.clone(),
            port: cfg.port,
            save_dir: cfg.save_dir.to_string_lossy().into_owned(),
            devices: inner.device_list(),
            incoming,
            transfers: lock(&inner.transfers).clone(),
            warnings: lock(&inner.warnings).clone(),
            local_network: match inner.local_network.load(Ordering::Relaxed) {
                1 => "granted",
                2 => "blocked",
                _ => "unknown",
            }
            .to_string(),
            scanning: inner.scanning.load(Ordering::Relaxed),
        }
    }

    /// Look for devices again now: announce on multicast and sweep the local
    /// networks (the sweep is skipped while one is already running).
    pub fn refresh(&self) {
        let inner = self.inner.clone();
        thread::spawn(move || {
            let _ = discovery::announce(&inner.me, inner.config().port, true);
        });
        self.inner.scan_requested.store(true, Ordering::Relaxed);
    }

    pub fn devices(&self) -> Vec<Device> {
        self.inner.device_list()
    }

    /// The fingerprint of this Mac's certificate.
    pub fn fingerprint(&self) -> String {
        self.inner.me.fingerprint.clone()
    }

    pub fn set_accept_known(&self, on: bool) {
        lock(&self.inner.cfg).accept_known = on;
    }

    pub fn set_save_dir(&self, directory: PathBuf) {
        lock(&self.inner.cfg).save_dir = directory;
    }

    /// The nearby device with this fingerprint (or, failing that, alias).
    fn find_device(&self, target: &str) -> Option<Device> {
        let devices = lock(&self.inner.devices);
        devices
            .iter()
            .find(|s| s.device.fingerprint == target)
            .or_else(|| {
                devices
                    .iter()
                    .find(|s| s.device.alias.eq_ignore_ascii_case(target))
            })
            .map(|s| s.device.clone())
    }

    /// Whether this device completed Pulse bridge pairing (both sides hold a key).
    pub fn is_paired(&self, fingerprint: &str) -> bool {
        lock(&self.inner.pairs).contains_key(fingerprint)
    }

    /// Fingerprints of every paired device, nearby or not.
    pub fn paired_fingerprints(&self) -> Vec<String> {
        lock(&self.inner.pairs).keys().cloned().collect()
    }

    /// This computer's name as other devices list it.
    pub fn alias(&self) -> String {
        self.inner.config().alias
    }

    /// Sign `envelope` with the pair key of the device (fingerprint or alias)
    /// and deliver it as a bridge upload. No prompt appears on the other side;
    /// it is accepted only from a paired device with a valid signature.
    /// Blocks until the device answers (at most 20 seconds); `from.device` and
    /// `to.device` are set here.
    pub fn send_bridge(&self, target: &str, mut envelope: Envelope) -> Result<(), String> {
        let device = self
            .find_device(target)
            .ok_or_else(|| "That device is no longer nearby.".to_string())?;
        let key = self.inner.pair_key(&device.fingerprint).ok_or_else(|| {
            format!(
                "{} isn't paired for the Pulse bridge yet. Pair it first.",
                device.alias
            )
        })?;
        envelope.from.device = self.inner.me.fingerprint.clone();
        envelope.to.device = device.fingerprint.clone();
        envelope.sign(&key);
        let status = post_preview(
            &self.inner.me,
            &device,
            envelope::BRIDGE_FILE_TYPE,
            "pulse-bridge.json",
            envelope.to_json(),
            Duration::from_secs(20),
        )?;
        match status {
            204 | 200 => Ok(()),
            403 => Err(format!(
                "{} rejected the message (not paired on its side, or the clocks differ).",
                device.alias
            )),
            other => Err(format!(
                "{} answered {other}; it may not run Pulse with the bridge.",
                device.alias
            )),
        }
    }

    /// Ask the device (fingerprint or alias) to pair for the Pulse bridge. A
    /// prompt appears there; on accept both sides keep the same new 32-byte
    /// key in `known-devices.json`. Blocks until answered (at most 190 seconds).
    pub fn bridge_pair(&self, target: &str) -> Result<(), String> {
        let device = self
            .find_device(target)
            .ok_or_else(|| "That device is no longer nearby.".to_string())?;
        let key = envelope::new_pair_key();
        let offer = serde_json::json!({ "offer": key }).to_string();
        // The receiving card shows this name: say what Accept means.
        let title = format!("Pair with {} for Pulse bridge", self.alias());
        let status = post_preview(
            &self.inner.me,
            &device,
            envelope::PAIR_FILE_TYPE,
            &title,
            offer,
            Duration::from_secs(190),
        )?;
        match status {
            204 => {
                self.inner
                    .store_pair(&device.fingerprint, &device.alias, &key);
                self.inner.emit(Event::Changed);
                Ok(())
            }
            403 => Err(format!("{} declined the pairing.", device.alias)),
            other => Err(format!(
                "{} answered {other}; it may not run Pulse with the bridge.",
                device.alias
            )),
        }
    }

    /// Send to the device with this fingerprint (or, failing that, alias).
    /// Returns the transfer id; progress arrives as events.
    pub fn send(&self, target: &str, items: Vec<SendItem>) -> Result<String, String> {
        let device = self
            .find_device(target)
            .ok_or_else(|| "That device is no longer nearby.".to_string())?;
        let ip: IpAddr = device
            .ip
            .parse()
            .map_err(|_| "That device has no usable address.".to_string())?;
        let peer = Peer {
            ip,
            port: device.port,
            https: device.protocol == "https",
            fingerprint: device.fingerprint.clone(),
            alias: device.alias.clone(),
        };
        let id = proto::random_hex(8);
        self.inner.add_transfer(Transfer {
            id: id.clone(),
            direction: "send".into(),
            peer: device.alias.clone(),
            peer_fingerprint: device.fingerprint.clone(),
            state: "waiting".into(),
            total_bytes: 0,
            done_bytes: 0,
            files_total: 0,
            files_done: 0,
            current: None,
            saved_to: None,
            saved_files: Vec::new(),
            error: None,
            message: None,
            started: now_ms(),
            finished: None,
        });
        let handle = Arc::new(CancelHandle {
            flag: AtomicBool::new(false),
            stream: Mutex::new(None),
        });
        lock(&self.inner.cancels).insert(id.clone(), handle.clone());
        self.inner.emit(Event::Changed);
        let inner = self.inner.clone();
        let transfer_id = id.clone();
        thread::spawn(move || run_send(inner, transfer_id, peer, items, handle));
        Ok(id)
    }

    /// Answer a waiting request.
    pub fn respond(&self, id: &str, accept: bool) -> bool {
        let pending = lock(&self.inner.pending).get(id).cloned();
        let Some(pending) = pending else { return false };
        *lock(&pending.decision) = Some(accept);
        pending.changed.notify_all();
        true
    }

    /// Cancel a transfer, either direction.
    pub fn cancel(&self, transfer_id: &str) -> bool {
        if let Some(handle) = lock(&self.inner.cancels).get(transfer_id).cloned() {
            handle.flag.store(true, Ordering::Relaxed);
            if let Some(stream) = lock(&handle.stream).as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            return true;
        }
        receive::cancel_receive(&self.inner, transfer_id)
    }

    /// Forget a finished transfer.
    pub fn dismiss(&self, transfer_id: &str) {
        lock(&self.inner.transfers).retain(|t| t.id != transfer_id || t.is_open());
        self.inner.emit(Event::Changed);
    }

    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        let pending: Vec<Arc<Pending>> = lock(&self.inner.pending).values().cloned().collect();
        for p in pending {
            *lock(&p.decision) = Some(false);
            p.changed.notify_all();
        }
        let handles: Vec<Arc<CancelHandle>> = lock(&self.inner.cancels).values().cloned().collect();
        for handle in handles {
            handle.flag.store(true, Ordering::Relaxed);
            if let Some(stream) = lock(&handle.stream).as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        for session in lock(&self.inner.sessions).values() {
            session.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- background threads ----------------------------------------------------

fn spawn_acceptor(inner: Arc<Inner>, listener: TcpListener) {
    thread::spawn(move || {
        while !inner.stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((socket, address)) => {
                    let _ = socket.set_nonblocking(false);
                    if inner.connections.load(Ordering::Relaxed) >= 64 {
                        continue;
                    }
                    inner.connections.fetch_add(1, Ordering::Relaxed);
                    let inner = inner.clone();
                    thread::spawn(move || {
                        receive::handle_connection(&inner, socket, address);
                        inner.connections.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(_) => thread::sleep(Duration::from_millis(250)),
            }
        }
    });
}

fn spawn_discovery(inner: Arc<Inner>, socket: UdpSocket) {
    thread::spawn(move || {
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let mut buffer = vec![0u8; 64 * 1024];
        let mut rejoined = Instant::now();
        while !inner.stop.load(Ordering::Relaxed) {
            if rejoined.elapsed() >= Duration::from_secs(30) {
                rejoined = Instant::now();
                discovery::rejoin(&socket);
            }
            let Ok((n, from)) = socket.recv_from(&mut buffer) else {
                continue;
            };
            let Some(heard) = discovery::parse(&buffer[..n], from) else {
                continue;
            };
            if !inner.upsert(&heard.info, heard.ip) {
                continue;
            }
            if heard.info.announce.unwrap_or(false) {
                let inner = inner.clone();
                thread::spawn(move || answer_announcement(&inner, &heard));
            }
        }
    });
}

/// Reply to an announcement the way the protocol asks: register with the
/// sender over HTTP, and fall back to a multicast reply.
fn answer_announcement(inner: &Arc<Inner>, heard: &discovery::Heard) {
    let mut me = inner.me.clone();
    me.announce = None;
    let registered = serde_json::to_vec(&me).ok().is_some_and(|body| {
        net::request_json(
            heard.ip,
            heard.info.port,
            heard.info.protocol == "https",
            &heard.info.fingerprint,
            "POST",
            &format!("{}/register", proto::API),
            Some(&body),
            Duration::from_secs(3),
        )
        .is_ok_and(|reply| reply.status == 200)
    });
    if !registered {
        let _ = discovery::announce(&inner.me, inner.config().port, false);
    }
}

fn spawn_announcer(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut waits = vec![100u64, 400, 1500].into_iter();
        while !inner.stop.load(Ordering::Relaxed) {
            let sent = discovery::announce(&inner.me, inner.config().port, true);
            inner.local_network.store(
                match &sent {
                    Ok(()) => 1,
                    // EHOSTUNREACH: what a denied Local Network permission looks like.
                    #[cfg(target_os = "macos")]
                    Err(e) if e.raw_os_error() == Some(65) => 2,
                    // WSAEACCES: Windows Firewall (or policy) refusing the multicast send.
                    #[cfg(windows)]
                    Err(e) if e.raw_os_error() == Some(10013) => 2,
                    Err(_) => 0,
                },
                Ordering::Relaxed,
            );
            inner.emit(Event::Changed);
            let wait = waits.next().unwrap_or(30_000);
            let until = Instant::now() + Duration::from_millis(wait);
            while Instant::now() < until && !inner.stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

/// Sweep the local networks shortly after start (multicast gets the first
/// word), when asked, and every minute while no device is known.
fn spawn_scanner(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut due = Instant::now() + Duration::from_secs(2);
        let mut first = true;
        while !inner.stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
            let asked = inner.scan_requested.load(Ordering::Relaxed);
            let waited = Instant::now() >= due;
            if !asked && !waited {
                continue;
            }
            if asked || first || lock(&inner.devices).is_empty() {
                inner.scan();
                inner.scan_requested.store(false, Ordering::Relaxed);
                first = false;
            }
            due = Instant::now() + Duration::from_secs(60);
        }
    });
}

/// Drop devices that stop answering `GET /info`.
fn spawn_liveness(inner: Arc<Inner>) {
    thread::spawn(move || {
        while !inner.stop.load(Ordering::Relaxed) {
            for _ in 0..200 {
                if inner.stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            let stale: Vec<Device> = lock(&inner.devices)
                .iter()
                .filter(|s| s.last_seen.elapsed() > Duration::from_secs(20))
                .map(|s| s.device.clone())
                .collect();
            for device in stale {
                let inner = inner.clone();
                thread::spawn(move || {
                    let alive = device.ip.parse::<IpAddr>().ok().is_some_and(|ip| {
                        net::request_json(
                            ip,
                            device.port,
                            device.protocol == "https",
                            &device.fingerprint,
                            "GET",
                            &format!("{}/info", proto::API),
                            None,
                            Duration::from_secs(2),
                        )
                        .is_ok_and(|reply| reply.status == 200)
                    });
                    let removed = {
                        let mut devices = lock(&inner.devices);
                        match devices
                            .iter()
                            .position(|s| s.device.fingerprint == device.fingerprint)
                        {
                            Some(index) if alive => {
                                devices[index].last_seen = Instant::now();
                                devices[index].misses = 0;
                                false
                            }
                            Some(index) => {
                                devices[index].misses += 1;
                                if devices[index].misses >= 2 {
                                    devices.remove(index);
                                    true
                                } else {
                                    false
                                }
                            }
                            None => false,
                        }
                    };
                    if removed {
                        inner.emit(Event::Devices);
                    }
                });
            }
        }
    });
}

fn run_send(
    inner: Arc<Inner>,
    id: String,
    peer: Peer,
    items: Vec<SendItem>,
    handle: Arc<CancelHandle>,
) {
    let finish = |state: &str, error: Option<String>| {
        inner.end_transfer(&id, state, error);
        lock(&inner.cancels).remove(&id);
    };
    let entries = match send::expand(&items) {
        Ok(entries) => entries,
        Err(message) => return finish("failed", Some(message)),
    };
    inner.update_transfer(&id, true, |t| {
        t.files_total = entries.len();
        t.total_bytes = entries.iter().map(|e| e.size).sum();
    });
    let register = |stream: Option<TcpStream>| {
        *lock(&handle.stream) = stream;
    };
    // A message counts as sent once it is on the other screen.
    let delivered = std::cell::Cell::new(false);
    let result = send::deliver(
        &inner.me,
        &peer,
        &entries,
        &handle.flag,
        &register,
        &mut |p| {
            if p.phase == send::Phase::Delivered {
                delivered.set(true);
                inner.end_transfer(&id, "done", None);
                return;
            }
            let waiting = p.phase == send::Phase::Waiting;
            inner.update_transfer(&id, p.total > 0 && p.done >= p.total, |t| {
                t.state = if waiting { "waiting" } else { "active" }.to_string();
                t.total_bytes = p.total;
                t.done_bytes = p.done;
                t.files_total = p.files_total;
                t.files_done = p.files_done;
                t.current = p.current.clone();
            });
        },
    );
    if delivered.get() {
        lock(&inner.cancels).remove(&id);
        return;
    }
    match result {
        Ok(send::Outcome::Done) => finish("done", None),
        Ok(send::Outcome::Declined) => finish("declined", None),
        Ok(send::Outcome::Cancelled) => finish("cancelled", None),
        Err(_) if handle.flag.load(Ordering::Relaxed) => finish("cancelled", None),
        Err(message) => finish("failed", Some(message)),
    }
}
