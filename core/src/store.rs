//! Opt-in local metadata history. Target files are never changed by a scan.
//!
//! Every operation on the state directory goes through one pinned descriptor:
//! on unix an `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` fd that is `fstat`-
//! verified (directory, owned by the effective user, not writable by group or
//! others) and then used as the `*at` base for enumeration (`fdopendir`),
//! checks (`fstatat`/`AT_SYMLINK_NOFOLLOW`), reads (`openat`/`O_NOFOLLOW`),
//! and publication (`linkat` + `unlinkat`, then `fsync` of the dir fd).
//! A renamed or swapped parent therefore cannot redirect reads or writes.
//! On Windows the directory is pinned by a `FILE_FLAG_BACKUP_SEMANTICS |
//! FILE_FLAG_OPEN_REPARSE_POINT` handle; enumeration runs on that handle via
//! `FileIdBothDirectoryInfo`, while child opens, publication, and deletion use
//! native handle-relative NT operations rooted at the pinned handle.
use crate::{ScanReport, rules::Finding};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// Current (and only writable) snapshot schema version.
pub const SCHEMA_VERSION: u32 = 1;
/// Largest snapshot file that will be written or read.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Largest number of snapshot candidates `history` will examine.
pub const MAX_SNAPSHOTS: usize = 1000;
/// Largest number of directory entries retained during one history scan.
pub const MAX_ENUMERATED_ENTRIES: usize = MAX_SNAPSHOTS * 2;
/// Longest accepted snapshot ID.
pub const MAX_ID_LEN: usize = 64;
const ID_PREFIX: &str = "scan-";

/// A snapshot file that `history_report` declined to load, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedSnapshot {
    pub file: String,
    pub reason: String,
}

/// Loaded snapshots (ordered by `created_at`, then `id`) plus explicit skips
/// (ordered by file name) and platform capability notes.
#[derive(Clone, Debug, Default)]
pub struct HistoryReport {
    pub snapshots: Vec<Snapshot>,
    pub skipped: Vec<SkippedSnapshot>,
    /// Platform limits that affect the privacy of the store (for example
    /// Windows ACL inheritance). Empty when nothing needs to be disclosed.
    pub capability_notes: Vec<String>,
}

/// Total bytes `history_report` will read across all snapshot files.
pub const MAX_AGGREGATE_READ_BYTES: u64 = 256 * 1024 * 1024;
/// Skip reason for files not read because the aggregate budget ran out.
pub const REASON_AGGREGATE_BUDGET: &str = "aggregate read budget";
/// Skip reason for leftover temp files of an unfinished `save`.
pub const REASON_INTERRUPTED: &str = "interrupted publication";
/// Skip reason for files whose names resolve to the same snapshot ID.
pub const REASON_DUPLICATE_ID: &str = "duplicate id";

/// Capability notes reported on Windows.
#[cfg(windows)]
const PLATFORM_NOTES: &[&str] = &[
    "windows: new directories and files inherit the ACL of the parent \
     (expected: a per-user profile directory); no explicit private ACL is applied \
     and existing ACLs are never rewritten",
    "windows: state directory and child operations use a pinned handle; new \
     files inherit the parent ACL and existing ACLs are never rewritten",
];
#[cfg(not(windows))]
const PLATFORM_NOTES: &[&str] = &[];

/// An ID is never a path: `scan-` prefix, then ASCII alphanumerics, `-` or `_`,
/// at most `MAX_ID_LEN` bytes in total.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.len() > MAX_ID_LEN {
        return Err(format!("snapshot id longer than {MAX_ID_LEN} bytes"));
    }
    let rest = id
        .strip_prefix(ID_PREFIX)
        .ok_or_else(|| format!("snapshot id lacks `{ID_PREFIX}` prefix"))?;
    if rest.is_empty()
        || !rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("snapshot id has characters outside [A-Za-z0-9_-]".into());
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(unix)]
fn denied(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub id: String,
    pub created_at: u64,
    pub report: ScanReport,
    pub findings: Vec<Finding>,
}
impl Snapshot {
    pub fn new(report: ScanReport, findings: Vec<Finding>) -> Self {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            schema_version: SCHEMA_VERSION,
            id: format!("scan-{}", time.as_nanos()),
            created_at: time.as_secs(),
            report,
            findings,
        }
    }
}

pub fn default_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("Pulse"));
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/Pulse"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .map(|p| p.join("pulse"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".local/state/pulse"))
        });
    let root = root.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "local metadata directory unavailable",
        )
    })?;
    let legacy = crate::state_migration::legacy_metadata_dir(&root);
    crate::state_migration::migrate_directory(&legacy, &root)?;
    Ok(root)
}

