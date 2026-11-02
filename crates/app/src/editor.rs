//! The editor window for tools that need choices, such as trim, redact or collage.
//! It shows a preview, then hands the result to the same progress and reveal path
//! as other jobs.

use crate::jobs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use transmute_core::edits::{self, Edit};
use transmute_core::extras::{self, Field};
use transmute_core::filetools::run_tool;
use transmute_core::formats::{extension_of, kind_of, Kind};
use transmute_core::image_edits::{self, CollageStyle, PhotoEdit};
use transmute_core::Tools;

const WINDOW: &str = "editor";

/// The files being edited and the tool chosen on the wheel.
#[derive(Default)]
pub struct Session(pub Mutex<Option<(Vec<PathBuf>, String)>>);

#[derive(Default, Serialize)]
pub struct Info {
    kind: String,
    name: String,
    names: Vec<String>,
    tool: String,
    preview: String,
    waveform: String,
    pages: Vec<String>,
    fields: Vec<Field>,
    bytes: u64,
    duration: f64,
    width: u32,
    height: u32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CollageRequest {
    layout: String,
    spacing: u32,
    background: String,
    #[serde(default)]
    corners: u32,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    order: Vec<usize>,
}

/// The sequence chosen for tools that combine several files.
#[derive(Clone, Debug, Deserialize)]
pub struct OrderRequest {
    order: Vec<usize>,
}

/// Everything the page can send. Media and photo edits carry a `tool` tag, and a
/// collage has its own shape.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum AnyEdit {
    Media(Edit),
    Photo(PhotoEdit),
    Collage(CollageRequest),
    Order(OrderRequest),
}

/// Files in the chosen order. Anything the page leaves out or repeats is ignored,
/// so a bad message can never duplicate or lose a selected file.
fn in_order(paths: &[PathBuf], order: &[usize]) -> Vec<PathBuf> {
    let mut seen = vec![false; paths.len()];
    let mut ordered = Vec::with_capacity(paths.len());
    for &index in order {
        if index < paths.len() && !seen[index] {
            seen[index] = true;
            ordered.push(paths[index].clone());
        }
    }
    for (index, path) in paths.iter().enumerate() {
        if !seen[index] {
            ordered.push(path.clone());
        }
    }
    ordered
}

/// Opens the editor for a tool, replacing any open editor.
pub fn open(app: &AppHandle, paths: Vec<PathBuf>, tool: String) {
    *app.state::<Session>().0.lock().unwrap_or_else(|p| p.into_inner()) = Some((paths, tool));
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window(WINDOW) {
            let _ = window.destroy();
        }
        let _ = WebviewWindowBuilder::new(&handle, WINDOW, WebviewUrl::App("editor.html".into()))
            .title("Transmute editor")
            .inner_size(780.0, 680.0)
            .build();
    });
}

fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("transmute-previews")
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn load_info(paths: &[PathBuf], tool: String) -> anyhow::Result<Info> {
    let tools = Tools::locate()?;
    let first = paths.first().ok_or_else(|| anyhow::anyhow!("nothing to edit"))?;
    let names: Vec<String> = paths.iter().map(|path| file_name(path)).collect();
    let bytes = paths.iter().filter_map(|path| std::fs::metadata(path).ok()).map(|m| m.len()).sum();
    let mut info = Info { name: names.join(", "), names, tool, bytes, ..Default::default() };
    let kind = extension_of(first).and_then(|ext| kind_of(&ext));
    match info.tool.as_str() {
        "collage" | "compress-to" | "normalize" | "merge-pdf" | "join" | "make-pdf" => info.kind = "list".into(),
        "metadata" => {
            info.kind = "list".into();
            info.fields = extras::read_metadata(&tools, first)?;
        }
        "organize" => {
            info.kind = "pdf".into();
            let pages = image_edits::pdf_thumbnails(&tools, first, &cache_dir().join("pages"))?;
            info.pages = pages.iter().map(|page| page.display().to_string()).collect();
        }
        _ if kind == Some(Kind::Image) => {
            info.kind = "image".into();
            info.preview = image_edits::image_preview(&tools, first, &cache_dir())?.display().to_string();
            (info.width, info.height) = image_edits::image_size(&tools, first)?;
        }
        _ => {
            info.kind = "media".into();
            info.preview = edits::playable_preview(&tools, first, &cache_dir())?.display().to_string();
            let (duration, size) = edits::media_info(&tools, first)?;
            info.duration = duration;
            (info.width, info.height) = size.unwrap_or((0, 0));
            if size.is_none() {
                info.waveform = edits::waveform(&tools, first, &cache_dir())?.display().to_string();
            }
        }
    }
    Ok(info)
}

