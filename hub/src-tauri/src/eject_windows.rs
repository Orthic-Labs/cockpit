//! Mounted disk images and removable drives on Windows: finding them and ejecting them.
//! The Mac side lists mounted installers (`diskutil` bus protocol "Disk Image") and
//! detaches them with `hdiutil`; this is the Windows counterpart, with no administrator
//! needed to detect and none to eject what the user mounted.
//!
//! Detection: `\\.\X:` opened with no access rights (`dwDesiredAccess = 0`), then
//! `IOCTL_STORAGE_QUERY_PROPERTY` / `StorageDeviceProperty`, whose `STORAGE_DEVICE_DESCRIPTOR`
//! carries `BusType` and `RemovableMedia`.
//!  - `BusTypeFileBackedVirtual` (15) is how the virtual-disk driver (vhdmp) shows a mounted
//!    VHD/VHDX/ISO. A mounted ISO is a CD-ROM class device with vendor "Msft" and product
//!    "Virtual ..."; that pair is accepted too, for CD-ROM class devices only.
//!  - USB, IEEE 1394, SD and MMC buses, or `RemovableMedia`, mean a removable drive (a USB
//!    stick or hard disk, an SD card, an optical drive). Anything else, and any drive whose
//!    query fails, is "fixed" and is never ejected.
//!
//! Ejecting a disk image or removable media (no administrator is needed when Windows grants
//! the volume handle, as it does for the media a user mounted): open the volume,
//! `FSCTL_LOCK_VOLUME` (fails while files are open, so nothing is yanked from under a
//! program), `FSCTL_DISMOUNT_VOLUME`, `IOCTL_STORAGE_MEDIA_REMOVAL` (allow), then
//! `IOCTL_STORAGE_EJECT_MEDIA`. That is the sequence in Microsoft's "How to Ejects Removable
//! Media in Windows" (KB165721) and what Explorer's Eject does for an ISO.
//!
//! Ejecting a removable drive (USB, SD): the "Safely Remove Hardware" path. Find the disk
//! behind the letter (`IOCTL_STORAGE_GET_DEVICE_NUMBER` matched against the
//! `GUID_DEVINTERFACE_DISK` interfaces from SetupAPI), take its parent device (the USBSTOR
//! or card-reader device) with `CM_Get_Parent` and ask Plug and Play to eject it with
//! `CM_Request_Device_EjectW`, which refuses (a veto) while files are open.
//! If no ejectable device is found it falls back to the media sequence above.
//!
//! The system drive and fixed internal disks are refused.

use std::ffi::c_void;
use std::time::Duration;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ_WRITE: u32 = 0x0000_0003;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE_VALUE: isize = -1;

const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002D_1080;
const IOCTL_STORAGE_MEDIA_REMOVAL: u32 = 0x002D_4804;
const IOCTL_STORAGE_EJECT_MEDIA: u32 = 0x002D_4808;
const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
const FSCTL_UNLOCK_VOLUME: u32 = 0x0009_001C;
const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;

const FILE_DEVICE_DISK: u32 = 7;
const DEVICE_TYPE_CD_ROM: u8 = 5; // STORAGE_DEVICE_DESCRIPTOR.DeviceType (SCSI peripheral type)
const BUS_1394: u32 = 4;
const BUS_USB: u32 = 7;
const BUS_SD: u32 = 12;
const BUS_MMC: u32 = 13;
const BUS_FILE_BACKED_VIRTUAL: u32 = 15;

const DIGCF_PRESENT: u32 = 0x0000_0002;
const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
const CR_SUCCESS: u32 = 0;
const CR_REMOVE_VETOED: u32 = 0x17;

const ERROR_ACCESS_DENIED: i32 = 5;
const LOCK_ATTEMPTS: u32 = 5;
const LOCK_WAIT: Duration = Duration::from_millis(300);
const EJECT_ATTEMPTS: u32 = 3;
const EJECT_WAIT: Duration = Duration::from_millis(400);

/// The fields only exist for the layout the Windows API reads and writes.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// `GUID_DEVINTERFACE_DISK` {53f56307-b6bf-11d0-94f2-00a0c91efb8b}.
const GUID_DEVINTERFACE_DISK: Guid =
    Guid { data1: 0x53f5_6307, data2: 0xb6bf, data3: 0x11d0, data4: [0x94, 0xf2, 0x00, 0xa0, 0xc9, 0x1e, 0xfb, 0x8b] };

/// `SP_DEVICE_INTERFACE_DATA`.
#[repr(C)]
#[allow(dead_code)]
struct InterfaceData {
    size: u32,
    class: Guid,
    flags: u32,
    reserved: usize,
}

