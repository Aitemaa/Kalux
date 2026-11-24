//! User actions and the default keymap (PRD §8). Keys are Win32 virtual-key codes as `u16`
//! so this crate stays platform-independent.

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
    let _ = (vk, mods);
    todo!("kalux-core")
}

/// (key label, description) rows for the `?` help overlay, in display order.
pub fn help_rows() -> &'static [(&'static str, &'static str)] {
    todo!("kalux-core")
}
