//! User actions and the default keymap (PRD §8 & Edit Mode PRD §8).
//! Keys are Win32 virtual-key codes as `u16` (and Linux evdev keycodes as `u32`)
//! so this crate stays platform-independent.

const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21; // Page Up
const VK_NEXT: u16 = 0x22; // Page Down
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_DELETE: u16 = 0x2E;
const VK_0: u16 = 0x30;
const VK_1: u16 = 0x31;
const VK_2: u16 = 0x32;
const VK_A: u16 = 0x41;
const VK_C: u16 = 0x43;
const VK_D: u16 = 0x44;
const VK_E: u16 = 0x45;
const VK_H: u16 = 0x48;
const VK_I: u16 = 0x49;
const VK_M: u16 = 0x4D;
const VK_O: u16 = 0x4F;
const VK_R: u16 = 0x52;
const VK_S: u16 = 0x53;
const VK_V: u16 = 0x56;
const VK_W: u16 = 0x57;
const VK_X: u16 = 0x58;
const VK_Y: u16 = 0x59;
const VK_Z: u16 = 0x5A;
const VK_NUMPAD0: u16 = 0x60;
const VK_NUMPAD1: u16 = 0x61;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
const VK_F11: u16 = 0x7A;
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_MINUS: u16 = 0xBD;
const VK_OEM_2: u16 = 0xBF; // '/' and '?' on US layout
const VK_OEM_5: u16 = 0xDC; // '\' on US layout

/// Win32 virtual keycode for the hold-to-compare key (`\`).
pub const VK_COMPARE: u16 = VK_OEM_5;
/// Linux evdev keycode for the hold-to-compare key (`\` = `KEY_BACKSLASH`).
pub const EVDEV_KEY_COMPARE: u32 = 43;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Next,
    Prev,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    ZoomActual,
    ToggleFullscreen,
    /// Esc: leave fullscreen if fullscreen, otherwise close.
    Escape,
    Close,
    ShowHelp,
    // Edit Mode (v0.3):
    EditMode,
    CropMode,
    // v0.2+:
    RotateCw,
    RotateCcw,
    ToggleInfo,
    Delete,
    CopyImage,
    CopyPath,
    EditInKapix,
}

/// Mode-specific actions while Edit Mode is active.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EditAction {
    TabCrop,
    TabAdjust,
    RotateCw,
    RotateCcw,
    FlipH,
    FlipV,
    SwapAspect,
    ToggleMonochrome,
    /// `(dx_px, dy_px, resize_bottom_right)`
    NudgeCrop(i32, i32, bool),
    Undo,
    Redo,
    ResetAll,
    CopyEdited,
    SmartSave,
    SaveCopy,
    ConfirmPrimary,
    ConfirmOverwrite,
    ConfirmDiscard,
    EditInKapix,
    ExitOrCancel,
    CloseWindow,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Map a Win32 virtual-key code + modifiers to a Viewer Mode [`Action`].
pub fn action_for_key(vk: u16, mods: Modifiers) -> Option<Action> {
    if mods.alt {
        return None;
    }

    match (vk, mods.ctrl, mods.shift) {
        (VK_RIGHT | VK_NEXT, false, false) => Some(Action::Next),
        (VK_LEFT | VK_PRIOR, false, false) => Some(Action::Prev),
        (VK_HOME, false, false) => Some(Action::First),
        (VK_END, false, false) => Some(Action::Last),

        // '+' on US keyboard is Shift + VK_OEM_PLUS; accept bare '=', Shift+'=' ('+'), and Ctrl+'+'/'='.
        (VK_OEM_PLUS, _, _) => Some(Action::ZoomIn),
        (VK_ADD, _, false) => Some(Action::ZoomIn),
        (VK_OEM_MINUS | VK_SUBTRACT, _, false) => Some(Action::ZoomOut),
        (VK_0 | VK_NUMPAD0, _, false) => Some(Action::ZoomFit),
        (VK_1 | VK_NUMPAD1, _, false) => Some(Action::ZoomActual),

        (VK_F11, false, false) => Some(Action::ToggleFullscreen),
        (VK_ESCAPE, false, false) => Some(Action::Escape),
        (VK_W, true, false) => Some(Action::Close),
        (VK_OEM_2, false, true) => Some(Action::ShowHelp),

        (VK_E, false, false) => Some(Action::EditMode),
        (VK_C, false, false) => Some(Action::CropMode),
        (VK_E, false, true) => Some(Action::EditInKapix),

        (VK_R, false, false) => Some(Action::RotateCw),
        (VK_R, false, true) => Some(Action::RotateCcw),
        (VK_I, false, false) => Some(Action::ToggleInfo),
        (VK_DELETE, false, false) => Some(Action::Delete),
        (VK_C, true, false) => Some(Action::CopyImage),
        (VK_C, true, true) => Some(Action::CopyPath),

        _ => None,
    }
}

