//! Bulk directory reads for macOS (`getattrlistbulk`).
//!
//! Adapted from Petal's `src/dirlist.rs` (MIT, Copyright (c) 2026 Henry
//! Dennis; see `docs/donors.md`). Petal returns names and attributes for many
//! entries per syscall instead of one `lstat` per entry. This port keeps only
//! what the scanner needs for regular files, bounds-checks every field read
//! from the kernel buffer, and returns anything it cannot describe completely
//! without metadata so the caller inspects that entry the usual way.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io;
use std::os::raw::{c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Facts about one regular file, read in the same call as its name.
#[derive(Clone, Debug)]
pub struct BulkFile {
    /// st_dev of the file.
    pub dev: u64,
    /// st_ino of the file.
    pub ino: u64,
    /// Logical size (st_size).
    pub logical: u64,
    /// Allocated bytes (st_blocks * 512).
    pub allocation: u64,
    /// Creation and modification times, Unix seconds (both non-negative).
    pub created: u64,
    pub modified: u64,
}

#[repr(C)]
#[allow(dead_code)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

/// `attribute_set_t`: which attributes a returned record carries.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct AttributeSet {
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        dirfd: c_int,
        attr_list: *mut AttrList,
        buffer: *mut c_void,
        buffer_size: usize,
        options: u64,
    ) -> c_int;
}

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FLAGS: u32 = 0x0010_0000;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const VREG: u32 = 1;
const SF_DATALESS: u32 = 0x4000_0000;
const BUFFER_SIZE: usize = 32 * 1024;

thread_local! {
    static BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0; BUFFER_SIZE]);
}