/// `SP_DEVINFO_DATA`.
#[repr(C)]
#[allow(dead_code)]
struct DevInfoData {
    size: u32,
    class: Guid,
    devinst: u32,
    reserved: usize,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: *mut c_void,
    ) -> *mut c_void;
    fn DeviceIoControl(
        handle: *mut c_void,
        code: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiGetClassDevsW(class: *const Guid, enumerator: *const u16, parent: *mut c_void, flags: u32) -> *mut c_void;
    fn SetupDiEnumDeviceInterfaces(
        set: *mut c_void,
        info: *const DevInfoData,
        class: *const Guid,
        index: u32,
        data: *mut InterfaceData,
    ) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(
        set: *mut c_void,
        data: *const InterfaceData,
        detail: *mut u32,
        detail_size: u32,
        required: *mut u32,
        info: *mut DevInfoData,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(set: *mut c_void) -> i32;
}

#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Get_Parent(parent: *mut u32, device: u32, flags: u32) -> u32;
    fn CM_Request_Device_EjectW(device: u32, veto_type: *mut u32, veto_name: *mut u16, name_len: u32, flags: u32) -> u32;
}

/// What a lettered drive is, as far as ejecting goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DriveKind {
    /// A mounted ISO, VHD or VHDX.
    DiskImage,
    /// A USB stick or disk, SD card, optical drive or other removable media.
    Removable,
    /// Anything else, including a drive that could not be queried. Never ejected.
    Fixed,
}

