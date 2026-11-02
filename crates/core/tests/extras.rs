//! Measures target compression, the metadata editor, QR reading and the PDF extras.
//! Needs the tools folder, so it is ignored by default:
//!   cargo test -p transmute-core --release --test extras -- --ignored --nocapture

use std::path::{Path, PathBuf};
use transmute_core::edits::{apply, frame_at, waveform, Edit};
use transmute_core::extras::{compress_to, measure_loudness, read_metadata, read_qr, write_metadata, CompressPlan, Field};
use transmute_core::filetools::run_tool;
use transmute_core::image_edits::image_size;
use transmute_core::tools::{hidden, run};
use transmute_core::Tools;

fn tools() -> Tools {
    Tools::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"))
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len()
}

fn plan(target_mb: Option<f64>, percent: Option<f64>, max_side: Option<u32>) -> CompressPlan {
    CompressPlan { target_mb, percent, max_side }
}

fn compress(src: &Path, p: CompressPlan) -> PathBuf {
    compress_to(&tools(), src, &p, &mut |_| {}).unwrap_or_else(|e| panic!("{p:?}: {e:#}"))
}

fn tool(id: &str, src: &Path) -> PathBuf {
    let mut errors = Vec::new();
    let produced = run_tool(&tools(), id, &[src.to_path_buf()], &mut |_| {}, &mut |_, e| errors.push(format!("{e:#}")));
    assert!(errors.is_empty(), "{id}: {errors:?}");
    produced.into_iter().next().unwrap()
}

fn field(fields: &[Field], name: &str) -> String {
    fields.iter().find(|f| f.name == name).map(|f| f.value.clone()).unwrap_or_default()
}

fn set(pairs: &[(&str, &str)]) -> Vec<Field> {
    pairs.iter().map(|(name, value)| Field { name: name.to_string(), value: value.to_string() }).collect()
}

/// A QR code as a greyscale PGM, eight pixels per module with a quiet zone.
fn qr_image(path: &Path, text: &str) {
    let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
    let (width, modules) = (code.width(), code.to_colors());
    let (scale, quiet) = (8, 4);
    let side = (width + quiet * 2) * scale;
    let mut pixels = vec![255u8; side * side];
    for y in 0..width {
        for x in 0..width {
            if modules[y * width + x] == qrcode::Color::Dark {
                for row in 0..scale {
                    for column in 0..scale {
                        pixels[((y + quiet) * scale + row) * side + (x + quiet) * scale + column] = 0;
                    }
                }
            }
        }
    }
    let mut file = format!("P5\n{side} {side}\n255\n").into_bytes();
    file.extend(pixels);
    std::fs::write(path, file).unwrap();
}

