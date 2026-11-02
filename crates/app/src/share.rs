//! Opens the Windows share sheet through the documented desktop API. This runs in
//! its own short-lived process (`transmute --share <paths>`): it needs a window and
//! a message loop of its own, and a failure in the share UI cannot reach the tray app.

#![allow(non_snake_case)]

use anyhow::{anyhow, Result};
use std::ffi::c_void;
use std::path::PathBuf;
use windows::core::{factory, w, Interface, GUID, HRESULT, HSTRING};
use windows::ApplicationModel::DataTransfer::{DataRequestedEventArgs, DataTransferManager};
use windows::Foundation::TypedEventHandler;
use windows::Storage::{IStorageItem, StorageFile, StorageFolder};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_collections::IIterable;
use windows_core::{IUnknown, IUnknown_Vtbl};

/// Declared in ShObjIdl_core.h; the `windows` crate does not expose it.
#[windows_core::interface("3A3DCD6C-3EAB-43DC-BCDE-45671CE800C8")]
unsafe trait IDataTransferManagerInterop: IUnknown {
    unsafe fn GetForWindow(&self, app_window: HWND, riid: *const GUID, manager: *mut *mut c_void) -> HRESULT;
    unsafe fn ShowShareUIForWindow(&self, app_window: HWND) -> HRESULT;
}

/// How long the helper stays alive after opening the sheet, so a slow transfer
/// keeps its data source. The sheet gives no signal when it closes.
const LIFETIME_MS: u32 = 5 * 60 * 1000;

fn log(message: &str) {
    if std::env::var_os("TRANSMUTE_LOG").is_some() {
        eprintln!("[share] {message}");
    }
}

/// Handlers must be `Send`, but this one only ever fires on the thread that created
/// these items, because it is delivered through this thread's message loop.
struct ThreadBound(Vec<IStorageItem>);
unsafe impl Send for ThreadBound {}

impl ThreadBound {
    fn list(&self) -> IIterable<IStorageItem> {
        self.0.iter().cloned().map(Some).collect::<Vec<_>>().into()
    }
}

fn storage_item(path: &PathBuf) -> Result<IStorageItem> {
    let name = HSTRING::from(path.as_os_str());
    let item: IStorageItem = if path.is_dir() {
        StorageFolder::GetFolderFromPathAsync(&name)?.join()?.cast()?
    } else {
        StorageFile::GetFileFromPathAsync(&name)?.join()?.cast()?
    };
    Ok(item)
}

unsafe extern "system" fn host_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_TIMER {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

/// A nearly invisible window at the cursor for the sheet to anchor to. It has to be
/// an ordinary, visible top-level window or the sheet never asks for the data.
unsafe fn create_host() -> Result<HWND> {
    let module = GetModuleHandleW(None)?;
    let class = w!("TransmuteShareHost");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(host_proc),
        hInstance: module.into(),
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassW(&window_class);
    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    let hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_APPWINDOW,
        class,
        w!("Share"),
        WS_POPUP,
        cursor.x,
        cursor.y,
        16,
        16,
        None,
        None,
        Some(module.into()),
        None,
    )?;
    SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA)?;
    let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    let _ = SetForegroundWindow(hwnd);
    Ok(hwnd)
}

fn show_share(paths: &[PathBuf]) -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let items: Vec<IStorageItem> = paths.iter().map(storage_item).collect::<Result<_>>()?;
        log(&format!("resolved {} items", items.len()));
        let title = if items.len() == 1 {
            paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        } else {
            format!("{} items", items.len())
        };

        let items = ThreadBound(items);

        let hwnd = create_host()?;
        let interop = factory::<DataTransferManager, IDataTransferManagerInterop>()?;
        let mut raw = std::ptr::null_mut();
        interop.GetForWindow(hwnd, &DataTransferManager::IID, &mut raw).ok()?;
        if raw.is_null() {
            return Err(anyhow!("no share manager for this window"));
        }
        let manager = DataTransferManager::from_raw(raw);

        manager.DataRequested(&TypedEventHandler::<DataTransferManager, DataRequestedEventArgs>::new(
            move |_, args| {
                let args = args.as_ref().ok_or_else(windows::core::Error::empty)?;
                let data = args.Request()?.Data()?;
                data.Properties()?.SetTitle(&HSTRING::from(title.as_str()))?;
                data.SetStorageItemsReadOnly(&items.list())?;
                log("data provided");
                Ok(())
            },
        ))?;

        interop.ShowShareUIForWindow(hwnd).ok()?;
        log("sheet requested");
        SetTimer(Some(hwnd), 1, LIFETIME_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

/// Entry point for `--share`. Returns the process exit code.
pub fn run_helper(paths: &[PathBuf]) -> i32 {
    match show_share(paths) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("share failed: {error:#}");
            1
        }
    }
}
