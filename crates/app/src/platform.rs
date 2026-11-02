//! Windows integration: single instance, autostart and showing results in Explorer.

use std::collections::BTreeMap;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject, INFINITE};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::WindowsAndMessaging::MB_OK;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("Transmute");

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// False if another instance holds the global input hooks already.
pub fn claim_single_instance() -> bool {
    unsafe {
        // Deliberately leaked: the mutex lives as long as the process.
        let created = CreateMutexW(None, true, w!("Local\\TransmuteSingleInstance"));
        created.is_ok() && GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Named so a second launch can ask the running copy to show its settings.
const SHOW_SETTINGS: PCWSTR = w!("Local\\TransmuteShowSettings");

/// Tells the running copy to open its settings window.
pub fn request_settings() {
    unsafe {
        if let Ok(event) = CreateEventW(None, false, false, SHOW_SETTINGS) {
            let _ = SetEvent(event);
        }
    }
}

/// Blocks until another launch calls `request_settings`, then calls `on_request`, forever.
pub fn wait_for_settings_requests(on_request: impl Fn()) {
    unsafe {
        let Ok(event) = CreateEventW(None, false, false, SHOW_SETTINGS) else { return };
        while WaitForSingleObject(event, INFINITE) == WAIT_OBJECT_0 {
            on_request();
        }
    }
}

/// A short system sound.
pub fn play_finish_sound() {
    unsafe {
        let _ = MessageBeep(MB_OK);
    }
}

pub fn set_autostart(enabled: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegCreateKeyW(HKEY_CURRENT_USER, RUN_KEY, &mut key).is_err() {
            return;
        }
        if enabled {
            if let Ok(exe) = std::env::current_exe() {
                let quoted = format!("\"{}\"\0", exe.display());
                let bytes: Vec<u8> = quoted.encode_utf16().flat_map(u16::to_le_bytes).collect();
                let _ = RegSetValueExW(key, RUN_VALUE, Some(0), REG_SZ, Some(&bytes));
            }
        } else {
            let _ = RegDeleteValueW(key, RUN_VALUE);
        }
        let _ = RegCloseKey(key);
    }
}

fn is_desktop(dir: &Path) -> bool {
    unsafe {
        let Ok(raw) = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) else {
            return false;
        };
        let desktop = raw.to_string().map(PathBuf::from).ok();
        CoTaskMemFree(Some(raw.as_ptr() as *const _));
        desktop.is_some_and(|desktop| desktop == dir)
    }
}

fn parse(path: &Path) -> Option<*mut ITEMIDLIST> {
    let mut list = std::ptr::null_mut();
    let name = wide(path);
    unsafe { SHParseDisplayName(PCWSTR(name.as_ptr()), None, &mut list, 0, None).ok()? };
    (!list.is_null()).then_some(list)
}

/// Opens each result folder with its new files selected. Results on the Desktop
/// are already in view, so by default they do not open anything.
pub fn reveal(paths: &[PathBuf], include_desktop: bool) {
    let mut folders: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            folders.entry(parent.to_path_buf()).or_default().push(path.clone());
        }
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        for (folder, items) in folders {
            if !include_desktop && is_desktop(&folder) {
                continue;
            }
            let Some(folder_list) = parse(&folder) else { continue };
            let lists: Vec<*mut ITEMIDLIST> = items.iter().filter_map(|item| parse(item)).collect();
            let children: Vec<*const ITEMIDLIST> =
                lists.iter().map(|list| ILFindLastID(*list) as *const ITEMIDLIST).collect();
            let _ = SHOpenFolderAndSelectItems(folder_list, Some(&children), 0);
            for list in lists {
                CoTaskMemFree(Some(list as *const _));
            }
            CoTaskMemFree(Some(folder_list as *const _));
        }
    }
}
