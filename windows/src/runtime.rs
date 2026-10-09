//! Runtime policy helpers that need no window state: notch placement math, placement
//! planning (monitor set + slots), the retry state machine for show/hide, cadence, and the
//! single-instance mutex. Production code in main.rs and the tests call the same functions.

use crate::layout::{self, Edge};
use crate::lifecycle::{
    Bounds, HIDDEN_INTERVAL_MS, MonitorSpec, PanelAction, plan_panels, sampling_interval_ms,
};
use crate::settings::POSITION_DEFAULT;
use std::ffi::c_void;
use std::path::Path;
use windows::Win32::Foundation::{
    CloseHandle, E_INVALIDARG, ERROR_ALREADY_EXISTS, GetLastError, HANDLE,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::{BOOL, Error, HRESULT, PCWSTR, PWSTR};

// ------------------------------------------------------------------ security bindings
//
// windows-rs 0.61.3 gates SECURITY_ATTRIBUTES, OpenProcessToken, GetTokenInformation,
// the SID/SDDL converters and the named-object security-info APIs behind the
// `Win32_Security` / `Win32_Security_Authorization` features, which are not enabled
// in windows/Cargo.toml (the manifest is owned elsewhere). `winsec` declares the same
// advapi32/kernel32 symbols with the ABI the crate would emit; when those features
// land, this module can be deleted and the crate types swapped in.
#[allow(non_snake_case)]
pub(crate) mod winsec {
    use super::*;

    #[allow(clippy::upper_case_acronyms)]
    pub type PSID = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SECURITY_ATTRIBUTES {
        pub nLength: u32,
        pub lpSecurityDescriptor: *mut c_void,
        pub bInheritHandle: BOOL,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ACE_HEADER {
        pub AceType: u8,
        pub AceFlags: u8,
        pub AceSize: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct ACCESS_ALLOWED_ACE {
        pub Header: ACE_HEADER,
        pub Mask: u32,
        pub SidStart: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(clippy::upper_case_acronyms)]
    pub struct ACL {
        pub AclRevision: u8,
        pub Sbz1: u8,
        pub AclSize: u16,
        pub AceCount: u16,
        pub Sbz2: u16,
    }

    /// TOKEN_USER: a SID_AND_ATTRIBUTES at offset 0.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TOKEN_USER {
        pub Sid: PSID,
        pub Attributes: u32,
    }

    pub const TOKEN_QUERY: u32 = 0x0008;
    pub const TOKEN_USER: i32 = 1;
    pub const SDDL_REVISION_1: u32 = 1;
    pub const SE_FILE_OBJECT: i32 = 1;
    pub const SE_KERNEL_OBJECT: i32 = 6;
    pub const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
    pub const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    pub const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const CREATE_NEW: u32 = 1;
    pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    pub const ERROR_FILE_EXISTS: u32 = 80;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        pub fn OpenProcessToken(process: HANDLE, access: u32, token: *mut HANDLE) -> BOOL;
        pub fn GetTokenInformation(
            token: HANDLE,
            class: i32,
            info: *mut c_void,
            info_len: u32,
            out_len: *mut u32,
        ) -> BOOL;
        pub fn ConvertSidToStringSidW(sid: PSID, out: *mut PWSTR) -> BOOL;
        pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: PCWSTR,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> BOOL;
        pub fn GetNamedSecurityInfoW(
            name: PCWSTR,
            object_type: i32,
            info: u32,
            owner: *mut PSID,
            group: *mut PSID,
            dacl: *mut *mut ACL,
            sacl: *mut *mut ACL,
            descriptor: *mut *mut c_void,
        ) -> u32;
        pub fn GetSecurityInfo(
            handle: HANDLE,
            object_type: i32,
            info: u32,
            owner: *mut PSID,
            group: *mut PSID,
            dacl: *mut *mut ACL,
            sacl: *mut *mut ACL,
            descriptor: *mut *mut c_void,
        ) -> u32;
        pub fn IsValidSid(sid: PSID) -> BOOL;
        pub fn EqualSid(a: PSID, b: PSID) -> BOOL;
        pub fn GetAce(acl: *const ACL, index: u32, ace: *mut *mut c_void) -> BOOL;
    }

    // installer.rs declares CreateFileW with raw pointer types; same ABI.
    #[allow(clashing_extern_declarations)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn CreateMutexW(
            attributes: *const SECURITY_ATTRIBUTES,
            initial_owner: BOOL,
            name: PCWSTR,
        ) -> HANDLE;
        pub fn CreateDirectoryW(path: PCWSTR, attributes: *const SECURITY_ATTRIBUTES) -> BOOL;
        pub fn CreateFileW(
            path: PCWSTR,
            access: u32,
            share: u32,
            attributes: *const SECURITY_ATTRIBUTES,
            disposition: u32,
            flags: u32,
            template: HANDLE,
        ) -> HANDLE;
        pub fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }
}

