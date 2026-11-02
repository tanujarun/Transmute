//! The wheel window. It starts as an invisible probe under the cursor: OLE only
//! tells us what is being dragged once the drag enters one of our windows, so a
//! file drag reveals itself through DragEnter and anything else times out.

use std::cell::RefCell;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;
use windows::core::{implement, w, Ref, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::hooks::{self, MSG_BUTTON_UP, MSG_CANCEL, MSG_TRIGGER, WHEEL_OPEN};
use crate::render::{hit_test, Hit, Renderer, WheelView, BASE_SIZE};
use crate::{log, Selection, SelectionHandler, Wheel, WheelProvider};

const MSG_HIDE: u32 = WM_APP + 5;
const TIMER_PROBE: usize = 1;
const TIMER_CLOSE: usize = 2;
const PROBE_MS: u32 = 220;
/// Button-up reaches the hook before OLE delivers Drop, so closing waits briefly.
const CLOSE_GRACE_MS: u32 = 150;
/// One notch on a standard wheel. High-resolution wheels send smaller steps.
const WHEEL_NOTCH: i32 = 120;
/// Free-spinning wheels fire many notches at once; one switch per this long.
const CYCLE_COOLDOWN: Duration = Duration::from_millis(180);

#[derive(PartialEq, Clone, Copy, Debug)]
enum Phase {
    Hidden,
    Probing,
    Open,
}

struct State {
    hwnd: HWND,
    phase: Phase,
    provider: WheelProvider,
    on_select: SelectionHandler,
    renderer: Renderer,
    wheels: Vec<Wheel>,
    paths: Vec<PathBuf>,
    active: usize,
    hovered: Option<usize>,
    /// Physical-pixel screen position of the window's top-left corner.
    origin: POINT,
    scale: f32,
    scroll_gate: ScrollGate,
}

/// Turns raw wheel deltas into switches, one per notch and no faster than the cooldown.
#[derive(Default)]
struct ScrollGate {
    total: i32,
    last: Option<Instant>,
}

impl ScrollGate {
    /// Returns the direction to switch, true for forward, once a notch adds up.
    fn feed(&mut self, delta: i32, now: Instant) -> Option<bool> {
        self.total += delta;
        if self.total.abs() < WHEEL_NOTCH {
            return None;
        }
        let forward = self.total < 0;
        self.total = 0;
        if self.last.is_some_and(|last| now.duration_since(last) < CYCLE_COOLDOWN) {
            return None;
        }
        self.last = Some(now);
        Some(forward)
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.borrow_mut().as_mut().map(f))
}

impl State {
    fn window_size(&self) -> i32 {
        (BASE_SIZE * self.scale).round() as i32
    }

    fn begin_probe(&mut self) {
        if self.phase != Phase::Hidden {
            return;
        }
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            self.scale = dpi_x as f32 / 96.0;

            let size = self.window_size();
            let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let area = info.rcMonitor;
            self.origin = POINT {
                x: (cursor.x - size / 2).clamp(area.left, (area.right - size).max(area.left)),
                y: (cursor.y - size / 2).clamp(area.top, (area.bottom - size).max(area.top)),
            };

            let mut probe = Pixmap::new(size as u32, size as u32).expect("probe pixmap");
            probe.data_mut().chunks_exact_mut(4).for_each(|p| p[3] = 1);
            self.present(&probe);
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                self.origin.x,
                self.origin.y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            SetTimer(Some(self.hwnd), TIMER_PROBE, PROBE_MS, None);
            nudge_cursor();
        }
        self.phase = Phase::Probing;
        log("probing");
    }

    fn open(&mut self, paths: Vec<PathBuf>) {
        let wheels: Vec<Wheel> =
            (self.provider)(&paths).into_iter().filter(|w| !w.segments.is_empty()).collect();
        if wheels.is_empty() {
            log("no wheels for these files");
            self.post_hide();
            return;
        }
        self.wheels = wheels;
        self.paths = paths;
        self.active = 0;
        self.hovered = None;
        self.phase = Phase::Open;
        WHEEL_OPEN.store(true, SeqCst);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PROBE);
        }
        self.redraw();
        log(&format!("open wheel={}", self.wheels[0].title));
    }

    /// Any scroll counts while the wheel is open. Alt is not required at this
    /// point, because after a tap it is already up, and scrolling has no other
    /// job during a drag.
    fn scroll(&mut self, delta: i32) {
        if self.phase != Phase::Open {
            self.scroll_gate = ScrollGate::default();
            return;
        }
        if let Some(forward) = self.scroll_gate.feed(delta, Instant::now()) {
            self.cycle(forward);
        }
    }

    fn cycle(&mut self, forward: bool) {
        if self.wheels.len() < 2 {
            return;
        }
        let count = self.wheels.len();
        self.active = (self.active + if forward { 1 } else { count - 1 }) % count;
        self.hovered = None;
        self.redraw();
        log(&format!("wheel={}", self.wheels[self.active].title));
    }

    fn hit_at(&self, pt: POINT) -> Hit {
        let half = self.window_size() as f32 / 2.0;
        let dx = (pt.x - self.origin.x) as f32 - half;
        let dy = (pt.y - self.origin.y) as f32 - half;
        let count = self.wheels.get(self.active).map_or(0, |w| w.segments.len());
        hit_test(dx / self.scale, dy / self.scale, count)
    }

    fn hover(&mut self, hit: Hit) {
        let hovered = if let Hit::Segment(i) = hit { Some(i) } else { None };
        if hovered != self.hovered {
            self.hovered = hovered;
            self.redraw();
        }
    }

    fn redraw(&mut self) {
        let wheel = &self.wheels[self.active];
        let view = WheelView {
            title: &wheel.title,
            labels: wheel.segments.iter().map(|s| s.label.as_str()).collect(),
            hovered: self.hovered,
            mode_index: self.active,
            mode_count: self.wheels.len(),
        };
        let pixmap = self.renderer.draw(&view, self.scale);
        self.present(&pixmap);
    }

    /// Pushes a premultiplied RGBA pixmap to the layered window.
    fn present(&self, pixmap: &Pixmap) {
        let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
        unsafe {
            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            if let Ok(bitmap) = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                let target = std::slice::from_raw_parts_mut(bits as *mut u8, pixmap.data().len());
                for (out, source) in target.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
                    out.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
                }
                let previous = SelectObject(memory, bitmap.into());
                let size = SIZE { cx: width, cy: height };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                    ..Default::default()
                };
                let result = UpdateLayeredWindow(
                    self.hwnd,
                    Some(screen),
                    Some(&self.origin),
                    Some(&size),
                    Some(memory),
                    Some(&POINT::default()),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
                if let Err(error) = result {
                    log(&format!("UpdateLayeredWindow failed: {error}"));
                }
                SelectObject(memory, previous);
                let _ = DeleteObject(bitmap.into());
            }
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
        }
    }

    fn hide(&mut self) {
        if self.phase == Phase::Hidden {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_PROBE);
            let _ = KillTimer(Some(self.hwnd), TIMER_CLOSE);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        WHEEL_OPEN.store(false, SeqCst);
        self.phase = Phase::Hidden;
        self.paths.clear();
        self.wheels.clear();
        log("hidden");
    }

    /// Hiding inside an OLE callback would upset the drag loop, so defer it.
    fn post_hide(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), MSG_HIDE, WPARAM(0), LPARAM(0));
        }
    }
}

