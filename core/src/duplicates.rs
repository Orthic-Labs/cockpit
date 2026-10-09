//! Explicit, bounded exact-content duplicate inspection.
//!
//! Normal scans never call this module.  Candidate selection is metadata-only;
//! bytes are opened and read only after this operation is explicitly invoked.

use crate::model::{EntryKind, FileIdentity, FileMetadata};
use crate::platform;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const DEFAULT_MIN_DUPLICATE_BYTES: u64 = 100 * 1024;
pub const DEFAULT_MAX_FILES: usize = 100_000;
pub const DEFAULT_MAX_TOTAL_READ_BYTES: u64 = 1 << 30;
pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(30);
const SAMPLE_BYTES: usize = 4096;
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Bounds for one explicit duplicate operation.  `deadline` is relative to
/// operation start, so callers cannot accidentally pass an already-expired
/// wall-clock value from another operation.
#[derive(Clone, Debug)]
pub struct DuplicateOptions {
    pub min_bytes: u64,
    pub max_files: usize,
    pub max_total_read_bytes: u64,
    pub deadline: Duration,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        Self {
            min_bytes: DEFAULT_MIN_DUPLICATE_BYTES,
            max_files: DEFAULT_MAX_FILES,
            max_total_read_bytes: DEFAULT_MAX_TOTAL_READ_BYTES,
            deadline: DEFAULT_DEADLINE,
        }
    }
}

/// Metadata returned by a pinned content handle.  Both identity and size are
/// checked before and after content reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentMetadata {
    pub file_id: FileIdentity,
    pub size: u64,
}

/// A content handle keeps one opened object pinned for the entire comparison.
/// `read_at` is used only for bounded sampling; `read_next` is used for the
/// complete byte-for-byte stream comparison.
pub trait ContentHandle {
    fn metadata(&self) -> Result<ContentMetadata, String>;
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<usize, String>;
    fn read_next(&mut self, buffer: &mut [u8]) -> Result<usize, String>;
}

