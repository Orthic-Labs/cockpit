//! macOS volume identity: volume UUID via getattrlist(ATTR_VOL_UUID), with an
//! explicit non-stable statfs fsid / st_dev fallback.

use crate::model::VolumeIdentity;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::{c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_VOL_INFO: u32 = 0x8000_0000;
const ATTR_VOL_UUID: u32 = 0x0004_0000;
const FSOPT_NOFOLLOW: u32 = 0x0000_0001;

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C, align(4))]
struct Buffer([u8; 64]);

unsafe extern "C" {
    fn getattrlist(
        path: *const c_char,
        attr_list: *mut AttrList,
        attr_buf: *mut c_void,
        attr_buf_size: usize,
        options: u32,
    ) -> c_int;
}

fn volume_uuid(path: &Path) -> Result<[u8; 16], String> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "path contains NUL; volume uuid unavailable".to_owned())?;
    let mut list = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: ATTR_VOL_INFO | ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = Buffer([0; 64]);
    // Metadata-only syscall; NOFOLLOW so a link reports its own volume.
    let status = unsafe {
        getattrlist(
            c_path.as_ptr(),
            &mut list,
            buffer.0.as_mut_ptr().cast(),
            buffer.0.len(),
            FSOPT_NOFOLLOW,
        )
    };
    if status != 0 {
        return Err(format!(
            "getattrlist(ATTR_VOL_UUID) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let length = u32::from_ne_bytes(buffer.0[0..4].try_into().expect("4 bytes")) as usize;
    if length < 20 {
        return Err("getattrlist returned no volume uuid".into());
    }
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&buffer.0[4..20]);
    if uuid == [0u8; 16] {
        return Err("volume uuid is all zero".into());
    }
    Ok(uuid)
}

fn mount_statistics(path: &Path, metadata: &fs::Metadata) -> Option<libc::statfs> {
    // Query the parent for a symlink so statfs never follows its target.
    let query = if metadata.file_type().is_symlink() {
        path.parent()?
    } else {
        path
    };
    let c_path = CString::new(query.as_os_str().as_bytes()).ok()?;
    let mut statistics = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(c_path.as_ptr(), statistics.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { statistics.assume_init() })
}

pub(super) fn volume_for(
    path: &Path,
    metadata: &fs::Metadata,
) -> (VolumeIdentity, bool, Option<String>) {
    // getattrlist volume attributes require a filesystem root, not an entry path.
    // statfs supplies that mount root; match st_dev to reject a raced mount/path.
    let statistics = mount_statistics(path, metadata);
    let uuid_result = statistics
        .as_ref()
        .ok_or_else(|| "mount root unavailable".to_owned())
        .and_then(|statistics| {
            let mount = unsafe { CStr::from_ptr(statistics.f_mntonname.as_ptr()) };
            let mount = Path::new(std::ffi::OsStr::from_bytes(mount.to_bytes()));
            let root_metadata = fs::symlink_metadata(mount).map_err(|e| e.to_string())?;
            if root_metadata.dev() != metadata.dev() || root_metadata.file_type().is_symlink() {
                return Err("mount identity changed during lookup".into());
            }
            volume_uuid(mount)
        });
    let uuid_error = match uuid_result {
        Ok(uuid) => {
            return (
                VolumeIdentity::new(format!("uuid:{}", super::format_uuid(&uuid))),
                true,
                None,
            );
        }
        Err(error) => error,
    };
    if let Some(statistics) = statistics {
        let [a, b] = statistics.f_fsid.val;
        return (
            VolumeIdentity::new(format!("fsid-unstable:{a:x}:{b:x}")),
            false,
            Some(format!(
                "volume identity not stable (statfs fsid fallback; {uuid_error})"
            )),
        );
    }
    (
        VolumeIdentity::new(format!("dev-unstable:{}", metadata.dev())),
        false,
        Some(format!(
            "volume identity not stable (st_dev fallback; {uuid_error})"
        )),
    )
}
