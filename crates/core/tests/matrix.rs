//! Runs every conversion route against generated files and validates each output
//! with an independent check. Needs the tools folder, so it is ignored by default:
//!   cargo test -p transmute-core --release --test matrix -- --ignored --nocapture

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use transmute_core::formats::{kind_of, targets_for_ext, Kind};
use transmute_core::tools::{hidden, run};
use transmute_core::{convert, Tools};

const MARKER: &str = "Transmute";

fn tools() -> Tools {
    Tools::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"))
}

fn write(path: PathBuf, text: &str) -> PathBuf {
    std::fs::write(&path, text).unwrap();
    path
}

/// Builds one seed per family, then derives the other inputs with the engine itself.
fn fixtures(dir: &Path) -> Vec<PathBuf> {
    let t = tools();
    std::fs::create_dir_all(dir).unwrap();
    let mut inputs = Vec::new();
    let derive = |seed: &Path, target: &str, inputs: &mut Vec<PathBuf>| {
        let path = convert(&t, seed, target, &mut |_| {})
            .unwrap_or_else(|e| panic!("fixture {target} from {}: {e:#}", seed.display()));
        inputs.push(path);
    };

    let png = dir.join("image.png");
    run(hidden(t.magick().unwrap()).args(["-size", "96x64", "gradient:red-blue"]).arg(&png)).unwrap();
    inputs.push(png.clone());
    for target in ["jpg", "webp", "heic", "tiff", "avif", "bmp"] {
        derive(&png, target, &mut inputs);
    }
    let ico = dir.join("icon.ico");
    run(hidden(t.magick().unwrap()).arg(&png).arg(&ico)).unwrap();
    inputs.push(ico);
    inputs.push(write(
        dir.join("drawing.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"60\" height=\"60\"><rect width=\"60\" height=\"60\" fill=\"#f80\"/></svg>",
    ));

    let mp4 = dir.join("clip.mp4");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "testsrc=size=160x120:rate=10:duration=1"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&mp4))
    .unwrap();
    inputs.push(mp4.clone());
    for target in ["mov", "mkv", "webm", "avi", "wmv", "gif", "flv", "m4v", "mpg"] {
        derive(&mp4, target, &mut inputs);
    }

    let wav = dir.join("tone.wav");
    run(hidden(t.ffmpeg().unwrap()).args(["-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1"]).arg(&wav)).unwrap();
    inputs.push(wav.clone());
    for target in ["mp3", "m4a", "flac", "ogg", "opus", "aiff", "wma", "aac", "ac3"] {
        derive(&wav, target, &mut inputs);
    }

    let md = write(dir.join("notes.md"), &format!("# {MARKER}\n\nA paragraph with **bold** text.\n\n- one\n- two\n"));
    inputs.push(md.clone());
    for target in ["docx", "doc", "rtf", "txt", "epub", "html", "pdf"] {
        derive(&md, target, &mut inputs);
    }
    let pdf = inputs.iter().find(|p| p.extension().is_some_and(|e| e == "pdf")).unwrap().clone();
    derive(&pdf, "pptx", &mut inputs);
    let pptx = inputs.last().unwrap().clone();
    let ppt = dir.join("deck.ppt");
    let lo = t.soffice().unwrap();
    run(hidden(&lo).args(["--headless", "--convert-to", "ppt", "--outdir"]).arg(dir).arg(&pptx)).unwrap();
    std::fs::rename(pptx.with_extension("ppt"), &ppt).unwrap();
    inputs.push(ppt);

    let csv = write(dir.join("table.csv"), &format!("name,count\n{MARKER},3\nother,4\n"));
    inputs.push(csv.clone());
    for target in ["xlsx", "ods", "tsv"] {
        derive(&csv, target, &mut inputs);
    }
    let xls = dir.join("table.xls");
    run(hidden(&lo).args(["--headless", "--convert-to", "xls:MS Excel 97", "--outdir"]).arg(dir).arg(&csv)).unwrap();
    assert!(xls.is_file());
    inputs.push(xls);

    let srt = write(dir.join("captions.srt"), &format!("1\n00:00:01,000 --> 00:00:02,000\n{MARKER}\n"));
    inputs.push(srt.clone());
    derive(&srt, "vtt", &mut inputs);

    let payload = write(dir.join("payload.txt"), MARKER);
    let zip = dir.join("bundle.zip");
    run(hidden(t.seven_zip().unwrap()).args(["a", "-tzip"]).arg(&zip).arg(&payload)).unwrap();
    inputs.push(zip.clone());
    for target in ["tar", "gz", "7z"] {
        derive(&zip, target, &mut inputs);
    }
    inputs.sort();
    inputs.dedup();
    inputs
}

