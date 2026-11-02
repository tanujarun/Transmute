//! PDF, document, slide and spreadsheet routes across Office, LibreOffice,
//! pandoc and Poppler.

use crate::office;
use crate::tools::{hidden, run, run_with_timeout, Tools};
use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const SOFFICE_LIMIT: Duration = Duration::from_secs(300);
const CSV_FILTER: &str = "Text - txt - csv (StarCalc)";

/// One LibreOffice at a time, because they share a profile.
static SOFFICE_LOCK: Mutex<()> = Mutex::new(());

/// Runs LibreOffice headless with its own profile, so it never collides with a
/// LibreOffice the user has open. The profile persists in the temp folder because
/// building one takes several seconds.
fn soffice(
    tools: &Tools,
    src: &Path,
    filter: &str,
    infilter: Option<&str>,
    work: &Path,
    produced: &str,
) -> Result<PathBuf> {
    let _guard = SOFFICE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let profile = std::env::temp_dir().join("transmute-lo-profile");
    let url = format!("file:///{}", profile.display().to_string().replace('\\', "/").replace(' ', "%20"));
    let outdir = work.join("lo-out");
    std::fs::create_dir_all(&outdir)?;
    let mut command = hidden(tools.soffice()?);
    command
        .arg(format!("-env:UserInstallation={url}"))
        .args(["--headless", "--norestore", "--nolockcheck"]);
    if let Some(infilter) = infilter {
        command.arg(format!("--infilter={infilter}"));
    }
    command.args(["--convert-to", filter, "--outdir"]).arg(&outdir).arg(src);
    run_with_timeout(&mut command, SOFFICE_LIMIT)?;
    let stem = src.file_stem().ok_or_else(|| anyhow!("file has no name"))?;
    let result = outdir.join(stem).with_extension(produced);
    if !result.is_file() {
        bail!("LibreOffice could not convert this file");
    }
    Ok(result)
}

fn place(result: PathBuf, out: &Path) -> Result<()> {
    std::fs::rename(result, out)?;
    Ok(())
}

/// Plain text has no markup, so it is wrapped as HTML rather than parsed as
/// Markdown, where stray asterisks would turn into formatting.
fn text_as_html(src: &Path, work: &Path) -> Result<PathBuf> {
    let bytes = std::fs::read(src)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut html = String::from("<!doctype html><meta charset=\"utf-8\"><body>");
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let escaped = block.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        html.push_str("<p>");
        html.push_str(&escaped.replace('\n', "<br>"));
        html.push_str("</p>");
    }
    let path = work.join("text-input.html");
    std::fs::write(&path, html)?;
    Ok(path)
}

fn pandoc(tools: &Tools, src: &Path, from: &str, target: &str, title: &str, out: &Path) -> Result<()> {
    let mut command = hidden(tools.pandoc()?);
    command.arg("-f").arg(from).arg("--wrap=none");
    if let Some(parent) = src.parent() {
        command.arg("--resource-path").arg(parent);
    }
    match target {
        "md" => {
            command.args(["-t", "gfm"]);
        }
        "txt" => {
            command.args(["-t", "plain"]);
        }
        "html" => {
            command.args(["-t", "html5", "-s", "--embed-resources"]);
            command.arg("--metadata").arg(format!("title={title}"));
        }
        "epub" => {
            command.args(["-t", "epub3"]);
            command.arg("--metadata").arg(format!("title={title}"));
        }
        "rtf" => {
            command.args(["-t", "rtf", "-s"]);
        }
        "docx" => {
            command.args(["-t", "docx"]);
        }
        other => bail!("pandoc has no {other} route"),
    }
    run(command.arg("-o").arg(out).arg(src))?;
    Ok(())
}

fn pandoc_reader(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "docx" => "docx",
        "rtf" => "rtf",
        "md" => "gfm",
        "epub" => "epub",
        "html" => "html",
        _ => return None,
    })
}