/// Injectable content boundary for deterministic tests and platform adapters.
/// Implementations must refuse links/placeholders rather than hydrating them.
pub trait ContentReader {
    fn open(&self, path: &Path) -> Result<Box<dyn ContentHandle>, String>;
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DuplicateGroup {
    pub kept_path: PathBuf,
    pub extras: Vec<PathBuf>,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DuplicateSkip {
    pub path: PathBuf,
    pub reason: String,
}

pub type SkippedDuplicate = DuplicateSkip;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub skipped: Vec<DuplicateSkip>,
    pub diagnostics: Vec<String>,
    pub truncated: bool,
    pub files_considered: usize,
    pub bytes_read: u64,
}

impl DuplicateReport {
    fn skip(&mut self, path: impl Into<PathBuf>, reason: impl Into<String>) {
        self.skipped.push(DuplicateSkip {
            path: path.into(),
            reason: reason.into(),
        });
    }
}

/// Run exact duplicate inspection with the platform content adapter.
pub fn find_duplicates(paths: &[PathBuf], options: &DuplicateOptions) -> DuplicateReport {
    #[cfg(unix)]
    {
        find_duplicates_with_reader(paths, options, &UnixContentReader)
    }
    #[cfg(windows)]
    {
        find_duplicates_with_reader(paths, options, &WindowsContentReader)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut report = DuplicateReport::default();
        let _ = (paths, options);
        report
            .diagnostics
            .push("exact duplicate content reads are unsupported by this native adapter".into());
        report.truncated = true;
        report
    }
}

/// Run duplicate inspection through an injected reader.  Candidate metadata
/// still comes from `symlink_metadata` and the native no-follow adapter.
pub fn find_duplicates_with_reader<R: ContentReader>(
    paths: &[PathBuf],
    options: &DuplicateOptions,
    reader: &R,
) -> DuplicateReport {
    let started = Instant::now();
    let mut state = OperationState {
        started,
        options,
        reader,
        report: DuplicateReport::default(),
    };
    let mut candidates = Vec::new();
    let mut roots = paths.to_vec();
    roots.sort();
    roots.dedup();
    for root in roots {
        if state.expired() || candidates.len() >= options.max_files {
            state.truncated();
            break;
        }
        collect_candidates(&mut state, &root, &mut candidates);
    }
    state.report.files_considered = candidates.len();
    candidates.sort_by(|a, b| a.path.cmp(&b.path));

    // Identity is deliberately deduplicated before any content read.  Two
    // names for one inode are hard links, not independent duplicate content.
    let mut identities = BTreeSet::new();
    candidates.retain(|candidate| {
        if !identities.insert(candidate.file_id.clone()) {
            state.report.skip(
                candidate.path.clone(),
                "hard link identity already considered",
            );
            false
        } else {
            true
        }
    });

    let mut by_size: BTreeMap<u64, Vec<Candidate>> = BTreeMap::new();
    for candidate in candidates {
        by_size.entry(candidate.size).or_default().push(candidate);
    }
    for (size, candidates) in by_size {
        if candidates.len() < 2 {
            continue;
        }
        let mut sampled: Vec<(Candidate, Vec<u8>)> = Vec::new();
        for candidate in candidates {
            if state.expired() {
                state.truncated();
                break;
            }
            match state.sample(&candidate) {
                Ok(sample) => sampled.push((candidate, sample)),
                Err(reason) => state.report.skip(candidate.path, reason),
            }
        }
        let mut by_sample: BTreeMap<Vec<u8>, Vec<Candidate>> = BTreeMap::new();
        for (candidate, sample) in sampled {
            by_sample.entry(sample).or_default().push(candidate);
        }
        for candidates in by_sample.into_values() {
            if candidates.len() < 2 {
                continue;
            }
            let mut groups: Vec<DuplicateGroup> = Vec::new();
            let mut representatives: Vec<Candidate> = Vec::new();
            for candidate in candidates {
                if state.expired() {
                    state.truncated();
                    break;
                }
                let mut matched = false;
                for (index, group) in groups.iter_mut().enumerate() {
                    match state.equal(&representatives[index], &candidate) {
                        Ok(true) => {
                            group.extras.push(candidate.path.clone());
                            matched = true;
                            break;
                        }
                        Ok(false) => {}
                        Err(reason) => {
                            state.report.skip(candidate.path.clone(), reason);
                            matched = true;
                            break;
                        }
                    }
                }
                if !matched {
                    representatives.push(candidate.clone());
                    groups.push(DuplicateGroup {
                        kept_path: candidate.path,
                        extras: Vec::new(),
                        size_bytes: size,
                    });
                }
            }
            state
                .report
                .groups
                .extend(groups.into_iter().filter(|group| !group.extras.is_empty()));
        }
    }
    state
        .report
        .groups
        .sort_by(|a, b| a.kept_path.cmp(&b.kept_path));
    state.report.skipped.sort_by(|a, b| a.path.cmp(&b.path));
    state.report
}

#[derive(Clone, Debug)]
struct Candidate {
    path: PathBuf,
    file_id: FileIdentity,
    size: u64,
}

struct OperationState<'a, R> {
    started: Instant,
    options: &'a DuplicateOptions,
    reader: &'a R,
    report: DuplicateReport,
}

impl<'a, R: ContentReader> OperationState<'a, R> {
    fn expired(&self) -> bool {
        self.started.elapsed() >= self.options.deadline
            || self.report.bytes_read >= self.options.max_total_read_bytes
    }

    fn truncated(&mut self) {
        self.report.truncated = true;
        if self.started.elapsed() >= self.options.deadline {
            self.report
                .diagnostics
                .push("duplicate operation deadline reached".into());
        } else if self.report.bytes_read >= self.options.max_total_read_bytes {
            self.report
                .diagnostics
                .push("duplicate content-read budget reached".into());
        } else {
            self.report
                .diagnostics
                .push("duplicate operation bound reached".into());
        }
    }

