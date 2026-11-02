//! Compress to a target, the metadata editor and QR code reading.

use crate::formats::{extension_of, kind_of, Kind};
use crate::tools::{hidden, run, Tools};
use crate::{heic, media, unique_path};
use anyhow::{anyhow, bail, Result};
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use little_exif::rational::uR64;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const RAW: &[&str] = &["cr2", "cr3", "nef", "arw", "dng", "orf", "rw2", "raf"];
const QUALITY_STEPS: u32 = 7;

/// How small to make a file. Either target wins over the other only if both are set:
/// a size in megabytes is used before a percentage.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressPlan {
    pub percent: Option<f64>,
    pub target_mb: Option<f64>,
    /// Longest side in pixels for images and video.
    pub max_side: Option<u32>,
}

impl CompressPlan {
    fn target_bytes(&self, before: u64) -> Option<u64> {
        match (self.target_mb, self.percent) {
            (Some(megabytes), _) => Some((megabytes * 1_000_000.0) as u64),
            (None, Some(percent)) => Some((before as f64 * percent / 100.0) as u64),
            _ => None,
        }
    }
}

fn input(src: &Path) -> std::ffi::OsString {
    let mut value = src.as_os_str().to_os_string();
    value.push("[0]");
    value
}

fn encode_image(tools: &Tools, src: &Path, out: &Path, ext: &str, quality: u32, plan: &CompressPlan, work: &Path) -> Result<()> {
    if ext == "heic" {
        return heic::write_heic_at(tools, src, out, work, 51 - quality / 2);
    }
    let mut command = hidden(tools.magick()?);
    command.arg(input(src)).arg("-auto-orient");
    if let Some(side) = plan.max_side {
        command.args(["-resize", &format!("{side}x{side}>")]);
    }
    if matches!(ext, "png" | "tiff" | "bmp") {
        // No quality setting here, so quality scales the picture instead.
        command.args(["-resize", &format!("{quality}%")]);
        if ext == "png" {
            command.args(["-define", "png:compression-level=9"]);
        }
        if ext == "tiff" {
            command.args(["-compress", "zip"]);
        }
    } else {
        command.args(["-quality", &quality.to_string()]);
    }
    run(command.arg(out))?;
    Ok(())
}

/// Searches quality for the best result at or under the target.
fn compress_image(tools: &Tools, src: &Path, ext: &str, out: &Path, target: Option<u64>, plan: &CompressPlan, work: &Path) -> Result<()> {
    let Some(target) = target else {
        return encode_image(tools, src, out, ext, 70, plan, work);
    };
    let (mut low, mut high) = (5u32, 95u32);
    let trial = work.join(format!("trial.{}", out.extension().and_then(|e| e.to_str()).unwrap_or("jpg")));
    let mut best = false;
    for _ in 0..QUALITY_STEPS {
        let quality = (low + high) / 2;
        encode_image(tools, src, &trial, ext, quality, plan, work)?;
        if std::fs::metadata(&trial)?.len() <= target {
            std::fs::copy(&trial, out)?;
            best = true;
            low = quality + 1;
        } else {
            high = quality.saturating_sub(1);
        }
        if low > high {
            break;
        }
    }
    if !best {
        bail!("could not get this image down to that size");
    }
    Ok(())
}

fn scale_filter(plan: &CompressPlan) -> String {
    match plan.max_side {
        Some(side) => format!(
            "scale='if(gt(iw,ih),min({side},iw),-2)':'if(gt(iw,ih),-2,min({side},ih))',scale=trunc(iw/2)*2:trunc(ih/2)*2"
        ),
        None => "scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string(),
    }
}

fn video_bitrate(target: u64, seconds: f64) -> u64 {
    const AUDIO_BITS: f64 = 96_000.0;
    // Containers add a little overhead, so aim slightly under.
    let total = target as f64 * 8.0 * 0.95 / seconds.max(0.1);
    (total - AUDIO_BITS).max(50_000.0) as u64
}

fn audio_bitrate(target: u64, seconds: f64) -> u64 {
    let total = target as f64 * 8.0 * 0.97 / seconds.max(0.1);
    (total / 1000.0).clamp(24.0, 320.0) as u64
}