fn is_link(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// Pre-open refusal of any existing ancestor (or the state path itself) that
/// is a symlink or reparse point, and of a state path that exists but is not
/// a directory. This is a best-effort early check: the pinned descriptor —
/// not this path scan — is what actually prevents a swapped ancestor from
/// redirecting later operations.
fn reject_links(path: &Path) -> io::Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if is_link(&meta) => {
                return Err(invalid_input(
                    "metadata directory contains symlink or reparse point",
                ));
            }
            Ok(meta) => {
                if ancestor == path && !meta.is_dir() {
                    return Err(invalid_input("metadata path is not a directory"));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_directory(directory: &Path) -> io::Result<()> {
    pinned::PinnedDir::ensure_path(directory)
}

#[cfg(windows)]
fn create_directory(directory: &Path) -> io::Result<()> {
    pinned::PinnedDir::ensure_path(directory)
}

#[cfg(not(any(unix, windows)))]
fn create_directory(directory: &Path) -> io::Result<()> {
    fs::create_dir(directory)
}

// ---------------------------------------------------------------------------
// Pinned state directory: unix.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod pinned {
    use super::{denied, invalid_input};
    use std::{
        ffi::{CStr, CString},
        fs, io,
        os::unix::ffi::OsStrExt,
        os::unix::fs::MetadataExt,
        os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd},
        path::Path,
    };

    /// A verified fd for the state directory. All entry operations are
    /// performed relative to this descriptor, so swapping or renaming the
    /// directory at its path cannot redirect reads or writes.
    pub struct PinnedDir(fs::File);

    fn c_name(name: &str) -> io::Result<CString> {
        // Snapshot names come from validated IDs; interior NUL is impossible
        // but never trusted blindly.
        CString::new(name).map_err(|_| invalid_input("entry name contains NUL"))
    }

    /// Clear `errno` so a null `readdir` result means end-of-directory.
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    unsafe fn reset_errno() {
        unsafe { *libc::__error() = 0 };
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    unsafe fn reset_errno() {
        unsafe { *libc::__errno_location() = 0 };
    }

    impl PinnedDir {
        fn fd(&self) -> RawFd {
            self.0.as_raw_fd()
        }

        fn open_dir_at(base: RawFd, name: &CStr) -> io::Result<fs::File> {
            let fd = unsafe {
                libc::openat(
                    base,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ELOOP) {
                    return Err(invalid_input(
                        "metadata directory contains symlink or reparse point",
                    ));
                }
                return Err(error);
            }
            Ok(unsafe { fs::File::from_raw_fd(fd) })
        }

        /// Open every path component relative to the descriptor acquired for its
        /// predecessor. This prevents a concurrent ancestor rename/symlink swap
        /// from redirecting the final descriptor; final identity is compared with
        /// an lstat taken before the walk.
        pub fn pin(path: &Path) -> io::Result<Self> {
            let before = fs::symlink_metadata(path)?;
            if before.file_type().is_symlink() {
                return Err(invalid_input(
                    "metadata directory contains symlink or reparse point",
                ));
            }
            if !before.is_dir() {
                return Err(invalid_input("metadata path is not a directory"));
            }
            let dot = CString::new(".").unwrap();
            let root = CString::new("/").unwrap();
            let mut file = Self::open_dir_at(
                libc::AT_FDCWD,
                if path.is_absolute() { &root } else { &dot },
            )?;
            for component in path.components() {
                let name = match component {
                    std::path::Component::RootDir | std::path::Component::CurDir => continue,
                    std::path::Component::ParentDir => CString::new("..").unwrap(),
                    std::path::Component::Normal(name) => CString::new(name.as_bytes())
                        .map_err(|_| invalid_input("metadata path contains NUL"))?,
                    std::path::Component::Prefix(_) => {
                        return Err(invalid_input("path prefix unsupported on Unix"));
                    }
                };
                file = Self::open_dir_at(file.as_raw_fd(), &name)?;
            }
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if st.st_dev as u64 != before.dev() || st.st_ino != before.ino() {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "metadata directory identity changed during pin",
                ));
            }
            if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(invalid_input("metadata path is not a directory"));
            }
            if st.st_uid != unsafe { libc::geteuid() } {
                return Err(denied(
                    "metadata directory is not owned by the current user; \
                     ownership is never changed by Pulse",
                ));
            }
            if st.st_mode & 0o022 != 0 {
                return Err(denied(
                    "metadata directory is writable by group or others; \
                     permissions are never changed by Pulse",
                ));
            }
            Ok(Self(file))
        }

        /// Create missing components through descriptor-relative `mkdirat`.
        /// Existing directories are opened and preserved; no pathname
        /// recursive creation is used.
        pub fn ensure_path(path: &Path) -> io::Result<()> {
            let dot = CString::new(".").unwrap();
            let root = CString::new("/").unwrap();
            let mut current = Self::open_dir_at(
                libc::AT_FDCWD,
                if path.is_absolute() { &root } else { &dot },
            )?;
            for component in path.components() {
                let name = match component {
                    std::path::Component::RootDir | std::path::Component::CurDir => continue,
                    std::path::Component::Normal(name) => CString::new(name.as_bytes())
                        .map_err(|_| invalid_input("metadata path contains NUL"))?,
                    std::path::Component::ParentDir => {
                        return Err(invalid_input("parent path component unsupported"));
                    }
                    std::path::Component::Prefix(_) => {
                        return Err(invalid_input("path prefix unsupported on Unix"));
                    }
                };
                match Self::open_dir_at(current.as_raw_fd(), &name) {
                    Ok(next) => current = next,
                    Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                        let made =
                            unsafe { libc::mkdirat(current.as_raw_fd(), name.as_ptr(), 0o700) };
                        if made != 0 {
                            let mkdir_error = io::Error::last_os_error();
                            if mkdir_error.raw_os_error() != Some(libc::EEXIST) {
                                return Err(mkdir_error);
                            }
                        }
                        current = Self::open_dir_at(current.as_raw_fd(), &name)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }

        /// Create/open one child relative to this already-pinned directory.
        /// No pathname lookup is involved, so renaming the parent path cannot
        /// redirect this child handle.
        pub fn ensure_child(&self, name: &str) -> io::Result<Self> {
            let c = c_name(name)?;
            let file = match Self::open_dir_at(self.fd(), &c) {
                Ok(file) => file,
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                    let made = unsafe { libc::mkdirat(self.fd(), c.as_ptr(), 0o700) };
                    if made != 0 {
                        let mkdir_error = io::Error::last_os_error();
                        if mkdir_error.raw_os_error() != Some(libc::EEXIST) {
                            return Err(mkdir_error);
                        }
                    }
                    Self::open_dir_at(self.fd(), &c)?
                }
                Err(error) => return Err(error),
            };
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(file.as_raw_fd(), &mut st) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(invalid_input("metadata child is not a directory"));
            }
            if st.st_uid != unsafe { libc::geteuid() } {
                return Err(denied("metadata child is not owned by the current user"));
            }
            if st.st_mode & 0o022 != 0 {
                return Err(denied("metadata child is writable by group or others"));
            }
            Ok(Self(file))
        }

        /// `fstatat` relative to the pinned fd, never following a symlinked
        /// final component. `Ok(None)` covers absent names (and a
        /// non-directory intermediate, which cannot occur for leaf names but
        /// is treated as absent defensively).
        pub fn stat(&self, name: &str) -> io::Result<Option<libc::stat>> {
            let c = c_name(name)?;
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstatat(self.fd(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) }
                == 0
            {
                return Ok(Some(st));
            }
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::ENOENT) | Some(libc::ENOTDIR) => Ok(None),
                _ => Err(e),
            }
        }

        /// Open `name` for reading relative to the pinned fd; `O_NOFOLLOW`
        /// refuses symlinks and the resulting handle is `fstat`-checked for a
        /// regular file.
        pub fn open_read(&self, name: &str) -> io::Result<fs::File> {
            let c = c_name(name)?;
            let fd = unsafe {
                libc::openat(
                    self.fd(),
                    c.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { fs::File::from_raw_fd(fd) })
        }

        /// Exclusively create `name` (mode 0o600 — only files created by this
        /// call are ever given a mode) relative to the pinned fd.
        pub fn create_temp(&self, name: &str) -> io::Result<fs::File> {
            let c = c_name(name)?;
            let fd = unsafe {
                libc::openat(
                    self.fd(),
                    c.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(unsafe { fs::File::from_raw_fd(fd) })
        }

        /// Atomic no-replace publication inside the pinned directory:
        /// `linkat` fails with `EEXIST` if `to` exists. `renameat` is
        /// deliberately not used: plain rename replaces an existing
        /// destination, and no-replace rename (`renameat2`/`RENAME_NOREPLACE`)
        /// is not portable to macOS.
        pub fn publish(&self, from: &str, to: &str) -> io::Result<()> {
            let cf = c_name(from)?;
            let ct = c_name(to)?;
            if unsafe { libc::linkat(self.fd(), cf.as_ptr(), self.fd(), ct.as_ptr(), 0) } != 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EEXIST) {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, e.to_string()));
                }
                return Err(io::Error::new(
                    e.kind(),
                    format!("atomic no-replace publication unavailable (hard link failed): {e}"),
                ));
            }
            let _ = self.unlink(from);
            // Make the new directory entry durable before reporting success.
            self.0.sync_all()?;
            Ok(())
        }

        pub fn unlink(&self, name: &str) -> io::Result<()> {
            let c = c_name(name)?;
            if unsafe { libc::unlinkat(self.fd(), c.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Enumerate names inside the pinned fd via `fdopendir` on a
        /// duplicated descriptor, so a swapped path can never change which
        /// directory is listed. `.`/`..` are skipped. Names that are not
        /// valid UTF-8 are lossy-decoded; they can never match snapshot or
        /// temp patterns, which are ASCII.
        pub fn names(&self, limit: usize) -> io::Result<Vec<String>> {
            let dup = self.0.try_clone()?.into_raw_fd();
            let raw = unsafe { libc::fdopendir(dup) };
            if raw.is_null() {
                let e = io::Error::last_os_error();
                unsafe { libc::close(dup) };
                return Err(e);
            }
            struct Dir(*mut libc::DIR);
            impl Drop for Dir {
                fn drop(&mut self) {
                    unsafe { libc::closedir(self.0) };
                }
            }
            let dirp = Dir(raw);
            let mut out = Vec::new();
            loop {
                unsafe { reset_errno() };
                let entry = unsafe { libc::readdir(dirp.0) };
                if entry.is_null() {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() == Some(0) {
                        return Ok(out);
                    }
                    return Err(e);
                }
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                let name = name.to_string_lossy().into_owned();
                if name != "." && name != ".." {
                    if out.len() >= limit {
                        return Err(invalid_input("metadata directory entry cap exceeded"));
                    }
                    out.push(name);
                }
            }
        }

        /// `dirent` free helper: is `name` a regular file per
        /// `fstatat(AT_SYMLINK_NOFOLLOW)`? Errors and non-files are `false`
        /// for classification purposes; the caller reports the reason.
        pub fn is_regular_file(&self, name: &str) -> io::Result<bool> {
            match self.stat(name)? {
                Some(st) => Ok(st.st_mode & libc::S_IFMT == libc::S_IFREG),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "entry vanished during enumeration",
                )),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pinned state directory: Windows.
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod pinned {
    use super::invalid_input;
    use std::{
        ffi::OsStr,
        fs, io,
        iter::once,
        os::windows::ffi::OsStrExt,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
        path::{Path, PathBuf},
    };
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND,
        ERROR_GEN_FAILURE, ERROR_MR_MID_NOT_FOUND, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND,
        GetLastError, HANDLE, NTSTATUS, RtlNtStatusToDosError, UNICODE_STRING,
    };
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_BOTH_DIR_INFO, FILE_INFO_BY_HANDLE_CLASS,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdBothDirectoryInfo,
        FlushFileBuffers, GetFileInformationByHandle, GetFileInformationByHandleEx, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::IO_STATUS_BLOCK;
    use windows::core::PWSTR;

    const NOT_FOUND: u32 = ERROR_FILE_NOT_FOUND.0;
    const PATH_NOT_FOUND: u32 = ERROR_PATH_NOT_FOUND.0;
    const NO_MORE_FILES: u32 = ERROR_NO_MORE_FILES.0;
    const ALREADY_EXISTS: u32 = ERROR_ALREADY_EXISTS.0;
    const FILE_EXISTS: u32 = ERROR_FILE_EXISTS.0;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_OPEN_NO_RECALL: u32 = 0x0040_0000;
    const FILE_OPEN: u32 = 1;
    const FILE_CREATE: u32 = 2;
    const FILE_OPEN_IF: u32 = 3;
    const FILE_DISPOSITION_INFORMATION_CLASS: i32 = 13;
    const FILE_LINK_INFORMATION_CLASS: i32 = 11;
    const STATUS_INVALID_PARAMETER: NTSTATUS = NTSTATUS(0xC000_000Du32 as i32);
    const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
    const OBJ_DONT_REPARSE: u32 = 0x0000_1000;
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    const FILE_WRITE_ATTRIBUTES_ACCESS: u32 = 0x0000_0100;
    const DELETE_ACCESS: u32 = 0x0001_0000;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    #[repr(C)]
    struct NtObjectAttributes {
        length: u32,
        root_directory: HANDLE,
        object_name: *const UNICODE_STRING,
        attributes: u32,
        security_descriptor: *const core::ffi::c_void,
        security_quality_of_service: *const core::ffi::c_void,
    }

    #[repr(C)]
    struct NtFileDispositionInformation {
        delete_file: u8,
    }

    #[repr(C)]
    struct NtFileLinkInformation {
        replace_if_exists: u8,
        _padding: [u8; 3],
        root_directory: HANDLE,
        file_name_length: u32,
        file_name: [u16; 1],
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            file_handle: *mut HANDLE,
            desired_access: u32,
            object_attributes: *const NtObjectAttributes,
            io_status_block: *mut IO_STATUS_BLOCK,
            allocation_size: *const i64,
            file_attributes: u32,
            share_access: u32,
            create_disposition: u32,
            create_options: u32,
            ea_buffer: *const core::ffi::c_void,
            ea_length: u32,
        ) -> NTSTATUS;
        fn NtSetInformationFile(
            file_handle: HANDLE,
            io_status_block: *mut IO_STATUS_BLOCK,
            file_information: *const core::ffi::c_void,
            length: u32,
            file_information_class: i32,
        ) -> NTSTATUS;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(once(0)).collect()
    }

    fn last_err() -> io::Error {
        io::Error::last_os_error()
    }

    struct Handle(HANDLE);
    impl Handle {
        fn raw(&self) -> HANDLE {
            self.0
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    impl From<Handle> for fs::File {
        fn from(h: Handle) -> Self {
            // SAFETY: `Handle` owned the HANDLE; moving it into a File moves
            // the close responsibility.
            let raw = h.raw();
            std::mem::forget(h);
            unsafe { fs::File::from(OwnedHandle::from_raw_handle(raw.0 as RawHandle)) }
        }
    }

    fn info(handle: HANDLE) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut i = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(handle, &mut i) }.map_err(|_| last_err())?;
        Ok(i)
    }

    fn reject_attrs(i: &BY_HANDLE_FILE_INFORMATION) -> io::Result<()> {
        let attrs = i.dwFileAttributes;
        if attrs
            & (FILE_ATTRIBUTE_REPARSE_POINT.0
                | FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0
        {
            return Err(invalid_input(
                "metadata entry is a reparse point or recall/offline placeholder",
            ));
        }
        Ok(())
    }

    fn reject_info(i: &BY_HANDLE_FILE_INFORMATION, directory: bool) -> io::Result<()> {
        reject_attrs(i)?;
        if directory != (i.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) {
            return Err(invalid_input(if directory {
                "metadata path is not a directory"
            } else {
                "metadata entry is a directory"
            }));
        }
        Ok(())
    }

    fn nt_error(status: NTSTATUS) -> io::Error {
        let mut code = unsafe { RtlNtStatusToDosError(status) };
        if code == ERROR_MR_MID_NOT_FOUND.0 {
            code = ERROR_GEN_FAILURE.0;
        }
        io::Error::from_raw_os_error(code as i32)
    }

    fn relative_name_os(name: &OsStr) -> io::Result<Vec<u16>> {
        let utf16: Vec<u16> = name.encode_wide().collect();
        if utf16.is_empty()
            || utf16 == [b'.' as u16]
            || utf16 == [b'.' as u16, b'.' as u16]
            || utf16
                .iter()
                .any(|c| *c == 0 || *c == b'/' as u16 || *c == b'\\' as u16 || *c == b':' as u16)
        {
            return Err(invalid_input(
                "metadata entry name is not a single path component",
            ));
        }
        let bytes = utf16
            .len()
            .checked_mul(2)
            .ok_or_else(|| invalid_input("metadata entry name is too long"))?;
        if bytes > u16::MAX as usize {
            return Err(invalid_input("metadata entry name is too long"));
        }
        Ok(utf16)
    }

    fn relative_name(name: &str) -> io::Result<Vec<u16>> {
        relative_name_os(OsStr::new(name))
    }

    fn nt_open_relative(
        root: HANDLE,
        name: &str,
        access: u32,
        disposition: u32,
        options: u32,
    ) -> io::Result<Handle> {
        let mut utf16 = relative_name(name)?;
        nt_open_relative_wide(root, &mut utf16, access, disposition, options)
    }

    fn nt_open_relative_os(
        root: HANDLE,
        name: &OsStr,
        access: u32,
        disposition: u32,
        options: u32,
    ) -> io::Result<Handle> {
        let mut utf16 = relative_name_os(name)?;
        nt_open_relative_wide(root, &mut utf16, access, disposition, options)
    }

    fn nt_open_relative_wide(
        root: HANDLE,
        utf16: &mut [u16],
        access: u32,
        disposition: u32,
        options: u32,
    ) -> io::Result<Handle> {
        let unicode = UNICODE_STRING {
            Length: (utf16.len() * 2) as u16,
            MaximumLength: (utf16.len() * 2) as u16,
            Buffer: PWSTR(utf16.as_mut_ptr()),
        };
        // Every open is a single relative component with FILE_OPEN_REPARSE_POINT
        // and a post-open attribute check, so reparse points are refused even
        // where the kernel rejects OBJ_DONT_REPARSE (STATUS_INVALID_PARAMETER);
        // that one flag is then dropped and the open retried once.
        let mut last = NTSTATUS(0);
        for attribute_flags in [
            OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
            OBJ_CASE_INSENSITIVE,
        ] {
            let attributes = NtObjectAttributes {
                length: std::mem::size_of::<NtObjectAttributes>() as u32,
                root_directory: root,
                object_name: &unicode,
                attributes: attribute_flags,
                security_descriptor: std::ptr::null(),
                security_quality_of_service: std::ptr::null(),
            };
            let mut opened = HANDLE(std::ptr::null_mut());
            let mut io_status = IO_STATUS_BLOCK::default();
            let status = unsafe {
                NtCreateFile(
                    &mut opened,
                    access | SYNCHRONIZE_ACCESS,
                    &attributes,
                    &mut io_status,
                    std::ptr::null(),
                    FILE_ATTRIBUTE_NORMAL,
                    (FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0,
                    disposition,
                    options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_NO_RECALL,
                    std::ptr::null(),
                    0,
                )
            };
            if status.0 >= 0 {
                return Ok(Handle(opened));
            }
            last = status;
            if status != STATUS_INVALID_PARAMETER {
                break;
            }
        }
        let error = nt_error(last);
        if last == STATUS_INVALID_PARAMETER {
            // Diagnosis only: find which single flag the kernel rejects. Each
            // probe's handle is closed at once and never used.
            let probe = |access: u32, attrs: u32, options: u32| -> i32 {
                let attributes = NtObjectAttributes {
                    length: std::mem::size_of::<NtObjectAttributes>() as u32,
                    root_directory: root,
                    object_name: &unicode,
                    attributes: attrs,
                    security_descriptor: std::ptr::null(),
                    security_quality_of_service: std::ptr::null(),
                };
                let mut opened = HANDLE(std::ptr::null_mut());
                let mut io_status = IO_STATUS_BLOCK::default();
                let status = unsafe {
                    NtCreateFile(
                        &mut opened,
                        access,
                        &attributes,
                        &mut io_status,
                        std::ptr::null(),
                        0,
                        (FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0,
                        disposition,
                        options,
                        std::ptr::null(),
                        0,
                    )
                };
                if status.0 >= 0 {
                    drop(Handle(opened));
                }
                status.0
            };
            let full = options | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_NO_RECALL;
            let sync = access | SYNCHRONIZE_ACCESS;
            let probes = [
                ("attr 0", probe(sync, OBJ_CASE_INSENSITIVE, full)),
                ("no NO_RECALL", probe(sync, OBJ_CASE_INSENSITIVE, full & !FILE_OPEN_NO_RECALL)),
                ("no OPEN_REPARSE_POINT", probe(sync, OBJ_CASE_INSENSITIVE, full & !FILE_OPEN_REPARSE_POINT)),
                ("no SYNCHRONOUS", probe(access, OBJ_CASE_INSENSITIVE, full & !FILE_SYNCHRONOUS_IO_NONALERT)),
                ("no OBJ flags", probe(sync, 0, full)),
                ("bare", probe(sync, 0, options & FILE_DIRECTORY_FILE)),
            ];
            let report: Vec<String> =
                probes.iter().map(|(name, st)| format!("{name}={:#010x}", *st as u32)).collect();
            // Keep the failing request visible; every other status keeps its
            // raw OS code for callers that match it.
            return Err(invalid_input(format!(
                "NtCreateFile rejected the request (status {:#010x}, access {access:#x}, \
                 disposition {disposition}, options {options:#x}, name_len {}; probes {}): {error}",
                last.0 as u32,
                unicode.Length,
                report.join(" ")
            )));
        }
        Err(error)
    }

    fn open_drive_root(letter: u8) -> io::Result<Handle> {
        let root = PathBuf::from(format!("{}:\\", letter as char));
        let w = wide(&root);
        let h = unsafe {
            CreateFileW(
                windows::core::PCWSTR(w.as_ptr()),
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|_| last_err())?;
        Ok(Handle(h))
    }

    /// A verified handle for the state directory. Every child operation uses
    /// `NtCreateFile` with this handle as `OBJECT_ATTRIBUTES.RootDirectory`;
    /// enumeration likewise runs on this held handle.
    pub struct PinnedDir {
        handle: Handle,
    }

    // NT directory handles are kernel-owned capabilities; the relative
    // operations below are thread-safe, and Handle owns the single close.
    unsafe impl Send for PinnedDir {}
    unsafe impl Sync for PinnedDir {}

    /// Verified child handle: an open file object whose on-disk name was
    /// opened relative to the pinned directory before any byte is read or written.
    pub struct VerifiedFile {
        pub file: fs::File,
    }

    impl PinnedDir {
        /// Open drive root once, then walk each normal component with
        /// `NtCreateFile(RootDirectory=held-parent)`. Disk & verbatim-disk
        /// roots are accepted; relative, UNC, device & parent paths refused.
        pub fn pin(path: &Path) -> io::Result<Self> {
            let mut components = path.components().peekable();
            let prefix = match components.next() {
                Some(std::path::Component::Prefix(prefix)) => prefix.kind(),
                _ => return Err(invalid_input("Windows metadata path must be absolute")),
            };
            let letter = match prefix {
                std::path::Prefix::Disk(letter) | std::path::Prefix::VerbatimDisk(letter) => letter,
                _ => return Err(invalid_input("UNC, device, or unsupported Windows prefix")),
            };
            if !matches!(components.next(), Some(std::path::Component::RootDir)) {
                return Err(invalid_input("Windows metadata path must include a root"));
            }
            let mut handle = open_drive_root(letter)?;
            reject_info(&info(handle.raw())?, true)?;
            while let Some(component) = components.next() {
                let name = match component {
                    std::path::Component::Normal(name) => name,
                    _ => {
                        return Err(invalid_input(
                            "parent or current path component unsupported",
                        ));
                    }
                };
                let final_component = components.peek().is_none();
                let access = if final_component {
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0
                } else {
                    FILE_GENERIC_READ.0
                };
                let next = nt_open_relative_os(
                    handle.raw(),
                    name,
                    access,
                    FILE_OPEN,
                    FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
                )?;
                reject_info(&info(next.raw())?, true)?;
                handle = next;
            }
            Ok(Self { handle })
        }

        /// Create only final missing component through a verified parent
        /// handle. Missing ancestors are intentionally refused.
        pub fn ensure_path(path: &Path) -> io::Result<()> {
            if fs::symlink_metadata(path).is_ok() {
                Self::pin(path)?;
                return Ok(());
            }
            let parent = path
                .parent()
                .ok_or_else(|| invalid_input("metadata path has no parent"))?;
            let name = path
                .file_name()
                .ok_or_else(|| invalid_input("metadata path has no final component"))?;
            let parent = Self::pin(parent)?;
            let child = nt_open_relative_os(
                parent.handle.raw(),
                name,
                FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                FILE_OPEN_IF,
                FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            )?;
            reject_info(&info(child.raw())?, true)
        }

        /// Create/open one child relative to this already-pinned directory.
        /// Reparse points remain refused by the opened handle's attributes.
        pub fn ensure_child(&self, name: &str) -> io::Result<Self> {
            let child = nt_open_relative(
                self.handle.raw(),
                name,
                FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                FILE_OPEN_IF,
                FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            )?;
            reject_info(&info(child.raw())?, true)?;
            Ok(Self { handle: child })
        }

        fn open_verified(
            &self,
            name: &str,
            access: u32,
            disposition: u32,
            options: u32,
        ) -> io::Result<VerifiedFile> {
            let handle = nt_open_relative(self.handle.raw(), name, access, disposition, options)?;
            let i = info(handle.raw())?;
            reject_info(&i, false)?;
            let file: fs::File = handle.into();
            Ok(VerifiedFile { file })
        }

        /// Does `name` exist in the pinned directory (by verified open)?
        /// `Ok(None)` only when the name is absent.
        pub fn stat(&self, name: &str) -> io::Result<Option<VerifiedFile>> {
            match nt_open_relative(
                self.handle.raw(),
                name,
                FILE_GENERIC_READ.0,
                FILE_OPEN,
                FILE_OPEN_REPARSE_POINT,
            )
            .and_then(|handle| {
                let i = info(handle.raw())?;
                reject_attrs(&i)?;
                let file: fs::File = handle.into();
                Ok(VerifiedFile { file })
            }) {
                Ok(v) => Ok(Some(v)),
                Err(e)
                    if matches!(
                        e.raw_os_error().map(|c| c as u32),
                        Some(NOT_FOUND) | Some(PATH_NOT_FOUND)
                    ) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        }

        /// Open `name` for reading; fails closed unless the opened object is a
        /// regular file inside the pinned directory (reparse points refused).
        pub fn open_read(&self, name: &str) -> io::Result<fs::File> {
            let v = self.open_verified(
                name,
                FILE_GENERIC_READ.0,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            )?;
            Ok(v.file)
        }

        /// Exclusively create `name` inside the pinned directory (name
        /// verified after open). No mode/ACL is applied: creation inherits
        /// the parent ACL, and existing ACLs are never rewritten.
        pub fn create_temp(&self, name: &str) -> io::Result<fs::File> {
            self.open_verified(
                name,
                FILE_GENERIC_WRITE.0,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            )
            .map(|v| v.file)
        }

        /// Atomic no-replace publication through `FileLinkInformation`.
        /// Both source and destination are resolved relative to the held
        /// directory handle; no path lookup or post-hoc identity check exists.
        pub fn publish(&self, from: &str, to: &str) -> io::Result<()> {
            let src = self.open_verified(
                from,
                FILE_GENERIC_READ.0 | FILE_WRITE_ATTRIBUTES_ACCESS,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            )?;
            let name = relative_name(to)?;
            let bytes = name.len() * 2;
            let header = std::mem::offset_of!(NtFileLinkInformation, file_name);
            let storage_len =
                std::cmp::max(std::mem::size_of::<NtFileLinkInformation>(), header + bytes);
            // `NtFileLinkInformation` contains a HANDLE and must be aligned;
            // retain byte length separately while backing it with u64 storage.
            let mut storage = vec![0u64; storage_len.div_ceil(std::mem::size_of::<u64>())];
            let link = storage.as_mut_ptr() as *mut NtFileLinkInformation;
            unsafe {
                (*link).replace_if_exists = 0;
                (*link).root_directory = self.handle.raw();
                (*link).file_name_length = bytes as u32;
                std::ptr::copy_nonoverlapping(
                    name.as_ptr(),
                    (*link).file_name.as_mut_ptr(),
                    name.len(),
                );
            }
            let mut status_block = IO_STATUS_BLOCK::default();
            let status = unsafe {
                NtSetInformationFile(
                    HANDLE(src.file.as_raw_handle() as *mut core::ffi::c_void),
                    &mut status_block,
                    link as *const core::ffi::c_void,
                    storage_len as u32,
                    FILE_LINK_INFORMATION_CLASS,
                )
            };
            if status.0 < 0 {
                let error = nt_error(status);
                return if matches!(
                    error.raw_os_error().map(|c| c as u32),
                    Some(ALREADY_EXISTS) | Some(FILE_EXISTS)
                ) {
                    Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        error.to_string(),
                    ))
                } else {
                    Err(error)
                };
            }
            let _ = self.unlink(from);
            // Flush the pinned directory handle before reporting publication.
            unsafe { FlushFileBuffers(self.handle.raw()) }.map_err(|_| last_err())?;
            Ok(())
        }

        /// Delete `name` inside the pinned directory by opening a handle
        /// relative to that directory and setting disposition on that handle.
        pub fn unlink(&self, name: &str) -> io::Result<()> {
            let v = self.open_verified(
                name,
                FILE_GENERIC_READ.0 | DELETE_ACCESS,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            )?;
            let disposition = NtFileDispositionInformation { delete_file: 1 };
            let mut status_block = IO_STATUS_BLOCK::default();
            let status = unsafe {
                NtSetInformationFile(
                    HANDLE(v.file.as_raw_handle() as *mut core::ffi::c_void),
                    &mut status_block,
                    &disposition as *const NtFileDispositionInformation as *const core::ffi::c_void,
                    std::mem::size_of::<NtFileDispositionInformation>() as u32,
                    FILE_DISPOSITION_INFORMATION_CLASS,
                )
            };
            if status.0 < 0 {
                Err(nt_error(status))
            } else {
                Ok(())
            }
        }

        /// Enumerate the pinned directory handle via
        /// `FileIdBothDirectoryInfo`. The listing comes from the handle, never
        /// from a path, so a swapped parent cannot change what is seen.
        /// Returns (name, attributes).
        pub fn names(&self, limit: usize) -> io::Result<Vec<(String, u32)>> {
            const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileIdBothDirectoryInfo;
            let mut out = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let r = unsafe {
                    GetFileInformationByHandleEx(
                        self.handle.raw(),
                        CLASS,
                        buf.as_mut_ptr() as *mut core::ffi::c_void,
                        buf.len() as u32,
                    )
                };
                if r.is_err() {
                    let e = unsafe { GetLastError() };
                    if e.0 == NO_MORE_FILES {
                        return Ok(out);
                    }
                    return Err(io::Error::from_raw_os_error(e.0 as i32));
                }
                let mut offset = 0usize;
                loop {
                    let next_offset_at =
                        std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, NextEntryOffset);
                    let name_length_at =
                        std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength);
                    let attributes_at = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileAttributes);
                    let name_at = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
                    let read_u32 = |at: usize| -> io::Result<u32> {
                        let start = offset
                            .checked_add(at)
                            .ok_or_else(|| invalid_input("malformed directory record"))?;
                        let end = start
                            .checked_add(4)
                            .ok_or_else(|| invalid_input("malformed directory record"))?;
                        let bytes = buf
                            .get(start..end)
                            .ok_or_else(|| invalid_input("malformed directory record"))?;
                        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
                    };
                    let next = read_u32(next_offset_at)? as usize;
                    let name_bytes = read_u32(name_length_at)? as usize;
                    let attributes = read_u32(attributes_at)?;
                    let record_len = if next == 0 {
                        buf.len().saturating_sub(offset)
                    } else {
                        next
                    };
                    if offset > buf.len()
                        || record_len < name_at
                        || offset
                            .checked_add(record_len)
                            .is_none_or(|end| end > buf.len())
                        || name_bytes % 2 != 0
                        || name_bytes > record_len - name_at
                        || (next != 0 && (next < name_at || next & 7 != 0))
                    {
                        return Err(invalid_input("malformed directory record"));
                    }
                    let start = offset + name_at;
                    let bytes = &buf[start..start + name_bytes];
                    let units: Vec<u16> = bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect();
                    let name = String::from_utf16_lossy(&units);
                    if name != "." && name != ".." {
                        if out.len() >= limit {
                            return Err(invalid_input("metadata directory entry cap exceeded"));
                        }
                        out.push((name, attributes));
                    }
                    if next == 0 {
                        break;
                    }
                    offset += next;
                }
            }
        }
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `snapshot` as `<id>.json` inside the pinned state directory. Never
/// overwrites an existing snapshot or unrelated file (`AlreadyExists`). The
/// write goes to an exclusively created `.<id>.<unique>.tmp` file (unix mode
/// 0o600), is synced, then published atomically and without replacement
/// (`linkat` on unix, `FileLinkInformation` relative to the pinned directory
/// handle on Windows).
/// Swapping or renaming the directory between pin and publish cannot
/// redirect the write: unix operations are descriptor-relative, and on
/// Windows every child operation is rooted at the pinned directory handle.
/// Newly created unix directories get 0o700; existing
/// directories must already be owned by the effective user and not writable
/// by group or others — Pulse refuses rather than changing ownership or
/// permissions of pre-existing paths (Windows creation inherits the parent
/// ACL; existing ACLs are never rewritten). Leftover temp files from
/// interrupted writes are never parsed; `history_report` lists them as
/// "interrupted publication".
pub fn save(directory: &Path, snapshot: &Snapshot) -> io::Result<PathBuf> {
    validate_id(&snapshot.id).map_err(|m| io::Error::new(io::ErrorKind::InvalidInput, m))?;
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err(invalid_input(
            "refusing to write unsupported snapshot schema version",
        ));
    }
    let bytes = serde_json::to_vec(snapshot).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(invalid_input("snapshot exceeds size cap"));
    }
    reject_links(directory)?;
    create_directory(directory)?;
    reject_links(directory)?;
    let dir = pinned::PinnedDir::pin(directory)?;
    let dest_name = format!("{}.json", snapshot.id);
    #[cfg(unix)]
    let exists = dir.stat(&dest_name)?.is_some();
    #[cfg(windows)]
    let exists = dir.stat(&dest_name)?.is_some();
    if exists {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "snapshot already exists",
        ));
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_name = format!(
        ".{}.{}-{}-{}.tmp",
        snapshot.id,
        std::process::id(),
        nanos,
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let result = (|| {
        #[cfg(unix)]
        {
            let mut file = dir.create_temp(&temp_name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            dir.publish(&temp_name, &dest_name)?;
        }
        #[cfg(windows)]
        {
            let mut file = dir.create_temp(&temp_name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            dir.publish(&temp_name, &dest_name)?;
        }
        Ok(directory.join(&dest_name))
    })();
    if result.is_err() {
        #[cfg(unix)]
        let _ = dir.unlink(&temp_name);
        #[cfg(windows)]
        let _ = dir.unlink(&temp_name);
    }
    result
}

#[derive(Deserialize)]
struct Header {
    schema_version: u64,
}

enum Fail {
    Budget,
    Reason(String),
}
impl From<io::Error> for Fail {
    fn from(e: io::Error) -> Self {
        Fail::Reason(e.to_string())
    }
}

/// Read up to the size/budget caps from an already-verified open handle.
/// The handle was opened `O_NOFOLLOW` (unix) or relative to the pinned
/// directory handle (Windows), so the object read is the object that was checked.
fn read_file(file: fs::File, meta_len: u64, remaining: &mut u64) -> Result<Vec<u8>, Fail> {
    if meta_len > MAX_SNAPSHOT_BYTES {
        return Err(Fail::Reason("snapshot exceeds size cap".into()));
    }
    if meta_len > *remaining {
        return Err(Fail::Budget);
    }
    let limit = MAX_SNAPSHOT_BYTES.min(*remaining);
    let mut bytes = Vec::new();
    // Read one extra byte so a file that grew after the check is still caught.
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(if limit < MAX_SNAPSHOT_BYTES {
            Fail::Budget
        } else {
            Fail::Reason("snapshot exceeds size cap".into())
        });
    }
    *remaining -= bytes.len() as u64;
    Ok(bytes)
}

#[cfg(unix)]
fn read_bounded(dir: &pinned::PinnedDir, name: &str, remaining: &mut u64) -> Result<Vec<u8>, Fail> {
    let file = dir.open_read(name)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Fail::Reason("not a regular file".into()));
    }
    read_file(file, meta.len(), remaining)
}

