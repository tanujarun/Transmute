//! The conversion matrix: which output formats each input format offers.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
    Pdf,
    Document,
    Slides,
    Sheet,
    Subtitle,
    Archive,
}

const IMAGE_OUT: &[&str] = &["jpg", "png", "webp", "heic", "tiff", "avif", "bmp", "pdf"];
const VIDEO_OUT: &[&str] =
    &["mp4", "mov", "mkv", "webm", "avi", "wmv", "gif", "flv", "m4v", "mpg", "mp3"];
const AUDIO_OUT: &[&str] =
    &["mp3", "m4a", "wav", "flac", "ogg", "opus", "aiff", "wma", "aac", "ac3"];
const PDF_OUT: &[&str] = &["docx", "pptx", "xlsx", "jpg", "png", "epub", "txt", "md"];
const DOCUMENT_OUT: &[&str] =
    &["docx", "doc", "rtf", "md", "txt", "epub", "html", "pdf", "jpg", "png"];
const SLIDES_OUT: &[&str] = &["pdf", "jpg", "png"];
const SHEET_OUT: &[&str] = &["xlsx", "ods", "csv", "tsv", "pdf"];
const SUBTITLE_OUT: &[&str] = &["srt", "vtt", "txt"];
const ARCHIVE_OUT: &[&str] = &["zip", "tar", "gz", "7z"];

/// Folds spelling variants so every format has one canonical extension.
pub fn canonical(ext: &str) -> String {
    let lower = ext.to_ascii_lowercase();
    match lower.as_str() {
        "jpeg" | "jpe" => "jpg",
        "tif" => "tiff",
        "heif" => "heic",
        "aif" => "aiff",
        "htm" => "html",
        "markdown" => "md",
        "mpeg" => "mpg",
        "tgz" => "gz",
        other => other,
    }
    .to_string()
}

pub fn kind_of(ext: &str) -> Option<Kind> {
    Some(match ext {
        "jpg" | "png" | "webp" | "heic" | "tiff" | "avif" | "bmp" | "svg" | "ico" => Kind::Image,
        "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" | "raf" => Kind::Image,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "gif" | "flv" | "m4v" | "mpg" => Kind::Video,
        "mp3" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aiff" | "wma" | "aac" | "ac3" => Kind::Audio,
        "pdf" => Kind::Pdf,
        "docx" | "doc" | "rtf" | "md" | "txt" | "epub" | "html" => Kind::Document,
        "ppt" | "pptx" => Kind::Slides,
        "xlsx" | "xls" | "ods" | "csv" | "tsv" => Kind::Sheet,
        "srt" | "vtt" => Kind::Subtitle,
        "zip" | "tar" | "gz" | "7z" | "rar" => Kind::Archive,
        _ => return None,
    })
}

pub fn extension_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(canonical)
}

/// Output formats for one input extension, in wheel order, excluding itself.
pub fn targets_for_ext(ext: &str) -> Vec<&'static str> {
    let all = match kind_of(ext) {
        Some(Kind::Image) => IMAGE_OUT,
        Some(Kind::Video) => VIDEO_OUT,
        Some(Kind::Audio) => AUDIO_OUT,
        Some(Kind::Pdf) => PDF_OUT,
        Some(Kind::Document) => DOCUMENT_OUT,
        Some(Kind::Slides) => SLIDES_OUT,
        Some(Kind::Sheet) => SHEET_OUT,
        Some(Kind::Subtitle) => SUBTITLE_OUT,
        Some(Kind::Archive) => ARCHIVE_OUT,
        None => &[],
    };
    // A GIF has no sound to export.
    all.iter().copied().filter(|target| *target != ext && !(ext == "gif" && *target == "mp3")).collect()
}

/// Targets every dragged file can reach. Folders and unknown files have none.
pub fn targets_for(paths: &[impl AsRef<Path>]) -> Vec<&'static str> {
    let mut common: Option<Vec<&'static str>> = None;
    for path in paths {
        let path = path.as_ref();
        if path.is_dir() {
            return Vec::new();
        }
        let own = extension_of(path).map(|ext| targets_for_ext(&ext)).unwrap_or_default();
        common = Some(match common {
            None => own,
            Some(previous) => previous.into_iter().filter(|t| own.contains(t)).collect(),
        });
    }
    common.unwrap_or_default()
}

/// The wheel label for a target extension.
pub fn label(target: &str) -> String {
    match target {
        "gz" => "GZIP".into(),
        "md" => "MD".into(),
        other => other.to_ascii_uppercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn spelling_variants_fold() {
        assert_eq!(canonical("JPEG"), "jpg");
        assert_eq!(canonical("TIF"), "tiff");
        assert_eq!(canonical("tgz"), "gz");
    }

    #[test]
    fn a_format_never_targets_itself() {
        assert!(!targets_for_ext("png").contains(&"png"));
        assert!(!targets_for_ext("mp4").contains(&"mp4"));
    }

    #[test]
    fn input_only_formats_are_not_targets() {
        assert!(!targets_for_ext("png").contains(&"svg"));
        assert!(!targets_for_ext("zip").contains(&"rar"));
        assert!(!targets_for_ext("xlsx").contains(&"xls"));
    }

    #[test]
    fn gif_does_not_offer_audio() {
        assert!(!targets_for_ext("gif").contains(&"mp3"));
        assert!(targets_for_ext("mp4").contains(&"mp3"));
    }

    #[test]
    fn pages_is_not_supported() {
        assert_eq!(kind_of("pages"), None);
        assert!(!targets_for_ext("docx").contains(&"pages"));
    }

    #[test]
    fn mixed_selection_offers_only_shared_targets() {
        let paths = [PathBuf::from("a.jpg"), PathBuf::from("b.docx")];
        assert_eq!(targets_for(&paths), vec!["png", "pdf"]);
    }

    #[test]
    fn unrelated_selection_offers_nothing() {
        let paths = [PathBuf::from("a.mp3"), PathBuf::from("b.zip")];
        assert!(targets_for(&paths).is_empty());
    }

    #[test]
    fn unknown_file_offers_nothing() {
        assert!(targets_for(&[PathBuf::from("a.xyz")]).is_empty());
    }
}
