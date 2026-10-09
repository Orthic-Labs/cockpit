//! Whole-disk index for the hub, backed by `rightkit-fsindex`: fast file-name
//! search and folder/file sizes. One index per mounted volume, built on a
//! background thread (never on a UI path) and kept live by the crate's own
//! FSEvents watcher. State lives in `<Pulse state dir>/fsindex/<volume>` so a
//! later launch resumes from a snapshot instead of crawling again.
//!
//! Content search is not used: the crate's `content` feature is not enabled
//! and file contents are never opened.
//!
//! Without Full Disk Access the crate skips protected and consent-gated
//! folders; `disk_index_status` reports that as `partial` and lists how many
//! folders were unreadable. Sizes the index does not know are `None`, never 0.
//!
//! Measurement lines (one per event, in scan.log via `scanner::log`):
//!   fsindex crawl start volume=<mount> footprint_mb=<f> rss_mb=<f>
//!   fsindex crawl end volume=<mount> entries=<n> crawl_ms=<n> open_ms=<n> resumed=<bool> full_disk_access=<bool> unreadable_dirs=<n> footprint_mb=<f> rss_mb=<f>
//!   fsindex crawl failed volume=<mount> error=<text>
//!   fsindex first search hits=<n> search_ms=<n> volumes=<n> footprint_mb=<f> rss_mb=<f>
//! `footprint_mb` is the process phys_footprint (TASK_VM_INFO), `rss_mb` its resident size.

use serde::Serialize;
use std::path::PathBuf;

/// One search result, mapped to a Storage row by the caller.
pub struct Found {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub bytes: u64,
}

#[derive(Serialize, Clone)]
pub struct VolumeStatus {
    pub mount_point: String,
    /// "building", "ready" or "failed".
    pub state: String,
    pub entries: usize,
    pub crawl_ms: u64,
    pub resumed_from_snapshot: bool,
    pub full_disk_access: bool,
    /// Folders skipped or unreadable: sizes and matches under them are unknown.
    pub partial: bool,
    pub unreadable_dirs: usize,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct DiskIndexStatus {
    pub supported: bool,
    pub volumes: Vec<VolumeStatus>,
}

/// Size of one path as the index knows it. `known` is false when the path is
/// not indexed (not built yet, excluded, unreadable): then the numbers are 0
/// and must not be shown.
#[derive(Serialize)]
pub struct IndexedSize {
    pub path: String,
    pub known: bool,
    pub allocated: u64,
    pub allocated_unique: u64,
    pub logical: u64,
    pub files: u64,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use rightkit_fsindex::{Config, EntryKind, Index, SearchOptions};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, Once};
    use std::time::Instant;

    use crate::scanner::{log, low_priority};

    enum State {
        Building,
        Ready(Arc<Mutex<Index>>),
        Failed(String),
    }

    struct Vol {
        mount: PathBuf,
        state: State,
        info: Option<VolumeStatus>,
    }

    static VOLUMES: Mutex<Vec<Vol>> = Mutex::new(Vec::new());
    static START: Once = Once::new();
    static FIRST_SEARCH_LOGGED: AtomicBool = AtomicBool::new(false);

    fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    // --- process memory (logging only) -------------------------------------------

    /// `task_vm_info` up to and including `phys_footprint` (revision 1).
    #[repr(C)]
    #[derive(Default)]
    struct TaskVmInfo {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }

    extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }

    const TASK_VM_INFO: i32 = 22;

    /// (phys_footprint, resident size) of this process, in bytes.
    fn memory() -> Option<(u64, u64)> {
        let mut info = TaskVmInfo::default();
        let mut count = (std::mem::size_of::<TaskVmInfo>() / 4) as u32;
        // SAFETY: `info` is a plain repr(C) struct at least `count` words long.
        let status = unsafe {
            task_info(mach_task_self(), TASK_VM_INFO, &mut info as *mut TaskVmInfo as *mut i32, &mut count)
        };
        (status == 0).then_some((info.phys_footprint, info.resident_size))
    }

    fn mem_fields() -> String {
        const MB: f64 = 1024.0 * 1024.0;
        match memory() {
            Some((footprint, rss)) => {
                format!("footprint_mb={:.1} rss_mb={:.1}", footprint as f64 / MB, rss as f64 / MB)
            }
            None => "footprint_mb=? rss_mb=?".to_string(),
        }
    }

    // --- build ---------------------------------------------------------------------

    /// Mount points to index: the startup disk and drives under /Volumes (not
    /// system volumes or mounted disk images), as the Storage volume list shows.
    fn mounts() -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for disk in pulse_core::system_status().disks {
            let mount = disk.mount_point.clone();
            let internal = mount == "/";
            if !internal && !mount.starts_with("/Volumes/") {
                continue;
            }
            if mount.contains("com.apple.") || out.iter().any(|m| m.as_os_str() == mount.as_str()) {
                continue;
            }
            if !internal && crate::is_disk_image(&mount) {
                continue;
            }
            out.push(PathBuf::from(mount));
        }
        // The startup disk first: it is the one search needs.
        out.sort_by_key(|m| m.as_os_str() != "/");
        out
    }

    fn state_name(mount: &std::path::Path) -> String {
        if mount.as_os_str() == "/" {
            return "root".to_string();
        }
        let text = mount.to_string_lossy();
        let name: String = text
            .trim_start_matches("/Volumes/")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("vol-{name}")
    }

    fn set_state(mount: &std::path::Path, state: State, info: Option<VolumeStatus>) {
        let mut vols = lock(&VOLUMES);
        if let Some(vol) = vols.iter_mut().find(|v| v.mount == mount) {
            vol.state = state;
            vol.info = info;
        }
    }

    fn build_one(mount: PathBuf) {
        log(&format!("fsindex crawl start volume={} {}", mount.display(), mem_fields()));
        let mut config = Config::new(vec![mount.clone()]);
        config.state_dir = Some(crate::cache::dir().join("fsindex").join(state_name(&mount)));
        // Never raise a privacy prompt: without Full Disk Access consent-gated
        // folders are skipped and the volume reports as partial.
        config.allow_consent_prompts = false;
        config.include_protected = false;
        config.watch = true;
        if let Some(dir) = &config.state_dir {
            let _ = std::fs::create_dir_all(dir);
        }
        let started = Instant::now();
        match Index::open(config) {
            Ok(index) => {
                let open_ms = started.elapsed().as_millis() as u64;
                let status = index.status();
                let unreadable = status.unreadable_count.max(status.unreadable_dirs.len());
                let roots_readable = status.roots.iter().all(|r| r.readable);
                let partial = !status.full_disk_access || unreadable > 0 || !status.skipped_dirs.is_empty() || !roots_readable;
                let crawl_ms = status.crawl_time.as_millis() as u64;
                log(&format!(
                    "fsindex crawl end volume={} entries={} crawl_ms={} open_ms={} resumed={} full_disk_access={} unreadable_dirs={} {}",
                    mount.display(),
                    status.entries,
                    crawl_ms,
                    open_ms,
                    status.resumed_from_snapshot,
                    status.full_disk_access,
                    unreadable,
                    mem_fields()
                ));
                let info = VolumeStatus {
                    mount_point: mount.to_string_lossy().into_owned(),
                    state: "ready".into(),
                    entries: status.entries,
                    crawl_ms,
                    resumed_from_snapshot: status.resumed_from_snapshot,
                    full_disk_access: status.full_disk_access,
                    partial,
                    unreadable_dirs: unreadable,
                    error: None,
                };
                set_state(&mount, State::Ready(Arc::new(Mutex::new(index))), Some(info));
            }
            Err(error) => {
                log(&format!("fsindex crawl failed volume={} error={error}", mount.display()));
                set_state(&mount, State::Failed(error.to_string()), None);
            }
        }
    }

    /// Start building the indexes in the background, once. Volumes are built one
    /// after another (startup disk first) at low priority so timings are clean
    /// and the machine stays responsive. A no-op in QA runs (isolated HOME) and
    /// when `PULSE_NO_DISK_INDEX` is set; search then uses the scan path.
    pub fn start_background() {
        if std::env::var_os("RIGHTKIT_PULSE_QA_HOME").is_some() || std::env::var_os("PULSE_NO_DISK_INDEX").is_some() {
            return;
        }
        START.call_once(|| {
            let _ = std::thread::Builder::new().name("pulse-fsindex".into()).spawn(|| {
                low_priority();
                let mounts = mounts();
                {
                    let mut vols = lock(&VOLUMES);
                    for mount in &mounts {
                        vols.push(Vol { mount: mount.clone(), state: State::Building, info: None });
                    }
                }
                for mount in mounts {
                    build_one(mount);
                }
            });
        });
    }

    // --- queries -------------------------------------------------------------------

    /// Ready indexes, startup disk first. Empty until the startup disk is ready.
    fn ready() -> Vec<(PathBuf, Arc<Mutex<Index>>)> {
        let vols = lock(&VOLUMES);
        let startup_ready = vols
            .iter()
            .any(|v| v.mount.as_os_str() == "/" && matches!(v.state, State::Ready(_)));
        if !startup_ready {
            return Vec::new();
        }
        vols.iter()
            .filter_map(|v| match &v.state {
                State::Ready(index) => Some((v.mount.clone(), index.clone())),
                _ => None,
            })
            .collect()
    }

    fn index_lock(index: &Arc<Mutex<Index>>) -> MutexGuard<'_, Index> {
        index.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn extension_is(name: &str, exts: &[String]) -> bool {
        name.rsplit_once('.').is_some_and(|(stem, found)| {
            !stem.is_empty() && exts.iter().any(|ext| found.eq_ignore_ascii_case(ext))
        })
    }

    /// Ranked name matches across the ready volumes, or `None` when the index is
    /// not ready, the query is empty (extensions only) or it cannot be parsed:
    /// the caller then uses the scan path. `exts` are lowercase, without dot.
    pub fn search(query: &str, limit: usize, exts: &[String]) -> Option<Vec<Found>> {
        if query.trim().is_empty() {
            return None;
        }
        let indexes = ready();
        if indexes.is_empty() {
            return None;
        }
        let started = Instant::now();
        // The crate has no extension filter: over-fetch, then filter.
        let fetch = if exts.is_empty() { limit } else { (limit * 8).min(8000) };
        let mut hits: Vec<(i32, Found)> = Vec::new();
        for (_, index) in &indexes {
            let opts = SearchOptions { limit: fetch, scope: None };
            let Ok(found) = index_lock(index).search_with(query, &opts) else {
                return None;
            };
            for hit in found {
                let name = hit.item.name.to_string_lossy().into_owned();
                let is_dir = hit.item.kind == EntryKind::Dir;
                if !exts.is_empty() && (is_dir || !extension_is(&name, exts)) {
                    continue;
                }
                hits.push((
                    hit.score,
                    Found { path: hit.item.path, name, is_dir, bytes: hit.item.size.allocated_unique },
                ));
            }
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0));
        hits.truncate(limit);
        if !FIRST_SEARCH_LOGGED.swap(true, Ordering::SeqCst) {
            log(&format!(
                "fsindex first search hits={} search_ms={} volumes={} {}",
                hits.len(),
                started.elapsed().as_millis(),
                indexes.len(),
                mem_fields()
            ));
        }
        Some(hits.into_iter().map(|(_, found)| found).collect())
    }

    /// Sizes for `paths`, each from the volume index that holds it.
    pub fn sizes(paths: &[String]) -> Vec<IndexedSize> {
        let indexes = ready();
        paths
            .iter()
            .map(|path| {
                let mut best: Option<IndexedSize> = None;
                for (mount, index) in &indexes {
                    if !std::path::Path::new(path).starts_with(mount) {
                        continue;
                    }
                    // The longest mount wins (an external drive under /Volumes beats "/").
                    if let Some(size) = index_lock(index).folder_size(path) {
                        best = Some(IndexedSize {
                            path: path.clone(),
                            known: true,
                            allocated: size.allocated,
                            allocated_unique: size.allocated_unique,
                            logical: size.logical,
                            files: size.files,
                        });
                        if mount.as_os_str() != "/" {
                            break;
                        }
                    }
                }
                best.unwrap_or(IndexedSize {
                    path: path.clone(),
                    known: false,
                    allocated: 0,
                    allocated_unique: 0,
                    logical: 0,
                    files: 0,
                })
            })
            .collect()
    }

    pub fn status() -> DiskIndexStatus {
        let vols = lock(&VOLUMES);
        DiskIndexStatus {
            supported: true,
            volumes: vols
                .iter()
                .map(|v| {
                    let mount_point = v.mount.to_string_lossy().into_owned();
                    match (&v.state, &v.info) {
                        (State::Ready(_), Some(info)) => info.clone(),
                        (State::Failed(error), _) => VolumeStatus {
                            mount_point,
                            state: "failed".into(),
                            entries: 0,
                            crawl_ms: 0,
                            resumed_from_snapshot: false,
                            full_disk_access: false,
                            partial: true,
                            unreadable_dirs: 0,
                            error: Some(error.clone()),
                        },
                        _ => VolumeStatus {
                            mount_point,
                            state: "building".into(),
                            entries: 0,
                            crawl_ms: 0,
                            resumed_from_snapshot: false,
                            full_disk_access: false,
                            partial: true,
                            unreadable_dirs: 0,
                            error: None,
                        },
                    }
                })
                .collect(),
        }
    }

}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn start_background() {}
    pub fn search(_query: &str, _limit: usize, _exts: &[String]) -> Option<Vec<Found>> {
        None
    }
    pub fn sizes(paths: &[String]) -> Vec<IndexedSize> {
        paths
            .iter()
            .map(|path| IndexedSize { path: path.clone(), known: false, allocated: 0, allocated_unique: 0, logical: 0, files: 0 })
            .collect()
    }
    pub fn status() -> DiskIndexStatus {
        DiskIndexStatus { supported: false, volumes: Vec::new() }
    }
}

pub(crate) use imp::{search, start_background};

/// State of each volume's index: building, ready (with entry count and whether
/// it is partial for lack of Full Disk Access), or failed.
#[tauri::command]
pub fn disk_index_status() -> DiskIndexStatus {
    imp::status()
}

/// Sizes of `paths` from the index. Unknown paths come back `known: false`.
#[tauri::command]
pub async fn disk_index_sizes(paths: Vec<String>) -> Result<Vec<IndexedSize>, String> {
    tauri::async_runtime::spawn_blocking(move || imp::sizes(&paths))
        .await
        .map_err(|e| e.to_string())
}
