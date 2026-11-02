//! Photo editing and PDF page organizing, driven by the editor window. Everything
//! goes through ImageMagick or qpdf and produces a new file beside the original.

use crate::formats::extension_of;
use crate::tools::{hidden, run, Tools};
use crate::unique_path;
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Forward slashes, because ImageMagick treats backslashes in a font path as escapes.
const ANNOTATION_FONT: &str = "C:/Windows/Fonts/arial.ttf";

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub blur: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Shape {
    Arrow { x1: f64, y1: f64, x2: f64, y2: f64, color: String },
    Box { x1: f64, y1: f64, x2: f64, y2: f64, color: String },
    Ellipse { x1: f64, y1: f64, x2: f64, y2: f64, color: String },
    Highlight { x1: f64, y1: f64, x2: f64, y2: f64 },
    Text { x: f64, y: f64, text: String, color: String, size: u32 },
    Number { x: f64, y: f64, value: u32, color: String, size: u32 },
    Pen { points: Vec<(f64, f64)>, color: String },
    Magnifier { x: f64, y: f64, radius: u32, zoom: f64 },
    Picture { x: f64, y: f64, width: u32, path: PathBuf },
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct PageChoice {
    /// 1-based page in the source PDF.
    pub page: u32,
    /// Clockwise rotation: 0, 90, 180 or 270.
    pub rotate: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "tool", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum PhotoEdit {
    CropImage { x: u32, y: u32, width: u32, height: u32 },
    ResizeImage { width: u32, height: u32 },
    AdjustImage {
        brightness: i32,
        contrast: i32,
        saturation: i32,
        hue: i32,
        sharpen: f64,
        blur: f64,
        grain: f64,
        #[serde(default)]
        dehaze: f64,
        #[serde(default)]
        clarity: f64,
        #[serde(default)]
        denoise: f64,
    },
    BackgroundImage {
        color: String,
        image: Option<PathBuf>,
        ratio: f64,
        padding: u32,
        radius: u32,
        shadow: bool,
        #[serde(default)]
        backdrop_blur: f64,
    },
    RedactImage { regions: Vec<Region> },
    AnnotateImage { shapes: Vec<Shape> },
    OrganizePdf { pages: Vec<PageChoice> },
}

fn is_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Colours reach ImageMagick as draw commands, so only plain #rrggbb gets through.
fn checked_color(value: &str) -> Result<&str> {
    if is_color(value) {
        Ok(value)
    } else {
        bail!("not a colour: {value}")
    }
}

/// Formats ImageMagick writes directly stay as they are; everything else, such as
/// HEIC or camera RAW, saves as JPG.
fn image_out_ext(ext: &str) -> &str {
    if matches!(ext, "jpg" | "png" | "webp" | "tiff" | "avif" | "bmp") {
        ext
    } else {
        "jpg"
    }
}

fn input(src: &Path) -> std::ffi::OsString {
    let mut value = src.as_os_str().to_os_string();
    value.push("[0]");
    value
}

pub fn image_size(tools: &Tools, src: &Path) -> Result<(u32, u32)> {
    let out = run(hidden(tools.magick()?).args(["identify", "-format", "%w %h"]).arg(input(src)))?;
    let text = String::from_utf8_lossy(&out);
    let mut parts = text.split_whitespace().map(|part| part.parse::<u32>());
    match (parts.next(), parts.next()) {
        (Some(Ok(width)), Some(Ok(height))) => Ok((width, height)),
        _ => bail!("could not read the image size"),
    }
}

/// Adds the commands that apply one redaction to the image on the stack.
fn push_region(command: &mut Command, region: &Region) {
    let geometry = format!("{}x{}+{}+{}", region.width, region.height, region.x, region.y);
    if region.blur {
        command.args(["(", "+clone", "-crop", &geometry, "+repage", "-blur", "0x14", ")"]);
        command.args(["-geometry", &format!("+{}+{}", region.x, region.y), "-composite"]);
    } else {
        let (x2, y2) = (region.x + region.width, region.y + region.height);
        command.args(["-fill", "black", "-stroke", "none"]);
        command.args(["-draw", &format!("rectangle {},{} {x2},{y2}", region.x, region.y)]);
    }
}

/// -annotate expands % codes and backslashes, so the words are escaped to print as typed.
fn literal_text(text: &str) -> String {
    text.replace('\\', "\\\\").replace('%', "%%")
}

fn arrow_head(x1: f64, y1: f64, x2: f64, y2: f64, size: f64) -> String {
    let angle = (y2 - y1).atan2(x2 - x1);
    let spread = 0.45;
    let (ax, ay) = (x2 - size * (angle - spread).cos(), y2 - size * (angle - spread).sin());
    let (bx, by) = (x2 - size * (angle + spread).cos(), y2 - size * (angle + spread).sin());
    format!("polygon {x2:.1},{y2:.1} {ax:.1},{ay:.1} {bx:.1},{by:.1}")
}

fn push_shape(command: &mut Command, shape: &Shape, stroke: u32) -> Result<()> {
    let line = stroke.to_string();
    match shape {
        Shape::Arrow { x1, y1, x2, y2, color } => {
            let color = checked_color(color)?;
            command.args(["-stroke", color, "-strokewidth", &line, "-fill", color]);
            command.args(["-draw", &format!("line {x1:.1},{y1:.1} {x2:.1},{y2:.1}")]);
            command.args(["-draw", &arrow_head(*x1, *y1, *x2, *y2, stroke as f64 * 4.0)]);
        }
        Shape::Box { x1, y1, x2, y2, color } => {
            let color = checked_color(color)?;
            command.args(["-stroke", color, "-strokewidth", &line, "-fill", "none"]);
            command.args(["-draw", &format!("rectangle {x1:.1},{y1:.1} {x2:.1},{y2:.1}")]);
        }
        Shape::Ellipse { x1, y1, x2, y2, color } => {
            let color = checked_color(color)?;
            let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
            let (rx, ry) = ((x2 - x1).abs() / 2.0, (y2 - y1).abs() / 2.0);
            command.args(["-stroke", color, "-strokewidth", &line, "-fill", "none"]);
            command.args(["-draw", &format!("ellipse {cx:.1},{cy:.1} {rx:.1},{ry:.1} 0,360")]);
        }
        Shape::Highlight { x1, y1, x2, y2 } => {
            command.args(["-stroke", "none", "-fill", "#ffe60066"]);
            command.args(["-draw", &format!("rectangle {x1:.1},{y1:.1} {x2:.1},{y2:.1}")]);
        }
        Shape::Text { x, y, text, color, size } => {
            let color = checked_color(color)?;
            command.args(["-font", ANNOTATION_FONT, "-stroke", "none", "-fill", color]);
            command.args(["-pointsize", &size.to_string()]);
            // -annotate takes the text as its own argument, so it is never drawn as a command.
            command.args(["-annotate", &format!("+{x:.0}+{y:.0}"), &literal_text(text)]);
        }
        Shape::Number { x, y, value, color, size } => {
            let color = checked_color(color)?;
            let radius = *size as f64 * 0.75;
            command.args(["-stroke", "none", "-fill", color]);
            command.args(["-draw", &format!("circle {x:.1},{y:.1} {:.1},{y:.1}", x + radius)]);
            command.args(["-font", ANNOTATION_FONT, "-fill", "white", "-pointsize", &size.to_string()]);
            let (dx, dy) = (*size as f64 * 0.28 * value.to_string().len() as f64, *size as f64 * 0.36);
            command.args(["-annotate", &format!("+{:.0}+{:.0}", x - dx, y + dy), &value.to_string()]);
        }
        Shape::Pen { points, color } => {
            let color = checked_color(color)?;
            let path: Vec<String> = points.iter().map(|(x, y)| format!("{x:.1},{y:.1}")).collect();
            if path.len() >= 2 {
                command.args(["-stroke", color, "-strokewidth", &line, "-fill", "none"]);
                command.args(["-draw", &format!("polyline {}", path.join(" "))]);
            }
        }
        Shape::Magnifier { x, y, radius, zoom } => {
            let side = radius * 2;
            let source = ((side as f64 / zoom.max(1.0)).round() as u32).max(2);
            let left = (x - source as f64 / 2.0).max(0.0);
            let top = (y - source as f64 / 2.0).max(0.0);
            let center = *radius as f64;
            let circle = format!("circle {center},{center} {center},0");
            command.args(["(", "+clone", "-crop", &format!("{source}x{source}+{left:.0}+{top:.0}"), "+repage"]);
            command.args(["-resize", &format!("{side}x{side}!"), "-alpha", "set"]);
            command.args(["(", "-size", &format!("{side}x{side}"), "xc:black", "-fill", "white", "-stroke", "none"]);
            command.args(["-draw", &circle, ")", "-alpha", "off", "-compose", "CopyOpacity", "-composite", ")"]);
            let at = format!("+{:.0}+{:.0}", x - center, y - center);
            command.args(["-compose", "Over", "-geometry", &at, "-composite"]);
            command.args(["-stroke", "white", "-strokewidth", &line, "-fill", "none"]);
            command.args(["-draw", &format!("circle {x:.1},{y:.1} {:.1},{y:.1}", x + center)]);
        }
        Shape::Picture { x, y, width, path } => {
            command.args(["("]).arg(input(path)).args(["-resize", &format!("{width}x"), ")"]);
            command.args(["-geometry", &format!("+{x:.0}+{y:.0}"), "-composite"]);
        }
    }
    Ok(())
}

/// The canvas size that fits an image plus padding at an aspect ratio. A ratio of
/// zero keeps the image's own shape.
fn background_canvas(width: u32, height: u32, ratio: f64, padding: u32) -> (u32, u32) {
    let (mut canvas_width, mut canvas_height) = (width + padding * 2, height + padding * 2);
    if ratio > 0.0 {
        if (canvas_width as f64) / (canvas_height as f64) < ratio {
            canvas_width = (canvas_height as f64 * ratio).round() as u32;
        } else {
            canvas_height = (canvas_width as f64 / ratio).round() as u32;
        }
    }
    (canvas_width, canvas_height)
}

fn run_background(
    tools: &Tools,
    src: &Path,
    out: &Path,
    (color, image, ratio, padding, radius, shadow, backdrop_blur): (&str, &Option<PathBuf>, f64, u32, u32, bool, f64),
) -> Result<()> {
    let (width, height) = image_size(tools, src)?;
    let (canvas_width, canvas_height) = background_canvas(width, height, ratio, padding);
    let size = format!("{canvas_width}x{canvas_height}");
    let mut command = hidden(tools.magick()?);
    match image {
        Some(backdrop) => {
            command.arg(input(backdrop)).args(["-resize", &format!("{size}^")]);
            command.args(["-gravity", "center", "-extent", &size]);
            if backdrop_blur > 0.0 {
                command.args(["-blur", &format!("0x{backdrop_blur}")]);
            }
        }
        None => {
            command.args(["-size", &size]).arg(format!("xc:{}", checked_color(color)?));
        }
    }
    command.args(["(", "("]).arg(input(src)).args(["-alpha", "set", ")"]);
    if radius > 0 {
        let corners = format!("roundrectangle 0,0 {},{} {radius},{radius}", width - 1, height - 1);
        command.args(["(", "-size", &format!("{width}x{height}"), "xc:black", "-fill", "white", "-stroke", "none"]);
        command.args(["-draw", &corners, ")", "-alpha", "off", "-compose", "CopyOpacity", "-composite"]);
        command.args(["-compose", "Over"]);
    }
    if shadow {
        command.args(["(", "+clone", "-background", "black", "-shadow", "60x24+0+14", ")"]);
        command.args(["+swap", "-background", "none", "-layers", "merge", "+repage"]);
    }
    command.args([")", "-gravity", "center", "-composite"]);
    run(command.arg(out))?;
    Ok(())
}

fn organize_pdf(tools: &Tools, src: &Path, pages: &[PageChoice], out: &Path, work: &Path) -> Result<()> {
    if pages.is_empty() {
        bail!("keep at least one page");
    }
    if pages.iter().any(|page| page.page == 0 || page.rotate % 90 != 0) {
        bail!("invalid page selection");
    }
    let order: Vec<String> = pages.iter().map(|page| page.page.to_string()).collect();
    let arranged = work.join("arranged.pdf");
    run(hidden(tools.qpdf()?).args(["--empty", "--pages"]).arg(src).arg(order.join(",")).arg("--").arg(&arranged))?;
    let rotations: Vec<String> = pages
        .iter()
        .enumerate()
        .filter(|(_, page)| page.rotate % 360 != 0)
        .map(|(index, page)| format!("--rotate=+{}:{}", page.rotate % 360, index + 1))
        .collect();
    if rotations.is_empty() {
        std::fs::rename(&arranged, out)?;
    } else {
        run(hidden(tools.qpdf()?).args(&rotations).arg(&arranged).arg(out))?;
    }
    Ok(())
}

/// Runs one photo or PDF edit and returns the new file.
pub fn apply(tools: &Tools, src: &Path, edit: &PhotoEdit) -> Result<PathBuf> {
    let ext = extension_of(src).ok_or_else(|| anyhow!("{} has no extension", src.display()))?;
    let dir = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let work = tempfile::Builder::new().prefix(".transmute-").tempdir_in(dir)?;
    let out_ext = if matches!(edit, PhotoEdit::OrganizePdf { .. }) { "pdf" } else { image_out_ext(&ext) };
    let out = work.path().join(format!("output.{out_ext}"));

    let mut command = hidden(tools.magick()?);
    command.arg(input(src)).arg("-auto-orient");
    let suffix = match edit {
        PhotoEdit::CropImage { x, y, width, height } => {
            command.args(["-crop", &format!("{width}x{height}+{x}+{y}"), "+repage"]);
            "cropped"
        }
        PhotoEdit::ResizeImage { width, height } => {
            if *width == 0 || *height == 0 {
                bail!("the size must not be zero");
            }
            command.args(["-resize", &format!("{width}x{height}!")]);
            "resized"
        }
        PhotoEdit::AdjustImage { brightness, contrast, saturation, hue, sharpen, blur, grain, dehaze, clarity, denoise } => {
            if *denoise > 0.0 {
                command.args(["-wavelet-denoise", &format!("{denoise}%")]);
            }
            let modulate = format!("{},{},{}", 100 + brightness, 100 + saturation, 100 + hue * 100 / 180);
            command.args(["-modulate", &modulate]);
            command.args(["-brightness-contrast", &format!("0x{contrast}")]);
            if *dehaze > 0.0 {
                // Haze lifts the shadows and lowers saturation, so stretch the range and restore colour.
                let stretch = dehaze * 0.15;
                command.args(["-contrast-stretch", &format!("{stretch}%x{stretch}%")]);
                command.args(["-modulate", &format!("100,{},100", 100.0 + dehaze * 0.3)]);
            }
            if *clarity > 0.0 {
                // A wide unsharp mask raises local contrast rather than fine detail.
                command.args(["-unsharp", &format!("0x30+{}+0", clarity / 100.0)]);
            }
            if *blur > 0.0 {
                command.args(["-blur", &format!("0x{blur}")]);
            }
            if *sharpen > 0.0 {
                command.args(["-sharpen", &format!("0x{sharpen}")]);
            }
            if *grain > 0.0 {
                command.args(["-attenuate", &grain.to_string(), "+noise", "Gaussian"]);
            }
            "edited"
        }
        PhotoEdit::BackgroundImage { color, image, ratio, padding, radius, shadow, backdrop_blur } => {
            run_background(tools, src, &out, (color, image, *ratio, *padding, *radius, *shadow, *backdrop_blur))?;
            "background"
        }
        PhotoEdit::RedactImage { regions } => {
            if regions.is_empty() {
                bail!("add at least one area");
            }
            for region in regions {
                push_region(&mut command, region);
            }
            "redacted"
        }
        PhotoEdit::AnnotateImage { shapes } => {
            if shapes.is_empty() {
                bail!("add at least one mark");
            }
            let (width, height) = image_size(tools, src)?;
            let stroke = (width.max(height) / 300).max(2);
            // A greyscale photo would otherwise turn coloured pictures grey.
            command.args(["-colorspace", "sRGB"]);
            for shape in shapes {
                push_shape(&mut command, shape, stroke)?;
            }
            "annotated"
        }
        PhotoEdit::OrganizePdf { pages } => {
            organize_pdf(tools, src, pages, &out, work.path())?;
            "organized"
        }
    };
    if !matches!(edit, PhotoEdit::BackgroundImage { .. } | PhotoEdit::OrganizePdf { .. }) {
        run(command.arg(&out))?;
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = unique_path(dir, &format!("{stem} {suffix}"), out_ext);
    std::fs::rename(&out, &path)?;
    Ok(path)
}

/// Optional finishing for a collage.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CollageStyle {
    /// Corner radius of each image in pixels.
    pub corners: u32,
    /// Final width in pixels, or 0 to keep the natural size.
    pub width: u32,
}

/// Lays several images out in one picture. `layout` is grid, row, column or featured.
/// Rows and columns keep each image's shape; grids and the featured layout crop to
/// fill their cells, so nothing floats in a band of background.
pub fn collage(tools: &Tools, paths: &[PathBuf], layout: &str, spacing: u32, background: &str, style: &CollageStyle) -> Result<PathBuf> {
    const CELL: u32 = 800;
    let first = paths.first().ok_or_else(|| anyhow!("nothing selected"))?;
    let dir = first.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let work = tempfile::Builder::new().prefix(".transmute-").tempdir_in(dir)?;
    let color = checked_color(background)?;
    let out = work.path().join("output.png");
    let gap = spacing.to_string();

    let resized = |image: &Path, name: String, geometry: &str| -> Result<PathBuf> {
        let cell = work.path().join(name);
        run(hidden(tools.magick()?).arg(input(image)).args(["-auto-orient", "-resize", geometry]).arg(&cell))?;
        Ok(cell)
    };
    let filled = |image: &Path, name: String, side: u32| -> Result<PathBuf> {
        let cell = work.path().join(name);
        let size = format!("{side}x{side}");
        run(hidden(tools.magick()?)
            .arg(input(image))
            .args(["-auto-orient", "-resize", &format!("{size}^"), "-gravity", "center", "-extent", &size])
            .arg(&cell))?;
        Ok(cell)
    };
    // Rounds a cell by flattening its corners onto the background colour.
    let round = |cell: &Path| -> Result<()> {
        if style.corners == 0 {
            return Ok(());
        }
        let (width, height) = image_size(tools, cell)?;
        let shape = format!("roundrectangle 0,0 {},{} {r},{r}", width - 1, height - 1, r = style.corners);
        let mut command = hidden(tools.magick()?);
        command.arg(cell).args(["-alpha", "set"]);
        command.args(["(", "-size", &format!("{width}x{height}"), "xc:black", "-fill", "white", "-stroke", "none"]);
        command.args(["-draw", &shape, ")", "-alpha", "off", "-compose", "CopyOpacity", "-composite"]);
        command.args(["-compose", "Over", "-background", color, "-flatten"]).arg(cell);
        run(&mut command)?;
        Ok(())
    };
    let joined = |cells: &[PathBuf], direction: &str, target: &Path| -> Result<()> {
        for cell in cells {
            if cell.file_name().is_some_and(|name| name.to_string_lossy().contains("cell") || name.to_string_lossy().contains("side") || name == "main.png") {
                round(cell)?;
            }
        }
        run(hidden(tools.magick()?)
            .args(cells)
            .args(["-background", color, "-gravity", "center", direction, &gap])
            .arg(target))?;
        Ok(())
    };

    let cells = |geometry: &str| -> Result<Vec<PathBuf>> {
        paths.iter().enumerate().map(|(i, image)| resized(image, format!("cell{i}.png"), geometry)).collect()
    };
    let count = paths.len();
    let border = |image: &Path| -> Result<()> {
        run(hidden(tools.magick()?)
            .arg(image)
            .args(["-bordercolor", color, "-border", &gap])
            .arg(&out))?;
        Ok(())
    };
    let inner = work.path().join("inner.png");
    match layout {
        "row" => joined(&cells(&format!("x{CELL}"))?, "+smush", &inner)?,
        "column" => joined(&cells(&format!("{CELL}x"))?, "-smush", &inner)?,
        "featured" => {
            if count < 2 {
                bail!("a featured collage needs at least two images");
            }
            let rest = count as u32 - 1;
            let side = (1200 - spacing * (rest - 1)) / rest;
            let main = filled(&paths[0], "main.png".into(), 1200)?;
            let others: Vec<PathBuf> = paths[1..]
                .iter()
                .enumerate()
                .map(|(i, image)| filled(image, format!("side{i}.png"), side))
                .collect::<Result<_>>()?;
            let column = work.path().join("column.png");
            joined(&others, "-smush", &column)?;
            joined(&[main, column], "+smush", &inner)?;
        }
        _ => {
            let columns = (count as f64).sqrt().ceil() as usize;
            let squares: Vec<PathBuf> = paths
                .iter()
                .enumerate()
                .map(|(i, image)| filled(image, format!("cell{i}.png"), CELL))
                .collect::<Result<_>>()?;
            let mut rows = Vec::new();
            for (index, chunk) in squares.chunks(columns).enumerate() {
                let row = work.path().join(format!("row{index}.png"));
                joined(chunk, "+smush", &row)?;
                rows.push(row);
            }
            joined(&rows, "-smush", &inner)?;
        }
    }
    border(&inner)?;
    if style.width > 0 {
        run(hidden(tools.magick()?).arg(&out).args(["-resize", &format!("{}x", style.width)]).arg(&out))?;
    }
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let path = unique_path(dir, &format!("{stem} collage"), "png");
    std::fs::rename(&out, &path)?;
    Ok(path)
}

/// What the editor window needs to show an image: browsers cannot show HEIC, RAW
/// or TIFF, so those get a temporary JPG at the same pixel size.
pub fn image_preview(tools: &Tools, src: &Path, cache: &Path) -> Result<PathBuf> {
    let ext = extension_of(src).unwrap_or_default();
    if matches!(ext.as_str(), "jpg" | "png" | "webp" | "avif" | "bmp" | "gif" | "svg") {
        return Ok(src.to_path_buf());
    }
    std::fs::create_dir_all(cache)?;
    let out = cache.join(format!("preview-{}.jpg", std::process::id()));
    run(hidden(tools.magick()?).arg(input(src)).args(["-auto-orient", "-quality", "90"]).arg(&out))?;
    Ok(out)
}

/// A copy for the detectors, no larger than they handle well. Returns its path and
/// the factor that maps its pixels back to the source.
pub fn detection_copy(tools: &Tools, src: &Path, out: &Path) -> Result<f64> {
    const MAX_SIDE: u32 = 2400;
    let (width, height) = image_size(tools, src)?;
    run(hidden(tools.magick()?)
        .arg(input(src))
        .args(["-auto-orient", "-resize", &format!("{MAX_SIDE}x{MAX_SIDE}>")])
        .arg(out))?;
    Ok(width.max(height).max(MAX_SIDE) as f64 / MAX_SIDE as f64)
}

/// Small thumbnails of every page, in page order, for the organizer.
pub fn pdf_thumbnails(tools: &Tools, src: &Path, cache: &Path) -> Result<Vec<PathBuf>> {
    let _ = std::fs::remove_dir_all(cache);
    std::fs::create_dir_all(cache)?;
    run(hidden(tools.poppler("pdftoppm.exe")?).args(["-r", "40", "-png"]).arg(src).arg(cache.join("page")))?;
    let mut pages: Vec<PathBuf> = std::fs::read_dir(cache)?.flatten().map(|entry| entry.path()).collect();
    pages.sort();
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_text_prints_as_typed() {
        assert_eq!(literal_text("100% off"), "100%% off");
        assert_eq!(literal_text(r"a\nb"), r"a\\nb");
    }

    #[test]
    fn colours_must_be_plain_hex() {
        assert!(checked_color("#ff8000").is_ok());
        assert!(checked_color("red").is_err());
        assert!(checked_color("#ff8000\" image over").is_err());
    }

    #[test]
    fn canvas_grows_to_the_ratio_and_never_crops_the_image() {
        assert_eq!(background_canvas(100, 100, 0.0, 10), (120, 120));
        assert_eq!(background_canvas(100, 100, 2.0, 0), (200, 100));
        assert_eq!(background_canvas(200, 100, 1.0, 0), (200, 200));
    }

    #[test]
    fn heic_and_raw_save_as_jpg() {
        assert_eq!(image_out_ext("png"), "png");
        assert_eq!(image_out_ext("heic"), "jpg");
        assert_eq!(image_out_ext("arw"), "jpg");
    }

    #[test]
    fn messages_from_the_editor_parse() {
        let edit: PhotoEdit = serde_json::from_str(
            r##"{"tool":"annotate-image","shapes":[{"kind":"arrow","x1":1,"y1":2,"x2":3,"y2":4,"color":"#ff0000"},{"kind":"text","x":1,"y":2,"text":"Hi","color":"#ff0000","size":24},{"kind":"highlight","x1":1,"y1":2,"x2":3,"y2":4}]}"##,
        )
        .unwrap();
        assert!(matches!(edit, PhotoEdit::AnnotateImage { shapes } if shapes.len() == 3));
        let edit: PhotoEdit =
            serde_json::from_str(r#"{"tool":"organize-pdf","pages":[{"page":2,"rotate":90}]}"#).unwrap();
        assert!(matches!(edit, PhotoEdit::OrganizePdf { .. }));
    }
}