#[tauri::command]
pub async fn editor_info(session: State<'_, Session>) -> Result<Info, String> {
    let (paths, tool) = session.0.lock().unwrap_or_else(|p| p.into_inner()).clone().ok_or("nothing to edit")?;
    tauri::async_runtime::spawn_blocking(move || load_info(&paths, tool).map_err(|e| format!("{e:#}")))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn editor_apply(app: AppHandle, session: State<'_, Session>, edit: AnyEdit) -> Result<(), String> {
    let (paths, tool) = session.0.lock().unwrap_or_else(|p| p.into_inner()).clone().ok_or("nothing to edit")?;
    let tools = Tools::locate().map_err(|e| format!("{e:#}"))?;
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.close();
    }
    std::thread::spawn(move || {
        if let AnyEdit::Collage(request) = &edit {
            let ordered = in_order(&paths, &request.order);
            let style = CollageStyle { corners: request.corners, width: request.width };
            jobs::run_edit(&app, &paths[..1], |_, _| {
                image_edits::collage(&tools, &ordered, &request.layout, request.spacing, &request.background, &style)
            });
        } else if let AnyEdit::Order(request) = &edit {
            let ordered = in_order(&paths, &request.order);
            jobs::run_edit(&app, &paths[..1], |_, report| {
                let mut failure = None;
                let produced = run_tool(&tools, &tool, &ordered, report, &mut |_, error| failure = Some(error));
                match failure {
                    Some(error) => Err(error),
                    None => produced.into_iter().next().ok_or_else(|| anyhow::anyhow!("nothing was produced")),
                }
            });
        } else {
            jobs::run_edit(&app, &paths, |path, report| match &edit {
                AnyEdit::Media(edit) => edits::apply(&tools, path, edit, report),
                AnyEdit::Photo(edit) => image_edits::apply(&tools, path, edit),
                AnyEdit::Collage(_) | AnyEdit::Order(_) => unreachable!("handled above"),
            });
        }
        let _ = std::fs::remove_dir_all(cache_dir());
    });
    Ok(())
}

#[derive(Default, Deserialize, Serialize)]
pub struct Detected {
    faces: Vec<[u32; 4]>,
    text: Vec<[u32; 4]>,
}

fn run_detection(path: &std::path::Path, seconds: Option<f64>) -> anyhow::Result<Detected> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let tools = Tools::locate()?;
    let cache = cache_dir();
    std::fs::create_dir_all(&cache)?;
    let source = match seconds {
        Some(seconds) => {
            let frame = cache.join("detect-frame.png");
            edits::frame_at(&tools, path, seconds, &frame)?;
            frame
        }
        None => path.to_path_buf(),
    };
    let copy = cache.join("detect.png");
    let scale = image_edits::detection_copy(&tools, &source, &copy)?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .arg("--detect")
        .arg(&copy)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let mut found: Detected = serde_json::from_slice(&output.stdout)?;
    let enlarge = |boxes: &mut Vec<[u32; 4]>| {
        for area in boxes {
            *area = area.map(|value| (value as f64 * scale).round() as u32);
        }
    };
    enlarge(&mut found.faces);
    enlarge(&mut found.text);
    Ok(found)
}

/// Finds faces and text in the photo, or in the video frame at `seconds`.
#[tauri::command]
pub async fn editor_detect(session: State<'_, Session>, seconds: Option<f64>) -> Result<Detected, String> {
    let (paths, _) = session.0.lock().unwrap_or_else(|p| p.into_inner()).clone().ok_or("nothing to edit")?;
    tauri::async_runtime::spawn_blocking(move || run_detection(&paths[0], seconds).map_err(|e| format!("{e:#}")))
        .await
        .map_err(|e| e.to_string())?
}

