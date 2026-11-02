//! Finds faces and text with the detectors built into Windows, so nothing leaves
//! the machine. Runs as a helper process (`transmute --detect <image>`), which keeps
//! any failure in these system components away from the tray app.

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::Path;
use windows::core::HSTRING;
use windows::Graphics::Imaging::{BitmapDecoder, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::FaceAnalysis::FaceDetector;
use windows::Media::Ocr::OcrEngine;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// Rectangles as [x, y, width, height] in the image's pixels.
#[derive(Default, Serialize)]
pub struct Found {
    pub faces: Vec<[u32; 4]>,
    pub text: Vec<[u32; 4]>,
}

fn load(path: &Path) -> Result<SoftwareBitmap> {
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?.join()?;
    let stream = file.OpenAsync(FileAccessMode::Read)?.join()?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    Ok(decoder.GetSoftwareBitmapAsync()?.join()?)
}

fn faces(bitmap: &SoftwareBitmap) -> Result<Vec<[u32; 4]>> {
    let grey = SoftwareBitmap::Convert(bitmap, BitmapPixelFormat::Gray8)?;
    let detector = FaceDetector::CreateAsync()?.join()?;
    let found = detector.DetectFacesAsync(&grey)?.join()?;
    let mut boxes = Vec::new();
    for face in found {
        let area = face.FaceBox()?;
        boxes.push([area.X, area.Y, area.Width, area.Height]);
    }
    Ok(boxes)
}

fn text(bitmap: &SoftwareBitmap) -> Result<Vec<[u32; 4]>> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|_| anyhow!("no text recognition language is installed"))?;
    let result = engine.RecognizeAsync(bitmap)?.join()?;
    let mut boxes = Vec::new();
    for line in result.Lines()? {
        for word in line.Words()? {
            let area = word.BoundingRect()?;
            boxes.push([area.X as u32, area.Y as u32, area.Width as u32, area.Height as u32]);
        }
    }
    Ok(boxes)
}

fn detect(path: &Path) -> Result<Found> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let bitmap = load(path)?;
    let bgra = SoftwareBitmap::Convert(&bitmap, BitmapPixelFormat::Bgra8)?;
    Ok(Found { faces: faces(&bitmap)?, text: text(&bgra)? })
}

/// Entry point for `--detect`. Prints JSON and returns the process exit code.
pub fn run_helper(path: &Path) -> i32 {
    match detect(path).and_then(|found| Ok(serde_json::to_string(&found)?)) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}