fn compress_video(tools: &Tools, src: &Path, out: &Path, target: Option<u64>, plan: &CompressPlan, progress: &mut dyn FnMut(f32)) -> Result<()> {
    let filter = scale_filter(plan);
    let quality: Vec<String> = match target {
        Some(bytes) => {
            let seconds = media::duration_seconds(tools, src).ok_or_else(|| anyhow!("unknown duration"))?;
            let rate = video_bitrate(bytes, seconds);
            vec!["-b:v".into(), format!("{rate}"), "-maxrate".into(), format!("{}", rate * 3 / 2), "-bufsize".into(), format!("{}", rate * 2)]
        }
        None => vec!["-crf".into(), "28".into()],
    };
    let mut args: Vec<String> = "-c:v libx264 -preset medium -pix_fmt yuv420p".split(' ').map(String::from).collect();
    args.extend(quality);
    args.extend(["-vf".to_string(), filter]);
    args.extend("-c:a aac -b:a 96k -movflags +faststart".split(' ').map(String::from));
    media::run_ffmpeg(tools, src, &args, out, progress)
}

fn compress_audio(tools: &Tools, src: &Path, ext: &str, out: &Path, target: Option<u64>, progress: &mut dyn FnMut(f32)) -> Result<()> {
    let seconds = media::duration_seconds(tools, src).ok_or_else(|| anyhow!("unknown duration"))?;
    let rate = target.map_or(128, |bytes| audio_bitrate(bytes, seconds));
    let codec = match ext {
        "mp3" => "libmp3lame",
        "m4a" | "aac" => "aac",
        "ogg" => "libvorbis",
        "opus" => "libopus",
        "wma" => "wmav2",
        "ac3" => "ac3",
        _ => bail!("{ext} files cannot be set to an exact size; use Compress or convert to MP3 first"),
    };
    let args: Vec<String> = ["-vn", "-c:a", codec, "-b:a", &format!("{rate}k")].iter().map(|a| a.to_string()).collect();
    media::run_ffmpeg(tools, src, &args, out, progress)
}

fn compress_pdf(tools: &Tools, src: &Path, out: &Path, target: Option<u64>) -> Result<()> {
    let qpdf = tools.qpdf()?;
    let mut smallest = u64::MAX;
    let trial = out.with_extension("trial.pdf");
    for quality in [80, 60, 40, 20] {
        run(hidden(&qpdf)
            .args(["--optimize-images", &format!("--jpeg-quality={quality}"), "--recompress-flate"])
            .args(["--compression-level=9", "--object-streams=generate"])
            .arg(src)
            .arg(&trial))?;
        let size = std::fs::metadata(&trial)?.len();
        if size < smallest {
            smallest = size;
            std::fs::copy(&trial, out)?;
        }
        if target.is_some_and(|bytes| size <= bytes) {
            break;
        }
    }
    std::fs::remove_file(&trial)?;
    Ok(())
}

/// Compresses one file toward a plan and returns the new file.
pub fn compress_to(tools: &Tools, src: &Path, plan: &CompressPlan, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let ext = extension_of(src).ok_or_else(|| anyhow!("{} has no extension", src.display()))?;
    let dir = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let work = tempfile::Builder::new().prefix(".transmute-").tempdir_in(dir)?;
    let kind = kind_of(&ext);
    let out_ext = match kind {
        Some(Kind::Video) => "mp4".to_string(),
        Some(Kind::Image) if RAW.contains(&ext.as_str()) => "jpg".to_string(),
        _ => ext.clone(),
    };
    let out = work.path().join(format!("output.{out_ext}"));
    let before = std::fs::metadata(src)?.len();
    let target = plan.target_bytes(before);
    match kind {
        Some(Kind::Image) => compress_image(tools, src, &out_ext, &out, target, plan, work.path())?,
        Some(Kind::Video) => compress_video(tools, src, &out, target, plan, progress)?,
        Some(Kind::Audio) => compress_audio(tools, src, &ext, &out, target, progress)?,
        Some(Kind::Pdf) => compress_pdf(tools, src, &out, target)?,
        _ => bail!("this file cannot be compressed"),
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = unique_path(dir, &format!("{stem} compressed"), &out_ext);
    std::fs::rename(&out, &path)?;
    Ok(path)
}

/// A named metadata value shown in the editor.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Field {
    pub name: String,
    pub value: String,
}

