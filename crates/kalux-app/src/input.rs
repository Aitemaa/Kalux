//! Win32 keyboard, mouse, wheel, and drag-and-drop input helpers.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use kalux_core::{Action, EditAction, Modifiers, action_for_key, edit_action_for_key};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{WHEEL_DELTA, XBUTTON1, XBUTTON2};

/// Zoom multiplier per standard mouse wheel notch (120 units).
const WHEEL_ZOOM_BASE: f32 = 1.2;

/// Extract signed X coordinate from mouse `lParam` (`GET_X_LPARAM`).
#[inline]
pub fn get_x_lparam(lp: LPARAM) -> i32 {
    (lp.0 as i16) as i32
}

/// Extract signed Y coordinate from mouse `lParam` (`GET_Y_LPARAM`).
#[inline]
pub fn get_y_lparam(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) as i16) as i32
}

/// Extract signed wheel delta from `WM_MOUSEWHEEL` `wParam` (`GET_WHEEL_DELTA_WPARAM`).
#[inline]
pub fn get_wheel_delta(wp: WPARAM) -> i16 {
    ((wp.0 >> 16) as u16) as i16
}

/// Extract X-button identifier from `WM_XBUTTONDOWN` `wParam` (`GET_XBUTTON_WPARAM`).
#[inline]
pub fn get_xbutton(wp: WPARAM) -> u16 {
    ((wp.0 >> 16) & 0xFFFF) as u16
}

/// Convert `WM_MOUSEWHEEL` screen coordinates into client coordinates (physical px).
pub fn wheel_cursor_client(hwnd: HWND, lp: LPARAM) -> (f32, f32) {
    let mut pt = POINT {
        x: get_x_lparam(lp),
        y: get_y_lparam(lp),
    };
    // SAFETY: `hwnd` is a valid window handle and `pt` is a valid stack pointer.
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
    (pt.x as f32, pt.y as f32)
}

/// Compute zoom factor from a `WM_MOUSEWHEEL` delta (supports high-resolution wheels).
#[inline]
pub fn wheel_zoom_factor(delta: i16) -> f32 {
    let notches = (delta as f32) / (WHEEL_DELTA as f32);
    WHEEL_ZOOM_BASE.powf(notches)
}

/// Query current Ctrl / Shift / Alt modifier state.
pub fn current_modifiers() -> Modifiers {
    // SAFETY: `GetKeyState` has no preconditions and takes standard virtual-key codes.
    unsafe {
        Modifiers {
            ctrl: GetKeyState(i32::from(VK_CONTROL.0)) < 0,
            shift: GetKeyState(i32::from(VK_SHIFT.0)) < 0,
            alt: GetKeyState(i32::from(VK_MENU.0)) < 0,
        }
    }
}

/// Extract the virtual-key code from a `WM_KEYDOWN` / `WM_KEYUP` `wParam`.
#[inline]
pub fn vk_from_wparam(wp: WPARAM) -> u16 {
    (wp.0 & 0xFFFF) as u16
}

/// Map a `WM_KEYDOWN` / `WM_SYSKEYDOWN` `wParam` to a Kalux [`Action`].
pub fn action_from_keydown(wp: WPARAM) -> Option<Action> {
    action_for_key(vk_from_wparam(wp), current_modifiers())
}

/// Map a `WM_KEYDOWN` `wParam` to a Kalux Edit Mode [`EditAction`].
pub fn edit_action_from_keydown(wp: WPARAM) -> Option<EditAction> {
    edit_action_for_key(vk_from_wparam(wp), current_modifiers())
}

/// Map a `WM_XBUTTONDOWN` `wParam` to `Prev` (XBUTTON1) or `Next` (XBUTTON2).
pub fn action_from_xbutton(wp: WPARAM) -> Option<Action> {
    match get_xbutton(wp) {
        XBUTTON1 => Some(Action::Prev),
        XBUTTON2 => Some(Action::Next),
        _ => None,
    }
}

/// Extract the first dropped file path from `WM_DROPFILES` and always release the `HDROP`.
pub fn extract_dropped_file(wp: WPARAM) -> Option<PathBuf> {
    let hdrop = HDROP(wp.0 as *mut _);
    if hdrop.is_invalid() || hdrop.0.is_null() {
        return None;
    }

    // SAFETY: `hdrop` is the valid `HDROP` handle delivered by `WM_DROPFILES`.
    let char_count = unsafe { DragQueryFileW(hdrop, 0, None) };
    let result = if char_count > 0 {
        let mut buf = vec![0u16; (char_count as usize) + 1];
        // SAFETY: `buf` is sized for `char_count + 1` UTF-16 code units including NUL.
        let written = unsafe { DragQueryFileW(hdrop, 0, Some(&mut buf)) };
        if written > 0 {
            Some(PathBuf::from(OsString::from_wide(&buf[..written as usize])))
        } else {
            None
        }
    } else {
        None
    };

    // SAFETY: Always finalize `HDROP` once handled so the shell frees its transfer memory.
    unsafe { DragFinish(hdrop) };
    result
}
