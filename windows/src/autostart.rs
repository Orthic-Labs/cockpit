//! Launch at login: one value under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Per-user, no elevation. Registry symbols are declared here against `advapi32.dll` so no
//! extra `windows` crate features are needed. The value is only rewritten when it differs.

use crate::diag;
use windows::core::PCWSTR;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "Pulse";
/// `HKEY_CURRENT_USER` (0x80000001 sign-extended to pointer width).
const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

#[allow(non_snake_case)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        key: isize,
        sub_key: PCWSTR,
        options: u32,
        desired: u32,
        result: *mut isize,
    ) -> i32;
    fn RegSetValueExW(
        key: isize,
        name: PCWSTR,
        reserved: u32,
        kind: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: PCWSTR,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: isize, name: PCWSTR) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

struct Key(isize);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful RegOpenKeyExW and is closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key() -> Result<Key, i32> {
    let sub_key = wide(RUN_KEY);
    let mut handle = 0isize;
    // SAFETY: NUL-terminated key path and a valid out pointer, both alive for the call.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub_key.as_ptr()),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut handle,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(Key(handle))
    } else {
        Err(status)
    }
}

fn current_value(key: &Key) -> Option<String> {
    let name = wide(VALUE_NAME);
    let mut kind = 0u32;
    let mut len = 0u32;
    // SAFETY: size probe (null data) then a read into a buffer of the reported size.
    unsafe {
        if RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            std::ptr::null_mut(),
            &mut kind,
            std::ptr::null_mut(),
            &mut len,
        ) != ERROR_SUCCESS
            || kind != REG_SZ
            || len == 0
            || len > 32 * 1024
        {
            return None;
        }
        let mut buffer = vec![0u16; (len as usize).div_ceil(2)];
        let mut capacity = (buffer.len() * 2) as u32;
        if RegQueryValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            std::ptr::null_mut(),
            &mut kind,
            buffer.as_mut_ptr().cast(),
            &mut capacity,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let units = (capacity as usize / 2).min(buffer.len());
        let text = &buffer[..units];
        let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(&text[..end]))
    }
}

/// Makes the Run entry match `enabled` (quoted path of this executable). Failures are
/// logged and never fatal: the notch runs the same either way.
pub fn apply(enabled: bool) {
    let key = match open_run_key() {
        Ok(key) => key,
        Err(code) => {
            diag::info(
                "autostart_unavailable",
                &[("status", format!("{code}").as_str())],
            );
            return;
        }
    };
    let name = wide(VALUE_NAME);
    if !enabled {
        // SAFETY: valid key handle and NUL-terminated value name.
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            diag::info("autostart_remove_failed", &[("status", format!("{status}").as_str())]);
        }
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        diag::info("autostart_unavailable", &[("reason", "current_exe")]);
        return;
    };
    let desired = format!("\"{}\"", exe.display());
    if current_value(&key).as_deref() == Some(desired.as_str()) {
        return;
    }
    let data = wide(&desired);
    // SAFETY: `data` is a NUL-terminated UTF-16 buffer; the length passed is its byte size.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(name.as_ptr()),
            0,
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if status == ERROR_SUCCESS {
        diag::info("autostart_enabled", &[]);
    } else {
        diag::info("autostart_set_failed", &[("status", format!("{status}").as_str())]);
    }
}
