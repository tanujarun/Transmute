//! Runs selections one at a time on a worker thread and reports to the progress
//! window. Files in a batch are independent: one failure does not stop the rest.

use crate::{actions, editor, float, platform, settings::Settings};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use tauri::{AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindowBuilder};
use transmute_core::extras::read_qr;
use transmute_core::filetools::{opens_editor, run_tool};
use transmute_core::{convert, Tools};
use transmute_gesture::Selection;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

#[derive(Clone, Serialize)]
struct Progress {
    title: String,
    fraction: Option<f32>,
}

const WINDOW: &str = "progress";

fn log(message: &str) {
    if std::env::var_os("TRANSMUTE_LOG").is_some() {
        eprintln!("[job] {message}");
    }
}

fn open_window(app: &AppHandle) {
    if app.get_webview_window(WINDOW).is_some() {
        return;
    }
    let (ready_sender, ready) = channel();
    let listener = app.once("progress-ready", move |_| {
        let _ = ready_sender.send(());
    });
    let handle = app.clone();
    // Windows must be created on the main thread.
    let _ = app.run_on_main_thread(move || {
        let _ = WebviewWindowBuilder::new(&handle, WINDOW, WebviewUrl::App("progress.html".into()))
            .title("Transmute")
            .inner_size(380.0, 90.0)
            .resizable(true)
            .always_on_top(true)
            .focused(false)
            .build();
    });
    // The page announces itself once it is listening. The limit only protects the
    // queue if the window never loads.
    let started = std::time::Instant::now();
    match ready.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(()) => log(&format!("progress window ready after {:?}", started.elapsed())),
        Err(_) => {
            app.unlisten(listener);
            log("progress window did not report ready");
        }
    }
}

fn close_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.close();
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn report_error(app: &AppHandle, name: &str, error: &anyhow::Error) {
    let message = format!("{name}: {error:#}");
    log(&format!("failed: {message}"));
    let _ = app.emit("job-error", message);
}

fn finish(app: &AppHandle, produced: &[PathBuf], failed: bool) {
    log(&format!("finished: {} produced, failed={failed}: {produced:?}", produced.len()));
    let settings = Settings::load();
    if settings.reveal_in_explorer {
        platform::reveal(produced, settings.reveal_on_desktop);
    }
    if settings.sound_on_finish {
        platform::play_finish_sound();
    }
    if failed {
        // Errors stay on screen until the user closes the window.
        let _ = app.emit("job-finished", "Finished with errors");
    } else {
        close_window(app);
    }
}

fn run_conversions(app: &AppHandle, tools: &Tools, selection: &Selection) {
    open_window(app);
    let total = selection.paths.len();
    let mut produced = Vec::new();
    let mut failed = false;
    for (index, path) in selection.paths.iter().enumerate() {
        let name = file_name(path);
        let title = if total > 1 { format!("{name} ({} of {total})", index + 1) } else { name.clone() };
        let _ = app.emit("job-progress", Progress { title: title.clone(), fraction: None });
        let mut report = |fraction: f32| {
            let _ = app.emit("job-progress", Progress { title: title.clone(), fraction: Some(fraction) });
        };
        match convert(tools, path, &selection.segment_id, &mut report) {
            Ok(output) => produced.push(output),
            Err(error) => {
                failed = true;
                report_error(app, &name, &error);
            }
        }
    }
    finish(app, &produced, failed);
}

/// Runs an editor's choices over its files. Called on its own thread. One failure
/// does not stop the rest of a selection.
pub fn run_edit(
    app: &AppHandle,
    paths: &[PathBuf],
    work: impl Fn(&Path, &mut dyn FnMut(f32)) -> anyhow::Result<PathBuf>,
) {
    open_window(app);
    let mut produced = Vec::new();
    let mut failed = false;
    for path in paths {
        let name = file_name(path);
        let _ = app.emit("job-progress", Progress { title: name.clone(), fraction: None });
        let mut report = |fraction: f32| {
            let _ = app.emit("job-progress", Progress { title: name.clone(), fraction: Some(fraction) });
        };
        match work(path, &mut report) {
            Ok(output) => produced.push(output),
            Err(error) => {
                failed = true;
                report_error(app, &name, &error);
            }
        }
    }
    finish(app, &produced, failed);
}

/// Reads QR codes, copies what was found, and leaves the text on screen.
fn read_codes(app: &AppHandle, tools: &Tools, selection: &Selection) {
    open_window(app);
    let mut found = Vec::new();
    for path in &selection.paths {
        match read_qr(tools, path) {
            Ok(codes) => found.extend(codes),
            Err(error) => report_error(app, &file_name(path), &error),
        }
    }
    if !found.is_empty() {
        let text = found.join("\n");
        let _ = actions::copy_text(&text);
        let _ = app.emit("job-note", text);
    }
    let title = if found.is_empty() { "No QR code" } else { "Copied to the clipboard" };
    let _ = app.emit("job-finished", title);
}

fn run_file_tool(app: &AppHandle, tools: &Tools, selection: &Selection) {
    if selection.segment_id == "qr" {
        read_codes(app, tools, selection);
        return;
    }
    if opens_editor(&selection.segment_id) {
        editor::open(app, selection.paths.clone(), selection.segment_id.clone());
        return;
    }
    open_window(app);
    let title = file_name(&selection.paths[0]);
    let _ = app.emit("job-progress", Progress { title: title.clone(), fraction: None });
    let mut failed = false;
    let mut report = |fraction: f32| {
        let _ = app.emit("job-progress", Progress { title: title.clone(), fraction: Some(fraction) });
    };
    let mut on_error = |path: &Path, error: anyhow::Error| {
        failed = true;
        report_error(app, &file_name(path), &error);
    };
    let produced = run_tool(tools, &selection.segment_id, &selection.paths, &mut report, &mut on_error);
    finish(app, &produced, failed);
}

fn run_action(app: &AppHandle, selection: &Selection) {
    if selection.segment_id == "float" {
        float::add(app, selection.paths.clone());
        return;
    }
    if let Err(error) = actions::run(&selection.segment_id, &selection.paths) {
        open_window(app);
        report_error(app, &selection.segment_id, &error);
        let _ = app.emit("job-finished", "Could not finish");
    }
}

/// Starts the worker and returns the queue that selections go into.
pub fn start(app: AppHandle) -> Sender<Selection> {
    let (sender, receiver) = channel::<Selection>();
    std::thread::spawn(move || {
        // Move and Share use shell COM objects, which need an apartment thread.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let tools = Tools::locate();
        for selection in receiver {
            log(&format!("start {} / {}: {:?}", selection.wheel, selection.segment_id, selection.paths));
            match (selection.wheel.as_str(), &tools) {
                ("Actions", _) => run_action(&app, &selection),
                ("Tools", Ok(tools)) => run_file_tool(&app, tools, &selection),
                (_, Ok(tools)) => run_conversions(&app, tools, &selection),
                (_, Err(error)) => {
                    open_window(&app);
                    report_error(&app, "Transmute", error);
                    let _ = app.emit("job-finished", "Could not start");
                }
            }
        }
    });
    sender
}
