//! Video and audio edits that need choices from an editor window: the window sends
//! an `Edit` and everything else matches the one-click tools. Work happens in a
//! temporary folder and a new file or folder appears beside the original.

use crate::extras::{self, CompressPlan, Field};
use crate::formats::extension_of;
use crate::filetools;
use crate::tools::{hidden, run, Tools};
use crate::{media, unique_path};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub start: f64,
    pub end: f64,
    pub blur: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "tool", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Edit {
    TrimVideo { start: f64, end: f64 },
    CropVideo { x: u32, y: u32, width: u32, height: u32 },
    SpeedVideo { factor: f64 },
    SplitVideo { points: Vec<f64> },
    SaveFrames { times: Vec<f64> },
    RedactVideo { areas: Vec<Area> },
    TrimAudio { start: f64, end: f64 },
    TrimSilence,
    Channels { left: f64, right: f64, mono: bool },
    Bleep { ranges: Vec<(f64, f64)> },
    Visualize { image: Option<PathBuf>, shape: String },
    Normalize { loudness: f64, peak: f64, range: f64 },
    CompressTo { plan: CompressPlan },
    Metadata { fields: Vec<Field> },
    RemoveMetadata,
}

fn strings(line: &str) -> Vec<String> {
    line.split(' ').map(String::from).collect()
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn parent(path: &Path) -> &Path {
    path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

/// Keeps containers that hold modern streams and moves the rest to MP4.
fn video_out_ext(ext: &str) -> &str {
    if matches!(ext, "mp4" | "mov" | "mkv" | "m4v" | "webm") {
        ext
    } else {
        "mp4"
    }
}

fn audio_out_args(ext: &str) -> Vec<String> {
    media::audio_args(ext).iter().map(|arg| arg.to_string()).collect()
}

fn check_range(start: f64, end: f64) -> Result<()> {
    if !(start >= 0.0 && end > start) {
        bail!("the end must come after the start");
    }
    Ok(())
}

/// atempo takes 0.5 to 100 per stage, so slower speeds chain stages.
fn atempo_chain(mut factor: f64) -> String {
    let mut stages = Vec::new();
    while factor < 0.5 {
        stages.push("atempo=0.5".to_string());
        factor /= 0.5;
    }
    stages.push(format!("atempo={factor}"));
    stages.join(",")
}

fn crop_expression(area: &Area) -> (u32, u32) {
    (area.width.max(2) & !1, area.height.max(2) & !1)
}

/// Builds a filter graph applying each redaction in turn, ending at `[out]`.
fn redact_graph(areas: &[Area]) -> String {
    let mut graph = String::new();
    let mut from = "0:v".to_string();
    for (index, area) in areas.iter().enumerate() {
        let (width, height) = crop_expression(area);
        let to = format!("r{index}");
        let when = format!("between(t\\,{}\\,{})", area.start, area.end);
        if area.blur {
            let (x, y) = (area.x, area.y);
            graph.push_str(&format!(
                "[{from}]split[a{index}][b{index}];\
                 [b{index}]crop={width}:{height}:{x}:{y},boxblur=20:2[z{index}];\
                 [a{index}][z{index}]overlay={x}:{y}:enable='{when}'[{to}];"
            ));
        } else {
            graph.push_str(&format!(
                "[{from}]drawbox=x={}:y={}:w={width}:h={height}:color=black:t=fill:enable='{when}'[{to}];",
                area.x, area.y
            ));
        }
        from = to;
    }
    graph.push_str(&format!("[{from}]format=yuv420p[out]"));
    graph
}

fn bleep_filter(ranges: &[(f64, f64)], duration: f64) -> String {
    let inside: Vec<String> = ranges.iter().map(|(a, b)| format!("between(t,{a},{b})")).collect();
    let inside = inside.join("+");
    format!(
        "[0:a]volume=0:enable='{inside}'[quiet];\
         sine=frequency=1000:sample_rate=48000:duration={duration},\
         volume=0.25,volume=0:enable='not({inside})'[tone];\
         [quiet][tone]amix=inputs=2:duration=first:normalize=0[out]"
    )
}

fn channels_filter(left: f64, right: f64, mono: bool) -> String {
    if mono {
        format!("aformat=channel_layouts=stereo,pan=mono|c0={:.3}*c0+{:.3}*c1", left * 0.5, right * 0.5)
    } else {
        format!("aformat=channel_layouts=stereo,pan=stereo|c0={left:.3}*c0|c1={right:.3}*c1")
    }
}

fn shape_size(shape: &str) -> (u32, u32) {
    match shape {
        "portrait" => (1080, 1920),
        "square" => (1080, 1080),
        _ => (1920, 1080),
    }
}

/// Runs one edit and returns the new file or folder.
pub fn apply(tools: &Tools, src: &Path, edit: &Edit, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    // These manage their own temporary folder.
    match edit {
        Edit::CompressTo { plan } => return extras::compress_to(tools, src, plan, progress),
        Edit::Metadata { fields } => return extras::write_metadata(tools, src, fields, progress),
        Edit::RemoveMetadata => {
            let mut failure = None;
            let produced = filetools::run_tool(tools, "strip", &[src.to_path_buf()], progress, &mut |_, error| failure = Some(error));
            return match failure {
                Some(error) => Err(error),
                None => produced.into_iter().next().ok_or_else(|| anyhow!("nothing was produced")),
            };
        }
        _ => {}
    }
    let ext = extension_of(src).ok_or_else(|| anyhow!("{} has no extension", src.display()))?;
    let dir = parent(src);
    let work = tempfile::Builder::new().prefix(".transmute-").tempdir_in(dir)?;
    let name = stem(src);
    let video_ext = video_out_ext(&ext);
    let video_out = work.path().join(format!("output.{video_ext}"));
    let audio_out = work.path().join(format!("output.{ext}"));
    let finish = |out: &Path, suffix: &str, out_ext: &str| -> Result<PathBuf> {
        let path = unique_path(dir, &format!("{name} {suffix}"), out_ext);
        std::fs::rename(out, &path)?;
        Ok(path)
    };

    match edit {
        Edit::TrimVideo { start, end } => {
            check_range(*start, *end)?;
            let mut args = strings(&format!("-ss {start} -to {end}"));
            args.extend(media::video_args(video_ext, None, None));
            media::run_ffmpeg(tools, src, &args, &video_out, progress)?;
            finish(&video_out, "trimmed", video_ext)
        }
        Edit::CropVideo { x, y, width, height } => {
            let (w, h) = ((*width).max(2) & !1, (*height).max(2) & !1);
            let mut args = vec!["-vf".to_string(), format!("crop={w}:{h}:{x}:{y}")];
            args.extend(media::video_codec_args(video_ext));
            args.extend(media::audio_codec_args(video_ext));
            media::run_ffmpeg(tools, src, &args, &video_out, progress)?;
            finish(&video_out, "cropped", video_ext)
        }
        Edit::SpeedVideo { factor } => {
            if !(*factor >= 0.1 && *factor <= 16.0) {
                bail!("speed must be between 0.1x and 16x");
            }
            let mut graph = format!("[0:v]setpts=PTS/{factor}[v]");
            let has_audio = media::has_audio(tools, src)?;
            if has_audio {
                graph.push_str(&format!(";[0:a]{}[a]", atempo_chain(*factor)));
            }
            let mut args = vec!["-filter_complex".to_string(), graph, "-map".into(), "[v]".into()];
            if has_audio {
                args.extend(["-map".to_string(), "[a]".to_string()]);
                args.extend(media::audio_codec_args(video_ext));
            }
            args.extend(media::video_codec_args(video_ext));
            media::run_ffmpeg(tools, src, &args, &video_out, progress)?;
            finish(&video_out, "speed", video_ext)
        }
        Edit::SplitVideo { points } => {
            let total = media::duration_seconds(tools, src).ok_or_else(|| anyhow!("unknown duration"))?;
            let mut cuts: Vec<f64> = points.iter().copied().filter(|p| *p > 0.0 && *p < total).collect();
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            if cuts.is_empty() {
                bail!("choose at least one split point");
            }
            cuts.insert(0, 0.0);
            cuts.push(total);
            let folder = work.path().join("parts");
            std::fs::create_dir_all(&folder)?;
            for (index, pair) in cuts.windows(2).enumerate() {
                let out = folder.join(format!("{name} part {}.{video_ext}", index + 1));
                let mut args = strings(&format!("-ss {} -to {}", pair[0], pair[1]));
                args.extend(media::video_args(video_ext, None, None));
                media::run_ffmpeg(tools, src, &args, &out, &mut |_| {})?;
                progress((index + 1) as f32 / (cuts.len() - 1) as f32);
            }
            let path = unique_path(dir, &format!("{name} parts"), "");
            std::fs::rename(&folder, &path)?;
            Ok(path)
        }
        Edit::SaveFrames { times } => {
            if times.is_empty() {
                bail!("choose at least one frame");
            }
            let folder = work.path().join("frames");
            std::fs::create_dir_all(&folder)?;
            for (index, time) in times.iter().enumerate() {
                let out = folder.join(format!("{name} frame {}.png", index + 1));
                run(hidden(tools.ffmpeg()?)
                    .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &time.to_string(), "-i"])
                    .arg(src)
                    .args(["-frames:v", "1"])
                    .arg(&out))?;
                if !out.is_file() {
                    bail!("no frame at {time} seconds");
                }
                progress((index + 1) as f32 / times.len() as f32);
            }
            let path = unique_path(dir, &format!("{name} frames"), "");
            std::fs::rename(&folder, &path)?;
            Ok(path)
        }
        Edit::RedactVideo { areas } => {
            if areas.is_empty() {
                bail!("add at least one redaction");
            }
            let mut args = vec![
                "-filter_complex".to_string(),
                redact_graph(areas),
                "-map".to_string(),
                "[out]".to_string(),
                "-map".to_string(),
                "0:a?".to_string(),
            ];
            args.extend(media::video_codec_args(video_ext));
            args.extend(media::audio_codec_args(video_ext));
            media::run_ffmpeg(tools, src, &args, &video_out, progress)?;
            finish(&video_out, "redacted", video_ext)
        }
        Edit::TrimAudio { start, end } => {
            check_range(*start, *end)?;
            let mut args = strings(&format!("-vn -ss {start} -to {end}"));
            args.extend(audio_out_args(&ext));
            media::run_ffmpeg(tools, src, &args, &audio_out, progress)?;
            finish(&audio_out, "trimmed", &ext)
        }
        Edit::TrimSilence => {
            let trim = "silenceremove=start_periods=1:start_threshold=-50dB";
            let mut args = vec!["-vn".to_string(), "-af".into(), format!("{trim},areverse,{trim},areverse")];
            args.extend(audio_out_args(&ext));
            media::run_ffmpeg(tools, src, &args, &audio_out, progress)?;
            finish(&audio_out, "trimmed", &ext)
        }
        Edit::Channels { left, right, mono } => {
            let filter = channels_filter(*left, *right, *mono);
            let mut args = vec!["-vn".to_string(), "-af".into(), filter];
            args.extend(audio_out_args(&ext));
            media::run_ffmpeg(tools, src, &args, &audio_out, progress)?;
            finish(&audio_out, "channels", &ext)
        }
        Edit::CompressTo { .. } | Edit::Metadata { .. } | Edit::RemoveMetadata => unreachable!("handled above"),
        Edit::Normalize { loudness, peak, range } => {
            let filter = format!("loudnorm=I={loudness}:TP={peak}:LRA={range}");
            let mut args = vec!["-vn".to_string(), "-af".into(), filter];
            args.extend(audio_out_args(&ext));
            media::run_ffmpeg(tools, src, &args, &audio_out, progress)?;
            finish(&audio_out, "normalized", &ext)
        }
        Edit::Bleep { ranges } => {
            if ranges.is_empty() {
                bail!("add at least one bleep");
            }
            let total = media::duration_seconds(tools, src).ok_or_else(|| anyhow!("unknown duration"))?;
            let mut args = vec![
                "-filter_complex".to_string(),
                bleep_filter(ranges, total),
                "-map".to_string(),
                "[out]".to_string(),
            ];
            args.extend(audio_out_args(&ext));
            media::run_ffmpeg(tools, src, &args, &audio_out, progress)?;
            finish(&audio_out, "bleeped", &ext)
        }
        Edit::Visualize { image, shape } => {
            let (width, height) = shape_size(shape);
            let out = work.path().join("output.mp4");
            let mut args: Vec<String> = Vec::new();
            let video = match image {
                Some(image) => {
                    args.extend(["-loop".to_string(), "1".to_string(), "-i".to_string()]);
                    args.push(image.to_string_lossy().into_owned());
                    format!(
                        "[1:v]scale={width}:{height}:force_original_aspect_ratio=decrease,\
                         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=yuv420p[v]"
                    )
                }
                None => format!(
                    "[0:a]showwaves=s={width}x{height}:mode=cline:rate=30:colors=white,format=yuv420p[v]"
                ),
            };
            args.extend(["-filter_complex".to_string(), video, "-map".into(), "[v]".into()]);
            args.extend(strings("-map 0:a -c:v libx264 -preset veryfast -crf 23 -c:a aac -b:a 192k -shortest"));
            media::run_ffmpeg(tools, src, &args, &out, progress)?;
            finish(&out, "visualizer", "mp4")
        }
    }
}