#[cfg(windows)]
fn read_bounded(dir: &pinned::PinnedDir, name: &str, remaining: &mut u64) -> Result<Vec<u8>, Fail> {
    let file = dir.open_read(name)?;
    let meta = file.metadata()?;
    if !meta.is_file() || is_link(&meta) {
        return Err(Fail::Reason("not a regular file".into()));
    }
    read_file(file, meta.len(), remaining)
}

#[cfg(unix)]
fn load(
    dir: &pinned::PinnedDir,
    name: &str,
    stem: &str,
    remaining: &mut u64,
) -> Result<Snapshot, Fail> {
    let bytes = read_bounded(dir, name, remaining)?;
    parse(&bytes, stem)
}

#[cfg(windows)]
fn load(
    dir: &pinned::PinnedDir,
    name: &str,
    stem: &str,
    remaining: &mut u64,
) -> Result<Snapshot, Fail> {
    let bytes = read_bounded(dir, name, remaining)?;
    parse(&bytes, stem)
}

fn parse(bytes: &[u8], stem: &str) -> Result<Snapshot, Fail> {
    let malformed = |e: serde_json::Error| Fail::Reason(format!("malformed snapshot: {e}"));
    let header: Header = serde_json::from_slice(bytes).map_err(malformed)?;
    if header.schema_version != u64::from(SCHEMA_VERSION) {
        return Err(Fail::Reason(format!(
            "unsupported schema version {} (supported: {SCHEMA_VERSION})",
            header.schema_version
        )));
    }
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(malformed)?;
    validate_id(&snapshot.id).map_err(Fail::Reason)?;
    if snapshot.id != stem {
        return Err(Fail::Reason("snapshot id does not match file name".into()));
    }
    Ok(snapshot)
}