// ------------------------------------------------------------------ placement

/// Where one monitor's notch sits: `edge` is the screen edge it is welded to, `along` the
/// per-mille position of its centre along that edge (Alt-drag remembers both), `folded`
/// whether it is the resting pill rather than the open body, and `dpi` sizes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub edge: Edge,
    pub along: u16,
    pub folded: bool,
    pub dpi: u32,
}

impl Slot {
    pub const DEFAULT: Self = Self {
        edge: Edge::Top,
        along: POSITION_DEFAULT,
        folded: false,
        dpi: 96,
    };
}

/// Notch rectangle for `monitor`: flush with the slot's edge, centred on `slot.along` and
/// clamped to stay fully inside the monitor whenever it fits. Plain comparisons keep this
/// correct for negative virtual-screen origins; on a monitor smaller than the notch the
/// notch hugs the monitor's top-left.
pub fn notch_bounds(monitor: Bounds, slot: Slot) -> Bounds {
    let (width, height) = layout::panel_size(slot.edge, slot.folded, slot.dpi);
    let (left, top) = layout::origin_for(
        slot.edge,
        (monitor.left, monitor.top, monitor.right, monitor.bottom),
        (width, height),
        slot.along,
    );
    Bounds {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub spec: MonitorSpec,
    pub slot: Slot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    Destroy(String),
    Move(Placed),
    Create(Placed),
}

/// Diff existing panels against the desired (enabled) monitors. Monitor-set changes come from
/// `lifecycle::plan_panels`; a slot change (position or DPI) on an otherwise unchanged
/// monitor adds a Move. A move that failed leaves the panel's recorded placement unchanged,
/// so the next plan contains the same Move again (retry without losing the window handle).
pub fn plan_placements(existing: &[Placed], desired: &[Placed]) -> Vec<Placement> {
    let existing_specs: Vec<MonitorSpec> = existing.iter().map(|p| p.spec.clone()).collect();
    let desired_specs: Vec<MonitorSpec> = desired.iter().map(|p| p.spec.clone()).collect();
    let slot_for = |id: &str| {
        desired
            .iter()
            .find(|p| p.spec.id == id)
            .map(|p| p.slot)
            .unwrap_or(Slot::DEFAULT)
    };
    let mut plan: Vec<Placement> = plan_panels(&existing_specs, &desired_specs)
        .into_iter()
        .map(|action| match action {
            PanelAction::Destroy(id) => Placement::Destroy(id),
            PanelAction::Move(spec) => {
                let slot = slot_for(&spec.id);
                Placement::Move(Placed { spec, slot })
            }
            PanelAction::Create(spec) => {
                let slot = slot_for(&spec.id);
                Placement::Create(Placed { spec, slot })
            }
        })
        .collect();
    for old in existing {
        let wanted = slot_for(&old.spec.id);
        let present = desired.iter().any(|d| d.spec.id == old.spec.id);
        let already = plan.iter().any(|p| match p {
            Placement::Move(m) => m.spec.id == old.spec.id,
            Placement::Destroy(id) => *id == old.spec.id,
            Placement::Create(_) => false,
        });
        if present && !already && wanted != old.slot {
            let spec = desired
                .iter()
                .find(|d| d.spec.id == old.spec.id)
                .map(|d| d.spec.clone())
                .unwrap_or_else(|| old.spec.clone());
            plan.push(Placement::Move(Placed { spec, slot: wanted }));
        }
    }
    plan
}

// ------------------------------------------------------------------ retry state machine

/// Tracks the last *successfully applied* value of one transition target (e.g. "hidden").
/// A failed attempt leaves `applied` untouched, so `needs` stays true and the next tick
/// retries; the window handle stays owned by the caller throughout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry<T: Copy + PartialEq> {
    applied: T,
    failures: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryReport {
    /// Applied; no earlier failure to report.
    Applied,
    /// Applied after `n` consecutive failures.
    Recovered(u32),
    /// First failure of an episode (log it).
    Failed,
    /// Further failure; `n` consecutive so far (do not log every tick).
    StillFailing(u32),
}

impl<T: Copy + PartialEq> Retry<T> {
    pub const fn new(applied: T) -> Self {
        Self {
            applied,
            failures: 0,
        }
    }
    pub fn applied(&self) -> T {
        self.applied
    }
    pub fn needs(&self, desired: T) -> bool {
        self.applied != desired
    }
    #[allow(dead_code)] // diagnostic accessor; the report from `record` drives logging
    pub fn failures(&self) -> u32 {
        self.failures
    }
    /// Record the result of attempting to reach `desired`.
    pub fn record(&mut self, desired: T, ok: bool) -> RetryReport {
        if ok {
            self.applied = desired;
            let earlier = std::mem::take(&mut self.failures);
            if earlier > 0 {
                RetryReport::Recovered(earlier)
            } else {
                RetryReport::Applied
            }
        } else {
            self.failures = self.failures.saturating_add(1);
            if self.failures == 1 {
                RetryReport::Failed
            } else {
                RetryReport::StillFailing(self.failures)
            }
        }
    }
}

/// Panel is hidden when the pill is globally not visible or fullscreen evidence says so.
pub fn desired_hidden(settings_visible: bool, suppressed: bool) -> bool {
    !settings_visible || suppressed
}

// ------------------------------------------------------------------ cadence

/// Visible cadence comes from settings (already clamped 2..=10 s); when no panel is visible
/// (all suppressed, pill hidden by settings, or no panels) polling stays at 10 s.
pub fn interval_ms(cadence_seconds: u32, total_panels: usize, hidden_panels: usize) -> u32 {
    let base = sampling_interval_ms(total_panels, hidden_panels);
    if base == HIDDEN_INTERVAL_MS {
        HIDDEN_INTERVAL_MS
    } else {
        crate::settings::clamp_cadence(cadence_seconds as i64) * 1000
    }
}

// ------------------------------------------------------------------ user identity & DACLs

/// Frees a LocalAlloc/Convert*-allocated buffer on drop (SID strings, security
/// descriptors returned by GetNamedSecurityInfoW and the SDDL converter).
struct LocalAllocGuard(*mut c_void);

impl Drop for LocalAllocGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { winsec::LocalFree(self.0) };
        }
    }
}

