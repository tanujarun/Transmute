//! HEIC output. The bundled ImageMagick can read HEIC but not write it, so FFmpeg
//! encodes a single HEVC frame into MP4 and that frame is rewrapped as HEIF. MP4
//! and HEIF both store it as length-prefixed NAL units with an `hvcC` box, so the
//! bytes move across unchanged.

use crate::tools::{hidden, run, Tools};
use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;

fn sized_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A version-0, zero-flags full box.
fn full_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut body = vec![0u8; 4];
    body.extend_from_slice(payload);
    sized_box(kind, &body)
}

fn be32(value: u32) -> [u8; 4] {
    value.to_be_bytes()
}

/// Top-level box as (kind, payload).
fn find_top_level<'a>(file: &'a [u8], wanted: &[u8; 4]) -> Option<&'a [u8]> {
    let mut at = 0;
    while at + 8 <= file.len() {
        let size = u32::from_be_bytes(file[at..at + 4].try_into().ok()?) as usize;
        if size < 8 || at + size > file.len() {
            return None;
        }
        if &file[at + 4..at + 8] == wanted {
            return Some(&file[at + 8..at + size]);
        }
        at += size;
    }
    None
}

/// The whole `hvcC` box, header included, wherever it sits inside `moov`.
fn find_hvcc(moov: &[u8]) -> Option<&[u8]> {
    let tag = moov.windows(4).position(|window| window == b"hvcC")?;
    let start = tag.checked_sub(4)?;
    let size = u32::from_be_bytes(moov[start..tag].try_into().ok()?) as usize;
    moov.get(start..start + size)
}

fn build_heif(hvcc: &[u8], sample: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut brands = Vec::new();
    brands.extend_from_slice(b"heic");
    brands.extend_from_slice(&be32(0));
    brands.extend_from_slice(b"mif1heic");
    let ftyp = sized_box(b"ftyp", &brands);

    let mut hdlr = vec![0u8; 4];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0u8; 13]);
    let hdlr = full_box(b"hdlr", &hdlr);
    let pitm = full_box(b"pitm", &1u16.to_be_bytes());

    let mut infe = vec![0, 1, 0, 0];
    infe.extend_from_slice(b"hvc1");
    infe.push(0);
    let mut infe_box = vec![2, 0, 0, 0];
    infe_box.extend_from_slice(&infe);
    let infe = sized_box(b"infe", &infe_box);
    let mut iinf = vec![0, 1];
    iinf.extend_from_slice(&infe);
    let iinf = full_box(b"iinf", &iinf);

    let mut ispe = Vec::new();
    ispe.extend_from_slice(&be32(width));
    ispe.extend_from_slice(&be32(height));
    let ispe = full_box(b"ispe", &ispe);
    let mut ipco = hvcc.to_vec();
    ipco.extend_from_slice(&ispe);
    let ipco = sized_box(b"ipco", &ipco);
    // Item 1 has property 1 (hvcC, essential) and property 2 (ispe).
    let mut ipma = Vec::new();
    ipma.extend_from_slice(&be32(1));
    ipma.extend_from_slice(&[0, 1, 2, 0x81, 0x02]);
    let ipma = full_box(b"ipma", &ipma);
    let mut iprp = ipco;
    iprp.extend_from_slice(&ipma);
    let iprp = sized_box(b"iprp", &iprp);

    // The file offset of the image data depends on meta's size, which is fixed
    // because iloc uses 32-bit fields, so it is measured with a placeholder.
    let iloc = |offset: u32| {
        let mut body = vec![0x44, 0x00, 0, 1, 0, 1, 0, 0, 0, 1];
        body.extend_from_slice(&be32(offset));
        body.extend_from_slice(&be32(sample.len() as u32));
        full_box(b"iloc", &body)
    };
    let meta = |offset: u32| {
        let mut body = hdlr.clone();
        body.extend_from_slice(&pitm);
        body.extend_from_slice(&iloc(offset));
        body.extend_from_slice(&iinf);
        body.extend_from_slice(&iprp);
        full_box(b"meta", &body)
    };
    let offset = ftyp.len() + meta(0).len() + 8;

    let mut file = ftyp;
    file.extend_from_slice(&meta(offset as u32));
    file.extend_from_slice(&sized_box(b"mdat", sample));
    file
}