fn starts_with(path: &Path, magic: &[u8]) -> bool {
    std::fs::read(path).is_ok_and(|bytes| bytes.starts_with(magic))
}

fn contains_marker(path: &Path) -> bool {
    String::from_utf8_lossy(&std::fs::read(path).unwrap_or_default()).contains(MARKER)
}

fn archive_contents(path: &Path) -> String {
    let t = tools();
    let listing = run(hidden(t.seven_zip().unwrap()).args(["l", "-ba"]).arg(path)).unwrap_or_default();
    String::from_utf8_lossy(&listing).into_owned()
}

/// Checks the output really is the requested format, using a different tool from
/// the one that wrote it wherever possible.
fn validate(output: &Path, target: &str, kind: Kind) -> Result<(), String> {
    let t = tools();
    if output.is_dir() {
        let pages = std::fs::read_dir(output).map_err(|e| e.to_string())?.count();
        return if pages >= 2 { Ok(()) } else { Err("empty page folder".into()) };
    }
    let ok = match target {
        "jpg" | "png" | "webp" | "heic" | "tiff" | "avif" | "bmp" => {
            run(hidden(t.magick().unwrap()).args(["identify", "-format", "%wx%h"]).arg(output)).is_ok()
        }
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "gif" | "flv" | "m4v" | "mpg" => {
            let out = run(hidden(t.ffprobe().unwrap())
                .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
                .arg(output));
            out.is_ok_and(|o| String::from_utf8_lossy(&o).contains("video"))
        }
        "mp3" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aiff" | "wma" | "aac" | "ac3" => {
            let out = run(hidden(t.ffprobe().unwrap())
                .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
                .arg(output));
            out.is_ok_and(|o| String::from_utf8_lossy(&o).contains("audio"))
        }
        "pdf" => {
            let pages = run(hidden(t.poppler("pdfinfo.exe").unwrap()).arg(output));
            pages.is_ok_and(|o| String::from_utf8_lossy(&o).contains("Pages:"))
        }
        "docx" | "xlsx" | "pptx" | "ods" | "epub" => {
            starts_with(output, b"PK") && !archive_contents(output).is_empty()
        }
        "doc" => starts_with(output, &[0xD0, 0xCF, 0x11, 0xE0]) || starts_with(output, b"{\\rtf"),
        "rtf" => starts_with(output, b"{\\rtf"),
        "zip" | "tar" | "gz" | "7z" => !archive_contents(output).is_empty(),
        "txt" | "md" | "html" | "csv" | "tsv" | "srt" | "vtt" => {
            contains_marker(output) || !matches!(kind, Kind::Document | Kind::Sheet | Kind::Subtitle)
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{} failed validation", output.display()))
    }
}

#[test]
#[ignore]
fn every_route_produces_a_valid_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/matrix");
    let _ = std::fs::remove_dir_all(&root);
    let seeds = root.join("inputs");
    let inputs = fixtures(&seeds);

    let mut queue = VecDeque::new();
    for (index, input) in inputs.iter().enumerate() {
        let ext = transmute_core::formats::extension_of(input).unwrap();
        for target in targets_for_ext(&ext) {
            queue.push_back((index, input.clone(), ext.clone(), target));
        }
    }
    let total = queue.len();
    println!("{} input formats, {total} routes", inputs.len());
    let queue = Mutex::new(queue);
    let failures = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| loop {
                let Some((index, input, ext, target)) = queue.lock().unwrap().pop_front() else { return };
                // Each route gets its own copy so outputs never collide.
                let dir = root.join(format!("{index}-{ext}-to-{target}"));
                std::fs::create_dir_all(&dir).unwrap();
                let src = dir.join(input.file_name().unwrap());
                std::fs::copy(&input, &src).unwrap();
                let kind = kind_of(&ext).unwrap();
                let result = convert(&tools(), &src, target, &mut |_| {})
                    .map_err(|e| format!("{e:#}"))
                    .and_then(|output| validate(&output, target, kind));
                let original_intact = std::fs::read(&src).ok() == std::fs::read(&input).ok();
                if let Err(error) = result {
                    failures.lock().unwrap().push(format!("{ext} -> {target}: {error}"));
                } else if !original_intact {
                    failures.lock().unwrap().push(format!("{ext} -> {target}: original changed"));
                }
                let _ = std::fs::remove_dir_all(&dir);
            });
        }
    });

    let failures = failures.into_inner().unwrap();
    println!("passed {} of {total}", total - failures.len());
    for failure in &failures {
        let short: String = failure.chars().take(240).collect();
        println!("FAIL {short}");
    }
    assert!(failures.is_empty(), "{} routes failed", failures.len());
}