/// Token-derived identity of the current user.
///
/// `token_user` is the TOKEN_USER buffer returned by GetTokenInformation: the SID
/// pointer inside it addresses this buffer, so it must stay alive (and un-moved)
/// for as long as `sid()` is used. `sid_string` scopes the single-instance mutex
/// name and builds the restrictive SDDL.
pub struct UserSecurity {
    token_user: Vec<u8>,
    sid_string: String,
}

impl UserSecurity {
    /// OpenProcessToken(TOKEN_QUERY) -> GetTokenInformation(TokenUser) ->
    /// ConvertSidToStringSidW. Fails only if the process token cannot be read.
    pub fn current() -> Result<Self, Error> {
        let mut token = HANDLE::default();
        let ok = unsafe {
            winsec::OpenProcessToken(GetCurrentProcess(), winsec::TOKEN_QUERY, &mut token)
        };
        if !ok.as_bool() {
            return Err(Error::from_win32());
        }
        struct TokenHandle(HANDLE);
        impl Drop for TokenHandle {
            fn drop(&mut self) {
                let _ = unsafe { CloseHandle(self.0) };
            }
        }
        let token = TokenHandle(token);
        // Size probe: the call must fail with ERROR_INSUFFICIENT_BUFFER and report
        // the required length; any other outcome is a hard failure.
        let mut needed = 0u32;
        unsafe {
            let _ = winsec::GetTokenInformation(
                token.0,
                winsec::TOKEN_USER,
                std::ptr::null_mut(),
                0,
                &mut needed,
            );
        }
        if needed == 0 || needed > 64 * 1024 {
            return Err(Error::new(E_INVALIDARG, "unexpected TokenUser size"));
        }
        let mut buffer = vec![0u8; needed as usize];
        let ok = unsafe {
            winsec::GetTokenInformation(
                token.0,
                winsec::TOKEN_USER,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        if !ok.as_bool() {
            return Err(Error::from_win32());
        }
        // `Vec<u8>` only guarantees byte alignment; TOKEN_USER contains a pointer.
        // Read the header unaligned, while keeping the backing buffer alive for SID use.
        let sid =
            unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<winsec::TOKEN_USER>()).Sid };
        if sid.is_null() || !unsafe { winsec::IsValidSid(sid) }.as_bool() {
            return Err(Error::new(E_INVALIDARG, "invalid token user SID"));
        }
        let mut raw = PWSTR::null();
        if !unsafe { winsec::ConvertSidToStringSidW(sid, &mut raw) }.as_bool() {
            return Err(Error::from_win32());
        }
        let _guard = LocalAllocGuard(raw.0.cast());
        let sid_string = unsafe { raw.to_string() }
            .map_err(|_| Error::new(E_INVALIDARG, "SID string not UTF-16"))?;
        Ok(Self {
            token_user: buffer,
            sid_string,
        })
    }

