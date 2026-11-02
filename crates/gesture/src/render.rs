//! Platform-neutral wheel geometry and drawing. Deliberately plain for now;
//! presentation comes after the feature set is locked.

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::f32::consts::{PI, TAU};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

pub const BASE_SIZE: f32 = 440.0;
const OUTER_RADIUS: f32 = 205.0;
const INNER_RADIUS: f32 = 78.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Center,
    Segment(usize),
    Outside,
}

/// Maps an offset from the wheel centre (at 1x scale) to what is under it.
/// Segment 0 is centred at twelve o'clock and indices run clockwise.
pub fn hit_test(dx: f32, dy: f32, count: usize) -> Hit {
    let distance = (dx * dx + dy * dy).sqrt();
    if distance < INNER_RADIUS {
        return Hit::Center;
    }
    if distance > OUTER_RADIUS || count == 0 {
        return Hit::Outside;
    }
    let step = TAU / count as f32;
    let angle = dx.atan2(-dy).rem_euclid(TAU);
    Hit::Segment((((angle + step / 2.0) / step) as usize) % count)
}

pub struct WheelView<'a> {
    pub title: &'a str,
    pub labels: Vec<&'a str>,
    pub hovered: Option<usize>,
    pub mode_index: usize,
    pub mode_count: usize,
}

pub struct Renderer {
    font: Option<FontVec>,
}

impl Renderer {
    pub fn new() -> Self {
        let font = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf")
            .ok()
            .and_then(|bytes| FontVec::try_from_vec(bytes).ok());
        Self { font }
    }

    /// Draws the wheel into a premultiplied RGBA pixmap of `BASE_SIZE * scale` pixels.
    /// Everything inside the outer circle has non-zero alpha so it receives the drop;
    /// the corners stay fully transparent and pass the drag through to what is below.
    pub fn draw(&self, view: &WheelView, scale: f32) -> Pixmap {
        let size = (BASE_SIZE * scale).round() as u32;
        let mut pixmap = Pixmap::new(size, size).expect("wheel pixmap");
        let center = size as f32 / 2.0;
        let count = view.labels.len().max(1);
        let step = TAU / count as f32;

        for (index, label) in view.labels.iter().enumerate() {
            let middle = index as f32 * step - PI / 2.0;
            let fill = if view.hovered == Some(index) {
                Color::from_rgba8(240, 130, 30, 245)
            } else {
                Color::from_rgba8(38, 38, 42, 235)
            };
            fill_annular_sector(
                &mut pixmap,
                center,
                INNER_RADIUS * scale,
                OUTER_RADIUS * scale,
                middle - step / 2.0 + 0.006,
                middle + step / 2.0 - 0.006,
                fill,
            );
            let radius = (INNER_RADIUS + OUTER_RADIUS) / 2.0 * scale;
            let x = center + middle.cos() * radius;
            let y = center + middle.sin() * radius;
            self.text(&mut pixmap, label, x, y, 15.0 * scale, [255, 255, 255]);
        }

        fill_circle(&mut pixmap, center, (INNER_RADIUS - 2.0) * scale, Color::from_rgba8(24, 24, 27, 240));
        let caption = view.hovered.and_then(|i| view.labels.get(i).copied()).unwrap_or("Cancel");
        self.text(&mut pixmap, view.title, center, center - 16.0 * scale, 13.0 * scale, [190, 190, 195]);
        self.text(&mut pixmap, caption, center, center + 6.0 * scale, 17.0 * scale, [255, 255, 255]);

        let gap = 12.0 * scale;
        let first = center - gap * (view.mode_count.saturating_sub(1)) as f32 / 2.0;
        for mode in 0..view.mode_count {
            let color = if mode == view.mode_index {
                Color::from_rgba8(240, 130, 30, 255)
            } else {
                Color::from_rgba8(110, 110, 115, 255)
            };
            fill_dot(&mut pixmap, first + mode as f32 * gap, center + 30.0 * scale, 3.5 * scale, color);
        }
        pixmap
    }