/// Bounds-checked reader over one record. Every read fails, rather than
/// running past the record's end.
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let end = self.at.checked_add(N)?;
        let bytes: [u8; N] = self.buf.get(self.at..end)?.try_into().ok()?;
        self.at = end;
        Some(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take::<4>().map(u32::from_ne_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.take::<4>().map(i32::from_ne_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take::<8>().map(i64::from_ne_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take::<8>().map(u64::from_ne_bytes)
    }
}

/// The name and, when complete, the regular-file facts of one record.
/// `None` for a record that carries no usable name (the kernel reported an
/// error for that entry); such an entry is left out, as Petal does.
fn parse(record: &[u8]) -> Option<(Vec<u8>, Option<BulkFile>)> {
    // The record's own length word was consumed by the caller.
    let mut cursor = Cursor { buf: record, at: 4 };
    let returned = AttributeSet {
        commonattr: cursor.u32()?,
        volattr: cursor.u32()?,
        dirattr: cursor.u32()?,
        fileattr: cursor.u32()?,
        forkattr: cursor.u32()?,
    };
    if returned.commonattr & ATTR_CMN_ERROR != 0 && cursor.u32()? != 0 {
        return None;
    }
    if returned.commonattr & ATTR_CMN_NAME == 0 {
        return None;
    }
    // The name is an attrreference: an offset from the reference itself, and
    // a length that includes the trailing NUL.
    let reference_at = cursor.at;
    let data_offset = cursor.i32()?;
    let length = cursor.u32()? as usize;
    let start = usize::try_from(reference_at as i64 + i64::from(data_offset)).ok()?;
    let name = record
        .get(start..start.checked_add(length.checked_sub(1)?)?)?
        .to_vec();

    let common = ATTR_CMN_DEVID
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_CRTIME
        | ATTR_CMN_MODTIME
        | ATTR_CMN_FLAGS
        | ATTR_CMN_FILEID;
    let file = if returned.commonattr & common == common
        && returned.fileattr & (ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH)
            == (ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH)
    {
        regular_file(&mut cursor)
    } else {
        None
    };
    Some((name, file))
}

/// Reads the fields after the name, in attribute bit order. Any shortfall or
/// non-regular entry yields `None`, and the caller inspects the entry instead.
fn regular_file(cursor: &mut Cursor<'_>) -> Option<BulkFile> {
    let dev = i64::from(cursor.i32()?) as u64;
    let objtype = cursor.u32()?;
    let created = cursor.i64()?; // timespec: seconds, then nanoseconds
    let _created_ns = cursor.i64()?;
    let modified = cursor.i64()?;
    let _modified_ns = cursor.i64()?;
    let flags = cursor.u32()?;
    let ino = cursor.u64()?;
    let allocation = cursor.i64()?;
    let logical = cursor.i64()?;
    if objtype != VREG || flags & SF_DATALESS != 0 {
        return None;
    }
    if created < 0 || modified < 0 || allocation < 0 || logical < 0 {
        return None;
    }
    Some(BulkFile {
        dev,
        ino,
        logical: logical as u64,
        allocation: allocation as u64,
        created: created as u64,
        modified: modified as u64,
    })
}

/// Reads up to `limit` entries of the directory open as `fd`. Returns the
/// children (joined to `parent`) with their file facts where complete, and
/// whether more entries were left unread. `.` and `..` are never returned.
pub(super) fn read_entries(
    fd: c_int,
    parent: &Path,
    limit: usize,
) -> io::Result<(Vec<(PathBuf, Option<BulkFile>)>, bool)> {
    let mut attrs = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | ATTR_CMN_NAME
            | ATTR_CMN_DEVID
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_CRTIME
            | ATTR_CMN_MODTIME
            | ATTR_CMN_FLAGS
            | ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH,
        forkattr: 0,
    };
    let mut children = Vec::new();
    let mut truncated = false;
    BUFFER.with_borrow_mut(|buf| -> io::Result<()> {
        loop {
            let count =
                unsafe { getattrlistbulk(fd, &mut attrs, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                return Ok(());
            }
            let mut offset = 0usize;
            for _ in 0..count {
                let malformed = || io::Error::other("malformed bulk directory record");
                let length_bytes: [u8; 4] = buf
                    .get(offset..offset + 4)
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(malformed)?;
                let length = u32::from_ne_bytes(length_bytes) as usize;
                let end = offset
                    .checked_add(length)
                    .filter(|&end| length >= 8 && end <= buf.len())
                    .ok_or_else(malformed)?;
                let record = &buf[offset..end];
                offset = end;
                let Some((name, file)) = parse(record) else {
                    continue;
                };
                if name == b"." || name == b".." {
                    continue;
                }
                if children.len() >= limit {
                    truncated = true;
                    return Ok(());
                }
                children.push((parent.join(OsStr::from_bytes(&name)), file));
            }
        }
    })?;
    Ok((children, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::time::UNIX_EPOCH;

    /// Bulk facts must agree with lstat for every regular file they describe.
    #[test]
    fn bulk_file_facts_match_lstat() {
        let dir = Path::new("/usr/bin");
        let (listed, truncated) = crate::platform::bulk_children_bounded(dir, 100_000).unwrap();
        assert!(!truncated);
        let (plain, _) = crate::platform::children_bounded(dir, 100_000).unwrap();
        let mut bulk_names: Vec<_> = listed.iter().map(|(p, _)| p.clone()).collect();
        bulk_names.sort();
        assert_eq!(
            bulk_names, plain,
            "bulk and plain listings must name the same entries"
        );
        let mut checked = 0;
        for (child, file) in listed {
            let Some(file) = file else { continue };
            let meta = fs::symlink_metadata(&child).unwrap();
            assert!(
                meta.file_type().is_file(),
                "{child:?} must be a regular file"
            );
            assert_eq!(file.dev, meta.dev(), "{child:?} dev");
            assert_eq!(file.ino, meta.ino(), "{child:?} ino");
            assert_eq!(file.logical, meta.len(), "{child:?} logical");
            assert_eq!(file.allocation, meta.blocks() * 512, "{child:?} allocation");
            assert_eq!(file.modified, meta.mtime() as u64, "{child:?} modified");
            let created = meta
                .created()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            assert_eq!(file.created, created, "{child:?} created");
            checked += 1;
        }
        assert!(checked > 0, "expected some regular files in {dir:?}");
    }
}
