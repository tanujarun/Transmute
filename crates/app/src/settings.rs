//! User preferences, stored as JSON under %APPDATA%\Transmute, and the window that
//! edits them.

use crate::platform;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub reveal_in_explorer: bool,
    pub reveal_on_desktop: bool,
    pub start_with_windows: bool,
    pub show_tray_icon: bool,
    pub sound_on_finish: bool,
    /// alt, rightalt, ctrl or shift.
    pub trigger_key: String,
    /// auto, powershell, cmd, or a path to any terminal program.
    pub terminal: String,
    /// Set once the first-run autostart default has been applied, so turning
    /// autostart off is respected from then on.
    pub autostart_applied: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            reveal_in_explorer: true,
            reveal_on_desktop: false,
            start_with_windows: true,
            show_tray_icon: true,
            sound_on_finish: false,
            trigger_key: "alt".into(),
            terminal: "auto".into(),
            autostart_applied: false,
        }
    }
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Transmute").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Opens the settings window, or brings it forward.
pub fn open(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window("settings") {
            let _ = window.set_focus();
            return;
        }
        let _ = WebviewWindowBuilder::new(&handle, "settings", WebviewUrl::App("settings.html".into()))
            .title("Transmute settings")
            .inner_size(420.0, 400.0)
            .build();
    });
}

#[tauri::command]
pub fn settings_get() -> Settings {
    Settings::load()
}

#[tauri::command]
pub fn settings_set(app: AppHandle, settings: Settings) {
    let previous = Settings::load();
    if !cfg!(debug_assertions) && previous.start_with_windows != settings.start_with_windows {
        platform::set_autostart(settings.start_with_windows);
    }
    transmute_gesture::set_trigger_key(&settings.trigger_key);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_visible(settings.show_tray_icon);
    }
    settings.save();
}
