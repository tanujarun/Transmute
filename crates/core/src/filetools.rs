//! One-click file tools for the Tools wheel. Like conversions, they work in a
//! temporary folder beside the source and always produce a new file.

use crate::formats::{extension_of, kind_of, Kind};
use crate::tools::{hidden, run, Tools};
use crate::{archives, documents, extras, heic, media, unique_path};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};

const COMPRESSIBLE_IMAGES: &[&str] = &["jpg", "png", "webp", "heic", "tiff", "avif", "bmp"];
const CAMERA_RAW: &[&str] = &["cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "raf"];

fn tools_for_file(path: &Path) -> Vec<(&'static str, &'static str)> {
    if path.is_dir() {
        return Vec::new();
    }
    let ext = extension_of(path).unwrap_or_default();
    match kind_of(&ext) {
        Some(Kind::Image) if COMPRESSIBLE_IMAGES.contains(&ext.as_str()) => {
            let mut tools = vec![("compress", "Compress"), ("compress-to", "To Size"), ("strip", "Metadata"), ("qr", "Read QR")];
            tools.extend(PHOTO_EDITORS);
            tools
        }
        // Camera RAW has no smaller form of itself, so Compress saves a JPG.
        Some(Kind::Image) if CAMERA_RAW.contains(&ext.as_str()) => {
            let mut tools = vec![("compress", "Compress"), ("compress-to", "To Size")];
            tools.extend(PHOTO_EDITORS);
            tools
        }
        Some(Kind::Video) if ext != "gif" => {
            vec![
                ("compress", "Compress"),
                ("compress-to", "To Size"),
                ("strip", "Metadata"),
                ("mute", "Mute"),
                ("trim-video", "Trim"),
                ("crop", "Crop"),
                ("speed", "Speed"),
                ("frames", "Frames"),
                ("split-video", "Split"),
                ("redact", "Redact"),
            ]
        }
        Some(Kind::Audio) => vec![
            ("compress", "Compress"),
            ("compress-to", "To Size"),
            ("strip", "Metadata"),
            ("normalize", "Normalize"),
            ("mono", "Mono"),
            ("stereo", "Stereo"),
            ("trim-audio", "Trim"),
            ("channels", "Channels"),
            ("bleep", "Bleep"),
            ("visualize", "Visualize"),
        ],
        Some(Kind::Pdf) => {
            vec![
                ("compress", "Compress"),
                ("compress-to", "To Size"),
                ("strip", "Metadata"),
                ("split", "Split"),
                ("organize", "Organize"),
                ("pdfa", "PDF/A"),
                ("epub-pages", "Page EPUB"),
            ]
        }
        Some(Kind::Archive) => vec![("extract", "Extract")],
        _ => Vec::new(),
    }
}

const PHOTO_EDITORS: [(&str, &str); 6] = [
    ("edit-image", "Edit"),
    ("annotate", "Annotate"),
    ("background", "Add BG"),
    ("crop-image", "Crop"),
    ("resize-image", "Resize"),
    ("redact-image", "Redact"),
];

const EDITOR_TOOLS: &[&str] = &[
    "trim-video", "crop", "speed", "frames", "split-video", "redact",
    "trim-audio", "channels", "bleep", "visualize",
    "edit-image", "annotate", "background", "crop-image", "resize-image", "redact-image",
    "organize", "collage", "compress-to", "normalize", "metadata",
];

/// Editors that can apply one set of choices to a whole selection.
const BATCH_EDITORS: &[&str] = &["compress-to", "normalize", "crop-image", "resize-image"];

/// Whether a tool opens the editor window rather than running straight away.
pub fn opens_editor(id: &str) -> bool {
    EDITOR_TOOLS.contains(&id)
}

/// Tools for a selection as (id, label): what every file shares, plus the tools
/// that combine several files of one kind.
pub fn tools_for(paths: &[impl AsRef<Path>]) -> Vec<(&'static str, &'static str)> {
    let mut common: Option<Vec<(&'static str, &'static str)>> = None;
    for path in paths {
        let own = tools_for_file(path.as_ref());
        common = Some(match common {
            None => own,
            Some(previous) => previous.into_iter().filter(|tool| own.contains(tool)).collect(),
        });
    }
    let mut tools = common.unwrap_or_default();
    // Most editors work on one file; Collage is added below and takes several.
    if paths.len() >= 2 {
        tools.retain(|(id, _)| !opens_editor(id) || BATCH_EDITORS.contains(id));
    } else if paths.first().is_some_and(|path| extras::metadata_editable(path.as_ref())) {
        // One editable file gets the full editor, which can still remove everything.
        for tool in &mut tools {
            if tool.0 == "strip" {
                tool.0 = "metadata";
            }
        }
    }
    if paths.len() >= 2 {
        let ext = |p: &Path| extension_of(p).unwrap_or_default();
        let all = |test: &dyn Fn(&str) -> bool| paths.iter().all(|p| test(&ext(p.as_ref())));
        if all(&|e| e == "pdf") {
            tools.push(("merge-pdf", "Merge"));
        }
        if all(&|e| kind_of(e) == Some(Kind::Image)) {
            tools.push(("make-pdf", "Make PDF"));
            tools.push(("collage", "Collage"));
        }
        if all(&|e| matches!(e, "mp4" | "mov" | "mkv")) {
            tools.push(("join", "Join"));
        }
    }
    tools
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn parent(path: &Path) -> &Path {
    path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn workspace(dir: &Path) -> Result<tempfile::TempDir> {
    Ok(tempfile::Builder::new().prefix(".transmute-").tempdir_in(dir)?)
}

fn publish(out: &Path, src: &Path, suffix: &str, ext: &str) -> Result<PathBuf> {
    let path = unique_path(parent(src), &format!("{} {suffix}", stem(src)), ext);
    std::fs::rename(out, &path)?;
    Ok(path)
}

/// The explicit [0] also stops brackets in a file name being read as a frame number.
fn first_frame(src: &Path) -> std::ffi::OsString {
    let mut value = src.as_os_str().to_os_string();
    value.push("[0]");
    value
}

fn compress_image(tools: &Tools, src: &Path, ext: &str, out: &Path, work: &Path) -> Result<()> {
    if ext == "heic" {
        return heic::write_heic_at(tools, src, out, work, 28);
    }
    let mut command = hidden(tools.magick()?);
    command.arg(first_frame(src));
    match ext {
        "png" => {
            command.args(["-colors", "256", "-define", "png:compression-level=9"]);
        }
        "tiff" => {
            command.args(["-compress", "zip"]);
        }
        "bmp" => {
            command.args(["-colors", "256", "-compress", "RLE"]);
        }
        _ => {
            command.args(["-quality", "70"]);
        }
    }
    run(command.arg(out))?;
    Ok(())
}

fn compress_audio_args(ext: &str) -> Vec<String> {
    let args: &[&str] = match ext {
        "mp3" => &["-c:a", "libmp3lame", "-b:a", "128k"],
        "m4a" | "aac" => &["-c:a", "aac", "-b:a", "96k"],
        "ogg" => &["-c:a", "libvorbis", "-q:a", "2"],
        "opus" => &["-c:a", "libopus", "-b:a", "64k"],
        "wma" => &["-c:a", "wmav2", "-b:a", "96k"],
        "ac3" => &["-c:a", "ac3", "-b:a", "96k"],
        "flac" => &["-c:a", "flac", "-compression_level", "12"],
        // Uncompressed formats can only shrink by lowering the sample rate.
        "aiff" => &["-c:a", "pcm_s16be", "-ar", "22050", "-ac", "1"],
        _ => &["-c:a", "pcm_s16le", "-ar", "22050", "-ac", "1"],
    };
    args.iter().map(|arg| arg.to_string()).collect()
}

/// Video compresses to MP4 unless its container handles H.264 or VP9 already.
fn compress_video(tools: &Tools, src: &Path, ext: &str, work: &Path, progress: &mut dyn FnMut(f32)) -> Result<(PathBuf, String)> {
    let keep = matches!(ext, "mp4" | "mov" | "mkv" | "m4v" | "webm");
    let out_ext = if keep { ext.to_string() } else { "mp4".to_string() };
    let out = work.join(format!("output.{out_ext}"));
    let line = if ext == "webm" {
        "-c:v libvpx-vp9 -crf 38 -b:v 0 -row-mt 1 -pix_fmt yuv420p -c:a libopus -b:a 96k".to_string()
    } else {
        let even = "scale=trunc(iw/2)*2:trunc(ih/2)*2";
        format!("-c:v libx264 -preset medium -crf 28 -pix_fmt yuv420p -vf {even} -c:a aac -b:a 128k")
    };
    let args: Vec<String> = line.split(' ').map(String::from).collect();
    media::run_ffmpeg(tools, src, &args, &out, progress)?;
    Ok((out, out_ext))
}

fn compress(tools: &Tools, src: &Path, ext: &str, work: &Path, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let kind = kind_of(ext);
    let (out, out_ext) = if kind == Some(Kind::Video) {
        compress_video(tools, src, ext, work, progress)?
    } else if CAMERA_RAW.contains(&ext) {
        let out = work.join("output.jpg");
        let mut input = src.as_os_str().to_os_string();
        input.push("[0]");
        run(hidden(tools.magick()?).arg(input).args(["-auto-orient", "-quality", "85"]).arg(&out))?;
        (out, "jpg".to_string())
    } else {
        let out = work.join(format!("output.{ext}"));
        match kind {
            Some(Kind::Image) => compress_image(tools, src, ext, &out, work)?,
            Some(Kind::Audio) => {
                let mut args = vec!["-vn".to_string()];
                args.extend(compress_audio_args(ext));
                media::run_ffmpeg(tools, src, &args, &out, progress)?;
            }
            Some(Kind::Pdf) => {
                run(hidden(tools.qpdf()?)
                    .args(["--optimize-images", "--jpeg-quality=70", "--recompress-flate"])
                    .args(["--compression-level=9", "--object-streams=generate"])
                    .arg(src)
                    .arg(&out))?;
            }
            _ => bail!("this file cannot be compressed"),
        }
        (out, ext.to_string())
    };
    let before = std::fs::metadata(src)?.len();
    let after = std::fs::metadata(&out)?.len();
    if after >= before {
        bail!("this file is already as small as it can get");
    }
    publish(&out, src, "compressed", &out_ext)
}

/// Removes metadata segments from a JPEG without re-encoding it. Keeps JFIF
/// (APP0), the colour profile (APP2) and the Adobe colour marker (APP14), which
/// change how the image decodes.
fn strip_jpeg(bytes: &[u8]) -> Result<Vec<u8>> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        bail!("not a valid JPEG");
    }
    let mut out = vec![0xFF, 0xD8];
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            bail!("damaged JPEG");
        }
        let marker = bytes[at + 1];
        if marker == 0xDA {
            // Start of scan: the rest is image data.
            out.extend_from_slice(&bytes[at..]);
            return Ok(out);
        }
        let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize + 2;
        let segment = bytes.get(at..at + length).ok_or_else(|| anyhow!("damaged JPEG"))?;
        let metadata = marker == 0xFE || (matches!(marker, 0xE1..=0xEF) && !matches!(marker, 0xE2 | 0xEE));
        if !metadata {
            out.extend_from_slice(segment);
        }
        at += length;
    }
    bail!("damaged JPEG")
}