/// Duration in seconds, and pixel size if the file has a picture.
pub fn media_info(tools: &Tools, src: &Path) -> Result<(f64, Option<(u32, u32)>)> {
    let duration = media::duration_seconds(tools, src).ok_or_else(|| anyhow!("unknown duration"))?;
    let size = if media::has_video(tools, src)? { Some(media::video_size(tools, src)?) } else { None };
    Ok((duration, size))
}

/// A wide picture of the sound for the audio editors.
pub fn waveform(tools: &Tools, src: &Path, cache: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(cache)?;
    let out = cache.join(format!("waveform-{}.png", std::process::id()));
    run(hidden(tools.ffmpeg()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-filter_complex", "showwavespic=s=1400x120:colors=#f0821e", "-frames:v", "1"])
        .arg(&out))?;
    Ok(out)
}

/// One channel of a recording on both speakers, for previewing it alone.
pub fn solo_channel(tools: &Tools, src: &Path, right: bool, cache: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(cache)?;
    let side = if right { 1 } else { 0 };
    let out = cache.join(format!("solo-{side}-{}.m4a", std::process::id()));
    let filter = format!("aformat=channel_layouts=stereo,pan=stereo|c0=c{side}|c1=c{side}");
    run(hidden(tools.ffmpeg()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-af", &filter, "-c:a", "aac", "-b:a", "160k"])
        .arg(&out))?;
    Ok(out)
}

/// One frame as an image, for detecting what to redact in a video.
pub fn frame_at(tools: &Tools, src: &Path, seconds: f64, out: &Path) -> Result<()> {
    run(hidden(tools.ffmpeg()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &seconds.to_string(), "-i"])
        .arg(src)
        .args(["-frames:v", "1"])
        .arg(out))?;
    Ok(())
}

/// A clip the editor window can play. WebView2 plays H.264 in MP4 and VP8/VP9 in
/// WebM, so other sources get a temporary proxy with the same timing and size.
pub fn playable_preview(tools: &Tools, src: &Path, cache: &Path) -> Result<PathBuf> {
    let ext = extension_of(src).unwrap_or_default();
    if media::plays_in_webview(tools, src, &ext)? {
        return Ok(src.to_path_buf());
    }
    std::fs::create_dir_all(cache)?;
    let is_video = media::has_video(tools, src)?;
    let out = cache.join(format!("preview-{}.{}", std::process::id(), if is_video { "mp4" } else { "m4a" }));
    let args = if is_video {
        strings("-c:v libx264 -preset veryfast -crf 26 -pix_fmt yuv420p -c:a aac -b:a 128k")
    } else {
        strings("-vn -c:a aac -b:a 160k")
    };
    media::run_ffmpeg(tools, src, &args, &out, &mut |_| {})?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_speeds_chain_atempo_stages() {
        assert_eq!(atempo_chain(2.0), "atempo=2");
        assert_eq!(atempo_chain(0.25), "atempo=0.5,atempo=0.5");
    }

    #[test]
    fn redactions_apply_one_after_another() {
        let area = Area { x: 1, y: 2, width: 11, height: 20, start: 0.0, end: 1.0, blur: false };
        let blurred = Area { blur: true, ..area };
        let graph = redact_graph(&[area, blurred]);
        assert!(graph.starts_with("[0:v]drawbox=x=1:y=2:w=10:h=20"));
        assert!(graph.contains("[r0]split[a1][b1]"));
        assert!(graph.ends_with("[r1]format=yuv420p[out]"));
    }

    /// These strings are copied from the real editor page's output.
    #[test]
    fn every_message_from_the_editor_page_parses() {
        let messages = [
            r#"{"tool":"trim-video","start":1,"end":2.5}"#,
            r#"{"tool":"trim-audio","start":0.5,"end":1.5}"#,
            r#"{"tool":"trim-silence"}"#,
            r#"{"tool":"crop-video","x":10,"y":20,"width":100,"height":50}"#,
            r#"{"tool":"speed-video","factor":1.5}"#,
            r#"{"tool":"split-video","points":[1.333,2.667]}"#,
            r#"{"tool":"save-frames","times":[0]}"#,
            r#"{"tool":"redact-video","areas":[{"x":0,"y":0,"width":50,"height":50,"start":1,"end":2,"blur":true}]}"#,
            r#"{"tool":"channels","left":0,"right":1,"mono":true}"#,
            r#"{"tool":"bleep","ranges":[[1,2]]}"#,
            r#"{"tool":"visualize","image":"C:\\pic.png","shape":"square"}"#,
            r#"{"tool":"visualize","image":null,"shape":"portrait"}"#,
        ];
        for message in messages {
            let parsed: Result<Edit, _> = serde_json::from_str(message);
            assert!(parsed.is_ok(), "{message}: {parsed:?}");
        }
    }

    #[test]
    fn the_edit_json_the_window_sends_is_understood() {
        let edit: Edit = serde_json::from_str(r#"{"tool":"trim-video","start":1,"end":2.5}"#).unwrap();
        assert!(matches!(edit, Edit::TrimVideo { start, end } if start == 1.0 && end == 2.5));
        let edit: Edit = serde_json::from_str(
            r#"{"tool":"redact-video","areas":[{"x":1,"y":2,"width":3,"height":4,"start":0,"end":1,"blur":true}]}"#,
        )
        .unwrap();
        assert!(matches!(edit, Edit::RedactVideo { .. }));
        let edit: Edit = serde_json::from_str(r#"{"tool":"bleep","ranges":[[1,2]]}"#).unwrap();
        assert!(matches!(edit, Edit::Bleep { .. }));
        let edit: Edit = serde_json::from_str(r#"{"tool":"trim-silence"}"#).unwrap();
        assert!(matches!(edit, Edit::TrimSilence));
    }
}