#[test]
#[ignore]
fn extras_do_what_they_say() {
    let t = tools();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    // Noise compresses badly, so these targets are real work.
    let photo = dir.join("photo.jpg");
    run(hidden(t.magick().unwrap()).args(["-size", "1600x1200", "plasma:", "-attenuate", "0.4", "+noise", "Gaussian", "-quality", "95"]).arg(&photo)).unwrap();
    let before = size(&photo);
    println!("photo {before} bytes");
    let half = compress(&photo, plan(None, Some(50.0), None));
    assert!(size(&half) <= before / 2, "percent target: {}", size(&half));
    assert!(size(&half) > before / 8, "does not overshoot wildly: {}", size(&half));
    let small = compress(&photo, plan(Some(0.2), None, None));
    assert!(size(&small) <= 200_000, "size target: {}", size(&small));
    let resized = compress(&photo, plan(None, Some(90.0), Some(800)));
    assert_eq!(image_size(&t, &resized).unwrap(), (800, 600));

    let png = dir.join("art.png");
    run(hidden(t.magick().unwrap()).args(["-size", "800x600", "plasma:"]).arg(&png)).unwrap();
    let png_small = compress(&png, plan(None, Some(40.0), None));
    assert!(size(&png_small) <= size(&png) * 4 / 10, "png target: {}", size(&png_small));

    let clip = dir.join("clip.mp4");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        .args(["-c:v", "libx264", "-crf", "10", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&clip))
    .unwrap();
    let target = 2.0;
    let video = compress(&clip, plan(Some(target), None, Some(640)));
    let bytes = size(&video) as f64 / 1e6;
    println!("video {} MB -> {bytes:.2} MB", size(&clip) as f64 / 1e6);
    assert!(bytes < target * 1.25 && bytes > target * 0.4, "video target: {bytes}");
    let dims = run(hidden(t.ffprobe().unwrap())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(&video))
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&dims).trim(), "640,360");

    let mp3 = dir.join("song.mp3");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=20", "-b:a", "320k"])
        .arg(&mp3))
    .unwrap();
    let audio = compress(&mp3, plan(Some(0.2), None, None));
    let bytes = size(&audio) as f64 / 1e6;
    assert!((0.1..0.25).contains(&bytes), "audio target: {bytes}");
    assert!(compress_to(&t, &dir.join("missing.wav"), &plan(Some(1.0), None, None), &mut |_| {}).is_err());

    // Metadata round trips, read back by ImageMagick and FFprobe rather than the writer.
    let fields = set(&[
        ("Description", "A bridge"),
        ("Author", "Transmute Test"),
        ("Copyright", "2026"),
        ("Camera make", "ACME"),
        ("Camera model", "One"),
        ("Date taken", "2026:01:02 03:04:05"),
        ("Latitude", "51.507400"),
        ("Longitude", "-0.127800"),
    ]);
    let original = std::fs::read(&photo).unwrap();
    let tagged = write_metadata(&t, &photo, &fields, &mut |_| {}).unwrap();
    let read = read_metadata(&t, &tagged).unwrap();
    for name in ["Description", "Author", "Copyright", "Camera make", "Camera model", "Date taken"] {
        assert_eq!(field(&read, name), field(&fields, name), "{name}");
    }
    let latitude: f64 = field(&read, "Latitude").parse().unwrap();
    let longitude: f64 = field(&read, "Longitude").parse().unwrap();
    assert!((latitude - 51.5074).abs() < 1e-4, "{latitude}");
    assert!((longitude + 0.1278).abs() < 1e-4, "{longitude}");
    assert_eq!(std::fs::read(&photo).unwrap(), original, "original changed");
    assert_eq!(image_size(&t, &tagged).unwrap(), (1600, 1200), "picture unchanged");
    let bad = set(&[("Latitude", "north")]);
    assert!(write_metadata(&t, &photo, &bad, &mut |_| {}).is_err(), "bad latitude refused");

    let removed = apply(&t, &tagged, &Edit::RemoveMetadata, &mut |_| {}).unwrap();
    assert_eq!(field(&read_metadata(&t, &removed).unwrap(), "Author"), "", "Remove all clears it");

    let media = write_metadata(&t, &mp3, &set(&[("title", "Song"), ("artist", "Band")]), &mut |_| {}).unwrap();
    let read = read_metadata(&t, &media).unwrap();
    assert_eq!(field(&read, "title"), "Song");
    assert_eq!(field(&read, "artist"), "Band");

    // Loudness: a quiet tone comes up to near the target.
    let quiet = dir.join("quiet.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=5", "-af", "volume=0.02"])
        .arg(&quiet))
    .unwrap();
    let louder = apply(&t, &quiet, &Edit::Normalize { loudness: -16.0, peak: -1.5, range: 11.0 }, &mut |_| {}).unwrap();
    let measured = hidden(t.ffmpeg().unwrap())
        .args(["-hide_banner", "-i"])
        .arg(&louder)
        .args(["-af", "ebur128=framelog=quiet", "-f", "null", "-"])
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&measured.stderr).into_owned();
    let line = report.lines().rev().find(|line| line.trim_start().starts_with("I:")).unwrap();
    let integrated: f64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!((integrated + 16.0).abs() < 2.0, "integrated loudness {integrated}");

    let picture = waveform(&t, &mp3, &dir.join("cache")).unwrap();
    assert_eq!(image_size(&t, &picture).unwrap(), (1400, 120));
    let frame = dir.join("frame.png");
    frame_at(&t, &clip, 2.0, &frame).unwrap();
    assert_eq!(image_size(&t, &frame).unwrap(), (1280, 720));

    // A real QR code, read straight and after being shrunk into a JPG.
    let text = "https://example.com/transmute?id=42";
    let code = dir.join("code.pgm");
    qr_image(&code, text);
    let png = transmute_core::convert(&t, &code.with_extension("pgm"), "png", &mut |_| {});
    assert!(png.is_err(), "PGM is not a supported input, so convert it directly");
    let as_png = dir.join("code.png");
    run(hidden(t.magick().unwrap()).arg(&code).arg(&as_png)).unwrap();
    assert_eq!(read_qr(&t, &as_png).unwrap(), vec![text.to_string()]);
    let as_jpg = dir.join("code.jpg");
    run(hidden(t.magick().unwrap()).arg(&as_png).args(["-quality", "70"]).arg(&as_jpg)).unwrap();
    assert_eq!(read_qr(&t, &as_jpg).unwrap(), vec![text.to_string()]);
    assert!(read_qr(&t, &photo).is_err(), "a photo has no code");

    // PDF extras.
    let doc = dir.join("doc.md");
    std::fs::write(&doc, "# Heading\n\nSome text.\n").unwrap();
    let pdf = transmute_core::convert(&t, &doc, "pdf", &mut |_| {}).unwrap();
    let archival = tool("pdfa", &pdf);
    let bytes = std::fs::read(&archival).unwrap();
    assert!(bytes.windows(10).any(|w| w == b"pdfaid:par"), "declares PDF/A");
    let info = run(hidden(t.poppler("pdftotext.exe").unwrap()).arg(&archival).arg("-")).unwrap();
    assert!(String::from_utf8_lossy(&info).contains("Heading"), "text survives");

    // PDF metadata, including non-ASCII text, read back by pdfinfo.
    let info_fields = set(&[("Title", "Bridge Report"), ("Author", "Zoë Müller"), ("Subject", "Tests"), ("Keywords", "a, b")]);
    let tagged_pdf = write_metadata(&t, &pdf, &info_fields, &mut |_| {}).unwrap();
    let read = read_metadata(&t, &tagged_pdf).unwrap();
    for name in ["Title", "Author", "Subject", "Keywords"] {
        assert_eq!(field(&read, name), field(&info_fields, name), "{name}");
    }
    assert!(String::from_utf8_lossy(&run(hidden(t.poppler("pdftotext.exe").unwrap()).arg(&tagged_pdf).arg("-")).unwrap()).contains("Heading"), "content kept");
    let cleared = tool("strip", &tagged_pdf);
    assert_eq!(field(&read_metadata(&t, &cleared).unwrap(), "Author"), "", "PDF metadata can be removed");

    // The loudness report describes a quiet input and a predicted output near target.
    let levels = measure_loudness(&t, &quiet, -16.0, -1.5, 11.0).unwrap();
    assert!(levels.input_loudness < -25.0, "input is quiet: {levels:?}");
    assert!((levels.output_loudness + 16.0).abs() < 3.0, "output near target: {levels:?}");

    // Soloing a channel: left has tone, right is silent.
    let one_side = dir.join("side.wav");
    run(hidden(t.ffmpeg().unwrap())
        .args(["-y", "-f", "lavfi", "-i", "aevalsrc='0.5*sin(2*PI*440*t)|0':s=44100:d=2"])
        .arg(&one_side))
    .unwrap();
    let peak = |path: &Path| -> f64 {
        let out = hidden(tools().ffmpeg().unwrap()).args(["-hide_banner", "-i"]).arg(path).args(["-af", "volumedetect", "-f", "null", "-"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stderr).into_owned();
        text.lines().find(|l| l.contains("max_volume:")).unwrap().split("max_volume:").nth(1).unwrap().trim().trim_end_matches(" dB").parse().unwrap()
    };
    let cache = dir.join("cache");
    assert!(peak(&transmute_core::edits::solo_channel(&t, &one_side, false, &cache).unwrap()) > -12.0, "left solo has sound");
    assert!(peak(&transmute_core::edits::solo_channel(&t, &one_side, true, &cache).unwrap()) < -60.0, "right solo is silent");

    let book = tool("epub-pages", &pdf);
    let listing = run(hidden(t.seven_zip().unwrap()).args(["l", "-ba"]).arg(&book)).unwrap();
    let listing = String::from_utf8_lossy(&listing).into_owned();
    assert!(listing.contains(".jpg") && listing.contains(".xhtml"), "{listing}");
}