/// Temp names written by `save`: `.scan-*.tmp` (also legacy `scan-*.json.tmp`).
fn is_temp_name(name: &str) -> bool {
    name.ends_with(".tmp")
        && (name.starts_with(&format!(".{ID_PREFIX}")) || name.starts_with(ID_PREFIX))
}

/// Load all snapshots, skipping (with a reason) any individual bad file:
/// malformed, oversized, unknown schema version, bad or mismatched id,
/// duplicate ids, symlink or non-regular entries named like snapshots, files
/// left unread once the aggregate read budget is spent, and leftover temp
/// files ("interrupted publication", never opened or parsed). Unrelated names
/// are ignored. Enumeration runs through the pinned descriptor (`fdopendir`
/// on unix, `FileIdBothDirectoryInfo` on the Windows handle), retains at most
/// `MAX_ENUMERATED_ENTRIES` names, and stops as soon as more than
/// `MAX_SNAPSHOTS` candidate names (snapshots plus temp files) are seen, and then errors. Errors are otherwise reserved for the
/// directory itself (link/reparse point, wrong ownership, group/other
/// writable, unreadable, swapped mid-check). Nothing is ever migrated or
/// deleted.
pub fn history_report(directory: &Path) -> io::Result<HistoryReport> {
    history_report_with_budget(directory, MAX_AGGREGATE_READ_BYTES)
}