const MEDIA_FIELDS: &[&str] = &["title", "artist", "album", "genre", "date", "comment"];
const IMAGE_FIELDS: &[(&str, &str)] = &[
    ("Description", "ImageDescription"),
    ("Author", "Artist"),
    ("Copyright", "Copyright"),
    ("Camera make", "Make"),
    ("Camera model", "Model"),
    ("Date taken", "DateTimeOriginal"),
    ("Latitude", "GPSLatitude"),
    ("Longitude", "GPSLongitude"),
];

fn is_image(ext: &str) -> bool {
    matches!(ext, "jpg" | "png" | "webp" | "tiff")
}

/// Whether the metadata editor can write this file.
pub fn metadata_editable(path: &Path) -> bool {
    let ext = extension_of(path).unwrap_or_default();
    is_image(&ext) || matches!(kind_of(&ext), Some(Kind::Video) | Some(Kind::Audio))
}

/// Parses EXIF's "51/1, 30/1, 2400/100" into decimal degrees.
fn exif_degrees(text: &str, reference: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut divisor = 1.0;
    for part in text.split(',') {
        let (top, bottom) = part.trim().split_once('/')?;
        total += top.trim().parse::<f64>().ok()? / bottom.trim().parse::<f64>().ok()? / divisor;
        divisor *= 60.0;
    }
    Some(if matches!(reference, "S" | "W") { -total } else { total })
}

fn degrees_to_rationals(value: f64) -> Vec<uR64> {
    let value = value.abs();
    let degrees = value.floor();
    let minutes = ((value - degrees) * 60.0).floor();
    let seconds = (value - degrees - minutes / 60.0) * 3600.0;
    vec![
        uR64 { nominator: degrees as u32, denominator: 1 },
        uR64 { nominator: minutes as u32, denominator: 1 },
        uR64 { nominator: (seconds * 10_000.0).round() as u32, denominator: 10_000 },
    ]
}

/// Current metadata for the editor.
pub fn read_metadata(tools: &Tools, src: &Path) -> Result<Vec<Field>> {
    let ext = extension_of(src).unwrap_or_default();
    if is_image(&ext) {
        let exif = |tag: &str| -> String {
            run(hidden(tools.magick().unwrap_or_default()).args(["identify", "-format", &format!("%[EXIF:{tag}]")]).arg(input(src)))
                .map(|out| String::from_utf8_lossy(&out).trim().to_string())
                .unwrap_or_default()
        };
        return Ok(IMAGE_FIELDS
            .iter()
            .map(|(name, tag)| {
                let raw = exif(tag);
                let value = match *tag {
                    "GPSLatitude" | "GPSLongitude" => {
                        exif_degrees(&raw, &exif(&format!("{tag}Ref"))).map_or(String::new(), |v| format!("{v:.6}"))
                    }
                    _ => raw,
                };
                Field { name: name.to_string(), value }
            })
            .collect());
    }
    Ok(MEDIA_FIELDS
        .iter()
        .map(|name| {
            let entry = format!("format_tags={name}");
            let value = run(hidden(tools.ffprobe().unwrap_or_default())
                .args(["-v", "error", "-show_entries", &entry, "-of", "default=nw=1:nk=1"])
                .arg(src))
            .map(|out| String::from_utf8_lossy(&out).trim().to_string())
            .unwrap_or_default();
            Field { name: name.to_string(), value }
        })
        .collect())
}

fn set_image_tags(path: &Path, fields: &[Field]) -> Result<()> {
    let mut metadata = Metadata::new_from_path(path).unwrap_or_else(|_| Metadata::new());
    let mut latitude = None;
    let mut longitude = None;
    for field in fields {
        let value = field.value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        match field.name.as_str() {
            "Description" => metadata.set_tag(ExifTag::ImageDescription(value)),
            "Author" => metadata.set_tag(ExifTag::Artist(value)),
            "Copyright" => metadata.set_tag(ExifTag::Copyright(value)),
            "Camera make" => metadata.set_tag(ExifTag::Make(value)),
            "Camera model" => metadata.set_tag(ExifTag::Model(value)),
            "Date taken" => metadata.set_tag(ExifTag::DateTimeOriginal(value)),
            "Latitude" => latitude = value.parse::<f64>().ok().filter(|v| v.abs() <= 90.0).or_else(|| Some(f64::NAN)),
            "Longitude" => longitude = value.parse::<f64>().ok().filter(|v| v.abs() <= 180.0).or_else(|| Some(f64::NAN)),
            _ => {}
        }
    }
    if latitude.is_some_and(f64::is_nan) || longitude.is_some_and(f64::is_nan) {
        bail!("latitude and longitude must be decimal degrees, such as 51.507 and -0.128");
    }
    if let Some(value) = latitude {
        metadata.set_tag(ExifTag::GPSLatitudeRef(if value < 0.0 { "S" } else { "N" }.to_string()));
        metadata.set_tag(ExifTag::GPSLatitude(degrees_to_rationals(value)));
    }
    if let Some(value) = longitude {
        metadata.set_tag(ExifTag::GPSLongitudeRef(if value < 0.0 { "W" } else { "E" }.to_string()));
        metadata.set_tag(ExifTag::GPSLongitude(degrees_to_rationals(value)));
    }
    metadata.write_to_file(path).map_err(|error| anyhow!("could not write the metadata: {error}"))
}

