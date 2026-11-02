//! Runs each file tool on generated files and checks the result independently.
//! Needs the tools folder, so it is ignored by default:
//!   cargo test -p transmute-core --release --test filetools -- --ignored --nocapture

use std::path::{Path, PathBuf};
use transmute_core::filetools::run_tool;
use transmute_core::tools::{hidden, run};
use transmute_core::Tools;

fn tools() -> Tools {
    Tools::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"))
}

fn probe(path: &Path, entries: &str) -> String {
    let out = run(hidden(tools().ffprobe().unwrap())
        .args(["-v", "error", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path))
    .unwrap();
    String::from_utf8_lossy(&out).trim().to_string()
}

fn tool(id: &str, paths: &[PathBuf]) -> PathBuf {
    let mut errors = Vec::new();
    let mut produced = run_tool(&tools(), id, paths, &mut |_| {}, &mut |path, error| {
        errors.push(format!("{}: {error:#}", path.display()));
    });
    assert!(errors.is_empty(), "{id}: {errors:?}");
    produced.remove(0)
}

fn pages(pdf: &Path) -> u32 {
    let info = run(hidden(tools().poppler("pdfinfo.exe").unwrap()).arg(pdf)).unwrap();
    let text = String::from_utf8_lossy(&info).into_owned();
    let line = text.lines().find(|line| line.starts_with("Pages:")).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn make_video(path: &Path, size: &str, audio: bool) {
    let t = tools();
    let mut command = hidden(t.ffmpeg().unwrap());
    command.args(["-y", "-f", "lavfi", "-i", &format!("testsrc=size={size}:rate=30:duration=2")]);
    if audio {
        command.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-c:a", "aac", "-shortest"]);
    }
    run(command.args(["-c:v", "libx264", "-crf", "5", "-pix_fmt", "yuv420p"]).arg(path)).unwrap();
}

#[test]
#[ignore]
fn every_tool_works() {
    let t = tools();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    // Images.
    let jpg = dir.join("photo.jpg");
    run(hidden(t.magick().unwrap())
        .args(["-size", "800x600", "plasma:", "-quality", "98", "-set", "comment", "SECRET-COMMENT"])
        .arg(&jpg))
    .unwrap();
    let original = std::fs::read(&jpg).unwrap();
    let small = tool("compress", std::slice::from_ref(&jpg));
    assert!(std::fs::metadata(&small).unwrap().len() < original.len() as u64);
    let clean = tool("strip", std::slice::from_ref(&jpg));
    let cleaned = std::fs::read(&clean).unwrap();
    let has = |bytes: &[u8]| bytes.windows(14).any(|w| w == b"SECRET-COMMENT");
    assert!(has(&original) && !has(&cleaned));
    assert_eq!(std::fs::read(&jpg).unwrap(), original, "original changed");

    let png = dir.join("art.png");
    run(hidden(t.magick().unwrap()).args(["-size", "400x300", "plasma:"]).arg(&png)).unwrap();
    let small_png = tool("compress", std::slice::from_ref(&png));
    assert!(std::fs::metadata(&small_png).unwrap().len() < std::fs::metadata(&png).unwrap().len());
    let made = tool("make-pdf", &[jpg.clone(), png.clone()]);
    assert_eq!(pages(&made), 2);

    // Video.
    let clip = dir.join("clip.mp4");
    make_video(&clip, "320x240", true);
    let silent = dir.join("silent.mp4");
    make_video(&silent, "640x360", false);
    assert!(probe(&tool("mute", std::slice::from_ref(&clip)), "stream=codec_type").contains("video"));
    assert!(!probe(&tool("mute", std::slice::from_ref(&clip)), "stream=codec_type").contains("audio"));
    let smaller = tool("compress", std::slice::from_ref(&clip));
    assert!(std::fs::metadata(&smaller).unwrap().len() < std::fs::metadata(&clip).unwrap().len());
    let joined = tool("join", &[clip.clone(), silent.clone()]);
    let duration: f64 = probe(&joined, "format=duration").parse().unwrap();
    assert!((duration - 4.0).abs() < 0.3, "joined duration {duration}");
    assert!(probe(&joined, "stream=width,height").contains("320,240"));

    let tagged = dir.join("tagged.mp4");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-i"])
        .arg(&clip)
        .args(["-c", "copy", "-metadata", "title=SECRET-TITLE"])
        .arg(&tagged))
    .unwrap();
    assert!(probe(&tagged, "format_tags=title").contains("SECRET-TITLE"));
    assert!(!probe(&tool("strip", std::slice::from_ref(&tagged)), "format_tags=title").contains("SECRET-TITLE"));

    // Audio.
    let wav = dir.join("tone.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-ac", "2", "-ar", "44100"])
        .arg(&wav))
    .unwrap();
    assert!(std::fs::metadata(tool("compress", std::slice::from_ref(&wav))).unwrap().len() < std::fs::metadata(&wav).unwrap().len());
    assert!(probe(&tool("mono", std::slice::from_ref(&wav)), "stream=channels").contains('1'));
    let mono = tool("mono", std::slice::from_ref(&wav));
    assert!(probe(&tool("stereo", std::slice::from_ref(&mono)), "stream=channels").contains('2'));
    assert!(probe(&tool("normalize", std::slice::from_ref(&wav)), "stream=codec_type").contains("audio"));
    let mp3 = dir.join("song.mp3");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-b:a", "320k"])
        .arg(&mp3))
    .unwrap();
    assert!(std::fs::metadata(tool("compress", std::slice::from_ref(&mp3))).unwrap().len() < std::fs::metadata(&mp3).unwrap().len());

    // PDF. A two-page file comes from Make PDF above.
    let split = tool("split", std::slice::from_ref(&made));
    let parts: Vec<PathBuf> = {
        let mut parts: Vec<PathBuf> = std::fs::read_dir(&split).unwrap().flatten().map(|e| e.path()).collect();
        parts.sort();
        parts
    };
    assert_eq!(parts.len(), 2);
    assert_eq!(pages(&parts[0]), 1);
    assert_eq!(pages(&tool("merge-pdf", &parts)), 2);
    assert_eq!(pages(&tool("strip", std::slice::from_ref(&made))), 2);

    // Archives, including a .tar.gz, which must unpack to the real contents.
    let payload = dir.join("payload.txt");
    std::fs::write(&payload, "hello").unwrap();
    let zip = dir.join("bundle.zip");
    run(hidden(t.seven_zip().unwrap()).args(["a", "-tzip"]).arg(&zip).arg(&payload)).unwrap();
    assert!(tool("extract", std::slice::from_ref(&zip)).join("payload.txt").is_file());
    let gz = transmute_core::convert(&t, &zip, "gz", &mut |_| {}).unwrap();
    let folder = tool("extract", std::slice::from_ref(&gz));
    assert!(folder.join("payload.txt").is_file(), "tar.gz contents in {}", folder.display());

    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".transmute-"))
        .collect();
    assert!(leftovers.is_empty(), "temp folders left behind");
}