/// `history_report` with an explicit aggregate read budget in bytes.
pub fn history_report_with_budget(directory: &Path, budget: u64) -> io::Result<HistoryReport> {
    reject_links(directory)?;
    let dir = match pinned::PinnedDir::pin(directory) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HistoryReport::default()),
        Err(e) => return Err(e),
    };
    let mut report = HistoryReport::default();
    report
        .capability_notes
        .extend(PLATFORM_NOTES.iter().map(|s| s.to_string()));
    // Candidates are (name, windows attributes); attributes are `None` on
    // unix, where the regular-file check goes through `fstatat` instead.
    let mut names: Vec<(String, Option<u32>)> = Vec::new();
    let mut candidates = 0usize;
    #[cfg(unix)]
    let entries: Vec<(String, Option<u32>)> = dir
        .names(MAX_ENUMERATED_ENTRIES)?
        .into_iter()
        .map(|n| (n, None))
        .collect();
    #[cfg(windows)]
    let entries: Vec<(String, Option<u32>)> = dir
        .names(MAX_ENUMERATED_ENTRIES)?
        .into_iter()
        .map(|(n, a)| (n, Some(a)))
        .collect();
    for (name, attrs) in entries {
        if is_temp_name(&name) {
            candidates += 1;
            report.skipped.push(SkippedSnapshot {
                file: name,
                reason: REASON_INTERRUPTED.into(),
            });
        } else if name.starts_with(ID_PREFIX) && name.ends_with(".json") {
            candidates += 1;
            names.push((name, attrs));
        }
        if candidates > MAX_SNAPSHOTS {
            return Err(invalid("history exceeds snapshot count cap"));
        }
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    // Names that differ only by ASCII case resolve to one ID on
    // case-insensitive filesystems; none of them is trusted.
    let mut groups: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (name, _) in &names {
        *groups.entry(name.to_ascii_lowercase()).or_default() += 1;
    }
    let mut remaining = budget;
    let mut exhausted = false;
    for (name, attrs) in names {
        #[cfg(unix)]
        let _ = attrs;
        let skip = |reason: &str| SkippedSnapshot {
            file: name.clone(),
            reason: reason.to_owned(),
        };
        if groups[&name.to_ascii_lowercase()] > 1 {
            report.skipped.push(skip(REASON_DUPLICATE_ID));
            continue;
        }
        if exhausted {
            report.skipped.push(skip(REASON_AGGREGATE_BUDGET));
            continue;
        }
        #[cfg(unix)]
        let regular = match dir.is_regular_file(&name) {
            Ok(r) => r,
            Err(e) => {
                report.skipped.push(skip(&e.to_string()));
                continue;
            }
        };
        #[cfg(windows)]
        let regular =
            attrs.is_some_and(|a| a & 0x10 == 0 && a & 0x400 == 0) && !name.contains('\\');
        if !regular {
            report.skipped.push(skip("not a regular file"));
            continue;
        }
        let stem = &name[..name.len() - ".json".len()];
        if let Err(reason) = validate_id(stem) {
            report.skipped.push(skip(&reason));
            continue;
        }
        #[cfg(unix)]
        let loaded = load(&dir, &name, stem, &mut remaining);
        #[cfg(windows)]
        let loaded = load(&dir, &name, stem, &mut remaining);
        match loaded {
            Ok(snapshot) => report.snapshots.push(snapshot),
            Err(Fail::Budget) => {
                exhausted = true;
                report.skipped.push(skip(REASON_AGGREGATE_BUDGET));
            }
            Err(Fail::Reason(reason)) => report.skipped.push(skip(&reason)),
        }
    }
    report
        .snapshots
        .sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
    report
        .skipped
        .sort_by(|a, b| (&a.file, &a.reason).cmp(&(&b.file, &b.reason)));
    Ok(report)
}