fn title_of(src: &Path) -> String {
    src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Normalises any document to something pandoc reads.
fn readable(tools: &Tools, src: &Path, ext: &str, work: &Path) -> Result<(PathBuf, &'static str)> {
    match ext {
        "txt" => Ok((text_as_html(src, work)?, "html")),
        "doc" => {
            let docx = work.join("legacy.docx");
            if office::word(src, &docx, office::WORD_DOCX).is_err() {
                let result = soffice(tools, src, "docx", None, work, "docx")?;
                std::fs::rename(result, &docx)?;
            }
            Ok((docx, "docx"))
        }
        _ => {
            let reader = pandoc_reader(ext).ok_or_else(|| anyhow!("unsupported document: {ext}"))?;
            Ok((src.to_path_buf(), reader))
        }
    }
}

fn document_to_docx(tools: &Tools, src: &Path, ext: &str, work: &Path) -> Result<PathBuf> {
    if ext == "docx" {
        return Ok(src.to_path_buf());
    }
    let (input, reader) = readable(tools, src, ext, work)?;
    if reader == "docx" {
        return Ok(input);
    }
    let docx = work.join("intermediate.docx");
    pandoc(tools, &input, reader, "docx", &title_of(src), &docx)?;
    Ok(docx)
}

/// Any supported Office-style input to PDF, preferring Microsoft Office.
pub fn to_pdf(tools: &Tools, src: &Path, ext: &str, out: &Path, work: &Path) -> Result<()> {
    let attempt = match ext {
        "ppt" | "pptx" => office::powerpoint_pdf(src, out),
        "xls" | "xlsx" => office::excel_pdf(src, out),
        "csv" | "tsv" | "ods" => Err(anyhow!("handled by LibreOffice")),
        _ => {
            let docx = document_to_docx(tools, src, ext, work)?;
            match office::word(&docx, out, office::WORD_PDF) {
                Ok(()) => Ok(()),
                Err(_) => return place(soffice(tools, &docx, "pdf", None, work, "pdf")?, out),
            }
        }
    };
    if attempt.is_ok() {
        return Ok(());
    }
    let infilter = (ext == "tsv").then(|| format!("{CSV_FILTER}:9,34,76"));
    place(soffice(tools, src, "pdf", infilter.as_deref(), work, "pdf")?, out)
}

pub fn convert_document(tools: &Tools, src: &Path, ext: &str, target: &str, out: &Path, work: &Path) -> Result<()> {
    match target {
        "doc" => {
            let docx = document_to_docx(tools, src, ext, work)?;
            if office::word(&docx, out, office::WORD_DOC).is_err() {
                place(soffice(tools, &docx, "doc", None, work, "doc")?, out)?;
            }
            Ok(())
        }
        "rtf" if matches!(ext, "docx" | "doc") => {
            if office::word(src, out, office::WORD_RTF).is_err() {
                place(soffice(tools, src, "rtf", None, work, "rtf")?, out)?;
            }
            Ok(())
        }
        "docx" if ext == "rtf" => {
            if office::word(src, out, office::WORD_DOCX).is_err() {
                place(soffice(tools, src, "docx", None, work, "docx")?, out)?;
            }
            Ok(())
        }
        _ => {
            let (input, reader) = readable(tools, src, ext, work)?;
            if input != src && reader == "docx" && target == "docx" {
                return place(input, out);
            }
            pandoc(tools, &input, reader, target, &title_of(src), out)
        }
    }
}

/// Renders every page of a PDF. Returns the images in page order.
pub fn pdf_pages(tools: &Tools, pdf: &Path, target: &str, dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut command = hidden(tools.poppler("pdftoppm.exe")?);
    command.args(["-r", "300"]);
    command.arg(if target == "jpg" { "-jpeg" } else { "-png" });
    run(command.arg(pdf).arg(dir.join("page")))?;
    let mut pages: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some())
        .collect();
    pages.sort();
    Ok(pages)
}

/// Rewrites a PDF as PDF/A-2b through LibreOffice's Draw import. Pages are rebuilt
/// rather than copied, so complex layouts can shift slightly.
pub fn pdf_to_pdfa(tools: &Tools, src: &Path, out: &Path, work: &Path) -> Result<()> {
    let filter = r#"pdf:draw_pdf_Export:{"SelectPdfVersion":{"type":"long","value":"2"}}"#;
    place(soffice(tools, src, filter, Some("draw_pdf_import"), work, "pdf")?, out)
}