    fn open_checked(&mut self, candidate: &Candidate) -> Result<Box<dyn ContentHandle>, String> {
        if self.expired() {
            self.truncated();
            return Err("duplicate operation bound reached".into());
        }
        let handle = self
            .reader
            .open(&candidate.path)
            .map_err(|e| format!("content open failed: {e}"))?;
        let metadata = handle
            .metadata()
            .map_err(|e| format!("content metadata failed: {e}"))?;
        if metadata.file_id != candidate.file_id || metadata.size != candidate.size {
            return Err("identity or size changed before content read".into());
        }
        Ok(handle)
    }

    fn charge(&mut self, amount: usize) -> Result<(), String> {
        let amount = u64::try_from(amount).unwrap_or(u64::MAX);
        if self.started.elapsed() >= self.options.deadline {
            self.truncated();
            return Err("duplicate operation deadline reached".into());
        }
        let next = self.report.bytes_read.saturating_add(amount);
        if next > self.options.max_total_read_bytes {
            self.truncated();
            return Err("duplicate content-read budget reached".into());
        }
        self.report.bytes_read = next;
        Ok(())
    }

    fn ensure_read_budget(&mut self, amount: usize) -> Result<(), String> {
        if self.started.elapsed() >= self.options.deadline {
            self.truncated();
            return Err("duplicate operation deadline reached".into());
        }
        let amount = u64::try_from(amount).unwrap_or(u64::MAX);
        if amount
            > self
                .options
                .max_total_read_bytes
                .saturating_sub(self.report.bytes_read)
        {
            self.truncated();
            return Err("duplicate content-read budget reached".into());
        }
        Ok(())
    }

    fn sample(&mut self, candidate: &Candidate) -> Result<Vec<u8>, String> {
        let mut handle = self.open_checked(candidate)?;
        let length = usize::try_from(candidate.size.min(SAMPLE_BYTES as u64))
            .map_err(|_| "sample size does not fit platform usize".to_owned())?;
        let mut sample = vec![0u8; length];
        if length > 0 {
            self.ensure_read_budget(length)?;
            let count = handle
                .read_at(0, &mut sample)
                .map_err(|e| format!("sample read failed: {e}"))?;
            self.charge(count)?;
            if count != length {
                return Err("partial sample read".into());
            }
            if candidate.size > u64::try_from(length).unwrap_or(u64::MAX) {
                let tail_offset = candidate.size - u64::try_from(length).unwrap_or(u64::MAX);
                let mut tail = vec![0u8; length];
                self.ensure_read_budget(length)?;
                let count = handle
                    .read_at(tail_offset, &mut tail)
                    .map_err(|e| format!("sample read failed: {e}"))?;
                self.charge(count)?;
                if count != length {
                    return Err("partial sample read".into());
                }
                sample.extend(tail);
            }
        }
        let after = handle
            .metadata()
            .map_err(|e| format!("content metadata failed after sample: {e}"))?;
        if after.file_id != candidate.file_id || after.size != candidate.size {
            return Err("identity or size changed after sample read".into());
        }
        Ok(sample)
    }