/// Valid snapshots only, deterministically ordered. Bad files are skipped; use
/// `history_report` to see why.
pub fn history(directory: &Path) -> io::Result<Vec<Snapshot>> {
    history_report(directory).map(|r| r.snapshots)
}

// ---------------------------------------------------------------------------
// Versioned durable records.
// ---------------------------------------------------------------------------

/// Schema version for generic state records. Snapshot schema is deliberately
/// separate: records are journals/projections, never scan snapshots.
pub const STATE_SCHEMA_VERSION: u32 = 1;
/// Maximum serialized record size. Records are bounded independently from
/// snapshots so a corrupt journal cannot consume the snapshot read budget.
pub const MAX_STATE_RECORD_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum number of record files retained in one state namespace.
pub const MAX_STATE_RECORDS: usize = 10_000;
/// Maximum number of version files examined for one namespace.
pub const MAX_STATE_VERSIONS: usize = MAX_STATE_RECORDS * 32;
const MAX_STATE_ID_BYTES: usize = 256;
const STATE_FILE_PREFIX: &str = "record-";

/// A generic, versioned state value. The `id` is data, not a path; record
/// filenames use a byte-safe hexadecimal key so activity IDs may retain their
/// existing syntax without weakening path validation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionedRecord<T> {
    pub schema_version: u32,
    pub id: String,
    pub version: u64,
    pub state: T,
}