/// Map a Win32 virtual-key code + modifiers to an [`EditAction`] while in Edit Mode.
pub fn edit_action_for_key(vk: u16, mods: Modifiers) -> Option<EditAction> {
    if mods.alt {
        return None;
    }

    let step = if mods.shift { 10 } else { 1 };
    match (vk, mods.ctrl, mods.shift) {
        (VK_1 | VK_C, false, false) => Some(EditAction::TabCrop),
        (VK_2 | VK_A, false, false) => Some(EditAction::TabAdjust),
        (VK_R, false, false) => Some(EditAction::RotateCw),
        (VK_R, false, true) => Some(EditAction::RotateCcw),
        (VK_H, false, false) => Some(EditAction::FlipH),
        (VK_V, false, false) => Some(EditAction::FlipV),
        (VK_X, false, false) => Some(EditAction::SwapAspect),
        (VK_M, false, false) => Some(EditAction::ToggleMonochrome),

        (VK_LEFT, ctrl, _) => Some(EditAction::NudgeCrop(-step, 0, ctrl)),
        (VK_RIGHT, ctrl, _) => Some(EditAction::NudgeCrop(step, 0, ctrl)),
        (VK_UP, ctrl, _) => Some(EditAction::NudgeCrop(0, -step, ctrl)),
        (VK_DOWN, ctrl, _) => Some(EditAction::NudgeCrop(0, step, ctrl)),

        (VK_Z, true, false) => Some(EditAction::Undo),
        (VK_Y, true, false) | (VK_Z, true, true) => Some(EditAction::Redo),
        (VK_R, true, false) => Some(EditAction::ResetAll),
        (VK_C, true, false) => Some(EditAction::CopyEdited),
        (VK_S, true, false) => Some(EditAction::SmartSave),
        (VK_S, true, true) => Some(EditAction::SaveCopy),

        (VK_RETURN, false, false) => Some(EditAction::ConfirmPrimary),
        (VK_O, false, false) => Some(EditAction::ConfirmOverwrite),
        (VK_D, false, false) => Some(EditAction::ConfirmDiscard),
        (VK_E, false, true) => Some(EditAction::EditInKapix),
        (VK_E, false, false) | (VK_ESCAPE, false, false) => Some(EditAction::ExitOrCancel),
        (VK_W, true, false) => Some(EditAction::CloseWindow),
        _ => None,
    }
}

/// Map a Linux evdev keycode (`linux/input-event-codes.h`, i.e. Wayland `wl_keyboard::key` raw
/// keycode) + modifiers to a Kalux Viewer Mode [`Action`].
pub fn action_for_linux_evdev(keycode: u32, mods: Modifiers) -> Option<Action> {
    if mods.alt {
        return None;
    }

    const KEY_ESC: u32 = 1;
    const KEY_1: u32 = 2;
    const KEY_0: u32 = 11;
    const KEY_MINUS: u32 = 12;
    const KEY_EQUAL: u32 = 13;
    const KEY_W: u32 = 17;
    const KEY_E: u32 = 18;
    const KEY_R: u32 = 19;
    const KEY_I: u32 = 23;
    const KEY_C: u32 = 46;
    const KEY_SLASH: u32 = 53;
    const KEY_KPMINUS: u32 = 74;
    const KEY_KPPLUS: u32 = 78;
    const KEY_KP1: u32 = 79;
    const KEY_KP0: u32 = 82;
    const KEY_F11: u32 = 87;
    const KEY_HOME: u32 = 102;
    const KEY_PAGEUP: u32 = 104;
    const KEY_LEFT: u32 = 105;
    const KEY_RIGHT: u32 = 106;
    const KEY_END: u32 = 107;
    const KEY_PAGEDOWN: u32 = 109;
    const KEY_DELETE: u32 = 111;

    match (keycode, mods.ctrl, mods.shift) {
        (KEY_RIGHT | KEY_PAGEDOWN, false, false) => Some(Action::Next),
        (KEY_LEFT | KEY_PAGEUP, false, false) => Some(Action::Prev),
        (KEY_HOME, false, false) => Some(Action::First),
        (KEY_END, false, false) => Some(Action::Last),

        (KEY_EQUAL, _, _) => Some(Action::ZoomIn),
        (KEY_KPPLUS, _, false) => Some(Action::ZoomIn),
        (KEY_MINUS | KEY_KPMINUS, _, false) => Some(Action::ZoomOut),
        (KEY_0 | KEY_KP0, _, false) => Some(Action::ZoomFit),
        (KEY_1 | KEY_KP1, _, false) => Some(Action::ZoomActual),

        (KEY_F11, false, false) => Some(Action::ToggleFullscreen),
        (KEY_ESC, false, false) => Some(Action::Escape),
        (KEY_W, true, false) => Some(Action::Close),
        (KEY_SLASH, false, true) => Some(Action::ShowHelp),

        (KEY_E, false, false) => Some(Action::EditMode),
        (KEY_C, false, false) => Some(Action::CropMode),
        (KEY_E, false, true) => Some(Action::EditInKapix),

        (KEY_R, false, false) => Some(Action::RotateCw),
        (KEY_R, false, true) => Some(Action::RotateCcw),
        (KEY_I, false, false) => Some(Action::ToggleInfo),
        (KEY_DELETE, false, false) => Some(Action::Delete),
        (KEY_C, true, false) => Some(Action::CopyImage),
        (KEY_C, true, true) => Some(Action::CopyPath),

        _ => None,
    }
}

