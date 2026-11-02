//! Runs each photo and PDF edit on generated files and measures the result.
//! Needs the tools folder, so it is ignored by default:
//!   cargo test -p transmute-core --release --test image_edits -- --ignored --nocapture

use std::path::{Path, PathBuf};
use transmute_core::image_edits::{apply, collage, image_size, pdf_thumbnails, CollageStyle, PageChoice, PhotoEdit, Region, Shape};
use transmute_core::tools::{hidden, run};
use transmute_core::Tools;

fn tools() -> Tools {
    Tools::at(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools"))
}

fn edit(src: &Path, edit: PhotoEdit) -> PathBuf {
    apply(&tools(), src, &edit).unwrap_or_else(|e| panic!("{edit:?}: {e:#}"))
}

/// Mean colour of a region as (r, g, b), each 0-255.
fn color_at(image: &Path, crop: &str) -> (f64, f64, f64) {
    let out = run(hidden(tools().magick().unwrap())
        .arg(image)
        .args(["-crop", crop, "+repage", "-scale", "1x1!", "-format", "%[fx:255*r] %[fx:255*g] %[fx:255*b]", "info:"]))
    .unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let v: Vec<f64> = text.split_whitespace().map(|p| p.parse().unwrap()).collect();
    (v[0], v[1], v[2])
}

fn pages(pdf: &Path) -> u32 {
    let info = run(hidden(tools().poppler("pdfinfo.exe").unwrap()).arg(pdf)).unwrap();
    let text = String::from_utf8_lossy(&info).into_owned();
    text.lines().find(|l| l.starts_with("Pages:")).unwrap().split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn page_text(pdf: &Path, page: u32) -> String {
    let out = run(hidden(tools().poppler("pdftotext.exe").unwrap())
        .args(["-f", &page.to_string(), "-l", &page.to_string()])
        .arg(pdf)
        .arg("-"))
    .unwrap();
    String::from_utf8_lossy(&out).trim().to_string()
}

#[test]
#[ignore]
fn every_photo_and_pdf_edit_does_what_it_says() {
    let t = tools();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();

    let white = dir.join("white.png");
    run(hidden(t.magick().unwrap()).args(["-size", "400x300", "xc:white"]).arg(&white)).unwrap();
    let original = std::fs::read(&white).unwrap();

    let crop = edit(&white, PhotoEdit::CropImage { x: 10, y: 20, width: 120, height: 80 });
    assert_eq!(image_size(&t, &crop).unwrap(), (120, 80));

    let resized = edit(&white, PhotoEdit::ResizeImage { width: 200, height: 50 });
    assert_eq!(image_size(&t, &resized).unwrap(), (200, 50));

    // Half brightness on a mid-grey must darken it.
    let grey = dir.join("grey.png");
    run(hidden(t.magick().unwrap()).args(["-size", "100x100", "xc:#808080"]).arg(&grey)).unwrap();
    let adjust = PhotoEdit::AdjustImage { brightness: -50, contrast: 0, saturation: 0, hue: 0, sharpen: 0.0, blur: 0.0, grain: 0.0, dehaze: 0.0, clarity: 0.0, denoise: 0.0 };
    let darker = edit(&grey, adjust);
    assert!(color_at(&darker, "100x100+0+0").0 < 90.0, "brightness lowered");
    let grainy = PhotoEdit::AdjustImage { brightness: 0, contrast: 10, saturation: 20, hue: 10, sharpen: 1.0, blur: 1.0, grain: 0.5, dehaze: 0.0, clarity: 0.0, denoise: 0.0 };
    assert!(edit(&grey, grainy).is_file());

    // Measured by the standard deviation of the pixels: noise reduction lowers it,
    // dehaze and clarity raise it.
    let spread = |image: &Path| -> f64 {
        let out = run(hidden(tools().magick().unwrap()).arg(image).args(["-format", "%[fx:standard_deviation]", "info:"])).unwrap();
        String::from_utf8_lossy(&out).trim().parse().unwrap()
    };
    let plain = PhotoEdit::AdjustImage { brightness: 0, contrast: 0, saturation: 0, hue: 0, sharpen: 0.0, blur: 0.0, grain: 0.0, dehaze: 0.0, clarity: 0.0, denoise: 0.0 };
    let noisy = dir.join("noisy.png");
    run(hidden(t.magick().unwrap()).args(["-size", "300x200", "xc:gray50", "-attenuate", "0.6", "+noise", "Gaussian"]).arg(&noisy)).unwrap();
    let PhotoEdit::AdjustImage { .. } = plain else { unreachable!() };
    let with = |dehaze: f64, clarity: f64, denoise: f64| PhotoEdit::AdjustImage {
        brightness: 0, contrast: 0, saturation: 0, hue: 0, sharpen: 0.0, blur: 0.0, grain: 0.0, dehaze, clarity, denoise,
    };
    assert!(spread(&edit(&noisy, with(0.0, 0.0, 15.0))) < spread(&noisy) * 0.7, "noise reduction works");
    let hazy = dir.join("hazy.png");
    run(hidden(t.magick().unwrap()).args(["-size", "300x200", "gradient:#808080-#b0b0b0"]).arg(&hazy)).unwrap();
    assert!(spread(&edit(&hazy, with(60.0, 0.0, 0.0))) > spread(&hazy) * 1.5, "dehaze restores contrast");
    let soft = dir.join("soft.png");
    run(hidden(t.magick().unwrap()).args(["-size", "300x200", "plasma:", "-blur", "0x4"]).arg(&soft)).unwrap();
    assert!(spread(&edit(&soft, with(0.0, 80.0, 0.0))) > spread(&soft) * 1.02, "clarity adds local contrast");

    // A red canvas, padding 50, image centred, corners rounded.
    let framed = edit(
        &white,
        PhotoEdit::BackgroundImage { color: "#ff0000".into(), image: None, ratio: 0.0, padding: 50, radius: 20, shadow: false, backdrop_blur: 0.0 },
    );
    assert_eq!(image_size(&t, &framed).unwrap(), (500, 400));
    assert!(color_at(&framed, "10x10+5+5").0 > 240.0 && color_at(&framed, "10x10+5+5").1 < 15.0, "canvas is red");
    assert!(color_at(&framed, "10x10+240+190").1 > 240.0, "image is in the centre");
    assert!(color_at(&framed, "3x3+50+50").1 < 100.0, "corner is rounded away");
    let wide = edit(
        &white,
        PhotoEdit::BackgroundImage { color: "#000000".into(), image: None, ratio: 2.0, padding: 0, radius: 0, shadow: true, backdrop_blur: 0.0 },
    );
    assert_eq!(image_size(&t, &wide).unwrap(), (600, 300));
    let backdrop = edit(
        &white,
        PhotoEdit::BackgroundImage { color: "#000000".into(), image: Some(grey.clone()), ratio: 0.0, padding: 40, radius: 0, shadow: false, backdrop_blur: 3.0 },
    );
    assert!((color_at(&backdrop, "10x10+5+5").0 - 128.0).abs() < 12.0, "background image is used");

    // Redaction hits only its own area.
    let split = dir.join("half.png");
    run(hidden(t.magick().unwrap())
        .args(["-size", "200x200", "xc:white", "-fill", "black", "-draw", "rectangle 0,0 99,199"])
        .arg(&split))
    .unwrap();
    let redacted = edit(
        &white,
        PhotoEdit::RedactImage { regions: vec![Region { x: 20, y: 20, width: 60, height: 60, blur: false }] },
    );
    assert!(color_at(&redacted, "20x20+40+40").0 < 10.0, "black box drawn");
    assert!(color_at(&redacted, "20x20+300+200").0 > 245.0, "rest untouched");
    let blurred = edit(
        &split,
        PhotoEdit::RedactImage { regions: vec![Region { x: 60, y: 60, width: 80, height: 80, blur: true }] },
    );
    let edge = color_at(&blurred, "4x4+98+100").0;
    assert!(edge > 40.0 && edge < 220.0, "hard edge is blurred: {edge}");
    assert!(color_at(&blurred, "4x4+98+10").0 < 200.0, "outside the area stays sharp");

    let shapes = vec![
        Shape::Box { x1: 20.0, y1: 20.0, x2: 100.0, y2: 100.0, color: "#ff0000".into() },
        Shape::Arrow { x1: 150.0, y1: 150.0, x2: 300.0, y2: 150.0, color: "#0000ff".into() },
        Shape::Ellipse { x1: 200.0, y1: 20.0, x2: 300.0, y2: 80.0, color: "#00ff00".into() },
        Shape::Highlight { x1: 20.0, y1: 200.0, x2: 120.0, y2: 240.0 },
        Shape::Text { x: 150.0, y: 240.0, text: "Hi \"there\" %w \\n".into(), color: "#000000".into(), size: 24 },
        Shape::Number { x: 350.0, y: 250.0, value: 7, color: "#ff0000".into(), size: 24 },
        Shape::Pen { points: vec![(320.0, 30.0), (380.0, 30.0), (380.0, 90.0)], color: "#ff00ff".into() },
    ];
    let marked = edit(&white, PhotoEdit::AnnotateImage { shapes });
    let (r, g, b) = color_at(&marked, "8x1+350+30");
    assert!(r > 200.0 && g < 60.0 && b > 200.0, "pen line drawn: {r},{g},{b}");

    // Magnifier: a red dot grows to fill the lens, and a picture lands where placed.
    let dotted = dir.join("dotted.png");
    run(hidden(t.magick().unwrap()).args(["-size", "400x300", "xc:white", "-fill", "red", "-draw", "circle 100,100 100,110"]).arg(&dotted)).unwrap();
    let lens = edit(&dotted, PhotoEdit::AnnotateImage { shapes: vec![Shape::Magnifier { x: 100.0, y: 100.0, radius: 60, zoom: 3.0 }] });
    assert!(color_at(&lens, "4x4+128+98").1 < 60.0, "dot is enlarged past its old edge");
    assert!(color_at(&lens, "4x4+300+250").1 > 245.0, "outside the lens untouched");
    let sticker = dir.join("sticker.png");
    run(hidden(t.magick().unwrap()).args(["-size", "50x50", "xc:#0000ff"]).arg(&sticker)).unwrap();
    let placed = edit(&white, PhotoEdit::AnnotateImage { shapes: vec![Shape::Picture { x: 200.0, y: 100.0, width: 100, path: sticker }] });
    assert!(color_at(&placed, "10x10+240+140").2 > 240.0 && color_at(&placed, "10x10+240+140").0 < 15.0, "picture placed and scaled");
    assert!(color_at(&placed, "10x10+100+50").0 > 245.0, "rest untouched");
    let (r, g, _) = color_at(&marked, "2x2+60+20");
    assert!(r > 200.0 && g < 60.0, "box outline is red: {r},{g}");
    assert!(color_at(&marked, "8x1+220+150").2 > 180.0 && color_at(&marked, "8x1+220+150").0 < 80.0, "arrow is blue");
    let (r, g, b) = color_at(&marked, "20x10+60+215");
    assert!(b < 200.0 && r > 230.0 && g > 230.0, "highlight tints yellow: {r},{g},{b}");
    assert!(color_at(&marked, "20x20+150+220").0 < 250.0, "text was drawn");

    // Rendered text must match a direct render in the real font, %w and all.
    let typed = edit(
        &white,
        PhotoEdit::AnnotateImage {
            shapes: vec![Shape::Text { x: 20.0, y: 100.0, text: "50% %w".into(), color: "#000000".into(), size: 40 }],
        },
    );
    let expected = dir.join("expected.png");
    run(hidden(t.magick().unwrap())
        .args(["-size", "400x300", "xc:white", "-font", "C:/Windows/Fonts/arial.ttf", "-pointsize", "40"])
        .args(["-fill", "#000000", "-stroke", "none", "-annotate", "+20+100", "50%% %%w"])
        .arg(&expected))
    .unwrap();
    let compared = hidden(t.magick().unwrap())
        .args(["compare", "-metric", "AE"])
        .arg(&typed)
        .arg(&expected)
        .arg("null:")
        .output()
        .unwrap();
    let difference = String::from_utf8_lossy(&compared.stderr).into_owned();
    let pixels: f64 = difference.split_whitespace().next().unwrap().parse().unwrap();
    assert!(pixels < 50.0, "text differs by {difference}");
    let warnings = hidden(t.magick().unwrap())
        .args(["-size", "10x10", "xc:white", "-font", "C:/Windows/Fonts/arial.ttf", "-annotate", "+1+1", "x", "null:"])
        .output()
        .unwrap();
    assert!(warnings.stderr.is_empty(), "font loads cleanly");

    // A file name with brackets is a name, not a frame selector.
    let bracketed = dir.join("shot[1].png");
    std::fs::copy(&white, &bracketed).unwrap();
    assert_eq!(image_size(&t, &edit(&bracketed, PhotoEdit::ResizeImage { width: 50, height: 50 })).unwrap(), (50, 50));
    assert!(color_at(&marked, "6x6+347+247").0 > 200.0, "number badge drawn");

    let bad = PhotoEdit::AnnotateImage {
        shapes: vec![Shape::Box { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0, color: "red\" image Over".into() }],
    };
    assert!(apply(&t, &white, &bad).is_err(), "unsafe colour refused");

    let parts: Vec<PathBuf> = (0..4)
        .map(|index| {
            let path = dir.join(format!("p{index}.png"));
            run(hidden(t.magick().unwrap()).args(["-size", "300x200", "xc:gray50"]).arg(&path)).unwrap();
            path
        })
        .collect();
    for layout in ["grid", "row", "column", "featured"] {
        let made = collage(&t, &parts, layout, 8, "#102030", &CollageStyle::default()).unwrap();
        let (width, height) = image_size(&t, &made).unwrap();
        println!("{layout}: {width}x{height}");
        match layout {
            // 300x200 images: four at 1200x800 in a row, 8px gaps and border.
            "row" => assert_eq!((width, height), (4840, 816)),
            "column" => assert_eq!((width, height), (816, 2172)),
            "grid" => assert_eq!((width, height), (1624, 1624)),
            _ => assert_eq!(height, 1216),
        }
        assert!(color_at(&made, "3x3+1+1").2 > 40.0, "{layout}: border uses the background colour");
    }

    let finished = collage(&t, &parts, "grid", 8, "#102030", &CollageStyle { corners: 60, width: 1000 }).unwrap();
    assert_eq!(image_size(&t, &finished).unwrap(), (1000, 1000), "final width");
    let (_, _, corner) = color_at(&finished, "3x3+6+6");
    assert!(corner > 40.0, "rounded corner shows the background: {corner}");

    assert_eq!(std::fs::read(&white).unwrap(), original, "original changed");

    // A three-page PDF with one word per page.
    let word = |text: &str| {
        let path = dir.join(format!("{text}.txt"));
        std::fs::write(&path, text).unwrap();
        path
    };
    let page_pdfs: Vec<PathBuf> = ["ALPHA", "BRAVO", "CHARLIE"]
        .iter()
        .map(|name| {
            let source = word(name);
            transmute_core::convert(&t, &source, "pdf", &mut |_| {}).unwrap()
        })
        .collect();
    let merged = transmute_core::filetools::run_tool(&t, "merge-pdf", &page_pdfs, &mut |_| {}, &mut |_, e| panic!("{e:#}"));
    let pdf = &merged[0];
    assert_eq!(pages(pdf), 3);

    let choice = |page, rotate| PageChoice { page, rotate };
    let organized = edit(pdf, PhotoEdit::OrganizePdf { pages: vec![choice(3, 0), choice(1, 90), choice(1, 0)] });
    assert_eq!(pages(&organized), 3);
    assert!(page_text(&organized, 1).contains("CHARLIE"));
    assert!(page_text(&organized, 2).contains("ALPHA"));
    assert!(page_text(&organized, 3).contains("ALPHA"));
    let info = run(hidden(t.poppler("pdfinfo.exe").unwrap()).args(["-f", "2", "-l", "2"]).arg(&organized)).unwrap();
    assert!(String::from_utf8_lossy(&info).contains("rot:   90"), "second page rotated");
    let thumbs = pdf_thumbnails(&t, pdf, &dir.join("thumbs")).unwrap();
    assert_eq!(thumbs.len(), 3);
}
