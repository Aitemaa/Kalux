//! Shared ka-suite design tokens (identical to Kavox, Kadex, and Kapix).
//!
//! Neutral warm charcoal in dark mode, warm paper in light mode, and one signature
//! red accent (`#E8262D`). Follows Windows light/dark theme.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Normalized `[r, g, b, a]` in `0.0..=1.0` for Direct2D `D2D1_COLOR_F`.
    pub const fn to_f32_rgba(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }

    /// Win32 `COLORREF` (`0x00BBGGRR`).
    pub const fn to_colorref(self) -> u32 {
        (self.r as u32) | ((self.g as u32) << 8) | ((self.b as u32) << 16)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub is_dark: bool,
    pub bg: Rgba,
    pub panel: Rgba,
    pub surface: Rgba,
    pub surface_2: Rgba,
    pub line: Rgba,
    pub line_strong: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub red: Rgba,
    pub red_text: Rgba,
    pub scrim: Rgba,
}

impl Palette {
    /// Ka-suite dark palette (matches Kavox, Kadex, and Kapix).
    pub const DARK: Palette = Palette {
        is_dark: true,
        bg: Rgba::rgb(0x14, 0x14, 0x14),
        panel: Rgba::rgb(0x1b, 0x1b, 0x1b),
        surface: Rgba::rgb(0x22, 0x22, 0x22),
        surface_2: Rgba::rgb(0x2b, 0x2b, 0x2b),
        line: Rgba::rgb(0x2b, 0x2b, 0x2b),
        line_strong: Rgba::rgb(0x3d, 0x3d, 0x3d),
        text: Rgba::rgb(0xf2, 0xf1, 0xee),
        muted: Rgba::rgb(0x8f, 0x8e, 0x89),
        faint: Rgba::rgb(0x5c, 0x5b, 0x57),
        red: Rgba::rgb(0xe8, 0x26, 0x2d),
        red_text: Rgba::rgb(0xff, 0x4d, 0x53),
        scrim: Rgba::rgb(0, 0, 0).with_alpha(153),
    };

    /// Ka-suite light palette (matches Kavox, Kadex, and Kapix).
    pub const LIGHT: Palette = Palette {
        is_dark: false,
        bg: Rgba::rgb(0xef, 0xee, 0xe9),
        panel: Rgba::rgb(0xf8, 0xf7, 0xf4),
        surface: Rgba::rgb(0xe4, 0xe3, 0xde),
        surface_2: Rgba::rgb(0xd9, 0xd8, 0xd2),
        line: Rgba::rgb(0xd8, 0xd6, 0xcf),
        line_strong: Rgba::rgb(0xbd, 0xbb, 0xb4),
        text: Rgba::rgb(0x19, 0x19, 0x19),
        muted: Rgba::rgb(0x69, 0x68, 0x63),
        faint: Rgba::rgb(0xa3, 0xa1, 0x9a),
        red: Rgba::rgb(0xe8, 0x26, 0x2d),
        red_text: Rgba::rgb(0xc2, 0x16, 0x1c),
        scrim: Rgba::rgb(20, 20, 20).with_alpha(89),
    };

    pub const fn for_light(light: bool) -> Palette {
        if light { Self::LIGHT } else { Self::DARK }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_match_ka_suite() {
        assert_eq!(Palette::DARK.bg, Rgba::rgb(0x14, 0x14, 0x14));
        assert_eq!(Palette::DARK.panel, Rgba::rgb(0x1b, 0x1b, 0x1b));
        assert_eq!(Palette::DARK.red, Rgba::rgb(0xe8, 0x26, 0x2d));
        assert_eq!(Palette::LIGHT.bg, Rgba::rgb(0xef, 0xee, 0xe9));
        assert_eq!(Palette::LIGHT.panel, Rgba::rgb(0xf8, 0xf7, 0xf4));
        assert_eq!(Palette::LIGHT.red, Rgba::rgb(0xe8, 0x26, 0x2d));
        assert_eq!(Palette::for_light(false), Palette::DARK);
        assert_eq!(Palette::for_light(true), Palette::LIGHT);
    }

    #[test]
    fn colorref_packs_bgr_order() {
        let c = Rgba::rgb(0xef, 0xee, 0xe9);
        assert_eq!(c.to_colorref(), 0x00E9_EEEF);
    }
}
