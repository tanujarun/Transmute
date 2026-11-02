//! The Alt+drag gesture: global input hooks plus a radial overlay that acts as
//! an OLE drop target. Everything in here except `render` is Windows-specific.

mod hooks;
mod overlay;
pub mod render;

pub use hooks::set_trigger_key;

use std::path::PathBuf;

/// One selectable slice of a wheel.
#[derive(Clone, Debug)]
pub struct Segment {
    pub id: String,
    pub label: String,
}

/// A mode of the wheel, such as Convert, Tools or Actions.
#[derive(Clone, Debug)]
pub struct Wheel {
    pub title: String,
    pub segments: Vec<Segment>,
}

/// Emitted when the user drops files on a segment.
#[derive(Clone, Debug)]
pub struct Selection {
    pub paths: Vec<PathBuf>,
    pub wheel: String,
    pub segment_id: String,
}

/// Which wheels to offer for the dragged files. Wheels with no segments are skipped.
pub type WheelProvider = Box<dyn Fn(&[PathBuf]) -> Vec<Wheel>>;
/// Called on the overlay thread during the drop, so it must return quickly.
pub type SelectionHandler = Box<dyn Fn(Selection)>;

/// Installs the hooks and blocks forever, running the overlay message loop.
pub fn run(provider: WheelProvider, on_select: SelectionHandler) -> windows::core::Result<()> {
    overlay::run(provider, on_select)
}

pub(crate) fn log(message: &str) {
    if std::env::var_os("TRANSMUTE_LOG").is_some() {
        eprintln!("[gesture] {message}");
    }
}