    fn equal(&mut self, kept: &Candidate, candidate: &Candidate) -> Result<bool, String> {
        let mut left = self.open_checked(kept)?;
        let mut right = self.open_checked(candidate)?;
        let mut left_buf = vec![0u8; READ_CHUNK_BYTES];
        let mut right_buf = vec![0u8; READ_CHUNK_BYTES];
        loop {
            let pair_limit = self
                .options
                .max_total_read_bytes
                .saturating_sub(self.report.bytes_read)
                / 2;
            let request = READ_CHUNK_BYTES.min(usize::try_from(pair_limit).unwrap_or(usize::MAX));
            if request == 0 {
                self.truncated();
                return Err("duplicate content-read budget reached".into());
            }
            let left_count = left
                .read_next(&mut left_buf[..request])
                .map_err(|e| format!("content read failed: {e}"))?;
            let right_count = right
                .read_next(&mut right_buf[..request])
                .map_err(|e| format!("content read failed: {e}"))?;
            if left_count != right_count {
                return Ok(false);
            }
            self.charge(left_count.saturating_add(right_count))?;
            if left_count == 0 {
                break;
            }
            if left_buf[..left_count] != right_buf[..right_count] {
                return Ok(false);
            }
        }
        let left_after = left
            .metadata()
            .map_err(|e| format!("content metadata failed after read: {e}"))?;
        let right_after = right
            .metadata()
            .map_err(|e| format!("content metadata failed after read: {e}"))?;
        if left_after.file_id != kept.file_id
            || left_after.size != kept.size
            || right_after.file_id != candidate.file_id
            || right_after.size != candidate.size
        {
            return Err("identity or size changed after content read".into());
        }
        Ok(true)
    }
}

fn collect_candidates<R: ContentReader>(
    state: &mut OperationState<'_, R>,
    path: &Path,
    candidates: &mut Vec<Candidate>,
) {
    if state.expired() {
        state.truncated();
        return;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            state.report.skip(path, format!("metadata failed: {error}"));
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        state.report.skip(path, "symlink refused");
        return;
    }
    if metadata.is_file() {
        let native = platform::inspect(path, &metadata);
        let complete = native.volume_stable && native.file_id.is_some() && !native.is_placeholder;
        if !complete || native.allocation_size.is_none() {
            state.report.skip(
                path,
                if native.is_placeholder {
                    "placeholder refused"
                } else {
                    "unknown or incomplete native metadata"
                },
            );
            return;
        }
        let Some(file_id) = native.file_id else {
            state.report.skip(path, "file identity unavailable");
            return;
        };
        let size = metadata.len();
        if size < state.options.min_bytes {
            return;
        }
        if candidates.len() >= state.options.max_files {
            state.truncated();
            return;
        }
        // clone_id is currently unavailable from the native metadata adapter;
        // if a future adapter supplies it, this conservative path excludes it.
        let file_metadata = FileMetadata {
            kind: EntryKind::File,
            volume: native.volume,
            logical_size: Some(size),
            allocation_size: native.allocation_size,
            file_id: Some(file_id.clone()),
            clone_id: None,
            created_at: metadata
                .created()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs()),
            modified_at: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs()),
            is_placeholder: native.is_placeholder,
            metadata_complete: complete,
        };
        if file_metadata.clone_id.is_some() {
            state
                .report
                .skip(path, "clone identity excluded conservatively");
            return;
        }
        candidates.push(Candidate {
            path: path.to_path_buf(),
            file_id,
            size,
        });
        return;
    }
    if !metadata.is_dir() {
        state.report.skip(path, "non-regular entry refused");
        return;
    }
    let native = platform::inspect(path, &metadata);
    if native.is_placeholder {
        state.report.skip(path, "placeholder directory refused");
        return;
    }
    let remaining = state.options.max_files.saturating_sub(candidates.len());
    if remaining == 0 {
        state.truncated();
        return;
    }
    let (children, listed_truncated) = match platform::children_bounded(path, remaining) {
        Ok(value) => value,
        Err(error) => {
            state.report.skip(
                path,
                format!("directory listing refused: {}", error.message),
            );
            return;
        }
    };
    if listed_truncated {
        state.truncated();
    }
    for child in children {
        if candidates.len() >= state.options.max_files || state.expired() {
            state.truncated();
            break;
        }
        collect_candidates(state, &child, candidates);
    }
}

#[cfg(unix)]
pub struct UnixContentReader;

#[cfg(unix)]
impl ContentReader for UnixContentReader {
    fn open(&self, path: &Path) -> Result<Box<dyn ContentHandle>, String> {
        UnixContentHandle::open(path).map(|handle| Box::new(handle) as Box<dyn ContentHandle>)
    }
}