/// Encodes any image ImageMagick can read as a HEIC file.
pub fn write_heic(tools: &Tools, src: &Path, out: &Path, work: &Path) -> Result<()> {
    write_heic_at(tools, src, out, work, 20)
}

/// As `write_heic`, at a chosen HEVC quality where a lower CRF means higher quality.
pub fn write_heic_at(tools: &Tools, src: &Path, out: &Path, work: &Path, crf: u32) -> Result<()> {
    let flat = work.join("heic-input.png");
    let mut input = src.as_os_str().to_os_string();
    input.push("[0]");
    run(hidden(tools.magick()?)
        .arg(input)
        .args(["-auto-orient", "-background", "white", "-alpha", "remove", "-alpha", "off"])
        .arg(&flat))?;
    let size = run(hidden(tools.magick()?).args(["identify", "-format", "%w %h"]).arg(&flat))?;
    let size = String::from_utf8_lossy(&size);
    let mut parts = size.split_whitespace().map(|part| part.parse::<u32>());
    let (Some(Ok(width)), Some(Ok(height))) = (parts.next(), parts.next()) else {
        bail!("could not read the image size");
    };
    // 4:2:0 video needs even dimensions.
    let (width, height) = (width & !1, height & !1);
    if width == 0 || height == 0 {
        bail!("image is too small for HEIC");
    }

    let frame = work.join("heic-frame.mp4");
    run(hidden(tools.ffmpeg()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&flat)
        .arg("-vf")
        .arg(format!("crop={width}:{height}:0:0,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p"))
        .args(["-frames:v", "1", "-an", "-c:v", "libx265", "-preset", "medium"])
        .arg("-crf")
        .arg(crf.to_string())
        .args(["-x265-params", "info=0:log-level=error", "-tag:v", "hvc1"])
        .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "iec61966-2-1"])
        .arg(&frame))?;

    let bytes = std::fs::read(&frame).context("reading the encoded frame")?;
    let moov = find_top_level(&bytes, b"moov").ok_or_else(|| anyhow!("encoded frame has no moov"))?;
    let hvcc = find_hvcc(moov).ok_or_else(|| anyhow!("encoded frame has no hvcC"))?;
    let sample = find_top_level(&bytes, b"mdat").ok_or_else(|| anyhow!("encoded frame has no mdat"))?;
    std::fs::write(out, build_heif(hvcc, sample, width, height))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iloc_points_at_the_image_data() {
        let hvcc = sized_box(b"hvcC", &[1, 2, 3]);
        let sample = [9u8, 8, 7, 6];
        let file = build_heif(&hvcc, &sample, 64, 48);
        let tag = file.windows(4).position(|w| w == b"iloc").unwrap();
        // size(4) tag(4) fullbox(4) layout(2) count(2) id(2) ref(2) extents(2)
        let at = tag - 4 + 8 + 4 + 2 + 2 + 2 + 2 + 2;
        let offset = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        let length = u32::from_be_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        assert_eq!(&file[offset..offset + length], &sample);
    }

    #[test]
    fn locates_nested_hvcc_box() {
        let hvcc = sized_box(b"hvcC", &[5, 5]);
        let mut moov = vec![0u8; 3];
        moov.extend_from_slice(&hvcc);
        moov.extend_from_slice(&[1, 1]);
        assert_eq!(find_hvcc(&moov), Some(hvcc.as_slice()));
    }

    #[test]
    fn walks_top_level_boxes() {
        let mut file = sized_box(b"ftyp", &[0; 4]);
        file.extend_from_slice(&sized_box(b"mdat", &[1, 2]));
        assert_eq!(find_top_level(&file, b"mdat"), Some(&[1u8, 2][..]));
        assert_eq!(find_top_level(&file, b"moov"), None);
    }
}