/// Map a Linux evdev keycode + modifiers to an [`EditAction`] while in Edit Mode.
pub fn edit_action_for_linux_evdev(keycode: u32, mods: Modifiers) -> Option<EditAction> {
    if mods.alt {
        return None;
    }

    const KEY_ESC: u32 = 1;
    const KEY_1: u32 = 2;
    const KEY_2: u32 = 3;
    const KEY_W: u32 = 17;
    const KEY_E: u32 = 18;
    const KEY_R: u32 = 19;
    const KEY_Y: u32 = 21;
    const KEY_O: u32 = 24;
    const KEY_ENTER: u32 = 28;
    const KEY_A: u32 = 30;
    const KEY_S: u32 = 31;
    const KEY_D: u32 = 32;
    const KEY_H: u32 = 35;
    const KEY_Z: u32 = 44;
    const KEY_X: u32 = 45;
    const KEY_C: u32 = 46;
    const KEY_V: u32 = 47;
    const KEY_M: u32 = 50;
    const KEY_KPENTER: u32 = 96;
    const KEY_UP: u32 = 103;
    const KEY_LEFT: u32 = 105;
    const KEY_RIGHT: u32 = 106;
    const KEY_DOWN: u32 = 108;

    let step = if mods.shift { 10 } else { 1 };
    match (keycode, mods.ctrl, mods.shift) {
        (KEY_1 | KEY_C, false, false) => Some(EditAction::TabCrop),
        (KEY_2 | KEY_A, false, false) => Some(EditAction::TabAdjust),
        (KEY_R, false, false) => Some(EditAction::RotateCw),
        (KEY_R, false, true) => Some(EditAction::RotateCcw),
        (KEY_H, false, false) => Some(EditAction::FlipH),
        (KEY_V, false, false) => Some(EditAction::FlipV),
        (KEY_X, false, false) => Some(EditAction::SwapAspect),
        (KEY_M, false, false) => Some(EditAction::ToggleMonochrome),

        (KEY_LEFT, ctrl, _) => Some(EditAction::NudgeCrop(-step, 0, ctrl)),
        (KEY_RIGHT, ctrl, _) => Some(EditAction::NudgeCrop(step, 0, ctrl)),
        (KEY_UP, ctrl, _) => Some(EditAction::NudgeCrop(0, -step, ctrl)),
        (KEY_DOWN, ctrl, _) => Some(EditAction::NudgeCrop(0, step, ctrl)),

        (KEY_Z, true, false) => Some(EditAction::Undo),
        (KEY_Y, true, false) | (KEY_Z, true, true) => Some(EditAction::Redo),
        (KEY_R, true, false) => Some(EditAction::ResetAll),
        (KEY_C, true, false) => Some(EditAction::CopyEdited),
        (KEY_S, true, false) => Some(EditAction::SmartSave),
        (KEY_S, true, true) => Some(EditAction::SaveCopy),

        (KEY_ENTER | KEY_KPENTER, false, false) => Some(EditAction::ConfirmPrimary),
        (KEY_O, false, false) => Some(EditAction::ConfirmOverwrite),
        (KEY_D, false, false) => Some(EditAction::ConfirmDiscard),
        (KEY_E, false, true) => Some(EditAction::EditInKapix),
        (KEY_E, false, false) | (KEY_ESC, false, false) => Some(EditAction::ExitOrCancel),
        (KEY_W, true, false) => Some(EditAction::CloseWindow),
        _ => None,
    }
}