#[cfg(unix)]
struct UnixContentHandle {
    fd: libc::c_int,
    metadata: ContentMetadata,
    cursor: u64,
}

#[cfg(unix)]
impl UnixContentHandle {
    fn open(path: &Path) -> Result<Self, String> {
        use std::os::unix::ffi::OsStrExt;
        let before = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !before.is_file() || before.file_type().is_symlink() {
            return Err("content handle requires a regular non-link file".into());
        }
        let native = platform::inspect(path, &before);
        let file_id = native
            .file_id
            .ok_or_else(|| "file identity unavailable".to_owned())?;
        if !native.volume_stable || native.is_placeholder {
            return Err("native adapter cannot guarantee local content".into());
        }
        let fd = open_pinned(path)?;
        let stat = fstat(fd)?;
        let size = u64::try_from(stat.st_size).map_err(|_| "negative file size".to_owned())?;
        let actual_id = FileIdentity {
            volume: file_id.volume.clone(),
            id: format!("{}:{}", stat.st_dev, stat.st_ino),
        };
        if actual_id != file_id || size != before.len() {
            unsafe { libc::close(fd) };
            return Err("identity or size changed while opening content".into());
        }
        let _ = path.as_os_str().as_bytes();
        Ok(Self {
            fd,
            metadata: ContentMetadata { file_id, size },
            cursor: 0,
        })
    }
}

#[cfg(unix)]
impl ContentHandle for UnixContentHandle {
    fn metadata(&self) -> Result<ContentMetadata, String> {
        let stat = fstat(self.fd)?;
        let size = u64::try_from(stat.st_size).map_err(|_| "negative file size".to_owned())?;
        let id = format!("{}:{}", stat.st_dev, stat.st_ino);
        let mut metadata = self.metadata.clone();
        metadata.file_id.id = id;
        metadata.size = size;
        Ok(metadata)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<usize, String> {
        let offset = i64::try_from(offset).map_err(|_| "read offset overflow".to_owned())?;
        let count = unsafe {
            libc::pread(
                self.fd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                offset as libc::off_t,
            )
        };
        if count < 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            usize::try_from(count).map_err(|_| "read count overflow".to_owned())
        }
    }

    fn read_next(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let count = self.read_at(self.cursor, buffer)?;
        self.cursor = self
            .cursor
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        Ok(count)
    }
}

#[cfg(unix)]
impl Drop for UnixContentHandle {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

#[cfg(unix)]
fn fstat(fd: libc::c_int) -> Result<libc::stat, String> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(unix)]
fn open_pinned(path: &Path) -> Result<libc::c_int, String> {
    use std::os::unix::ffi::OsStrExt;
    let mut components = path.components();
    let absolute = path.is_absolute();
    let mut dir_fd = if absolute {
        open_component(libc::AT_FDCWD, b"/", true)?
    } else {
        open_component(libc::AT_FDCWD, b".", true)?
    };
    let mut names: Vec<Vec<u8>> = Vec::new();
    for component in components.by_ref() {
        use std::path::Component;
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => names.push(b"..".to_vec()),
            Component::Normal(name) => names.push(name.as_bytes().to_vec()),
            Component::Prefix(_) => return Err("path prefix unsupported by Unix adapter".into()),
        }
    }
    let final_name = names
        .pop()
        .ok_or_else(|| "content path has no file component".to_owned())?;
    for name in names {
        let next = open_component(dir_fd, &name, true);
        unsafe { libc::close(dir_fd) };
        dir_fd = next?;
    }
    let fd = open_component(dir_fd, &final_name, false);
    unsafe { libc::close(dir_fd) };
    fd
}