    /// `S-1-5-21-...` text form of the token user's SID.
    pub fn sid_string(&self) -> &str {
        &self.sid_string
    }

    /// Raw SID pointer (into `token_user`; valid while `self` lives, do not move it
    /// out before use — pointers are only taken inside calls here).
    pub(crate) fn sid(&self) -> winsec::PSID {
        unsafe {
            std::ptr::read_unaligned(self.token_user.as_ptr().cast::<winsec::TOKEN_USER>()).Sid
        }
    }

    /// SECURITY_DESCRIPTOR granting GENERIC_ALL to this user and to SYSTEM only
    /// (protected DACL; nothing inherited). Used for the instance mutex and for
    /// every settings directory/file we create.
    pub(crate) fn restrictive_descriptor(&self) -> Result<RestrictiveSecurity, Error> {
        RestrictiveSecurity::for_user(&self.sid_string)
    }
}

/// Well-known NT AUTHORITY\SYSTEM SID (S-1-5-18) as raw bytes: revision 1, one
/// sub-authority, identifier authority 5 (NT), RID 18.
fn system_sid() -> winsec::PSID {
    const SYSTEM_SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
    SYSTEM_SID.as_ptr().cast::<c_void>().cast_mut()
}

/// Well-known BUILTIN\Administrators SID (S-1-5-32-544) as raw bytes: revision 1, two
/// sub-authorities, identifier authority 5 (NT), RIDs 32 and 544.
fn administrators_sid() -> winsec::PSID {
    const ADMINISTRATORS_SID: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0];
    ADMINISTRATORS_SID.as_ptr().cast::<c_void>().cast_mut()
}

/// SDDL for the restrictive DACL: protected DACL, allow GENERIC_ALL to `user_sid`
/// and to `SY` (SYSTEM), with explicit user ownership. A token's default
/// owner may be a group; creation must match our current-user owner check.
pub fn restrictive_sddl(user_sid: &str) -> String {
    format!("O:{user_sid}D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)")
}

/// Owns the LocalAlloc'd self-relative security descriptor behind `attributes()`.
/// Must outlive any object created with those attributes.
pub(crate) struct RestrictiveSecurity {
    descriptor: *mut c_void,
}

impl RestrictiveSecurity {
    /// Descriptor for an arbitrary SDDL string (tests use this to build deliberately
    /// broad DACLs; production only ever calls `for_user`).
    pub(crate) fn from_sddl(sddl: &str) -> Result<Self, Error> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        let ok = unsafe {
            winsec::ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                winsec::SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if !ok.as_bool() || descriptor.is_null() {
            return Err(Error::from_win32());
        }
        Ok(Self { descriptor })
    }

    pub(crate) fn for_user(user_sid: &str) -> Result<Self, Error> {
        Self::from_sddl(&restrictive_sddl(user_sid))
    }

    /// SECURITY_ATTRIBUTES view of the descriptor (non-inheritable handle).
    pub(crate) fn attributes(&self) -> winsec::SECURITY_ATTRIBUTES {
        winsec::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<winsec::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: BOOL(0),
        }
    }
}

impl Drop for RestrictiveSecurity {
    fn drop(&mut self) {
        unsafe { winsec::LocalFree(self.descriptor) };
    }
}

// ------------------------------------------------------------------ settings trust check

/// Why an existing on-disk object failed the trust check. `reason` is a stable
/// machine-readable tag; `source` carries the Win32 code for API failures.
#[derive(Debug)]
pub struct TrustError {
    pub reason: &'static str,
    pub source: Option<Error>,
}

impl TrustError {
    fn api(code: u32) -> Self {
        // GetNamedSecurityInfoW returns a WIN32_ERROR, not an HRESULT.
        let hresult = HRESULT((0x8007_0000u32 | code) as i32);
        Self {
            reason: "security_info",
            source: Some(Error::from_hresult(hresult)),
        }
    }
    fn tag(reason: &'static str) -> Self {
        Self {
            reason,
            source: None,
        }
    }
    pub fn describe(&self) -> String {
        match &self.source {
            Some(e) => format!("{}:0x{:08X}", self.reason, e.code().0 as u32),
            None => self.reason.to_string(),
        }
    }
}

