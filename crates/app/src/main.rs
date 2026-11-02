#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod detect;
mod editor;
mod float;
mod jobs;
mod platform;
mod settings;
mod share;

use settings::Settings;
use std::path::PathBuf;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{App, RunEvent};
use transmute_core::filetools::tools_for;
use transmute_core::formats::{label, targets_for};
use transmute_gesture::{Segment, Selection, Wheel};

fn segment(id: &str, label: &str) -> Segment {
    Segment { id: id.to_string(), label: label.to_string() }
}

/// Wheels with no segments are skipped, so a folder opens straight on Actions.
fn wheels_for(paths: &[PathBuf]) -> Vec<Wheel> {
    let convert = targets_for(paths).into_iter().map(|t| segment(t, &label(t))).collect();
    let tools = tools_for(paths).into_iter().map(|(id, name)| segment(id, name)).collect();
    let actions = actions::WHEEL.iter().map(|(id, name)| segment(id, name)).collect();
    vec![
        Wheel { title: "Convert".into(), segments: convert },
        Wheel { title: "Tools".into(), segments: tools },
        Wheel { title: "Actions".into(), segments: actions },
    ]
}

fn build_tray(app: &App) -> tauri::Result<()> {
    let settings = Settings::load();
    let open_settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Transmute", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open_settings, &separator, &quit])?;

    let tray = TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .tooltip("Transmute: drag a file and press Alt")
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "settings" => settings::open(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    tray.set_visible(settings.show_tray_icon)?;
    Ok(())
}

/// Autostart only applies to release builds, so a debug run never registers
/// a binary from the target folder.
fn apply_first_run_defaults() {
    let mut settings = Settings::load();
    if !cfg!(debug_assertions) && !settings.autostart_applied {
        platform::set_autostart(settings.start_with_windows);
        settings.autostart_applied = true;
        settings.save();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--share") {
        let paths: Vec<PathBuf> = args[1..].iter().map(PathBuf::from).collect();
        std::process::exit(share::run_helper(&paths));
    }
    if args.first().map(String::as_str) == Some("--detect") {
        std::process::exit(detect::run_helper(std::path::Path::new(&args[1])));
    }
    if !platform::claim_single_instance() {
        // Already running, perhaps with its tray icon hidden.
        platform::request_settings();
        return;
    }
    transmute_gesture::set_trigger_key(&Settings::load().trigger_key);
    let app = tauri::Builder::default()
        .manage(float::Shelf::default())
        .manage(editor::Session::default())
        .invoke_handler(tauri::generate_handler![
            float::shelf_items,
            float::shelf_clear,
            float::shelf_drag,
            float::shelf_pin,
            settings::settings_get,
            settings::settings_set,
            editor::editor_detect,
            editor::editor_info,
            editor::editor_apply,
            editor::editor_pick_image,
        ])
        .setup(|app| {
            apply_first_run_defaults();
            build_tray(app)?;
            let handle = app.handle().clone();
            std::thread::spawn(move || platform::wait_for_settings_requests(|| settings::open(&handle)));
            let queue = jobs::start(app.handle().clone());
            // `transmute --run Convert png a.jpg b.jpg` queues a job as the wheel would.
            let mut args = std::env::args().skip(1);
            if args.next().as_deref() == Some("--run") {
                if let (Some(wheel), Some(segment_id)) = (args.next(), args.next()) {
                    let paths = args.map(PathBuf::from).collect();
                    let _ = queue.send(Selection { paths, wheel, segment_id });
                }
            }
            std::thread::Builder::new().name("transmute-gesture".into()).spawn(move || {
                let provider = Box::new(|paths: &[PathBuf]| wheels_for(paths));
                let on_select = Box::new(move |selection| {
                    let _ = queue.send(selection);
                });
                if let Err(error) = transmute_gesture::run(provider, on_select) {
                    eprintln!("gesture loop stopped: {error}");
                }
            })?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("build Transmute");

    // Closing the progress window must not end the tray app.
    app.run(|_handle, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}
