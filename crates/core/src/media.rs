//! Video and audio through FFmpeg, and images through ImageMagick.

use crate::tools::{hidden, run, Tools};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

fn probe(tools: &Tools, src: &Path, stream: &str) -> Result<Option<String>> {
    let out = run(hidden(tools.ffprobe()?)
        .args(["-v", "error", "-select_streams", stream, "-show_entries", "stream=codec_name"])
        .args(["-of", "default=nw=1:nk=1"])
        .arg(src))?;
    let text = String::from_utf8_lossy(&out);
    Ok(text.lines().next().map(|codec| codec.trim().to_string()).filter(|c| !c.is_empty()))
}

pub(crate) fn duration_seconds(tools: &Tools, src: &Path) -> Option<f64> {
    let out = run(hidden(tools.ffprobe().ok()?)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(src))
    .ok()?;
    String::from_utf8_lossy(&out).trim().parse().ok()
}

/// Encoder settings per container. Copying is preferred when the streams already
/// fit, so a container swap is fast and lossless.
pub(crate) fn video_args(target: &str, video: Option<&str>, audio: Option<&str>) -> Vec<String> {
    let copy_video = matches!(target, "mp4" | "mov" | "mkv" | "m4v")
        && matches!(video, Some("h264") | Some("hevc"));
    let copy_audio = match target {
        "mp4" | "mov" | "m4v" => matches!(audio, Some("aac")),
        "mkv" => matches!(audio, Some("aac") | Some("opus") | Some("flac") | Some("mp3")),
        _ => false,
    };
    let even = "scale=trunc(iw/2)*2:trunc(ih/2)*2";
    let h264 = format!("-c:v libx264 -preset medium -crf 20 -pix_fmt yuv420p -vf {even}");
    let video_codec = if copy_video {
        "-c:v copy".to_string()
    } else {
        match target {
            "webm" => "-c:v libvpx-vp9 -crf 32 -b:v 0 -row-mt 1 -pix_fmt yuv420p".to_string(),
            "avi" => "-c:v mpeg4 -q:v 3 -pix_fmt yuv420p".to_string(),
            "wmv" => "-c:v wmv2 -q:v 3 -pix_fmt yuv420p".to_string(),
            "mpg" => "-c:v mpeg2video -q:v 3 -pix_fmt yuv420p".to_string(),
            _ => h264,
        }
    };
    let audio_codec = if copy_audio {
        "-c:a copy"
    } else {
        match target {
            "webm" => "-c:a libopus -b:a 128k",
            "avi" => "-c:a libmp3lame -b:a 192k",
            "wmv" => "-c:a wmav2 -b:a 192k",
            "mpg" => "-c:a mp2 -b:a 192k",
            _ => "-c:a aac -b:a 192k",
        }
    };
    let faststart = if matches!(target, "mp4" | "m4v" | "mov") { " -movflags +faststart" } else { "" };
    format!("{video_codec} {audio_codec}{faststart}").split(' ').map(String::from).collect()
}

pub(crate) fn audio_args(target: &str) -> &'static [&'static str] {
    match target {
        "mp3" => &["-c:a", "libmp3lame", "-q:a", "2"],
        "m4a" | "aac" => &["-c:a", "aac", "-b:a", "192k"],
        "wav" => &["-c:a", "pcm_s16le"],
        "flac" => &["-c:a", "flac"],
        "ogg" => &["-c:a", "libvorbis", "-q:a", "5"],
        "opus" => &["-c:a", "libopus", "-b:a", "128k"],
        "aiff" => &["-c:a", "pcm_s16be"],
        "wma" => &["-c:a", "wmav2", "-b:a", "192k"],
        "ac3" => &["-c:a", "ac3", "-b:a", "192k"],
        _ => &[],
    }
}

pub(crate) fn run_ffmpeg(
    tools: &Tools,
    src: &Path,
    args: &[String],
    out: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    let total = duration_seconds(tools, src);
    let stderr_log = out.with_extension("ffmpeg.log");
    let mut child = hidden(tools.ffmpeg()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-nostdin", "-i"])
        .arg(src)
        .args(args)
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(out)
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&stderr_log)?)
        .spawn()
        .context("could not start ffmpeg")?;
    let stdout = child.stdout.take().context("ffmpeg stdout")?;
    for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
        if let (Some(value), Some(total)) = (line.strip_prefix("out_time_us="), total) {
            if let Ok(micros) = value.trim().parse::<f64>() {
                progress(((micros / 1e6 / total) as f32).clamp(0.0, 1.0));
            }
        }
    }
    let status = child.wait()?;
    let errors = std::fs::read_to_string(&stderr_log).unwrap_or_default();
    let _ = std::fs::remove_file(&stderr_log);
    if !status.success() {
        bail!("ffmpeg failed: {}", errors.trim());
    }
    Ok(())
}