fn strip(tools: &Tools, src: &Path, ext: &str, work: &Path, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let out = work.join(format!("output.{ext}"));
    match kind_of(ext) {
        Some(Kind::Image) if ext == "jpg" => std::fs::write(&out, strip_jpeg(&std::fs::read(src)?)?)?,
        Some(Kind::Image) if ext == "heic" => heic::write_heic_at(tools, src, &out, work, 18)?,
        Some(Kind::Image) => {
            run(hidden(tools.magick()?).arg(first_frame(src)).args(["-strip", "-quality", "95"]).arg(&out))?;
        }
        Some(Kind::Video) | Some(Kind::Audio) => {
            let line = "-map 0 -map_metadata -1 -map_chapters -1 -c copy";
            let args: Vec<String> = line.split(' ').map(String::from).collect();
            media::run_ffmpeg(tools, src, &args, &out, progress)?;
        }
        Some(Kind::Pdf) => {
            run(hidden(tools.qpdf()?).args(["--remove-info", "--remove-metadata"]).arg(src).arg(&out))?;
        }
        _ => bail!("this file has no metadata to remove"),
    }
    publish(&out, src, "clean", ext)
}

fn ffmpeg_tool(tools: &Tools, src: &Path, ext: &str, work: &Path, line: &str, suffix: &str, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let out = work.join(format!("output.{ext}"));
    let args: Vec<String> = line.split(' ').map(String::from).collect();
    media::run_ffmpeg(tools, src, &args, &out, progress)?;
    publish(&out, src, suffix, ext)
}