/// Writes metadata into a new copy. Streams are copied, so quality does not change.
pub fn write_metadata(tools: &Tools, src: &Path, fields: &[Field], progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let ext = extension_of(src).ok_or_else(|| anyhow!("{} has no extension", src.display()))?;
    let dir = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let work = tempfile::Builder::new().prefix(".transmute-").tempdir_in(dir)?;
    let out = work.path().join(format!("output.{ext}"));
    if is_image(&ext) {
        std::fs::copy(src, &out)?;
        set_image_tags(&out, fields)?;
    } else {
        let mut args: Vec<String> = "-map 0 -c copy".split(' ').map(String::from).collect();
        for field in fields {
            args.extend(["-metadata".to_string(), format!("{}={}", field.name, field.value)]);
        }
        media::run_ffmpeg(tools, src, &args, &out, progress)?;
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = unique_path(dir, &format!("{stem} tagged"), &ext);
    std::fs::rename(&out, &path)?;
    Ok(path)
}

/// Every QR code readable in an image.
pub fn read_qr(tools: &Tools, src: &Path) -> Result<Vec<String>> {
    let size = run(hidden(tools.magick()?).args(["identify", "-format", "%w %h"]).arg(input(src)))?;
    let size = String::from_utf8_lossy(&size);
    let mut parts = size.split_whitespace().map(|part| part.parse::<usize>());
    let (Some(Ok(width)), Some(Ok(height))) = (parts.next(), parts.next()) else {
        bail!("could not read the image size");
    };
    let pixels = run(hidden(tools.magick()?)
        .arg(input(src))
        .args(["-background", "white", "-alpha", "remove", "-colorspace", "Gray", "-depth", "8", "gray:-"]))?;
    if pixels.len() < width * height {
        bail!("could not read the image");
    }
    let mut image = rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    let found: Vec<String> = image.detect_grids().iter().filter_map(|grid| grid.decode().ok()).map(|(_, text)| text).collect();
    if found.is_empty() {
        bail!("no QR code found in this image");
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_beats_a_percentage() {
        let plan = CompressPlan { percent: Some(50.0), target_mb: Some(2.0), max_side: None };
        assert_eq!(plan.target_bytes(10_000_000), Some(2_000_000));
        let plan = CompressPlan { percent: Some(25.0), ..Default::default() };
        assert_eq!(plan.target_bytes(1000), Some(250));
        assert_eq!(CompressPlan::default().target_bytes(1000), None);
    }

    #[test]
    fn bitrates_fit_the_target() {
        // 10 MB over 100 seconds is 800 kbps in total, less margin and audio.
        assert_eq!(video_bitrate(10_000_000, 100.0), 664_000);
        assert_eq!(audio_bitrate(1_000_000, 100.0), 77);
        assert_eq!(audio_bitrate(100_000_000, 10.0), 320);
    }

    #[test]
    fn degrees_survive_the_round_trip() {
        let parts = degrees_to_rationals(51.5074);
        let text: Vec<String> = parts.iter().map(|p| format!("{}/{}", p.nominator, p.denominator)).collect();
        let back = exif_degrees(&text.join(", "), "N").unwrap();
        assert!((back - 51.5074).abs() < 1e-6, "{back}");
        assert!(exif_degrees("10/1, 0/1, 0/1", "S").unwrap() < 0.0);
    }
}