/// Encoder arguments for edits, which always re-encode video.
pub(crate) fn video_codec_args(target: &str) -> Vec<String> {
    let args = video_args(target, None, Some("aac"));
    let split = args.iter().position(|arg| arg == "-c:a").unwrap_or(args.len());
    let mut video = args[..split].to_vec();
    // Edits set their own -vf or filter graph, which a second -vf would replace.
    if let Some(at) = video.iter().position(|arg| arg == "-vf") {
        video.drain(at..at + 2);
    }
    video
}

pub(crate) fn audio_codec_args(target: &str) -> Vec<String> {
    let args = video_args(target, None, None);
    let Some(start) = args.iter().position(|arg| arg == "-c:a") else { return Vec::new() };
    args[start..start + 4].to_vec()
}

pub(crate) fn has_video(tools: &Tools, src: &Path) -> Result<bool> {
    Ok(probe(tools, src, "v:0")?.is_some())
}

/// WebView2 ships without licensed codecs beyond H.264 and AAC in MP4, plus the
/// open WebM formats.
pub(crate) fn plays_in_webview(tools: &Tools, src: &Path, ext: &str) -> Result<bool> {
    let video = probe(tools, src, "v:0")?;
    let audio = probe(tools, src, "a:0")?;
    Ok(match ext {
        "mp4" | "m4v" | "mov" => {
            matches!(video.as_deref(), Some("h264"))
                && matches!(audio.as_deref(), Some("aac") | Some("mp3") | None)
        }
        "webm" => true,
        "mp3" | "wav" | "ogg" | "opus" | "flac" | "m4a" | "aac" => video.is_none(),
        _ => false,
    })
}

pub(crate) fn has_audio(tools: &Tools, src: &Path) -> Result<bool> {
    Ok(probe(tools, src, "a:0")?.is_some())
}

pub(crate) fn video_size(tools: &Tools, src: &Path) -> Result<(u32, u32)> {
    let out = run(hidden(tools.ffprobe()?)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height"])
        .args(["-of", "csv=p=0"])
        .arg(src))?;
    let text = String::from_utf8_lossy(&out);
    let mut parts = text.trim().split(',').map(|part| part.trim().parse::<u32>());
    match (parts.next(), parts.next()) {
        (Some(Ok(width)), Some(Ok(height))) => Ok((width, height)),
        _ => bail!("could not read the video size"),
    }
}

pub fn convert_video(
    tools: &Tools,
    src: &Path,
    target: &str,
    out: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    let args: Vec<String> = match target {
        "mp3" => {
            if probe(tools, src, "a:0")?.is_none() {
                bail!("this video has no sound to save as MP3");
            }
            "-vn -c:a libmp3lame -q:a 2".split(' ').map(String::from).collect()
        }
        "gif" => vec![
            "-vf".into(),
            "fps=15,scale='min(iw,640)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse".into(),
            "-an".into(),
        ],
        _ => {
            let video = probe(tools, src, "v:0")?;
            if video.is_none() {
                bail!("this file has no video stream");
            }
            let audio = probe(tools, src, "a:0")?;
            video_args(target, video.as_deref(), audio.as_deref())
        }
    };
    run_ffmpeg(tools, src, &args, out, progress)
}

pub fn convert_audio(
    tools: &Tools,
    src: &Path,
    target: &str,
    out: &Path,
    progress: &mut dyn FnMut(f32),
) -> Result<()> {
    let mut args: Vec<String> = vec!["-vn".into(), "-map_metadata".into(), "0".into()];
    args.extend(audio_args(target).iter().map(|arg| arg.to_string()));
    run_ffmpeg(tools, src, &args, out, progress)
}

pub fn convert_image(tools: &Tools, src: &Path, target: &str, out: &Path) -> Result<()> {
    let magick = tools.magick()?;
    let mut command: Command = hidden(&magick);
    // [0] takes the first frame of multi-image inputs such as ICO.
    let mut input = src.as_os_str().to_os_string();
    input.push("[0]");
    command.arg(input).arg("-auto-orient");
    if matches!(target, "jpg" | "bmp" | "pdf") {
        command.args(["-background", "white", "-alpha", "remove", "-alpha", "off"]);
    }
    match target {
        "jpg" | "webp" | "avif" => {
            command.args(["-quality", "92"]);
        }
        "pdf" => {
            command.args(["-density", "300"]);
        }
        _ => {}
    }
    run(command.arg(out))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::video_args;

    #[test]
    fn matching_streams_are_copied() {
        let args = video_args("mp4", Some("h264"), Some("aac")).join(" ");
        assert!(args.contains("-c:v copy") && args.contains("-c:a copy"));
    }

    #[test]
    fn webm_always_reencodes() {
        let args = video_args("webm", Some("h264"), Some("aac")).join(" ");
        assert!(args.contains("libvpx-vp9") && args.contains("libopus"));
    }
}