fn split_pdf(tools: &Tools, src: &Path, work: &Path) -> Result<PathBuf> {
    let pages = work.join("pages");
    std::fs::create_dir_all(&pages)?;
    run(hidden(tools.qpdf()?).arg("--split-pages").arg(src).arg(pages.join("page-%d.pdf")))?;
    let folder = unique_path(parent(src), &format!("{} pages", stem(src)), "");
    std::fs::rename(&pages, &folder)?;
    Ok(folder)
}

fn extract(tools: &Tools, src: &Path, work: &Path) -> Result<PathBuf> {
    let contents = work.join("contents");
    archives::extract(tools, src, &contents)?;
    let name = stem(src);
    let name = name.strip_suffix(".tar").unwrap_or(&name);
    let folder = unique_path(parent(src), name, "");
    std::fs::rename(&contents, &folder)?;
    Ok(folder)
}

fn run_single(tools: &Tools, id: &str, src: &Path, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let ext = extension_of(src).ok_or_else(|| anyhow!("{} has no extension", src.display()))?;
    let work = workspace(parent(src))?;
    let dir = work.path();
    match id {
        "compress" => compress(tools, src, &ext, dir, progress),
        "strip" => strip(tools, src, &ext, dir, progress),
        "mute" => ffmpeg_tool(tools, src, &ext, dir, "-an -c:v copy", "muted", progress),
        "normalize" => {
            let codec = media::audio_args(&ext).join(" ");
            let line = format!("-vn -af loudnorm=I=-16:TP=-1.5:LRA=11 {codec}");
            ffmpeg_tool(tools, src, &ext, dir, line.trim(), "normalized", progress)
        }
        "mono" | "stereo" => {
            let channels = if id == "mono" { 1 } else { 2 };
            let codec = media::audio_args(&ext).join(" ");
            let line = format!("-vn -ac {channels} {codec}");
            ffmpeg_tool(tools, src, &ext, dir, line.trim(), id, progress)
        }
        "split" => split_pdf(tools, src, dir),
        "extract" => extract(tools, src, dir),
        "pdfa" => {
            let out = dir.join("output.pdf");
            documents::pdf_to_pdfa(tools, src, &out, dir)?;
            publish(&out, src, "PDF-A", "pdf")
        }
        "epub-pages" => {
            let out = dir.join("output.epub");
            documents::pdf_to_page_epub(tools, src, &out, dir)?;
            publish(&out, src, "pages", "epub")
        }
        other => bail!("unknown tool: {other}"),
    }
}

