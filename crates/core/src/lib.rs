//! The conversion engine. Everything happens in a temporary folder beside the
//! source and is moved into place only on success, so a failed conversion leaves
//! nothing behind and an original is never touched.

mod archives;
mod documents;
pub mod edits;
pub mod extras;
pub mod filetools;
pub mod formats;
mod heic;
pub mod image_edits;
mod media;
mod office;
mod subtitles;
pub mod tools;

use anyhow::{anyhow, bail, Context, Result};
use formats::{extension_of, kind_of, Kind};
use std::path::{Path, PathBuf};
pub use tools::Tools;

/// `dir/stem.ext`, or `dir/stem (1).ext` and so on if that is already taken.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let name = |suffix: String| {
        let file = if ext.is_empty() { format!("{stem}{suffix}") } else { format!("{stem}{suffix}.{ext}") };
        dir.join(file)
    };
    let mut candidate = name(String::new());
    let mut counter = 1;
    while candidate.exists() {
        candidate = name(format!(" ({counter})"));
        counter += 1;
    }
    candidate
}

/// The base name with archive double extensions removed.
fn output_stem(src: &Path) -> String {
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    stem.strip_suffix(".tar").map(str::to_string).unwrap_or(stem)
}

fn output_ext(target: &str) -> &str {
    if target == "gz" {
        "tar.gz"
    } else {
        target
    }
}

/// Saves page images: a lone page as a file, several in their own folder.
fn publish_pages(pages: Vec<PathBuf>, pages_dir: &Path, parent: &Path, stem: &str, target: &str) -> Result<PathBuf> {
    match pages.as_slice() {
        [] => bail!("the document produced no pages"),
        [only] => {
            let path = unique_path(parent, stem, target);
            std::fs::rename(only, &path)?;
            Ok(path)
        }
        _ => {
            let folder = unique_path(parent, &format!("{stem} ({})", target.to_ascii_uppercase()), "");
            std::fs::rename(pages_dir, &folder)?;
            Ok(folder)
        }
    }
}

/// Converts one file and returns the new file or folder.
/// `progress` receives 0.0 to 1.0 when the engine can report it.
pub fn convert(
    tools: &Tools,
    src: &Path,
    target: &str,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf> {
    let ext = extension_of(src).ok_or_else(|| anyhow!("{} has no extension", src.display()))?;
    let kind = kind_of(&ext).ok_or_else(|| anyhow!("{ext} files are not supported"))?;
    if !formats::targets_for_ext(&ext).contains(&target) {
        bail!("cannot convert {ext} to {target}");
    }
    let parent = src.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let work = tempfile::Builder::new()
        .prefix(".transmute-")
        .tempdir_in(parent)
        .with_context(|| format!("cannot write to {}", parent.display()))?;
    let stem = output_stem(src);
    let out = work.path().join(format!("output.{}", output_ext(target)));

    let renders_pages = matches!(target, "jpg" | "png")
        && matches!(kind, Kind::Pdf | Kind::Document | Kind::Slides);
    if renders_pages {
        let pdf = if kind == Kind::Pdf {
            src.to_path_buf()
        } else {
            let pdf = work.path().join("pages-source.pdf");
            documents::to_pdf(tools, src, &ext, &pdf, work.path())?;
            pdf
        };
        let pages_dir = work.path().join("pages");
        let pages = documents::pdf_pages(tools, &pdf, target, &pages_dir)?;
        return publish_pages(pages, &pages_dir, parent, &stem, target);
    }

    match kind {
        Kind::Image if target == "heic" => heic::write_heic(tools, src, &out, work.path())?,
        Kind::Image => media::convert_image(tools, src, target, &out)?,
        Kind::Video => media::convert_video(tools, src, target, &out, progress)?,
        Kind::Audio => media::convert_audio(tools, src, target, &out, progress)?,
        Kind::Pdf => documents::convert_pdf(tools, src, target, &out, work.path())?,
        Kind::Document | Kind::Slides | Kind::Sheet if target == "pdf" => {
            documents::to_pdf(tools, src, &ext, &out, work.path())?
        }
        Kind::Document => documents::convert_document(tools, src, &ext, target, &out, work.path())?,
        Kind::Slides => bail!("cannot convert slides to {target}"),
        Kind::Sheet => documents::convert_sheet(tools, src, &ext, target, &out, work.path())?,
        Kind::Subtitle => subtitles::convert(src, target, &out)?,
        Kind::Archive => archives::convert(tools, src, target, &out, work.path())?,
    }

    if !out.is_file() || std::fs::metadata(&out)?.len() == 0 {
        bail!("the conversion produced no output");
    }
    let path = unique_path(parent, &stem, output_ext(target));
    std::fs::rename(&out, &path)?;
    progress(1.0);
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_path_never_reuses_a_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a", "png"), dir.path().join("a.png"));
        std::fs::write(dir.path().join("a.png"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a", "png"), dir.path().join("a (1).png"));
        std::fs::write(dir.path().join("a (1).png"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a", "png"), dir.path().join("a (2).png"));
    }

    #[test]
    fn tar_gz_stem_drops_the_inner_extension() {
        assert_eq!(output_stem(Path::new("backup.tar.gz")), "backup");
        assert_eq!(output_stem(Path::new("photo.png")), "photo");
    }

    #[test]
    fn failed_conversion_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("notes.srt");
        std::fs::write(&src, b"x").unwrap();
        // Unknown target is refused before any work starts.
        assert!(convert(&Tools::at("."), &src, "mp3", &mut |_| {}).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
