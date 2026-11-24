//! User actions and the default keymap (PRD §8). Keys are Win32 virtual-key codes as `u16`
//! so this crate stays platform-independent.

const VK_ESCAPE: u16 = 0x1B;
const VK_PRIOR: u16 = 0x21; // Page Up
const VK_NEXT: u16 = 0x22; // Page Down
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_RIGHT: u16 = 0x27;
const VK_DELETE: u16 = 0x2E;
const VK_0: u16 = 0x30;
const VK_1: u16 = 0x31;
const VK_C: u16 = 0x43;
const VK_E: u16 = 0x45;
const VK_I: u16 = 0x49;
const VK_R: u16 = 0x52;
const VK_W: u16 = 0x57;
const VK_NUMPAD0: u16 = 0x60;
const VK_NUMPAD1: u16 = 0x61;
const VK_ADD: u16 = 0x6B;
const VK_SUBTRACT: u16 = 0x6D;
const VK_F11: u16 = 0x7A;
const VK_OEM_PLUS: u16 = 0xBB;
const VK_OEM_MINUS: u16 = 0xBD;
const VK_OEM_2: u16 = 0xBF; // '/' and '?' on US layout

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
    // v0.2 (keymap entries exist now so the help overlay is complete; app may no-op them):
    RotateCw,
    RotateCcw,
    ToggleInfo,
    Delete,
    CopyImage,
    CopyPath,
    EditInKapix,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Map a Win32 virtual-key code + modifiers to an action.
/// Must cover: Left/Right/PageUp/PageDown/Home/End, `+`/`-` (VK_OEM_PLUS/MINUS, VK_ADD/SUBTRACT),
/// `0`/`1` (top row and numpad), F11, Esc, `?` (Shift+VK_OEM_2), R / Shift+R, I, Delete,
/// Ctrl+C / Ctrl+Shift+C, E, Ctrl+W (Close).
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

        (VK_R, false, false) => Some(Action::RotateCw),
        (VK_R, false, true) => Some(Action::RotateCcw),
        (VK_I, false, false) => Some(Action::ToggleInfo),
        (VK_DELETE, false, false) => Some(Action::Delete),
        (VK_C, true, false) => Some(Action::CopyImage),
        (VK_C, true, true) => Some(Action::CopyPath),
        (VK_E, false, false) => Some(Action::EditInKapix),

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
    ("R / Shift+R", "Rotate clockwise / counter-clockwise"),
    ("I", "Toggle info panel"),
    ("Del", "Move to Recycle Bin"),
    ("Ctrl+C / Ctrl+Shift+C", "Copy image / file path"),
    ("E", "Edit in kapix"),
    ("?", "Toggle shortcuts overlay"),
    ("Esc / Ctrl+W", "Exit fullscreen / close"),
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
    fn window_and_v02_keys() {
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
        assert_eq!(action_for_key(VK_C, NONE), None);
        assert_eq!(action_for_key(VK_E, NONE), Some(Action::EditInKapix));
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
}