/// A pinned private child namespace below a caller-provided state directory.
/// Each publication is append-only and exact: an existing `(id, version)` can
/// never be replaced. Readers select the highest valid version for each id.
#[derive(Clone)]
pub struct StateStore {
    directory: PathBuf,
    namespace: String,
    pinned: Arc<pinned::PinnedDir>,
}

impl std::fmt::Debug for StateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StateStore")
            .field("directory", &self.directory)
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl StateStore {
    /// Open (and, when absent, create) a private child namespace. Existing
    /// paths are only accepted after the same pinned ownership/mode checks as
    /// snapshot storage; no permissions or ACLs are rewritten.
    pub fn open(directory: &Path, namespace: &str) -> io::Result<Self> {
        validate_namespace(namespace)?;
        reject_links(directory)?;
        create_directory(directory)?;
        reject_links(directory)?;
        // Pin parent first so a concurrently swapped parent cannot redirect
        // child creation. Child creation is descriptor-relative.
        let parent = pinned::PinnedDir::pin(directory)?;
        let child = directory.join(namespace);
        let child_handle = parent.ensure_child(namespace)?;
        Ok(Self {
            directory: child,
            namespace: namespace.to_owned(),
            pinned: Arc::new(child_handle),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Publish one immutable version. `AlreadyExists` is returned for an
    /// exact existing claim; unrelated files are never overwritten.
    pub fn publish<T: Serialize>(&self, id: &str, version: u64, state: &T) -> io::Result<PathBuf> {
        validate_state_id(id)?;
        if version == 0 {
            return Err(invalid_input("state record version must be nonzero"));
        }
        let record = VersionedRecord {
            schema_version: STATE_SCHEMA_VERSION,
            id: id.to_owned(),
            version,
            state,
        };
        let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX_STATE_RECORD_BYTES {
            return Err(invalid_input("state record exceeds size cap"));
        }
        let dir = self.pin()?;
        let destination = state_file_name(id, version)?;
        if dir.stat(&destination)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "state record already exists",
            ));
        }
        let existing_names = state_names(&dir)?;
        if existing_names
            .iter()
            .filter(|name| parse_state_file_name(name).is_some())
            .count()
            >= MAX_STATE_VERSIONS
        {
            return Err(invalid_input("state record version cap exceeded"));
        }
        let encoded = encode_id(id);
        let has_id = existing_names
            .iter()
            .filter_map(|name| parse_state_file_name(name))
            .any(|(key, _)| key == encoded);
        if !has_id {
            let mut ids = std::collections::BTreeSet::new();
            for name in &existing_names {
                if let Some((key, _)) = parse_state_file_name(name) {
                    ids.insert(key);
                }
            }
            if ids.len() >= MAX_STATE_RECORDS {
                return Err(invalid_input("state record id cap exceeded"));
            }
        }
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = format!(
            ".{}{}-{}-{}.tmp",
            STATE_FILE_PREFIX,
            encode_id(id),
            std::process::id(),
            unique
        );
        let result = (|| {
            let mut file = dir.create_temp(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            dir.publish(&temporary, &destination)?;
            Ok(self.directory.join(destination))
        })();
        if result.is_err() {
            let _ = dir.unlink(&temporary);
        }
        result
    }

    /// Return newest record for `id`, or `None` if no record has been claimed.
    pub fn load<T: for<'de> Deserialize<'de>>(
        &self,
        id: &str,
    ) -> io::Result<Option<VersionedRecord<T>>> {
        validate_state_id(id)?;
        let dir = self.pin()?;
        let names = state_names(&dir)?;
        let encoded = encode_id(id);
        let mut newest: Option<(u64, String)> = None;
        for name in names {
            if let Some((key, version)) = parse_state_file_name(&name)
                && key == encoded
            {
                newest = match newest {
                    Some((v, n)) if v >= version => Some((v, n)),
                    _ => Some((version, name)),
                };
            }
        }
        newest
            .map(|(_, name)| read_state_record(&dir, &name, id))
            .transpose()
    }

    /// Load newest record for every id. Malformed records are an error: using
    /// an older journal silently would risk repeating an indeterminate effect.
    pub fn records<T: for<'de> Deserialize<'de>>(&self) -> io::Result<Vec<VersionedRecord<T>>> {
        let dir = self.pin()?;
        let names = state_names(&dir)?;
        let mut newest: std::collections::BTreeMap<String, (u64, String)> =
            std::collections::BTreeMap::new();
        for name in names {
            if let Some((key, version)) = parse_state_file_name(&name) {
                let id = decode_id(&key)?;
                let entry = newest.entry(id).or_insert((version, name.clone()));
                if version > entry.0 {
                    *entry = (version, name);
                }
            }
        }
        if newest.len() > MAX_STATE_RECORDS {
            return Err(invalid("state record id cap exceeded"));
        }
        newest
            .into_iter()
            .map(|(id, (_, name))| read_state_record(&dir, &name, &id))
            .collect()
    }

    fn pin(&self) -> io::Result<Arc<pinned::PinnedDir>> {
        Ok(Arc::clone(&self.pinned))
    }
}

fn validate_namespace(namespace: &str) -> io::Result<()> {
    if namespace.is_empty()
        || namespace.len() > 64
        || !namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid_input("invalid state namespace"));
    }
    Ok(())
}

