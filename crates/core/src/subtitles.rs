//! SRT and VTT conversion, keeping cue timing exact.

use anyhow::Result;
use std::path::Path;

struct Cue {
    timing: String,
    text: Vec<String>,
}

fn parse(source: &str) -> Vec<Cue> {
    let normalised = source.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in normalised.split("\n\n") {
        let mut lines = block.lines().skip_while(|line| !line.contains("-->"));
        if let Some(timing) = lines.next() {
            cues.push(Cue {
                timing: timing.trim().to_string(),
                text: lines.map(String::from).collect(),
            });
        }
    }
    cues
}

fn strip_tags(line: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    plain
}

fn render(cues: &[Cue], target: &str) -> String {
    let mut out = String::new();
    match target {
        "vtt" => {
            out.push_str("WEBVTT\n\n");
            for cue in cues {
                out.push_str(&cue.timing.replace(',', "."));
                out.push('\n');
                out.push_str(&cue.text.join("\n"));
                out.push_str("\n\n");
            }
        }
        "srt" => {
            for (index, cue) in cues.iter().enumerate() {
                // VTT cue settings follow the end time and have no SRT form.
                let timing: Vec<&str> = cue.timing.split_whitespace().take(3).collect();
                out.push_str(&format!("{}\n{}\n", index + 1, timing.join(" ").replace('.', ",")));
                out.push_str(&cue.text.join("\n"));
                out.push_str("\n\n");
            }
        }
        _ => {
            for cue in cues {
                for line in &cue.text {
                    out.push_str(&strip_tags(line));
                    out.push('\n');
                }
            }
        }
    }
    out
}

pub fn convert(src: &Path, target: &str, out: &Path) -> Result<()> {
    let text = String::from_utf8_lossy(&std::fs::read(src)?).into_owned();
    std::fs::write(out, render(&parse(&text), target))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "1\r\n00:00:01,500 --> 00:00:03,000\r\nHello <i>there</i>\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,000\r\nSecond\r\nline\r\n";

    #[test]
    fn srt_to_vtt_keeps_timing() {
        let vtt = render(&parse(SRT), "vtt");
        assert!(vtt.starts_with("WEBVTT\n\n00:00:01.500 --> 00:00:03.000\nHello"));
    }

    #[test]
    fn vtt_to_srt_numbers_cues_and_drops_settings() {
        let vtt = "WEBVTT\n\n00:00:01.500 --> 00:00:03.000 align:start\nHi\n";
        assert_eq!(render(&parse(vtt), "srt"), "1\n00:00:01,500 --> 00:00:03,000\nHi\n\n");
    }

    #[test]
    fn text_export_removes_tags_and_timing() {
        assert_eq!(render(&parse(SRT), "txt"), "Hello there\nSecond\nline\n");
    }
}
