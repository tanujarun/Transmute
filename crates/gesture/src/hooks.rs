//! Low-level keyboard and mouse hooks. They run on their own thread so overlay
//! rendering can never delay system input; they only update flags and post
//! messages to the overlay window. The trigger key is never swallowed, because
//! other apps rely on it during drags and an unbalanced key state would leave it stuck.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, Ordering::SeqCst};
use std::thread;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const MSG_TRIGGER: u32 = WM_APP + 1;
pub const MSG_BUTTON_UP: u32 = WM_APP + 3;
pub const MSG_CANCEL: u32 = WM_APP + 4;

static LEFT_DOWN: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static TRIGGER_DOWN: AtomicBool = AtomicBool::new(false);
static START_X: AtomicI32 = AtomicI32::new(0);
static START_Y: AtomicI32 = AtomicI32::new(0);
static OVERLAY: AtomicIsize = AtomicIsize::new(0);

/// True while a wheel is visible, so scrolling belongs to us.
pub static WHEEL_OPEN: AtomicBool = AtomicBool::new(false);


fn post(message: u32, wparam: usize) {
    let hwnd = HWND(OVERLAY.load(SeqCst) as *mut _);
    unsafe {
        let _ = PostMessageW(Some(hwnd), message, WPARAM(wparam), LPARAM(0));
    }
}

const TRIGGER_ALT: u8 = 0;
const TRIGGER_RIGHT_ALT: u8 = 1;
const TRIGGER_CTRL: u8 = 2;
const TRIGGER_SHIFT: u8 = 3;
static TRIGGER: AtomicU8 = AtomicU8::new(TRIGGER_ALT);

/// Chooses the key that opens the wheel: alt, rightalt, ctrl or shift.
pub fn set_trigger_key(name: &str) {
    let key = match name {
        "rightalt" => TRIGGER_RIGHT_ALT,
        "ctrl" => TRIGGER_CTRL,
        "shift" => TRIGGER_SHIFT,
        _ => TRIGGER_ALT,
    };
    TRIGGER.store(key, SeqCst);
    TRIGGER_DOWN.store(false, SeqCst);
}

fn trigger_matches(trigger: u8, vk: u16) -> bool {
    match trigger {
        TRIGGER_RIGHT_ALT => vk == VK_RMENU.0,
        TRIGGER_CTRL => vk == VK_LCONTROL.0 || vk == VK_RCONTROL.0 || vk == VK_CONTROL.0,
        TRIGGER_SHIFT => vk == VK_LSHIFT.0 || vk == VK_RSHIFT.0 || vk == VK_SHIFT.0,
        _ => vk == VK_LMENU.0 || vk == VK_RMENU.0 || vk == VK_MENU.0,
    }
}

fn is_trigger_key(vk: u16) -> bool {
    trigger_matches(TRIGGER.load(SeqCst), vk)
}

fn past_threshold(start: (i32, i32), now: (i32, i32), limit: (i32, i32)) -> bool {
    (now.0 - start.0).abs() > limit.0 || (now.1 - start.1).abs() > limit.1
}

/// Reads the cursor directly when Alt goes down, so a drag still counts if some
/// of the move events never reached the hook.
fn is_dragging_now() -> bool {
    if !LEFT_DOWN.load(SeqCst) {
        return false;
    }
    if DRAGGING.load(SeqCst) {
        return true;
    }
    let mut cursor = POINT::default();
    let limit = unsafe {
        let _ = GetCursorPos(&mut cursor);
        (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG))
    };
    let start = (START_X.load(SeqCst), START_Y.load(SeqCst));
    let dragging = past_threshold(start, (cursor.x, cursor.y), limit);
    DRAGGING.store(dragging, SeqCst);
    dragging
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let vk = info.vkCode as u16;
        let message = wparam.0 as u32;
        let is_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
        let is_trigger = is_trigger_key(vk);

        if is_trigger {
            let was_down = TRIGGER_DOWN.swap(is_down, SeqCst);
            if is_down && !was_down {
                let dragging = is_dragging_now();
                crate::log(&format!("trigger pressed, dragging={dragging}"));
                if dragging {
                    post(MSG_TRIGGER, 0);
                }
            }
        } else if vk == VK_ESCAPE.0 && is_down && WHEEL_OPEN.load(SeqCst) {
            post(MSG_CANCEL, 0);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        match wparam.0 as u32 {
            WM_LBUTTONDOWN => {
                LEFT_DOWN.store(true, SeqCst);
                DRAGGING.store(false, SeqCst);
                START_X.store(info.pt.x, SeqCst);
                START_Y.store(info.pt.y, SeqCst);
            }
            WM_LBUTTONUP => {
                LEFT_DOWN.store(false, SeqCst);
                DRAGGING.store(false, SeqCst);
                post(MSG_BUTTON_UP, 0);
            }
            WM_MOUSEMOVE if LEFT_DOWN.load(SeqCst) && !DRAGGING.load(SeqCst) => {
                let start = (START_X.load(SeqCst), START_Y.load(SeqCst));
                let limit = (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG));
                if past_threshold(start, (info.pt.x, info.pt.y), limit) {
                    DRAGGING.store(true, SeqCst);
                    // A trigger held from before the drag started counts as a press.
                    if TRIGGER_DOWN.load(SeqCst) {
                        post(MSG_TRIGGER, 0);
                    }
                }
            }
            // The overlay reads the scroll through raw input; this only stops the
            // app under the wheel from scrolling too.
            WM_MOUSEWHEEL if WHEEL_OPEN.load(SeqCst) => return LRESULT(1),
            _ => {}
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_keys_match_only_their_own_keys() {
        assert!(trigger_matches(TRIGGER_ALT, VK_LMENU.0) && trigger_matches(TRIGGER_ALT, VK_RMENU.0));
        assert!(trigger_matches(TRIGGER_RIGHT_ALT, VK_RMENU.0) && !trigger_matches(TRIGGER_RIGHT_ALT, VK_LMENU.0));
        assert!(trigger_matches(TRIGGER_CTRL, VK_LCONTROL.0) && !trigger_matches(TRIGGER_CTRL, VK_LMENU.0));
        assert!(trigger_matches(TRIGGER_SHIFT, VK_RSHIFT.0) && !trigger_matches(TRIGGER_SHIFT, VK_RCONTROL.0));
    }

    #[test]
    fn small_movement_is_not_a_drag() {
        assert!(!past_threshold((100, 100), (103, 96), (4, 4)));
    }

    #[test]
    fn movement_past_either_axis_is_a_drag() {
        assert!(past_threshold((100, 100), (105, 100), (4, 4)));
        assert!(past_threshold((100, 100), (100, 95), (4, 4)));
    }

    #[test]
    fn works_on_a_monitor_left_of_the_primary() {
        assert!(past_threshold((-1800, 500), (-1700, 500), (4, 4)));
    }
}

/// Starts the hook thread. Posts gesture messages to `overlay` from then on.
pub fn install(overlay: HWND) {
    OVERLAY.store(overlay.0 as isize, SeqCst);
    thread::Builder::new()
        .name("transmute-hooks".into())
        .spawn(|| unsafe {
            let module = GetModuleHandleW(None).ok();
            let instance = module.map(|m| m.into());
            let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), instance, 0);
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), instance, 0);
            if keyboard.is_err() || mouse.is_err() {
                crate::log("failed to install input hooks");
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        })
        .expect("spawn hook thread");
}