    fn text(&self, pixmap: &mut Pixmap, text: &str, cx: f32, cy: f32, px: f32, rgb: [u8; 3]) {
        let Some(font) = &self.font else { return };
        let scaled = font.as_scaled(PxScale::from(px));
        let width: f32 = text.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum();
        let mut x = cx - width / 2.0;
        let baseline = cy + (scaled.ascent() + scaled.descent()) / 2.0;
        let stride = pixmap.width() as i32;
        let height = pixmap.height() as i32;
        let pixels = pixmap.data_mut();
        for c in text.chars() {
            let id = font.glyph_id(c);
            let glyph = id.with_scale_and_position(PxScale::from(px), point(x, baseline));
            x += scaled.h_advance(id);
            let Some(outlined) = font.outline_glyph(glyph) else { continue };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                let px_x = bounds.min.x as i32 + gx as i32;
                let px_y = bounds.min.y as i32 + gy as i32;
                if px_x < 0 || px_y < 0 || px_x >= stride || px_y >= height {
                    return;
                }
                let offset = ((px_y * stride + px_x) * 4) as usize;
                let a = coverage.clamp(0.0, 1.0);
                for channel in 0..3 {
                    let below = pixels[offset + channel] as f32;
                    pixels[offset + channel] = (below * (1.0 - a) + rgb[channel] as f32 * a) as u8;
                }
                let alpha = pixels[offset + 3] as f32;
                pixels[offset + 3] = (alpha * (1.0 - a) + 255.0 * a) as u8;
            });
        }
    }
}

fn solid(color: Color) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    paint
}

fn fill_annular_sector(
    pixmap: &mut Pixmap,
    center: f32,
    inner: f32,
    outer: f32,
    start: f32,
    end: f32,
    color: Color,
) {
    const STEPS: usize = 48;
    let mut path = PathBuilder::new();
    for i in 0..=STEPS {
        let angle = start + (end - start) * i as f32 / STEPS as f32;
        let (x, y) = (center + angle.cos() * outer, center + angle.sin() * outer);
        if i == 0 {
            path.move_to(x, y);
        } else {
            path.line_to(x, y);
        }
    }
    for i in (0..=STEPS).rev() {
        let angle = start + (end - start) * i as f32 / STEPS as f32;
        path.line_to(center + angle.cos() * inner, center + angle.sin() * inner);
    }
    path.close();
    if let Some(path) = path.finish() {
        pixmap.fill_path(&path, &solid(color), FillRule::Winding, Transform::identity(), None);
    }
}

fn fill_circle(pixmap: &mut Pixmap, center: f32, radius: f32, color: Color) {
    fill_dot(pixmap, center, center, radius, color);
}

fn fill_dot(pixmap: &mut Pixmap, x: f32, y: f32, radius: f32, color: Color) {
    if let Some(path) = PathBuilder::from_circle(x, y, radius) {
        pixmap.fill_path(&path, &solid(color), FillRule::Winding, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centre_and_outside() {
        assert_eq!(hit_test(0.0, 0.0, 6), Hit::Center);
        assert_eq!(hit_test(300.0, 0.0, 6), Hit::Outside);
    }

    #[test]
    fn segments_run_clockwise_from_the_top() {
        assert_eq!(hit_test(0.0, -150.0, 4), Hit::Segment(0));
        assert_eq!(hit_test(150.0, 0.0, 4), Hit::Segment(1));
        assert_eq!(hit_test(0.0, 150.0, 4), Hit::Segment(2));
        assert_eq!(hit_test(-150.0, 0.0, 4), Hit::Segment(3));
    }

    #[test]
    fn segment_zero_wraps_across_twelve_o_clock() {
        assert_eq!(hit_test(-20.0, -150.0, 4), Hit::Segment(0));
    }
}