fn merge_pdfs(tools: &Tools, paths: &[PathBuf]) -> Result<PathBuf> {
    let first = &paths[0];
    let work = workspace(parent(first))?;
    let out = work.path().join("output.pdf");
    run(hidden(tools.qpdf()?).args(["--empty", "--pages"]).args(paths).arg("--").arg(&out))?;
    publish(&out, first, "merged", "pdf")
}

fn images_to_pdf(tools: &Tools, paths: &[PathBuf]) -> Result<PathBuf> {
    let first = &paths[0];
    let work = workspace(parent(first))?;
    let out = work.path().join("output.pdf");
    run(hidden(tools.magick()?)
        .args(paths)
        .args(["-auto-orient", "-background", "white", "-alpha", "remove", "-alpha", "off"])
        .arg(&out))?;
    publish(&out, first, "images", "pdf")
}

/// Joins clips onto the first clip's canvas. Clips with no sound get silence so
/// picture and audio stay aligned.
fn join_videos(tools: &Tools, paths: &[PathBuf], progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let first = &paths[0];
    let work = workspace(parent(first))?;
    let out = work.path().join("output.mp4");
    let (width, height) = media::video_size(tools, first)?;
    let (width, height) = (width & !1, height & !1);

    let mut inputs: Vec<String> = Vec::new();
    let mut filters = String::new();
    let mut joined = String::new();
    let mut next_input = paths.len();
    for (index, clip) in paths.iter().enumerate() {
        if index > 0 {
            inputs.push("-i".into());
            inputs.push(clip.to_string_lossy().into_owned());
        }
        filters.push_str(&format!(
            "[{index}:v]scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v{index}];"
        ));
        let audio = if media::has_audio(tools, clip)? {
            index
        } else {
            next_input += 1;
            next_input - 1
        };
        filters.push_str(&format!("[{audio}:a]aresample=48000,aformat=channel_layouts=stereo[a{index}];"));
        joined.push_str(&format!("[v{index}][a{index}]"));
    }
    for clip in paths {
        if !media::has_audio(tools, clip)? {
            let seconds = media::duration_seconds(tools, clip).unwrap_or(1.0);
            inputs.extend(["-f".into(), "lavfi".into(), "-t".into(), seconds.to_string()]);
            inputs.extend(["-i".into(), "anullsrc=r=48000:cl=stereo".into()]);
        }
    }
    filters.push_str(&format!("{joined}concat=n={}:v=1:a=1[v][a]", paths.len()));
    let mut args = inputs;
    args.extend(["-filter_complex".into(), filters, "-map".into(), "[v]".into(), "-map".into(), "[a]".into()]);
    let tail = "-c:v libx264 -preset medium -crf 20 -c:a aac -b:a 192k -movflags +faststart";
    args.extend(tail.split(' ').map(String::from));
    media::run_ffmpeg(tools, first, &args, &out, progress)?;
    publish(&out, first, "joined", "mp4")
}