#[cfg(unix)]
fn open_component(
    parent: libc::c_int,
    bytes: &[u8],
    directory: bool,
) -> Result<libc::c_int, String> {
    let name = std::ffi::CString::new(bytes).map_err(|_| "path contains NUL".to_owned())?;
    let mut flags = libc::O_CLOEXEC | libc::O_NOFOLLOW;
    flags |= if directory {
        libc::O_RDONLY | libc::O_DIRECTORY
    } else {
        libc::O_RDONLY
    };
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if fd < 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(fd)
    }
}

/// Windows content adapter. Opens with `FILE_FLAG_OPEN_REPARSE_POINT` and
/// `FILE_FLAG_OPEN_NO_RECALL`, then refuses (from the opened handle's own
/// attributes) reparse points, offline and recall-on-access items, so a link
/// is never followed and a cloud placeholder is never hydrated. Identity is
/// the NTFS `FILE_ID_INFO` read from the handle, formatted exactly as
/// `platform::inspect` formats it, so hard links collapse before any read.
#[cfg(windows)]
pub struct WindowsContentReader;

#[cfg(windows)]
impl ContentReader for WindowsContentReader {
    fn open(&self, path: &Path) -> Result<Box<dyn ContentHandle>, String> {
        WindowsContentHandle::open(path).map(|handle| Box::new(handle) as Box<dyn ContentHandle>)
    }
}

#[cfg(windows)]
struct WindowsContentHandle {
    file: fs::File,
    cursor: u64,
}

#[cfg(windows)]
impl WindowsContentHandle {
    fn open(path: &Path) -> Result<Self, String> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let before = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !before.is_file() || before.file_type().is_symlink() {
            return Err("content handle requires a regular non-link file".into());
        }
        let native = platform::inspect(path, &before);
        if native.file_id.is_none() || !native.volume_stable || native.is_placeholder {
            return Err("native adapter cannot guarantee local content".into());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_OPEN_NO_RECALL.0)
            .open(path)
            .map_err(|e| e.to_string())?;
        let handle = Self { file, cursor: 0 };
        let (attributes, _) = handle.handle_facts()?;
        const REFUSED: u32 = 0x0000_0400 | 0x0000_1000 | 0x0004_0000 | 0x0040_0000;
        if attributes & (REFUSED | 0x0000_0010) != 0 {
            return Err("placeholder, reparse point or directory refused".into());
        }
        let opened = handle.metadata()?;
        if Some(&opened.file_id) != native.file_id.as_ref() || opened.size != before.len() {
            return Err("identity or size changed while opening content".into());
        }
        Ok(handle)
    }

    /// Attributes and the volume-qualified id of the opened object.
    fn handle_facts(&self) -> Result<(u32, FileIdentity), String> {
        use std::ffi::c_void;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
            GetFileInformationByHandleEx,
        };
        let handle = HANDLE(self.file.as_raw_handle());
        let mut basic = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle, &mut basic) }.map_err(|e| e.to_string())?;
        let mut id_info = FILE_ID_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&mut id_info as *mut FILE_ID_INFO).cast::<c_void>(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        }
        .map_err(|e| e.to_string())?;
        let hex: String = id_info
            .FileId
            .Identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok((
            basic.dwFileAttributes,
            FileIdentity {
                volume: crate::model::VolumeIdentity::new(format!(
                    "serial:{:016x}",
                    id_info.VolumeSerialNumber
                )),
                id: hex,
            },
        ))
    }
}

#[cfg(windows)]
impl ContentHandle for WindowsContentHandle {
    fn metadata(&self) -> Result<ContentMetadata, String> {
        let (_, file_id) = self.handle_facts()?;
        let size = self.file.metadata().map_err(|e| e.to_string())?.len();
        Ok(ContentMetadata { file_id, size })
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<usize, String> {
        use std::os::windows::fs::FileExt;
        self.file
            .seek_read(buffer, offset)
            .map_err(|e| e.to_string())
    }

    fn read_next(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let count = self.read_at(self.cursor, buffer)?;
        self.cursor = self
            .cursor
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        Ok(count)
    }
}
