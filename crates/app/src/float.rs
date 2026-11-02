//! The Float shelf: a small always-on-top window holding files until they are
//! dragged out into another app. Files are referenced, never copied or moved.

use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

const WINDOW: &str = "float";

#[derive(Default)]
pub struct Shelf(pub Mutex<Vec<PathBuf>>);

fn names(items: &[PathBuf]) -> Vec<String> {
    items.iter().map(|path| path.display().to_string()).collect()
}

/// Adds files to the shelf, opening it if needed. Called off the main thread.
pub fn add(app: &AppHandle, paths: Vec<PathBuf>) {
    let shelf = app.state::<Shelf>();
    let items = {
        let mut items = shelf.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for path in paths {
            if !items.contains(&path) {
                items.push(path);
            }
        }
        names(&items)
    };
    if app.get_webview_window(WINDOW).is_none() {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let _ = WebviewWindowBuilder::new(&handle, WINDOW, WebviewUrl::App("float.html".into()))
                .title("Float")
                .inner_size(260.0, 300.0)
                .always_on_top(true)
                .focused(false)
                .build();
        });
    }
    let _ = app.emit("shelf-changed", items);
}

#[tauri::command]
pub fn shelf_items(shelf: State<'_, Shelf>) -> Vec<String> {
    names(&shelf.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
}

#[tauri::command]
pub fn shelf_clear(app: AppHandle, shelf: State<'_, Shelf>) {
    shelf.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.close();
    }
}

/// Moves the shelf to the left or right edge of its screen.
#[tauri::command]
pub fn shelf_pin(window: tauri::WebviewWindow, side: String) -> Result<(), String> {
    let monitor = window.current_monitor().map_err(|e| e.to_string())?.ok_or("no monitor")?;
    let size = window.outer_size().map_err(|e| e.to_string())?;
    let x = if side == "left" {
        monitor.position().x
    } else {
        monitor.position().x + monitor.size().width as i32 - size.width as i32
    };
    let y = monitor.position().y + (monitor.size().height as i32 - size.height as i32) / 2;
    window.set_position(tauri::PhysicalPosition::new(x, y)).map_err(|e| e.to_string())
}

/// Starts a native file drag out of the shelf. Synchronous so Tauri runs it on
/// the main thread, which OLE drags require.
#[tauri::command]
pub fn shelf_drag(window: tauri::WebviewWindow, path: String) -> Result<(), String> {
    let preview = drag::Image::Raw(include_bytes!("../icons/tray.png").to_vec());
    let item = drag::DragItem::Files(vec![PathBuf::from(path)]);
    drag::start_drag(&window, item, preview, |_, _| {}, Default::default()).map_err(|e| e.to_string())
}