/// Trust policy for the settings path (applied to the directory and the file):
/// the object must be owned by the current user and its DACL may contain only
/// allow-ACEs for that user, SYSTEM and the built-in Administrators group (the
/// default `%LOCALAPPDATA%` inheritance grants exactly these). Anything else — a foreign
/// owner, a missing/empty DACL, deny or callback ACEs, or any other principal (e.g.
/// Everyone/WD, Users or Authenticated Users) — is *refused*, never silently repaired.
pub(crate) fn verify_restricted(path: &Path, ctx: &UserSecurity) -> Result<(), TrustError> {
    let wide = crate::settings::wide_path(path);
    let mut owner: winsec::PSID = std::ptr::null_mut();
    let mut dacl: *mut winsec::ACL = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let code = unsafe {
        winsec::GetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            winsec::SE_FILE_OBJECT,
            winsec::OWNER_SECURITY_INFORMATION | winsec::DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    // The descriptor buffer is returned on success and on some failure modes;
    // guard frees it either way (LocalFree(null) is filtered by the guard).
    let _sd = LocalAllocGuard(descriptor);
    if code != 0 {
        return Err(TrustError::api(code));
    }
    verify_restricted_parts(owner, dacl, ctx)
}

/// Verify a kernel object handle's owner and DACL before accepting an existing named object.
/// `SECURITY_ATTRIBUTES` is only applied when CreateMutexW creates the object; it is ignored
/// when a caller opens an existing name.
pub(crate) fn verify_restricted_handle(
    handle: HANDLE,
    ctx: &UserSecurity,
) -> Result<(), TrustError> {
    let mut owner: winsec::PSID = std::ptr::null_mut();
    let mut dacl: *mut winsec::ACL = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    let code = unsafe {
        winsec::GetSecurityInfo(
            handle,
            winsec::SE_KERNEL_OBJECT,
            winsec::OWNER_SECURITY_INFORMATION | winsec::DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    let _sd = LocalAllocGuard(descriptor);
    if code != 0 {
        return Err(TrustError::api(code));
    }
    verify_restricted_parts(owner, dacl, ctx)
}

/// Access-mask bits that let a grantee change the file or folder, its contents, its
/// children, or its security: FILE_WRITE_DATA, FILE_APPEND_DATA, FILE_WRITE_EA,
/// FILE_DELETE_CHILD, FILE_WRITE_ATTRIBUTES, DELETE, WRITE_DAC, WRITE_OWNER,
/// GENERIC_WRITE, GENERIC_ALL.
const WRITE_BITS: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x4000_0000 | 0x1000_0000;

fn verify_restricted_parts(
    owner: winsec::PSID,
    dacl: *mut winsec::ACL,
    ctx: &UserSecurity,
) -> Result<(), TrustError> {
    if owner.is_null() || !unsafe { winsec::IsValidSid(owner) }.as_bool() {
        return Err(TrustError::tag("owner_unknown"));
    }
    if !unsafe { winsec::EqualSid(owner, ctx.sid()) }.as_bool() {
        return Err(TrustError::tag("foreign_owner"));
    }
    if dacl.is_null() {
        // NULL DACL = everyone full access; absent DACL pointer is reported the same.
        return Err(TrustError::tag("missing_dacl"));
    }
    let acl = unsafe { &*dacl };
    if acl.AceCount == 0 {
        return Err(TrustError::tag("empty_dacl"));
    }
    for index in 0..u32::from(acl.AceCount) {
        let mut ace: *mut c_void = std::ptr::null_mut();
        if !unsafe { winsec::GetAce(dacl, index, &mut ace) }.as_bool() || ace.is_null() {
            return Err(TrustError::tag("ace_walk"));
        }
        let header = unsafe { &*ace.cast::<winsec::ACE_HEADER>() };
        if header.AceType != winsec::ACCESS_ALLOWED_ACE_TYPE {
            return Err(TrustError::tag("non_allow_ace"));
        }
        // The SID begins inline at ACCESS_ALLOWED_ACE.SidStart (variable length).
        let sid = unsafe { &raw const (*ace.cast::<winsec::ACCESS_ALLOWED_ACE>()).SidStart }
            as winsec::PSID;
        if sid.is_null() || !unsafe { winsec::IsValidSid(sid) }.as_bool() {
            return Err(TrustError::tag("ace_sid_invalid"));
        }
        let allowed = unsafe { winsec::EqualSid(sid, ctx.sid()) }.as_bool()
            || unsafe { winsec::EqualSid(sid, system_sid()) }.as_bool()
            || unsafe { winsec::EqualSid(sid, administrators_sid()) }.as_bool();
        // Anyone else may read (inherited read-only ACEs are common: sandbox groups,
        // backup agents); what must not happen is someone else writing.
        let mask = unsafe { (*ace.cast::<winsec::ACCESS_ALLOWED_ACE>()).Mask };
        if !allowed && mask & WRITE_BITS != 0 {
            return Err(TrustError::tag("broad_dacl"));
        }
    }
    Ok(())
}

/// Create `path` as a directory with the restrictive DACL (CreateDirectoryW + explicit
/// SECURITY_ATTRIBUTES). The parent must already exist — callers create exactly one
/// level under a known-good base.
pub(crate) fn create_dir_restricted(path: &Path, sd: &RestrictiveSecurity) -> Result<(), Error> {
    let wide = crate::settings::wide_path(path);
    let attrs = sd.attributes();
    if !unsafe { winsec::CreateDirectoryW(PCWSTR(wide.as_ptr()), &attrs) }.as_bool() {
        return Err(Error::from_win32());
    }
    Ok(())
}

/// CREATE_NEW file with the restrictive DACL; Err(ERROR_FILE_EXISTS) lets the
/// caller pick another temp name. The returned file owns the handle.
pub(crate) fn create_file_restricted(
    path: &Path,
    sd: &RestrictiveSecurity,
) -> Result<std::fs::File, Error> {
    use std::os::windows::io::FromRawHandle;
    let wide = crate::settings::wide_path(path);
    let attrs = sd.attributes();
    let handle = unsafe {
        winsec::CreateFileW(
            PCWSTR(wide.as_ptr()),
            winsec::GENERIC_WRITE,
            0, // no sharing: the temp file is private until published
            &attrs,
            winsec::CREATE_NEW,
            winsec::FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        )
    };
    if handle.is_invalid() {
        return Err(Error::from_win32());
    }
    // SAFETY: fresh handle from CreateFileW, exclusively owned.
    Ok(unsafe { std::fs::File::from_raw_handle(handle.0) })
}

// ------------------------------------------------------------------ single instance

pub enum InstanceError {
    AlreadyRunning,
    Failed(Error),
}

/// Per-user single-instance object name: `Local\Pulse.Pill.v1.<user-sid>`.
///
/// Settled semantics — per-user-across-sessions *name*: the token SID keeps two
/// different local/domain users on one machine from colliding or from blocking
/// each other (a fixed name in `Local\` is still visible to every user logged
/// into the same session).
///
/// Remaining caveat, stated plainly: `Local\` objects live in the caller's
/// session namespace, so the *same* user logged into two sessions gets one pill
/// per session. Cross-session exclusivity would need `Global\` (separate
/// privilege/attack-surface trade-offs) and is intentionally not claimed.
pub fn instance_mutex_name(user_sid: &str) -> String {
    format!("Local\\Pulse.Pill.v1.{user_sid}")
}

/// Holds the named mutex for the life of the process; closed on drop (declare it first in
/// `run` so it is released last). Created with a DACL granting GENERIC_ALL to the
/// current user and SYSTEM only, so no other principal can squat on the name.
pub struct InstanceLock {
    handle: HANDLE,
}

impl InstanceLock {
    pub fn acquire() -> Result<Self, InstanceError> {
        let identity = UserSecurity::current().map_err(InstanceError::Failed)?;
        let name = instance_mutex_name(identity.sid_string());
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // The descriptor only has to live until CreateMutexW returns.
        let sd = identity
            .restrictive_descriptor()
            .map_err(InstanceError::Failed)?;
        let attrs = sd.attributes();
        let handle = unsafe { winsec::CreateMutexW(&attrs, BOOL(0), PCWSTR(wide.as_ptr())) };
        if handle.0.is_null() {
            return Err(InstanceError::Failed(Error::from_win32()));
        }
        // GetLastError is read immediately, before anything else can overwrite it.
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let lock = Self { handle };
        if exists {
            if let Err(error) = verify_restricted_handle(handle, &identity) {
                let reason = error.describe();
                drop(lock);
                return Err(InstanceError::Failed(Error::new(
                    HRESULT(0x8007_0005u32 as i32),
                    reason,
                )));
            }
            drop(lock); // closes only our duplicate handle; the first instance keeps ownership
            return Err(InstanceError::AlreadyRunning);
        }
        Ok(lock)
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseHandle(self.handle) } {
            crate::diag::win32_error("CloseHandle", &error, "instance_lock");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::VISIBLE_INTERVAL_MS;

    const M1: Bounds = Bounds {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    const NEG: Bounds = Bounds {
        left: -1920,
        top: -200,
        right: 0,
        bottom: 880,
    };

    fn slot(along: u16) -> Slot {
        Slot {
            along,
            ..Slot::DEFAULT
        }
    }

    fn placed(id: &str, bounds: Bounds, along: u16) -> Placed {
        Placed {
            spec: MonitorSpec {
                id: id.into(),
                bounds,
            },
            slot: slot(along),
        }
    }

    #[test]
    fn notch_is_flush_with_the_top_edge_and_centred_by_default() {
        let (w, h) = layout::body_size(96);
        let b = notch_bounds(M1, slot(POSITION_DEFAULT));
        assert_eq!((b.top, b.width(), b.height()), (0, w, h));
        // Centred to within a pixel: an odd width rounds the half-pixel either way.
        assert!(
            (b.left - (1920 - w) / 2).abs() <= 1,
            "left {} for width {w}",
            b.left
        );
    }

    #[test]
    fn notch_on_negative_origin_monitor_stays_inside() {
        let (w, _) = layout::body_size(96);
        let far_left = notch_bounds(NEG, slot(0));
        assert_eq!((far_left.left, far_left.top), (NEG.left, NEG.top));
        let far_right = notch_bounds(NEG, slot(1000));
        assert_eq!(far_right.right, NEG.right);
        assert_eq!(far_right.left, NEG.right - w);
    }

    #[test]
    fn notch_never_leaves_origin_on_tiny_monitor() {
        let tiny = Bounds {
            left: 100,
            top: 50,
            right: 150,
            bottom: 90,
        };
        for along in [0, 500, 1000] {
            let b = notch_bounds(tiny, slot(along));
            assert_eq!((b.left, b.top), (tiny.left, tiny.top));
        }
    }

    #[test]
    fn startup_creates_with_stored_slot() {
        let desired = [placed("A", M1, 800)];
        assert_eq!(
            plan_placements(&[], &desired),
            vec![Placement::Create(placed("A", M1, 800))]
        );
    }

    #[test]
    fn unchanged_placement_plans_nothing() {
        let set = [placed("A", M1, 500)];
        assert!(plan_placements(&set, &set).is_empty());
    }

    #[test]
    fn slot_change_on_same_bounds_moves() {
        let existing = [placed("A", M1, 500)];
        let desired = [placed("A", M1, 900)];
        assert_eq!(
            plan_placements(&existing, &desired),
            vec![Placement::Move(placed("A", M1, 900))]
        );
        let dpi_change = [Placed {
            slot: Slot {
                along: 500,
                dpi: 144,
                ..Slot::DEFAULT
            },
            ..placed("A", M1, 500)
        }];
        assert_eq!(plan_placements(&existing, &dpi_change).len(), 1);
    }

    #[test]
    fn bounds_and_slot_change_yields_single_move() {
        let big = Bounds {
            right: 2560,
            bottom: 1440,
            ..M1
        };
        let existing = [placed("A", M1, 500)];
        let desired = [placed("A", big, 100)];
        assert_eq!(
            plan_placements(&existing, &desired),
            vec![Placement::Move(placed("A", big, 100))]
        );
    }

    #[test]
    fn disabled_or_unplugged_monitor_is_destroyed_and_reconnect_recreates() {
        let existing = [placed("A", M1, 500), placed("B", NEG, 200)];
        let desired = [placed("A", M1, 500)];
        assert_eq!(
            plan_placements(&existing, &desired),
            vec![Placement::Destroy("B".into())]
        );
        assert_eq!(
            plan_placements(&desired, &existing),
            vec![Placement::Create(placed("B", NEG, 200))]
        );
    }

    #[test]
    fn failed_move_is_replanned_until_applied() {
        // Panel keeps its old recorded placement while SetWindowPos fails: same plan again.
        let existing = [placed("A", M1, 500)];
        let desired = [placed("A", M1, 100)];
        assert_eq!(
            plan_placements(&existing, &desired),
            plan_placements(&existing, &desired)
        );
        assert_eq!(plan_placements(&existing, &desired).len(), 1);
        // After success the recorded placement matches and the plan is empty.
        assert!(plan_placements(&desired, &desired).is_empty());
    }

    #[test]
    fn retry_stays_pending_until_success_and_reports_edges() {
        let mut state = Retry::new(true); // created hidden
        assert!(!state.needs(true));
        assert!(state.needs(false));
        assert_eq!(state.record(false, false), RetryReport::Failed);
        assert!(state.applied() && state.needs(false), "still retried");
        assert_eq!(state.record(false, false), RetryReport::StillFailing(2));
        assert_eq!(state.record(false, false), RetryReport::StillFailing(3));
        assert_eq!(state.record(false, true), RetryReport::Recovered(3));
        assert!(!state.applied() && !state.needs(false));
        assert_eq!(state.failures(), 0);
        assert_eq!(state.record(true, true), RetryReport::Applied);
        assert!(state.applied());
    }

    #[test]
    fn retry_target_can_change_between_attempts() {
        let mut state = Retry::new(true);
        state.record(false, false); // show failed
        assert!(
            !state.needs(true),
            "fullscreen app arrived: already hidden, nothing to do"
        );
    }

    #[test]
    fn hidden_policy() {
        assert!(!desired_hidden(true, false));
        assert!(desired_hidden(true, true));
        assert!(desired_hidden(false, false));
        assert!(desired_hidden(false, true));
    }

    #[test]
    fn cadence_uses_setting_only_while_a_panel_is_visible() {
        assert_eq!(interval_ms(2, 2, 1), VISIBLE_INTERVAL_MS);
        assert_eq!(interval_ms(5, 2, 1), 5_000);
        assert_eq!(interval_ms(10, 1, 0), 10_000);
        assert_eq!(interval_ms(5, 2, 2), HIDDEN_INTERVAL_MS);
        assert_eq!(interval_ms(5, 0, 0), HIDDEN_INTERVAL_MS);
        assert_eq!(interval_ms(0, 1, 0), 2_000, "clamped up");
        assert_eq!(interval_ms(99, 1, 0), 10_000, "clamped down");
    }

    #[test]
    fn mutex_name_is_sid_scoped() {
        assert_eq!(
            instance_mutex_name("S-1-5-21-1-2-3-1001"),
            "Local\\Pulse.Pill.v1.S-1-5-21-1-2-3-1001"
        );
        assert_ne!(
            instance_mutex_name("S-1-5-21-1-2-3-1001"),
            instance_mutex_name("S-1-5-21-1-2-3-1002")
        );
    }

    #[test]
    fn restrictive_sddl_grants_only_user_and_system() {
        assert_eq!(
            restrictive_sddl("S-1-5-21-9-9-9-500"),
            "O:S-1-5-21-9-9-9-500D:P(A;;GA;;;S-1-5-21-9-9-9-500)(A;;GA;;;SY)"
        );
    }

    // The tests below exercise the real advapi32/kernel32 security APIs; they run in
    // hosted Windows CI and need no elevation (fixtures live under %TEMP%).

    #[test]
    fn live_current_user_security_yields_user_sid() {
        let ctx = UserSecurity::current().expect("process token user SID");
        assert!(
            ctx.sid_string().starts_with("S-1-5-"),
            "unexpected SID form: {}",
            ctx.sid_string()
        );
        assert!(instance_mutex_name(ctx.sid_string()).contains(ctx.sid_string()));
    }

    #[test]
    fn live_second_acquire_reports_already_running() {
        match InstanceLock::acquire() {
            Ok(_first) => assert!(matches!(
                InstanceLock::acquire(),
                Err(InstanceError::AlreadyRunning)
            )),
            // An ambient pill already holding the mutex also proves dedupe.
            Err(InstanceError::AlreadyRunning) => {}
            Err(InstanceError::Failed(e)) => panic!("acquire failed: {e}"),
        }
    }

    #[test]
    fn live_restricted_dir_passes_and_world_dacl_is_refused() {
        let ctx = UserSecurity::current().unwrap();
        let base = std::env::temp_dir().join(format!("pulse-sec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        let ok_dir = base.join("ok");
        let bad_dir = base.join("bad");
        // Our own restrictive descriptor: owned by us, user+SYSTEM DACL → trusted.
        let sd = ctx.restrictive_descriptor().unwrap();
        create_dir_restricted(&ok_dir, &sd).unwrap();
        verify_restricted(&ok_dir, &ctx).expect("restricted dir must pass");
        // Same owner, but the DACL grants Everyone (WD): must be refused.
        let world = RestrictiveSecurity::from_sddl("D:P(A;;GA;;;WD)").unwrap();
        create_dir_restricted(&bad_dir, &world).unwrap();
        let err = verify_restricted(&bad_dir, &ctx).expect_err("world DACL must fail");
        assert_eq!(err.reason, "broad_dacl");
        let _ = std::fs::remove_dir_all(&base);
    }
}