fn validate_state_id(id: &str) -> io::Result<()> {
    if id.is_empty() || id.len() > MAX_STATE_ID_BYTES || !id.is_char_boundary(id.len()) {
        return Err(invalid_input("invalid state record id"));
    }
    Ok(())
}

fn encode_id(id: &str) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_id(key: &str) -> io::Result<String> {
    if key.is_empty() || !key.len().is_multiple_of(2) || !key.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("invalid state record filename"));
    }
    let mut bytes = Vec::with_capacity(key.len() / 2);
    for pair in key.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16).unwrap();
        let low = (pair[1] as char).to_digit(16).unwrap();
        bytes.push((high * 16 + low) as u8);
    }
    let id = String::from_utf8(bytes).map_err(|_| invalid("state record id is not UTF-8"))?;
    validate_state_id(&id)?;
    Ok(id)
}

fn state_file_name(id: &str, version: u64) -> io::Result<String> {
    validate_state_id(id)?;
    Ok(format!(
        "{}{}-v{}.json",
        STATE_FILE_PREFIX,
        encode_id(id),
        version
    ))
}

fn parse_state_file_name(name: &str) -> Option<(String, u64)> {
    let rest = name
        .strip_prefix(STATE_FILE_PREFIX)?
        .strip_suffix(".json")?;
    let (key, version) = rest.rsplit_once("-v")?;
    if key.is_empty() || version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let version = version.parse().ok()?;
    (version > 0).then(|| (key.to_owned(), version))
}

fn state_names(dir: &pinned::PinnedDir) -> io::Result<Vec<String>> {
    #[cfg(unix)]
    let names = dir.names(MAX_STATE_VERSIONS)?;
    #[cfg(windows)]
    let names: Vec<String> = dir
        .names(MAX_STATE_VERSIONS)?
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    if names
        .iter()
        .filter(|name| parse_state_file_name(name).is_some())
        .count()
        > MAX_STATE_VERSIONS
    {
        return Err(invalid("state record count cap exceeded"));
    }
    Ok(names)
}

fn read_state_record<T: for<'de> Deserialize<'de>>(
    dir: &pinned::PinnedDir,
    name: &str,
    expected_id: &str,
) -> io::Result<VersionedRecord<T>> {
    let file = dir.open_read(name)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(invalid("state record is not a regular file"));
    }
    if meta.len() > MAX_STATE_RECORD_BYTES {
        return Err(invalid("state record exceeds size cap"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_RECORD_BYTES {
        return Err(invalid("state record exceeds size cap"));
    }
    let record: VersionedRecord<T> = serde_json::from_slice(&bytes)
        .map_err(|e| invalid(format!("malformed state record: {e}")))?;
    if record.schema_version != STATE_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported state schema version {}",
            record.schema_version
        )));
    }
    validate_state_id(&record.id)?;
    if record.id != expected_id || record.version == 0 {
        return Err(invalid("state record identity does not match filename"));
    }
    Ok(record)
}