/// OLE only re-evaluates the window under the cursor when the mouse moves.
fn nudge_cursor() {
    let step = |dx: i32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dwFlags: MOUSEEVENTF_MOVE, ..Default::default() },
        },
    };
    unsafe {
        SendInput(&[step(1), step(-1)], size_of::<INPUT>() as i32);
    }
}

/// Asks for mouse events even while another window has focus. Raw input comes
/// straight from the device, so it works during a modal OLE drag.
fn register_raw_mouse(hwnd: HWND) -> Result<()> {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }
}

fn raw_wheel_delta(lparam: LPARAM) -> Option<i32> {
    const RI_MOUSE_WHEEL: u16 = 0x0400;
    let mut input = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    let read = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut _),
            RID_INPUT,
            Some(&mut input as *mut RAWINPUT as *mut _),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if read == u32::MAX || input.header.dwType != RIM_TYPEMOUSE.0 {
        return None;
    }
    let buttons = unsafe { input.data.mouse.Anonymous.Anonymous };
    (buttons.usButtonFlags & RI_MOUSE_WHEEL != 0).then_some(buttons.usButtonData as i16 as i32)
}

fn file_paths(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return Vec::new() };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let length = DragQueryFileW(hdrop, index, None) as usize;
            let mut buffer = vec![0u16; length + 1];
            DragQueryFileW(hdrop, index, Some(&mut buffer));
            buffer.truncate(length);
            paths.push(PathBuf::from(String::from_utf16_lossy(&buffer)));
        }
        ReleaseStgMedium(&mut medium);
        paths
    }
}

/// Offers copy so the source never treats the drop as a move and deletes anything.
fn offer(hit: Hit, effect: *mut DROPEFFECT) {
    unsafe {
        let allowed = *effect;
        *effect = match hit {
            Hit::Segment(_) if allowed.contains(DROPEFFECT_COPY) => DROPEFFECT_COPY,
            Hit::Segment(_) if allowed.contains(DROPEFFECT_LINK) => DROPEFFECT_LINK,
            _ => DROPEFFECT_NONE,
        };
    }
}