const HELP_ROWS: &[(&str, &str)] = &[
    ("← / → / PgUp / PgDn", "Previous / next image"),
    ("Home / End", "First / last in folder"),
    ("Wheel / + / -", "Zoom in / out"),
    ("0 / 1", "Fit to window / 100%"),
    ("Drag", "Pan when zoomed"),
    ("F11 / Double-click", "Toggle fullscreen"),
    ("E / C", "Edit mode / Crop directly"),
    ("R / Shift+R · H / V", "Rotate / Flip (in Edit mode)"),
    ("Ctrl+S / Ctrl+Shift+S", "Smart save / Save copy"),
    ("Ctrl+C", "Copy image to clipboard"),
    ("Shift+E", "Annotate in Kapix"),
    ("?", "Toggle shortcuts overlay"),
    ("Esc / Ctrl+W", "Exit edit / fullscreen / close"),
];

/// (key label, description) rows for the `?` help overlay, in display order.
pub fn help_rows() -> &'static [(&'static str, &'static str)] {
    HELP_ROWS
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
        alt: false,
    };
    const SHIFT: Modifiers = Modifiers {
        ctrl: false,
        shift: true,
        alt: false,
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };
    const CTRL_SHIFT: Modifiers = Modifiers {
        ctrl: true,
        shift: true,
        alt: false,
    };
    const ALT: Modifiers = Modifiers {
        ctrl: false,
        shift: false,
        alt: true,
    };

    #[test]
    fn navigation_keys() {
        assert_eq!(action_for_key(VK_RIGHT, NONE), Some(Action::Next));
        assert_eq!(action_for_key(VK_NEXT, NONE), Some(Action::Next));
        assert_eq!(action_for_key(VK_LEFT, NONE), Some(Action::Prev));
        assert_eq!(action_for_key(VK_PRIOR, NONE), Some(Action::Prev));
        assert_eq!(action_for_key(VK_HOME, NONE), Some(Action::First));
        assert_eq!(action_for_key(VK_END, NONE), Some(Action::Last));
        assert_eq!(action_for_key(VK_LEFT, ALT), None);
    }

    #[test]
    fn zoom_keys() {
        assert_eq!(action_for_key(VK_OEM_PLUS, NONE), Some(Action::ZoomIn));
        assert_eq!(action_for_key(VK_OEM_PLUS, SHIFT), Some(Action::ZoomIn));
        assert_eq!(action_for_key(VK_ADD, NONE), Some(Action::ZoomIn));
        assert_eq!(action_for_key(VK_OEM_MINUS, NONE), Some(Action::ZoomOut));
        assert_eq!(action_for_key(VK_SUBTRACT, NONE), Some(Action::ZoomOut));
        assert_eq!(action_for_key(VK_0, NONE), Some(Action::ZoomFit));
        assert_eq!(action_for_key(VK_NUMPAD0, NONE), Some(Action::ZoomFit));
        assert_eq!(action_for_key(VK_1, NONE), Some(Action::ZoomActual));
        assert_eq!(action_for_key(VK_NUMPAD1, NONE), Some(Action::ZoomActual));
    }

    #[test]
    fn window_and_edit_entry_keys() {
        assert_eq!(action_for_key(VK_F11, NONE), Some(Action::ToggleFullscreen));
        assert_eq!(action_for_key(VK_ESCAPE, NONE), Some(Action::Escape));
        assert_eq!(action_for_key(VK_W, CTRL), Some(Action::Close));
        assert_eq!(action_for_key(VK_W, NONE), None);
        assert_eq!(action_for_key(VK_OEM_2, SHIFT), Some(Action::ShowHelp));
        assert_eq!(action_for_key(VK_OEM_2, NONE), None);
        assert_eq!(action_for_key(VK_R, NONE), Some(Action::RotateCw));
        assert_eq!(action_for_key(VK_R, SHIFT), Some(Action::RotateCcw));
        assert_eq!(action_for_key(VK_I, NONE), Some(Action::ToggleInfo));
        assert_eq!(action_for_key(VK_DELETE, NONE), Some(Action::Delete));
        assert_eq!(action_for_key(VK_C, CTRL), Some(Action::CopyImage));
        assert_eq!(action_for_key(VK_C, CTRL_SHIFT), Some(Action::CopyPath));
        assert_eq!(action_for_key(VK_C, NONE), Some(Action::CropMode));
        assert_eq!(action_for_key(VK_E, NONE), Some(Action::EditMode));
        assert_eq!(action_for_key(VK_E, SHIFT), Some(Action::EditInKapix));
    }

    #[test]
    fn edit_mode_win32_and_linux_keys() {
        assert_eq!(edit_action_for_key(VK_R, NONE), Some(EditAction::RotateCw));
        assert_eq!(
            edit_action_for_key(VK_R, SHIFT),
            Some(EditAction::RotateCcw)
        );
        assert_eq!(edit_action_for_key(VK_H, NONE), Some(EditAction::FlipH));
        assert_eq!(edit_action_for_key(VK_V, NONE), Some(EditAction::FlipV));
        assert_eq!(edit_action_for_key(VK_S, CTRL), Some(EditAction::SmartSave));
        assert_eq!(
            edit_action_for_key(VK_S, CTRL_SHIFT),
            Some(EditAction::SaveCopy)
        );
        assert_eq!(edit_action_for_key(VK_Z, CTRL), Some(EditAction::Undo));
        assert_eq!(
            edit_action_for_key(VK_Z, CTRL_SHIFT),
            Some(EditAction::Redo)
        );
        assert_eq!(
            edit_action_for_key(VK_RIGHT, SHIFT),
            Some(EditAction::NudgeCrop(10, 0, false))
        );
        assert_eq!(
            edit_action_for_key(VK_DOWN, CTRL),
            Some(EditAction::NudgeCrop(0, 1, true))
        );

        assert_eq!(
            edit_action_for_linux_evdev(31, CTRL),
            Some(EditAction::SmartSave)
        );
        assert_eq!(
            edit_action_for_linux_evdev(31, CTRL_SHIFT),
            Some(EditAction::SaveCopy)
        );
        assert_eq!(
            edit_action_for_linux_evdev(44, CTRL),
            Some(EditAction::Undo)
        );
        assert_eq!(
            edit_action_for_linux_evdev(106, SHIFT),
            Some(EditAction::NudgeCrop(10, 0, false))
        );
    }

    #[test]
    fn help_rows_non_empty() {
        let rows = help_rows();
        assert!(rows.len() >= 10);
        for &(key, desc) in rows {
            assert!(!key.is_empty());
            assert!(!desc.is_empty());
        }
    }

    #[test]
    fn linux_evdev_keys() {
        assert_eq!(action_for_linux_evdev(106, NONE), Some(Action::Next));
        assert_eq!(action_for_linux_evdev(105, NONE), Some(Action::Prev));
        assert_eq!(action_for_linux_evdev(102, NONE), Some(Action::First));
        assert_eq!(action_for_linux_evdev(107, NONE), Some(Action::Last));
        assert_eq!(action_for_linux_evdev(13, SHIFT), Some(Action::ZoomIn));
        assert_eq!(action_for_linux_evdev(12, NONE), Some(Action::ZoomOut));
        assert_eq!(action_for_linux_evdev(11, NONE), Some(Action::ZoomFit));
        assert_eq!(action_for_linux_evdev(2, NONE), Some(Action::ZoomActual));
        assert_eq!(
            action_for_linux_evdev(87, NONE),
            Some(Action::ToggleFullscreen)
        );
        assert_eq!(action_for_linux_evdev(1, NONE), Some(Action::Escape));
        assert_eq!(action_for_linux_evdev(17, CTRL), Some(Action::Close));
        assert_eq!(action_for_linux_evdev(53, SHIFT), Some(Action::ShowHelp));
        assert_eq!(action_for_linux_evdev(53, NONE), None);
        assert_eq!(action_for_linux_evdev(18, NONE), Some(Action::EditMode));
        assert_eq!(action_for_linux_evdev(46, NONE), Some(Action::CropMode));
        assert_eq!(action_for_linux_evdev(18, SHIFT), Some(Action::EditInKapix));
        assert_eq!(action_for_linux_evdev(106, ALT), None);
    }
}
