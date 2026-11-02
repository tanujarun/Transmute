//! Standalone gesture test: three fixed wheels, selections printed to stdout.
//! Run with TRANSMUTE_LOG=1 to trace the state machine on stderr.

use transmute_gesture::{run, Segment, Wheel};

fn wheel(title: &str, labels: &[&str]) -> Wheel {
    Wheel {
        title: title.into(),
        segments: labels
            .iter()
            .map(|label| Segment { id: label.to_lowercase(), label: (*label).into() })
            .collect(),
    }
}

fn main() {
    let provider = Box::new(|paths: &[std::path::PathBuf]| {
        println!("drag: {paths:?}");
        vec![
            wheel("Convert", &["PNG", "WEBP", "HEIC", "TIFF", "AVIF", "BMP", "PDF"]),
            wheel("Tools", &["Compress", "Metadata", "Crop"]),
            wheel("Actions", &["Share", "Float", "Copy", "Move", "Copy Path", "Terminal"]),
        ]
    });
    let on_select = Box::new(|selection: transmute_gesture::Selection| {
        println!("selected: {} / {} for {:?}", selection.wheel, selection.segment_id, selection.paths);
    });
    run(provider, on_select).expect("gesture loop");
}