#[implement(IDropTarget)]
struct DropTarget;

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        let paths = data.as_ref().map(file_paths).unwrap_or_default();
        log(&format!("drag enter files={}", paths.len()));
        let hit = with_state(|state| {
            if state.phase == Phase::Probing {
                if paths.is_empty() {
                    state.post_hide();
                } else {
                    state.open(paths);
                }
            }
            let hit = state.hit_at(POINT { x: pt.x, y: pt.y });
            if state.phase == Phase::Open {
                state.hover(hit);
            }
            hit
        });
        offer(hit.unwrap_or(Hit::Outside), effect);
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let hit = with_state(|state| {
            let hit = state.hit_at(POINT { x: pt.x, y: pt.y });
            if state.phase == Phase::Open {
                state.hover(hit);
            }
            hit
        });
        offer(hit.unwrap_or(Hit::Outside), effect);
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        with_state(|state| {
            if state.phase == Phase::Open {
                state.hover(Hit::Outside);
            }
        });
        Ok(())
    }

    fn Drop(
        &self,
        _data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        // Reporting no effect guarantees the source leaves the originals alone.
        unsafe { *effect = DROPEFFECT_NONE };
        let selection = with_state(|state| {
            let hit = state.hit_at(POINT { x: pt.x, y: pt.y });
            let selection = match (state.phase, hit) {
                (Phase::Open, Hit::Segment(index)) => {
                    let wheel = &state.wheels[state.active];
                    Some(Selection {
                        paths: state.paths.clone(),
                        wheel: wheel.title.clone(),
                        segment_id: wheel.segments[index].id.clone(),
                    })
                }
                _ => None,
            };
            state.hide();
            selection
        })
        .flatten();
        match selection {
            Some(selection) => {
                log(&format!("selected {} / {}", selection.wheel, selection.segment_id));
                STATE.with(|cell| {
                    if let Some(state) = cell.borrow().as_ref() {
                        (state.on_select)(selection);
                    }
                });
            }
            None => log("dropped without a selection"),
        }
        Ok(())
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        MSG_TRIGGER => {
            with_state(State::begin_probe);
        }
        WM_INPUT => {
            if let Some(delta) = raw_wheel_delta(lparam) {
                with_state(|state| state.scroll(delta));
            }
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        MSG_BUTTON_UP => {
            with_state(|state| {
                if state.phase != Phase::Hidden {
                    SetTimer(Some(state.hwnd), TIMER_CLOSE, CLOSE_GRACE_MS, None);
                }
            });
        }
        MSG_CANCEL | MSG_HIDE => {
            with_state(State::hide);
        }
        WM_TIMER => {
            with_state(|state| {
                if wparam.0 == TIMER_PROBE && state.phase == Phase::Probing {
                    log("probe timed out: not a file drag");
                }
                if wparam.0 == TIMER_CLOSE || state.phase == Phase::Probing {
                    state.hide();
                }
                let _ = KillTimer(Some(state.hwnd), wparam.0);
            });
        }
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        _ => return DefWindowProcW(hwnd, message, wparam, lparam),
    }
    LRESULT(0)
}

pub fn run(provider: WheelProvider, on_select: SelectionHandler) -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        OleInitialize(None)?;

        let module: HINSTANCE = GetModuleHandleW(None)?.into();
        let class: PCWSTR = w!("TransmuteWheel");
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&window_class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("Transmute"),
            WS_POPUP,
            0,
            0,
            BASE_SIZE as i32,
            BASE_SIZE as i32,
            None,
            None,
            Some(module),
            None,
        )?;

        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                hwnd,
                phase: Phase::Hidden,
                provider,
                on_select,
                renderer: Renderer::new(),
                wheels: Vec::new(),
                paths: Vec::new(),
                active: 0,
                hovered: None,
                origin: POINT::default(),
                scale: 1.0,
                scroll_gate: ScrollGate::default(),
            });
        });

        let target: IDropTarget = DropTarget.into();
        RegisterDragDrop(hwnd, &target)?;
        register_raw_mouse(hwnd)?;
        hooks::install(hwnd);
        log("ready");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_notch_switches_and_direction_follows_the_scroll() {
        let start = Instant::now();
        assert_eq!(ScrollGate::default().feed(-120, start), Some(true));
        assert_eq!(ScrollGate::default().feed(120, start), Some(false));
    }

    #[test]
    fn small_steps_add_up_to_a_notch() {
        let mut gate = ScrollGate::default();
        let now = Instant::now();
        assert_eq!(gate.feed(-40, now), None);
        assert_eq!(gate.feed(-40, now), None);
        assert_eq!(gate.feed(-40, now), Some(true));
    }

    #[test]
    fn a_fast_spin_cannot_skip_wheels() {
        let mut gate = ScrollGate::default();
        let start = Instant::now();
        assert_eq!(gate.feed(-120, start), Some(true));
        assert_eq!(gate.feed(-120, start + Duration::from_millis(50)), None);
        assert_eq!(gate.feed(-120, start + Duration::from_millis(300)), Some(true));
    }
}
