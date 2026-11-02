//! The Actions wheel: file operations that work on anything, including folders.
//! Each runs on the worker thread, which holds a single-threaded COM apartment.

use crate::settings::Settings;
use anyhow::{anyhow, bail, Result};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, HANDLE, HGLOBAL, POINT};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::*;

pub const WHEEL: &[(&str, &str)] = &[
    ("share", "Share"),
    ("float", "Float"),
    ("copy", "Copy"),
    ("move", "Move"),
    ("copy-path", "Copy Path"),
    ("terminal", "Terminal"),
];

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().collect()
}

/// Puts an owned memory block on the clipboard under one format.
fn set_clipboard(format: u16, bytes: &[u8]) -> Result<()> {
    unsafe {
        let mut opened = OpenClipboard(None);
        for _ in 0..10 {
            if opened.is_ok() {
                break;
            }
            // Another app may hold the clipboard for a moment.
            std::thread::sleep(std::time::Duration::from_millis(30));
            opened = OpenClipboard(None);
        }
        opened?;
        let result = (|| -> Result<()> {
            EmptyClipboard()?;
            let memory: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
            let target = GlobalLock(memory) as *mut u8;
            if target.is_null() {
                bail!("could not lock clipboard memory");
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
            let _ = GlobalUnlock(memory);
            // The clipboard owns the memory once this succeeds.
            SetClipboardData(format as u32, Some(HANDLE(memory.0)))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// CF_HDROP payload: a DROPFILES header, then NUL-separated wide paths, double-NUL ended.
fn hdrop_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let mut names: Vec<u16> = Vec::new();
    for path in paths {
        names.extend(wide(path.as_os_str()));
        names.push(0);
    }
    names.push(0);
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        pt: POINT::default(),
        fNC: false.into(),
        fWide: true.into(),
    };
    let mut bytes = unsafe {
        std::slice::from_raw_parts(&header as *const DROPFILES as *const u8, size_of::<DROPFILES>()).to_vec()
    };
    bytes.extend(names.iter().flat_map(|unit| unit.to_le_bytes()));
    bytes
}

fn path_text(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\r\n")
}

pub fn copy_text(text: &str) -> Result<()> {
    let units: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    set_clipboard(CF_UNICODETEXT.0, &bytes)
}

fn open_console(program: &str, folder: &Path) -> Result<()> {
    // CREATE_NEW_CONSOLE, because this process has no console of its own.
    use std::os::windows::process::CommandExt;
    std::process::Command::new(program).current_dir(folder).creation_flags(0x0000_0010).spawn()?;
    Ok(())
}

fn open_terminal(paths: &[PathBuf]) -> Result<()> {
    let first = paths.first().ok_or_else(|| anyhow!("nothing selected"))?;
    let folder: &Path = if first.is_dir() { first } else { first.parent().unwrap_or(first) };
    match Settings::load().terminal.as_str() {
        "powershell" => open_console("powershell.exe", folder),
        "cmd" => open_console("cmd.exe", folder),
        "auto" | "" => {
            if std::process::Command::new("wt.exe").arg("-d").arg(folder).spawn().is_err() {
                open_console("powershell.exe", folder)?;
            }
            Ok(())
        }
        custom => open_console(custom, folder),
    }
}

/// Moves through the shell, so it can be undone and asks before replacing anything.
fn move_items(paths: &[PathBuf]) -> Result<()> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_PICKFOLDERS)?;
        dialog.SetTitle(&HSTRING::from("Move to"))?;
        if let Err(error) = dialog.Show(None) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(());
            }
            return Err(error.into());
        }
        let destination: IShellItem = dialog.GetResult()?;
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO)?;
        for path in paths {
            let name = HSTRING::from(path.as_os_str());
            let item: IShellItem = SHCreateItemFromParsingName(&name, None)?;
            operation.MoveItem(&item, &destination, PCWSTR::null(), None)?;
        }
        operation.PerformOperations()?;
    }
    Ok(())
}

/// Opens the share sheet from a helper process, so a failure there cannot take the
/// tray app with it. A quick non-zero exit is reported; after that the helper is
/// left to live for as long as the sheet needs it.
fn share(paths: &[PathBuf]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut helper = std::process::Command::new(std::env::current_exe()?)
        .arg("--share")
        .args(paths)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    for _ in 0..40 {
        if let Some(status) = helper.try_wait()? {
            if !status.success() {
                bail!("the Windows share panel could not open");
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

/// Asks for an image file. Returns None if the dialog is cancelled.
pub fn pick_image() -> Result<Option<String>> {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        if let Err(error) = dialog.Show(None) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(error.into());
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = name.to_string()?;
        windows::Win32::System::Com::CoTaskMemFree(Some(name.as_ptr() as *const _));
        Ok(Some(path))
    }
}

/// Runs an action. Float is handled by the caller because it needs the app handle.
pub fn run(id: &str, paths: &[PathBuf]) -> Result<()> {
    match id {
        "copy" => set_clipboard(CF_HDROP.0, &hdrop_bytes(paths)),
        "copy-path" => copy_text(&path_text(paths)),
        "terminal" => open_terminal(paths),
        "move" => move_items(paths),
        "share" => share(paths),
        other => bail!("unknown action: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdrop_payload_is_double_nul_terminated_wide_text() {
        let bytes = hdrop_bytes(&[PathBuf::from(r"C:\a.txt"), PathBuf::from(r"C:\b.txt")]);
        let header = size_of::<DROPFILES>();
        let units: Vec<u16> =
            bytes[header..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let expected: Vec<u16> = "C:\\a.txt\0C:\\b.txt\0\0".encode_utf16().collect();
        assert_eq!(units, expected);
    }

    #[test]
    fn copied_paths_are_one_per_line() {
        let text = path_text(&[PathBuf::from(r"C:\a"), PathBuf::from(r"D:\b c")]);
        assert_eq!(text, "C:\\a\r\nD:\\b c");
    }
}
