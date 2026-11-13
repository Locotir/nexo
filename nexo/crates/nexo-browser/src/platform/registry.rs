//! Windows: where the registry says Chrome, Edge and Brave are installed (their `App Paths` keys).

use std::path::PathBuf;

/// The exe names whose `App Paths` key is read.
const EXES: [&str; 3] = ["chrome.exe", "msedge.exe", "brave.exe"];

/// The default value of each `App Paths` key that exists, as (exe file name, full path). Empty off Windows.
#[cfg(not(windows))]
pub fn app_paths() -> Vec<(String, PathBuf)> {
    let _ = EXES;
    Vec::new()
}

#[cfg(windows)]
pub fn app_paths() -> Vec<(String, PathBuf)> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    // Machine-wide installs first (64-bit view, then the 32-bit one), then per-user installs.
    let views = [(HKEY_LOCAL_MACHINE, WOW_NONE), (HKEY_LOCAL_MACHINE, WOW_32), (HKEY_CURRENT_USER, WOW_NONE)];
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for exe in EXES {
        for (root, view) in views {
            if let Some(p) = default_value(root, exe, view) {
                if !out.iter().any(|(_, known)| *known == p) {
                    out.push((exe.to_string(), p));
                }
            }
        }
    }
    out
}

#[cfg(windows)]
const WOW_NONE: u32 = 0;
/// `RRF_SUBKEY_WOW6432KEY`: look in the 32-bit view of the registry.
#[cfg(windows)]
const WOW_32: u32 = 0x0002_0000;

#[cfg(windows)]
fn default_value(root: windows_sys::Win32::System::Registry::HKEY, exe: &str, view: u32) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};

    let subkey: Vec<u16> = std::ffi::OsStr::new(&format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}"))
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | view;
    let mut bytes: u32 = 0;
    // SAFETY: `subkey` is NUL-terminated and outlives both calls; a null value name reads the key's default
    // value; the first call only reports the size, the second writes at most `bytes` bytes into `buf`.
    unsafe {
        let probe = RegGetValueW(
            root,
            subkey.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut bytes,
        );
        if probe != 0 || bytes < 2 {
            return None;
        }
        let mut buf: Vec<u16> = vec![0; (bytes as usize).div_ceil(2)];
        let mut size = (buf.len() * 2) as u32;
        let read = RegGetValueW(
            root,
            subkey.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        );
        if read != 0 {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let text = OsString::from_wide(&buf[..len]);
        let text = text.to_string_lossy();
        let path = PathBuf::from(text.trim().trim_matches('"'));
        path.is_absolute().then_some(path)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn app_paths_are_absolute_and_come_from_the_known_exes() {
        let found = app_paths();
        eprintln!("registry App Paths: {found:?}");
        for (exe, path) in found {
            assert!(EXES.contains(&exe.as_str()), "{exe}");
            assert!(path.is_absolute(), "{}", path.display());
            assert!(!path.to_string_lossy().contains('"'));
        }
    }
}