/// Runs a tool on a selection and returns everything it produced. Per-file tools
/// keep going after a failure; `on_error` receives each file's error.
pub fn run_tool(
    tools: &Tools,
    id: &str,
    paths: &[PathBuf],
    progress: &mut dyn FnMut(f32),
    on_error: &mut dyn FnMut(&Path, anyhow::Error),
) -> Vec<PathBuf> {
    let combined = match id {
        "merge-pdf" => Some(merge_pdfs(tools, paths)),
        "make-pdf" => Some(images_to_pdf(tools, paths)),
        "join" => Some(join_videos(tools, paths, progress)),
        _ => None,
    };
    if let Some(result) = combined {
        return match result {
            Ok(path) => vec![path],
            Err(error) => {
                on_error(&paths[0], error);
                Vec::new()
            }
        };
    }
    let mut produced = Vec::new();
    for path in paths {
        match run_single(tools, id, path, progress) {
            Ok(output) => produced.push(output),
            Err(error) => on_error(path, error),
        }
    }
    produced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_jpeg_removes_exif_and_comments_but_keeps_jfif_and_image_data() {
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0, 4, 1, 2]); // JFIF
        jpeg.extend_from_slice(&[0xFF, 0xE1, 0, 4, 9, 9]); // EXIF
        jpeg.extend_from_slice(&[0xFF, 0xFE, 0, 3, 7]); // comment
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0, 2, 5, 5, 0xFF, 0xD9]); // scan
        let stripped = strip_jpeg(&jpeg).unwrap();
        assert_eq!(stripped, [0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2, 0xFF, 0xDA, 0, 2, 5, 5, 0xFF, 0xD9]);
    }

    #[test]
    fn strip_jpeg_keeps_the_colour_profile() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE2, 0, 3, 1];
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0xFF, 0xD9]);
        assert_eq!(strip_jpeg(&jpeg).unwrap(), jpeg);
    }

    #[test]
    fn strip_jpeg_rejects_other_files() {
        assert!(strip_jpeg(b"not a jpeg").is_err());
    }

    #[test]
    fn selections_offer_the_right_tools() {
        let ids = |names: &[&str]| -> Vec<&'static str> {
            let paths: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
            tools_for(&paths).into_iter().map(|(id, _)| id).collect()
        };
        assert_eq!(ids(&["a.zip"]), vec!["extract"]);
        assert!(ids(&["a.ARW"]).contains(&"compress") && ids(&["a.ARW"]).contains(&"crop-image"));
        assert!(!ids(&["a.ARW"]).contains(&"strip"));
        assert!(ids(&["a.pdf"]).contains(&"organize"));
        assert!(ids(&["a.jpg", "b.png"]).contains(&"collage"));
        assert!(ids(&["a.jpg", "b.png"]).contains(&"crop-image"), "crop is a batch editor");
        assert!(!ids(&["a.jpg", "b.png"]).contains(&"annotate"));
        assert!(ids(&["a.jpg"]).contains(&"metadata") && !ids(&["a.jpg"]).contains(&"strip"));
        assert!(ids(&["a.jpg", "b.jpg"]).contains(&"strip"), "selections can still remove metadata");
        assert!(ids(&["a.pdf"]).contains(&"strip") && ids(&["a.pdf"]).contains(&"pdfa"));
        assert!(ids(&["a.mp4"]).contains(&"mute"));
        assert!(!ids(&["a.mp3"]).contains(&"mute"));
        assert!(ids(&["a.pdf", "b.pdf"]).contains(&"merge-pdf"));
        assert!(!ids(&["a.pdf"]).contains(&"merge-pdf"));
        assert!(ids(&["a.jpg", "b.png"]).contains(&"make-pdf"));
        assert!(ids(&["a.mp4", "b.mov"]).contains(&"join"));
        assert!(ids(&["a.docx"]).is_empty());
    }
}