/// An EPUB whose pages are pictures of the PDF's pages, for files where layout
/// matters more than adjustable text.
pub fn pdf_to_page_epub(tools: &Tools, src: &Path, out: &Path, work: &Path) -> Result<()> {
    let pages_dir = work.join("pages");
    let pages = pdf_pages(tools, src, "jpg", &pages_dir)?;
    let mut book = format!("---\ntitle: \"{}\"\n---\n\n", title_of(src).replace('"', "'"));
    for page in &pages {
        let name = page.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        book.push_str(&format!("![]({name})\n\n"));
    }
    let source = pages_dir.join("book.md");
    std::fs::write(&source, book)?;
    run(hidden(tools.pandoc()?)
        .args(["-f", "markdown", "-t", "epub3", "--resource-path"])
        .arg(&pages_dir)
        .arg("-o")
        .arg(out)
        .arg(&source))?;
    Ok(())
}

fn pdf_as_html(tools: &Tools, src: &Path, work: &Path) -> Result<PathBuf> {
    let html = work.join("pdf-text.html");
    run(hidden(tools.poppler("pdftohtml.exe")?)
        .args(["-s", "-i", "-noframes", "-enc", "UTF-8"])
        .arg(src)
        .arg(&html))?;
    Ok(html)
}

pub fn pdf_text(tools: &Tools, src: &Path, out: &Path, layout: bool) -> Result<()> {
    let mut command = hidden(tools.poppler("pdftotext.exe")?);
    command.args(["-enc", "UTF-8"]);
    if layout {
        command.arg("-layout");
    }
    run(command.arg(src).arg(out))?;
    Ok(())
}

/// Best effort: treats runs of two or more spaces in the page layout as cell
/// breaks, since PDFs hold positioned text rather than tables.
fn layout_to_tsv(text: &str) -> String {
    let mut rows = String::new();
    for line in text.replace('\u{c}', "\n").lines() {
        let cells: Vec<&str> =
            line.trim().split("  ").map(str::trim).filter(|cell| !cell.is_empty()).collect();
        rows.push_str(&cells.join("\t"));
        rows.push('\n');
    }
    rows
}

pub fn convert_pdf(tools: &Tools, src: &Path, target: &str, out: &Path, work: &Path) -> Result<()> {
    match target {
        "txt" => pdf_text(tools, src, out, false),
        "md" | "epub" => {
            let html = pdf_as_html(tools, src, work)?;
            pandoc(tools, &html, "html", target, &title_of(src), out)
        }
        "docx" => {
            if office::word(src, out, office::WORD_DOCX).is_err() {
                place(soffice(tools, src, "docx", Some("writer_pdf_import"), work, "docx")?, out)?;
            }
            Ok(())
        }
        "pptx" => place(soffice(tools, src, "pptx", Some("impress_pdf_import"), work, "pptx")?, out),
        "xlsx" => {
            let text = work.join("layout.txt");
            pdf_text(tools, src, &text, true)?;
            let tsv = work.join(format!("{}.tsv", title_of(src)));
            std::fs::write(&tsv, layout_to_tsv(&std::fs::read_to_string(&text)?))?;
            convert_sheet(tools, &tsv, "tsv", "xlsx", out, work)
        }
        other => bail!("no PDF route to {other}"),
    }
}

pub fn convert_sheet(tools: &Tools, src: &Path, ext: &str, target: &str, out: &Path, work: &Path) -> Result<()> {
    let infilter = (ext == "tsv").then(|| format!("{CSV_FILTER}:9,34,76"));
    // Comma-separated, UTF-8, values as shown, first sheet.
    let (filter, produced) = match target {
        "csv" => (format!("csv:{CSV_FILTER}:44,34,76,1"), "csv"),
        "tsv" => (format!("csv:{CSV_FILTER}:9,34,76,1"), "csv"),
        "xlsx" => ("xlsx:Calc MS Excel 2007 XML".to_string(), "xlsx"),
        "ods" => ("ods".to_string(), "ods"),
        other => bail!("no spreadsheet route to {other}"),
    };
    place(soffice(tools, src, &filter, infilter.as_deref(), work, produced)?, out)
}

#[cfg(test)]
mod tests {
    use super::layout_to_tsv;

    #[test]
    fn wide_gaps_become_cells_and_single_spaces_do_not() {
        assert_eq!(layout_to_tsv("Item one   12   3.50"), "Item one\t12\t3.50\n");
    }
}