/// An open Win32 handle, closed on drop.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed exactly once, here.
        unsafe { CloseHandle(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(name: &str, access: u32) -> std::io::Result<Handle> {
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated and outlives the call; no security attributes or template.
    let handle = unsafe {
        CreateFileW(name.as_ptr(), access, FILE_SHARE_READ_WRITE, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null_mut())
    };
    if handle.is_null() || handle as isize == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(Handle(handle))
    }
}

/// One synchronous `DeviceIoControl`; returns how many bytes of `output` were filled.
fn ioctl(handle: &Handle, code: u32, input: &[u8], output: &mut [u8]) -> std::io::Result<usize> {
    let mut returned = 0u32;
    let input_ptr = if input.is_empty() { std::ptr::null() } else { input.as_ptr().cast() };
    let output_ptr = if output.is_empty() { std::ptr::null_mut() } else { output.as_mut_ptr().cast() };
    // SAFETY: the pointers are null or point at live slices of the sizes passed, `returned` is a
    // live u32 and the handle is open; no overlapped I/O.
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            code,
            input_ptr,
            input.len() as u32,
            output_ptr,
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 { Ok((returned as usize).min(output.len())) } else { Err(std::io::Error::last_os_error()) }
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The upper-case drive letter of a mount point like `E:\` or `E:`.
fn letter_of(mount: &str) -> Option<char> {
    let mut chars = mount.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then(|| letter.to_ascii_uppercase())
}

/// The fields of a `STORAGE_DEVICE_DESCRIPTOR` that classify a drive.
struct Descriptor {
    device_type: u8,
    removable_media: bool,
    bus: u32,
    vendor: String,
    product: String,
}

/// `StorageDeviceProperty` through the volume handle. Layout: Version u32, Size u32,
/// DeviceType u8 (8), DeviceTypeModifier u8, RemovableMedia u8 (10), CommandQueueing u8,
/// Vendor/Product/Revision/Serial offsets u32 (12, 16, 20, 24), BusType u32 (28).
fn descriptor(handle: &Handle) -> Option<Descriptor> {
    let query = [0u8; 12]; // STORAGE_PROPERTY_QUERY: PropertyId 0, PropertyStandardQuery
    let mut output = [0u8; 1024];
    let filled = ioctl(handle, IOCTL_STORAGE_QUERY_PROPERTY, &query, &mut output).ok()?;
    if filled < 32 {
        return None;
    }
    let text = |offset: usize| -> String {
        if offset == 0 || offset >= filled {
            return String::new();
        }
        let end = output[offset..filled].iter().position(|b| *b == 0).map_or(filled, |n| offset + n);
        String::from_utf8_lossy(&output[offset..end]).trim().to_string()
    };
    Some(Descriptor {
        device_type: output[8],
        removable_media: output[10] != 0,
        bus: le_u32(&output, 28)?,
        vendor: text(le_u32(&output, 12)? as usize),
        product: text(le_u32(&output, 16)? as usize),
    })
}

impl Descriptor {
    fn kind(&self) -> DriveKind {
        let virtual_optical = self.device_type == DEVICE_TYPE_CD_ROM
            && self.vendor.eq_ignore_ascii_case("msft")
            && self.product.to_ascii_lowercase().starts_with("virtual");
        if self.bus == BUS_FILE_BACKED_VIRTUAL || virtual_optical {
            DriveKind::DiskImage
        } else if matches!(self.bus, BUS_1394 | BUS_USB | BUS_SD | BUS_MMC) || self.removable_media {
            DriveKind::Removable
        } else {
            DriveKind::Fixed
        }
    }
}

/// What the drive at `mount` (like `E:\`) is. A read-only query on a handle opened with no
/// access rights; any failure means `Fixed`, which is never ejected.
pub(crate) fn classify(mount: &str) -> DriveKind {
    let Some(letter) = letter_of(mount) else { return DriveKind::Fixed };
    let Ok(handle) = open(&format!(r"\\.\{letter}:"), 0) else { return DriveKind::Fixed };
    descriptor(&handle).map_or(DriveKind::Fixed, |d| d.kind())
}

/// A mounted ISO, VHD or VHDX, not a drive.
pub(crate) fn is_disk_image(mount: &str) -> bool {
    classify(mount) == DriveKind::DiskImage
}

fn in_use(drive: &str) -> String {
    format!("{drive} is in use; close the files on it and try again.")
}

fn is_system_drive(drive: &str) -> bool {
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    system.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(drive)
}

/// Lock, dismount and eject the media in a drive. Fails with "in use" while files are open.
fn eject_media(drive: &str) -> Result<(), String> {
    let name = format!(r"\\.\{drive}");
    // Write access is needed to lock most media; a CD-ROM class device (a mounted ISO) can be
    // locked with read access alone.
    let (handle, writable) = match open(&name, GENERIC_READ | GENERIC_WRITE) {
        Ok(handle) => (handle, true),
        Err(_) => match open(&name, GENERIC_READ) {
            Ok(handle) => (handle, false),
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                return Err(format!("Windows needs administrator rights to eject {drive}; eject it from File Explorer."));
            }
            Err(e) if matches!(e.raw_os_error(), Some(2 | 3 | 21)) => {
                return Err(format!("{drive} is no longer there."));
            }
            Err(e) => return Err(format!("Could not open {drive}: {e}")),
        },
    };
    let mut locked = Err(std::io::Error::other("not tried"));
    for attempt in 0..LOCK_ATTEMPTS {
        locked = ioctl(&handle, FSCTL_LOCK_VOLUME, &[], &mut []);
        if locked.is_ok() {
            break;
        }
        if attempt + 1 < LOCK_ATTEMPTS {
            std::thread::sleep(LOCK_WAIT);
        }
    }
    if let Err(e) = locked {
        // Open files are the usual reason. A read-only handle that Windows refuses to lock
        // could also be a permission problem, so say both rather than guess.
        return Err(if !writable && e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
            format!("{drive} is in use, or Windows needs administrator rights to eject it; close the files on it and try again, or eject it from File Explorer.")
        } else {
            in_use(drive)
        });
    }
    let unlock = |handle: &Handle| {
        let _ = ioctl(handle, FSCTL_UNLOCK_VOLUME, &[], &mut []);
    };
    if let Err(e) = ioctl(&handle, FSCTL_DISMOUNT_VOLUME, &[], &mut []) {
        unlock(&handle);
        return Err(format!("Could not dismount {drive}: {e}"));
    }
    // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }; best effort, a drive without a lock ignores it.
    let _ = ioctl(&handle, IOCTL_STORAGE_MEDIA_REMOVAL, &[0u8], &mut []);
    if let Err(e) = ioctl(&handle, IOCTL_STORAGE_EJECT_MEDIA, &[], &mut []) {
        unlock(&handle);
        return Err(format!("Could not eject {drive}: {e}"));
    }
    Ok(())
}

/// `STORAGE_DEVICE_NUMBER` { DeviceType, DeviceNumber, PartitionNumber } of an open handle.
fn device_number(handle: &Handle) -> Option<(u32, u32)> {
    let mut output = [0u8; 12];
    let filled = ioctl(handle, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut output).ok()?;
    (filled >= 12).then_some(())?;
    Some((le_u32(&output, 0)?, le_u32(&output, 4)?))
}

/// The device instance of the disk numbered `number`, from the present disk interfaces.
fn disk_devinst(number: u32) -> Option<u32> {
    // SAFETY: the GUID is a live constant; no enumerator, parent window or extra flags.
    let set = unsafe {
        SetupDiGetClassDevsW(&GUID_DEVINTERFACE_DISK, std::ptr::null(), std::ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
    };
    if set.is_null() || set as isize == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut found = None;
    for index in 0u32.. {
        let mut data = InterfaceData {
            size: std::mem::size_of::<InterfaceData>() as u32,
            class: GUID_DEVINTERFACE_DISK,
            flags: 0,
            reserved: 0,
        };
        // SAFETY: `data` is a live, correctly sized SP_DEVICE_INTERFACE_DATA; `set` is open.
        if unsafe { SetupDiEnumDeviceInterfaces(set, std::ptr::null(), &GUID_DEVINTERFACE_DISK, index, &mut data) } == 0 {
            break; // ERROR_NO_MORE_ITEMS, or nothing readable
        }
        let mut required = 0u32;
        // SAFETY: the size query: a null detail buffer of size 0 reports the size needed.
        unsafe { SetupDiGetDeviceInterfaceDetailW(set, &data, std::ptr::null_mut(), 0, &mut required, std::ptr::null_mut()) };
        if required < 8 {
            continue;
        }
        // SP_DEVICE_INTERFACE_DETAIL_DATA_W: a u32 cbSize (8 on 64-bit, 6 on 32-bit) and then
        // the NUL-terminated wide DevicePath at byte 4. A u32 buffer keeps it aligned.
        let mut detail = vec![0u32; (required as usize).div_ceil(4) + 1];
        detail[0] = if cfg!(target_pointer_width = "64") { 8 } else { 6 };
        let mut info = DevInfoData {
            size: std::mem::size_of::<DevInfoData>() as u32,
            class: GUID_DEVINTERFACE_DISK,
            devinst: 0,
            reserved: 0,
        };
        // SAFETY: `detail` holds at least `required` bytes with cbSize set; `info` is sized.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(set, &data, detail.as_mut_ptr(), required, &mut required, &mut info)
        };
        if ok == 0 {
            continue;
        }
        // The path starts at byte 4: the second u32 of the little-endian buffer, as u16 words.
        let words = detail.iter().skip(1).flat_map(|w| [(*w & 0xFFFF) as u16, (*w >> 16) as u16]);
        let path = String::from_utf16_lossy(&words.take_while(|w| *w != 0).collect::<Vec<u16>>());
        let Ok(disk) = open(&path, 0) else { continue };
        if device_number(&disk) == Some((FILE_DEVICE_DISK, number)) {
            found = Some(info.devinst);
            break;
        }
    }
    // SAFETY: `set` came from SetupDiGetClassDevsW and is destroyed once.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    found
}

enum Ejected {
    Done,
    Vetoed,
    /// Plug and Play could not be used for this drive (no disk found, or it refused for another reason).
    Unavailable,
}

/// "Safely Remove Hardware" for the disk behind `letter`.
fn request_eject(letter: char) -> Ejected {
    let Ok(volume) = open(&format!(r"\\.\{letter}:"), 0) else { return Ejected::Unavailable };
    let Some((FILE_DEVICE_DISK, number)) = device_number(&volume) else { return Ejected::Unavailable };
    drop(volume);
    let Some(disk) = disk_devinst(number) else { return Ejected::Unavailable };
    let mut parent = 0u32;
    // SAFETY: `parent` is a live u32.
    let target = if unsafe { CM_Get_Parent(&mut parent, disk, 0) } == CR_SUCCESS { parent } else { disk };
    let mut result = Ejected::Unavailable;
    for attempt in 0..EJECT_ATTEMPTS {
        let mut veto_type = 0u32;
        let mut veto_name = [0u16; 260];
        // SAFETY: `veto_type` is a live u32 and `veto_name` a writable buffer of the length passed.
        let code = unsafe { CM_Request_Device_EjectW(target, &mut veto_type, veto_name.as_mut_ptr(), veto_name.len() as u32, 0) };
        result = match code {
            CR_SUCCESS => return Ejected::Done,
            CR_REMOVE_VETOED => Ejected::Vetoed,
            _ => return Ejected::Unavailable,
        };
        if attempt + 1 < EJECT_ATTEMPTS {
            // Handles a program has just closed can take a moment to go away.
            std::thread::sleep(EJECT_WAIT);
        }
    }
    result
}

/// Eject the mounted disk image or removable drive at `mount`. Refuses the system drive and
/// fixed internal disks. Blocking: call it off the UI thread.
pub(crate) fn eject(mount: &str) -> Result<(), String> {
    let letter = letter_of(mount).ok_or_else(|| "That is not a drive letter.".to_string())?;
    let drive = format!("{letter}:");
    if is_system_drive(&drive) {
        return Err("Windows can't eject the system drive.".into());
    }
    match classify(mount) {
        DriveKind::DiskImage => eject_media(&drive),
        DriveKind::Removable => match request_eject(letter) {
            Ejected::Done => Ok(()),
            Ejected::Vetoed => Err(in_use(&drive)),
            Ejected::Unavailable => eject_media(&drive),
        },
        DriveKind::Fixed => Err(format!("{drive} is a fixed internal disk and can't be ejected.")),
    }
}
