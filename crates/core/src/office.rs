//! Microsoft Office automation, the preferred route for Office formats on Windows.
//! Callers fall back to LibreOffice whenever this returns an error, which covers
//! machines where Office is missing or not activated. Paths travel in environment
//! variables so a file name can never be interpreted as script.
//!
//! PowerPoint runs as a single instance, so automation can attach to a copy the
//! user has open. Every script therefore records which Office processes it
//! started, and only those are ever quit or killed.

use crate::tools::{hidden, run_with_timeout};
use anyhow::Result;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(180);

/// One job at a time, so "processes that appeared" means "processes we started".
static OFFICE_LOCK: Mutex<()> = Mutex::new(());

const PRELUDE: &str = r#"
$ErrorActionPreference = 'Stop'
$before = @(Get-Process $env:TM_PROCESS -ErrorAction SilentlyContinue | ForEach-Object Id)
$app = New-Object -ComObject $env:TM_PROGID
$ours = @(Get-Process $env:TM_PROCESS -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | ForEach-Object Id)
Set-Content -LiteralPath $env:TM_PIDS -Value $ours
"#;

const WORD: &str = r#"
$app.Visible = $false; $app.DisplayAlerts = 0
try {
  $doc = $app.Documents.Open($env:TM_IN, $false, $true, $false)
  $doc.SaveAs2([ref]$env:TM_OUT, [ref][int]$env:TM_FORMAT)
  $doc.Close(0)
} finally { if ($ours.Count -gt 0) { $app.Quit() } }
"#;

const POWERPOINT: &str = r#"
try {
  $deck = $app.Presentations.Open($env:TM_IN, -1, 0, 0)
  $deck.SaveAs($env:TM_OUT, [int]$env:TM_FORMAT)
  $deck.Close()
} finally { if ($ours.Count -gt 0) { $app.Quit() } }
"#;

const EXCEL: &str = r#"
$app.Visible = $false; $app.DisplayAlerts = $false
try {
  $book = $app.Workbooks.Open($env:TM_IN, 0, $true)
  $book.ExportAsFixedFormat(0, $env:TM_OUT)
  $book.Close($false)
} finally { if ($ours.Count -gt 0) { $app.Quit() } }
"#;

struct App {
    prog_id: &'static str,
    process: &'static str,
    script: &'static str,
}

const WORD_APP: App = App { prog_id: "Word.Application", process: "WINWORD", script: WORD };
const POWERPOINT_APP: App =
    App { prog_id: "PowerPoint.Application", process: "POWERPNT", script: POWERPOINT };
const EXCEL_APP: App = App { prog_id: "Excel.Application", process: "EXCEL", script: EXCEL };

fn kill_started(pids_file: &Path) {
    let text = std::fs::read_to_string(pids_file).unwrap_or_default();
    for pid in text.lines().map(str::trim).filter(|pid| pid.parse::<u32>().is_ok()) {
        let _ = hidden("taskkill.exe").args(["/PID", pid, "/F"]).output();
    }
}

fn automate(app: &App, src: &Path, out: &Path, format: u32) -> Result<()> {
    let _guard = OFFICE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let scratch = tempfile::tempdir()?;
    let pids = scratch.path().join("started.txt");
    let script = format!("{PRELUDE}{}", app.script);
    let result = run_with_timeout(
        hidden("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .env("TM_IN", src)
            .env("TM_OUT", out)
            .env("TM_FORMAT", format.to_string())
            .env("TM_PROGID", app.prog_id)
            .env("TM_PROCESS", app.process)
            .env("TM_PIDS", &pids),
        LIMIT,
    );
    if result.is_err() {
        kill_started(&pids);
    }
    result?;
    if !out.is_file() {
        anyhow::bail!("Office did not produce {}", out.display());
    }
    Ok(())
}

pub const WORD_DOC: u32 = 0;
pub const WORD_RTF: u32 = 6;
pub const WORD_DOCX: u32 = 16;
pub const WORD_PDF: u32 = 17;
const POWERPOINT_PDF: u32 = 32;

pub fn word(src: &Path, out: &Path, format: u32) -> Result<()> {
    automate(&WORD_APP, src, out, format)
}

pub fn powerpoint_pdf(src: &Path, out: &Path) -> Result<()> {
    automate(&POWERPOINT_APP, src, out, POWERPOINT_PDF)
}

pub fn excel_pdf(src: &Path, out: &Path) -> Result<()> {
    automate(&EXCEL_APP, src, out, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Calls Office directly with no fallback, so a pass proves Office did the work.
    /// Needs real Office: cargo test -p transmute-core --lib office -- --ignored
    #[test]
    #[ignore]
    fn office_converts_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let docx = dir.path().join("a.docx");
        let rtf = dir.path().join("a.rtf");
        std::fs::write(&rtf, r"{\rtf1\ansi Hello from Word}").unwrap();
        word(&rtf, &docx, WORD_DOCX).expect("Word");
        let pdf = dir.path().join("a.pdf");
        word(&docx, &pdf, WORD_PDF).expect("Word PDF");
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));

        let round = dir.path().join("round.docx");
        word(&pdf, &round, WORD_DOCX).expect("Word reading a PDF");

        let csv = dir.path().join("b.csv");
        std::fs::write(&csv, "a,b\n1,2\n").unwrap();
        let sheet = dir.path().join("b.pdf");
        excel_pdf(&csv, &sheet).expect("Excel");
        assert!(std::fs::read(&sheet).unwrap().starts_with(b"%PDF"));

        // PDF to PPTX has no Office route, so a deck is made through LibreOffice.
        let tools = crate::Tools::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"));
        let deck = dir.path().join("deck.pptx");
        crate::documents::convert_pdf(&tools, &pdf, "pptx", &deck, dir.path()).expect("deck");
        let slides = dir.path().join("deck.pdf");
        powerpoint_pdf(&deck, &slides).expect("PowerPoint");
        assert!(std::fs::read(&slides).unwrap().starts_with(b"%PDF"));
    }
}
