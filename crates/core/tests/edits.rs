//! Runs each editor operation on generated media and measures the result.
//! Needs the tools folder, so it is ignored by default:
//!   cargo test -p transmute-core --release --test edits -- --ignored --nocapture

use std::path::{Path, PathBuf};
use transmute_core::edits::{apply, playable_preview, Area, Edit};
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

fn duration(path: &Path) -> f64 {
    probe(path, "format=duration").parse().unwrap()
}

fn close(actual: f64, expected: f64, label: &str) {
    assert!((actual - expected).abs() < 0.25, "{label}: {actual} vs {expected}");
}

fn edit(src: &Path, edit: Edit) -> PathBuf {
    apply(&tools(), src, &edit, &mut |_| {}).unwrap_or_else(|e| panic!("{edit:?}: {e:#}"))
}

/// Mean brightness of one frame as a 0-255 level.
fn brightness(video: &Path, seconds: f64, crop: &str) -> f64 {
    let out = run(hidden(tools().ffmpeg().unwrap())
        .args(["-v", "error", "-ss", &seconds.to_string(), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", &format!("crop={crop},scale=1:1,format=gray"), "-f", "rawvideo", "-"]))
    .unwrap();
    out[0] as f64
}

/// Loudest sample level in dB over a window of the file.
fn peak_db(audio: &Path, start: f64, length: f64) -> f64 {
    let out = hidden(tools().ffmpeg().unwrap())
        .args(["-hide_banner", "-ss", &start.to_string(), "-t", &length.to_string(), "-i"])
        .arg(audio)
        .args(["-af", "volumedetect", "-f", "null", "-"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    let line = text.lines().find(|line| line.contains("max_volume:")).unwrap_or_else(|| panic!("{text}"));
    line.split("max_volume:").nth(1).unwrap().trim().trim_end_matches(" dB").parse().unwrap()
}

#[test]
#[ignore]
fn every_edit_does_what_it_says() {
    let t = tools();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    // A bright 640x360 clip, four seconds long, with a tone throughout.
    let clip = dir.join("clip.mp4");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "color=white:size=640x360:rate=30:duration=4"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&clip))
    .unwrap();
    let original = std::fs::read(&clip).unwrap();

    close(duration(&edit(&clip, Edit::TrimVideo { start: 1.0, end: 3.0 })), 2.0, "trim");

    let cropped = edit(&clip, Edit::CropVideo { x: 10, y: 20, width: 201, height: 100 });
    assert_eq!(probe(&cropped, "stream=width,height").lines().next().unwrap(), "200,100");
    assert!(probe(&cropped, "stream=codec_type").contains("audio"), "crop keeps audio");

    close(duration(&edit(&clip, Edit::SpeedVideo { factor: 2.0 })), 2.0, "double speed");
    close(duration(&edit(&clip, Edit::SpeedVideo { factor: 0.5 })), 8.0, "half speed");
    assert!(apply(&t, &clip, &Edit::SpeedVideo { factor: 0.0 }, &mut |_| {}).is_err());

    let silent = dir.join("silent.mp4");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "color=red:size=320x240:rate=30:duration=2", "-pix_fmt", "yuv420p"])
        .arg(&silent))
    .unwrap();
    close(duration(&edit(&silent, Edit::SpeedVideo { factor: 2.0 })), 1.0, "silent speed");

    let parts = edit(&clip, Edit::SplitVideo { points: vec![1.0, 2.5] });
    let mut files: Vec<PathBuf> = std::fs::read_dir(&parts).unwrap().flatten().map(|e| e.path()).collect();
    files.sort();
    assert_eq!(files.len(), 3);
    close(duration(&files[0]), 1.0, "part 1");
    close(duration(&files[1]), 1.5, "part 2");
    close(duration(&files[2]), 1.5, "part 3");

    let frames = edit(&clip, Edit::SaveFrames { times: vec![0.5, 2.0] });
    assert_eq!(std::fs::read_dir(&frames).unwrap().count(), 2);

    // Black box from 1s to 2s, in the top-left corner only.
    let area = Area { x: 0, y: 0, width: 100, height: 100, start: 1.0, end: 2.0, blur: false };
    let redacted = edit(&clip, Edit::RedactVideo { areas: vec![area] });
    assert!(brightness(&redacted, 1.5, "100:100:0:0") < 40.0, "box is on during its range");
    assert!(brightness(&redacted, 3.0, "100:100:0:0") > 200.0, "box is off afterwards");
    assert!(brightness(&redacted, 1.5, "100:100:300:200") > 200.0, "rest of frame untouched");
    let blurred = Area { blur: true, ..area };
    close(duration(&edit(&clip, Edit::RedactVideo { areas: vec![blurred] })), 4.0, "blur keeps length");
    assert_eq!(std::fs::read(&clip).unwrap(), original, "original changed");

    // Audio: one second of silence, two of tone, one of silence.
    let wav = dir.join("speech.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i"])
        .arg("aevalsrc='if(between(t,1,3),0.5*sin(2*PI*440*t),0)':s=44100:d=4:c=stereo")
        .arg(&wav))
    .unwrap();
    close(duration(&edit(&wav, Edit::TrimAudio { start: 0.5, end: 2.5 })), 2.0, "audio trim");
    close(duration(&edit(&wav, Edit::TrimSilence)), 2.0, "silence removed at the ends");

    // Left silent, right kept.
    let right_only = edit(&wav, Edit::Channels { left: 0.0, right: 1.0, mono: false });
    let left_channel = dir.join("left.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-i"])
        .arg(&right_only)
        .args(["-af", "pan=mono|c0=c0"])
        .arg(&left_channel))
    .unwrap();
    assert!(peak_db(&left_channel, 1.0, 2.0) < -60.0, "left channel is silent");
    assert!(peak_db(&right_only, 1.0, 2.0) > -12.0, "right channel is kept");
    let mono = edit(&wav, Edit::Channels { left: 1.0, right: 1.0, mono: true });
    assert_eq!(probe(&mono, "stream=channels"), "1");

    // During the bleep the 440 Hz tone goes and a 1 kHz tone takes over.
    let tone = dir.join("tone.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=4", "-ar", "48000"])
        .arg(&tone))
    .unwrap();
    let bleeped = edit(&tone, Edit::Bleep { ranges: vec![(1.0, 2.0)] });
    close(duration(&bleeped), 4.0, "bleep keeps length");
    let highpassed = dir.join("high.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-i"])
        .arg(&bleeped)
        .args(["-af", "highpass=f=800:poles=2,highpass=f=800:poles=2,highpass=f=800:poles=2"])
        .arg(&highpassed))
    .unwrap();
    assert!(peak_db(&highpassed, 1.2, 0.6) > peak_db(&highpassed, 2.5, 0.6) + 15.0, "bleep tone present only in range");

    let waves = edit(&tone, Edit::Visualize { image: None, shape: "square".into() });
    assert_eq!(probe(&waves, "stream=width,height").lines().next().unwrap(), "1080,1080");
    close(duration(&waves), 4.0, "waveform video length");

    let still = dir.join("cover.png");
    run(hidden(t.magick().unwrap()).args(["-size", "400x400", "plasma:"]).arg(&still)).unwrap();
    let poster = edit(&tone, Edit::Visualize { image: Some(still), shape: "landscape".into() });
    assert_eq!(probe(&poster, "stream=width,height").lines().next().unwrap(), "1920,1080");
    close(duration(&poster), 4.0, "still-image video length");

    // A format the window cannot play gets a proxy with the same length.
    let avi = dir.join("old.avi");
    run(hidden(t.ffmpeg().unwrap()).args(["-y", "-i"]).arg(&clip).args(["-c:v", "mpeg4", "-c:a", "libmp3lame"]).arg(&avi)).unwrap();
    let preview = playable_preview(&t, &avi, &dir.join("cache")).unwrap();
    assert_ne!(preview, avi);
    close(duration(&preview), 4.0, "proxy length");
    assert_eq!(playable_preview(&t, &clip, &dir.join("cache")).unwrap(), clip, "H.264 MP4 plays as is");
}