/// Measures the selected recording against the chosen loudness targets.
#[tauri::command]
pub async fn editor_loudness(
    session: State<'_, Session>,
    loudness: f64,
    peak: f64,
    range: f64,
) -> Result<extras::Loudness, String> {
    let (paths, _) = session.0.lock().unwrap_or_else(|p| p.into_inner()).clone().ok_or("nothing to edit")?;
    tauri::async_runtime::spawn_blocking(move || {
        let tools = Tools::locate().map_err(|e| format!("{e:#}"))?;
        extras::measure_loudness(&tools, &paths[0], loudness, peak, range).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A temporary version with one channel on both speakers, for the channels editor.
#[tauri::command]
pub async fn editor_solo(session: State<'_, Session>, right: bool) -> Result<String, String> {
    let (paths, _) = session.0.lock().unwrap_or_else(|p| p.into_inner()).clone().ok_or("nothing to edit")?;
    tauri::async_runtime::spawn_blocking(move || {
        let tools = Tools::locate().map_err(|e| format!("{e:#}"))?;
        edits::solo_channel(&tools, &paths[0], right, &cache_dir())
            .map(|path| path.display().to_string())
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Picks a picture for the audio visualizer or a photo background.
#[tauri::command]
pub async fn editor_pick_image() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| crate::actions::pick_image().map_err(|e| format!("{e:#}")))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_never_loses_or_repeats_a_file() {
        let paths: Vec<PathBuf> = ["a", "b", "c"].iter().map(PathBuf::from).collect();
        let names = |order: &[usize]| -> Vec<String> {
            in_order(&paths, order).iter().map(|p| p.display().to_string()).collect()
        };
        assert_eq!(names(&[2, 0, 1]), ["c", "a", "b"]);
        assert_eq!(names(&[1, 1, 9]), ["b", "a", "c"], "repeats and bad indexes are dropped");
        assert_eq!(names(&[]), ["a", "b", "c"], "no order keeps the selection order");
    }

    /// Each string is what the page sends, so a mismatch shows up here first.
    #[test]
    fn page_messages_reach_the_right_edit() {
        let parse = |json: &str| serde_json::from_str::<AnyEdit>(json).unwrap();
        assert!(matches!(parse(r#"{"tool":"trim-video","start":0,"end":1}"#), AnyEdit::Media(_)));
        assert!(matches!(parse(r#"{"tool":"crop-image","x":0,"y":0,"width":9,"height":9}"#), AnyEdit::Photo(_)));
        assert!(matches!(parse(r#"{"tool":"organize-pdf","pages":[{"page":1,"rotate":0}]}"#), AnyEdit::Photo(_)));
        assert!(matches!(parse(r##"{"layout":"grid","spacing":8,"background":"#ffffff"}"##), AnyEdit::Collage(_)));
        assert!(matches!(
            parse(r##"{"layout":"row","spacing":8,"background":"#ffffff","corners":12,"width":1000,"order":[1,0]}"##),
            AnyEdit::Collage(_)
        ));
        assert!(matches!(parse(r#"{"order":[1,0]}"#), AnyEdit::Order(_)));
        assert!(matches!(
            parse(r#"{"tool":"adjust-image","brightness":0,"contrast":0,"saturation":0,"hue":0,"sharpen":0,"blur":0,"grain":0,"dehaze":20,"clarity":30,"denoise":10}"#),
            AnyEdit::Photo(_)
        ));
        assert!(matches!(
            parse(r##"{"tool":"background-image","color":"#112233","image":null,"backdropBlur":8,"ratio":0,"padding":0,"radius":0,"shadow":false}"##),
            AnyEdit::Photo(_)
        ));
        assert!(matches!(
            parse(r##"{"tool":"annotate-image","shapes":[{"kind":"pen","points":[[1,2],[3,4]],"color":"#ff0000"},{"kind":"magnifier","x":1,"y":2,"radius":50,"zoom":2.5},{"kind":"picture","x":1,"y":2,"width":90,"path":"C:\\a.png"}]}"##),
            AnyEdit::Photo(_)
        ));
        for json in [
            r#"{"tool":"normalize","loudness":-16,"peak":-1.5,"range":11}"#,
            r#"{"tool":"compress-to","plan":{"percent":50,"targetMb":null,"maxSide":1920}}"#,
            r#"{"tool":"metadata","fields":[{"name":"title","value":"Song"}]}"#,
            r#"{"tool":"remove-metadata"}"#,
        ] {
            assert!(matches!(parse(json), AnyEdit::Media(_)), "{json}");
        }
        // Copied from the page's real output for the photo tools.
        for json in [
            r#"{"tool":"resize-image","width":320,"height":180}"#,
            r#"{"tool":"adjust-image","brightness":20,"contrast":0,"saturation":0,"hue":30,"sharpen":0,"blur":0,"grain":0}"#,
            r##"{"tool":"background-image","color":"#112233","image":"C:\\pic.png","ratio":1,"padding":64,"radius":12,"shadow":true}"##,
            r##"{"tool":"background-image","color":"#112233","image":null,"ratio":0,"padding":0,"radius":0,"shadow":false}"##,
            r#"{"tool":"redact-image","regions":[{"x":0,"y":0,"width":50,"height":50,"blur":true}]}"#,
            r##"{"tool":"annotate-image","shapes":[{"kind":"arrow","x1":10,"y1":10,"x2":100,"y2":50,"color":"#00ff00"},{"kind":"highlight","x1":0,"y1":0,"x2":20,"y2":20},{"kind":"text","x":50,"y":60,"text":"Hi","color":"#00ff00","size":16},{"kind":"number","x":70,"y":80,"value":1,"color":"#00ff00","size":16}]}"##,
        ] {
            assert!(matches!(parse(json), AnyEdit::Photo(_)), "{json}");
        }
    }
}
