//! Lazy pure-Rust `ab_glyph` text & ka-suite UI overlay renderer for Linux/Wayland.
//!
//! Mirrors the Windows DirectWrite overlay layout and `kalux_core::Palette` tokens:
//! - Warm dark (`#141414` / `#1B1B1B` / `#222222` / `#F2F1EE`) & light (`#EFEEE9` / `#F8F7F4` / `#191919`)
//! - One red accent (`#E8262D` dark / `#C2161C` light)
//! - Fonts are loaded lazily on first overlay draw (0 ms added to image startup).

use std::fs;
use std::path::Path;

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use kalux_core::action::help_rows;
use kalux_core::{Palette, Rgba};

use crate::linux::render::Canvas;

#[derive(Clone, Debug)]
pub struct ErrorCard {
    pub title: String,
    pub subject: String,
    pub message: String,
    pub hint: Option<String>,
}

pub struct OverlayView<'a> {
    pub empty_hint: bool,
    pub error: Option<&'a ErrorCard>,
    pub loading_label: Option<&'a str>,
    pub toast: Option<&'a str>,
    pub show_help: bool,
}

impl OverlayView<'_> {
    pub fn is_empty(&self) -> bool {
        !self.empty_hint
            && self.error.is_none()
            && self.loading_label.is_none()
            && self.toast.is_none()
            && !self.show_help
    }
}

struct LoadedFonts {
    sans: Option<FontVec>,
    sans_bold: Option<FontVec>,
    mono: Option<FontVec>,
}

impl LoadedFonts {
    fn load() -> Self {
        let sans = load_first_font(&[
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/Roboto-Regular.ttf",
            "/usr/share/fonts/TTF/Inter-Regular.ttf",
            "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf",
            "/usr/share/fonts/cantarell/Cantarell-Regular.otf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        ]);
        let sans_bold = load_first_font(&[
            "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
            "/usr/share/fonts/noto/NotoSans-Bold.ttf",
            "/usr/share/fonts/TTF/Roboto-Medium.ttf",
            "/usr/share/fonts/TTF/Inter-SemiBold.ttf",
            "/usr/share/fonts/Adwaita/AdwaitaSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        ]);
        let mono = load_first_font(&[
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
            "/usr/share/fonts/Adwaita/AdwaitaMono-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
        ]);

        Self {
            sans,
            sans_bold,
            mono,
        }
    }

    fn sans(&self) -> Option<&FontVec> {
        self.sans.as_ref()
    }

    fn sans_bold(&self) -> Option<&FontVec> {
        self.sans_bold.as_ref().or(self.sans.as_ref())
    }

    fn mono(&self) -> Option<&FontVec> {
        self.mono.as_ref().or(self.sans.as_ref())
    }
}

fn load_first_font(candidates: &[&str]) -> Option<FontVec> {
    for path in candidates {
        if Path::new(path).exists()
            && let Ok(bytes) = fs::read(path)
            && let Ok(font) = FontVec::try_from_vec(bytes)
        {
            return Some(font);
        }
    }
    None
}

/// Lazy overlay renderer that keeps zero fonts in RAM until an overlay is drawn.
pub struct LinuxOverlay {
    fonts: Option<LoadedFonts>,
}

impl LinuxOverlay {
    pub fn new() -> Self {
        Self { fonts: None }
    }

    fn ensure_fonts(&mut self) -> &LoadedFonts {
        self.fonts.get_or_insert_with(LoadedFonts::load)
    }

    pub fn draw(
        &mut self,
        canvas: &mut Canvas<'_>,
        scale: f32,
        palette: Palette,
        view: &OverlayView<'_>,
    ) {
        if view.is_empty() || canvas.width == 0 || canvas.height == 0 {
            return;
        }
        let s = scale.max(1.0);
        self.ensure_fonts();

        if view.empty_hint && view.error.is_none() {
            self.draw_empty_hint(canvas, s, palette);
        }
        if let Some(err) = view.error {
            self.draw_error_card(canvas, s, palette, err);
        } else if let Some(label) = view.loading_label {
            self.draw_pill(canvas, s, palette, label, false, true);
        }
        if let Some(toast) = view.toast {
            self.draw_pill(canvas, s, palette, toast, true, false);
        }
        if view.show_help {
            self.draw_help_sheet(canvas, s, palette);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_text_line(
        &mut self,
        canvas: &mut Canvas<'_>,
        text: &str,
        x: f32,
        y_top: f32,
        size_px: f32,
        color: Rgba,
        bold: bool,
        mono: bool,
    ) {
        let fonts = self.ensure_fonts();
        let font = if mono {
            fonts.mono()
        } else if bold {
            fonts.sans_bold()
        } else {
            fonts.sans()
        };
        let Some(font) = font else {
            return;
        };
        render_text_left(canvas, font, text, x, y_top, size_px, color);
    }

    fn draw_empty_hint(&mut self, canvas: &mut Canvas<'_>, s: f32, palette: Palette) {
        let w = canvas.width as f32;
        let h = canvas.height as f32;
        let card_w = (410.0 * s).min(w - 32.0 * s).max(180.0 * s);
        let card_h = 128.0 * s;
        let left = (w - card_w) * 0.5;
        let top = (h - card_h) * 0.5;

        canvas.fill_rounded_rect(
            left,
            top,
            left + card_w,
            top + card_h,
            16.0 * s,
            palette.panel.with_alpha(245),
            Some((palette.line, 1.0 * s)),
        );

        let accent_bar_w = 24.0 * s;
        let accent_bar_h = 3.0 * s;
        canvas.fill_rounded_rect(
            (w - accent_bar_w) * 0.5,
            top + 18.0 * s,
            (w + accent_bar_w) * 0.5,
            top + 18.0 * s + accent_bar_h,
            1.5 * s,
            palette.red,
            None,
        );

        let fonts = self.ensure_fonts();
        if let Some(font) = fonts.sans_bold() {
            render_text_centered(
                canvas,
                font,
                "Drop an image here to view",
                w * 0.5,
                top + 42.0 * s,
                16.5 * s,
                palette.text,
            );
        }
        if let Some(font) = fonts.sans() {
            render_text_centered(
                canvas,
                font,
                "or pass a file path on the command line  ·  ? for shortcuts",
                w * 0.5,
                top + 74.0 * s,
                12.5 * s,
                palette.muted,
            );
        }
    }

    fn draw_error_card(
        &mut self,
        canvas: &mut Canvas<'_>,
        s: f32,
        palette: Palette,
        err: &ErrorCard,
    ) {
        let w = canvas.width as f32;
        let h = canvas.height as f32;
        let card_w = (490.0 * s).min(w - 32.0 * s).max(220.0 * s);
        let pad = 22.0 * s;
        let card_h = (if err.hint.is_some() { 156.0 } else { 128.0 }) * s;
        let left = (w - card_w) * 0.5;
        let top = (h - card_h) * 0.5;

        canvas.fill_rounded_rect(
            left,
            top,
            left + card_w,
            top + card_h,
            16.0 * s,
            palette.panel.with_alpha(247),
            Some((palette.line_strong, 1.0 * s)),
        );

        canvas.fill_rounded_rect(
            left + pad,
            top + pad + 2.0 * s,
            left + pad + 3.0 * s,
            top + pad + 20.0 * s,
            1.5 * s,
            palette.red,
            None,
        );

        let fonts = self.ensure_fonts();
        let text_x = left + pad + 11.0 * s;
        if let Some(font) = fonts.sans_bold() {
            render_text_left(
                canvas,
                font,
                &err.title,
                text_x,
                top + pad,
                16.0 * s,
                palette.text,
            );
        }
        let mut cursor_y = top + pad + 28.0 * s;
        if !err.subject.is_empty() {
            if let Some(font) = fonts.mono() {
                render_text_left(
                    canvas,
                    font,
                    &err.subject,
                    left + pad,
                    cursor_y,
                    12.0 * s,
                    palette.muted,
                );
            }
            cursor_y += 22.0 * s;
        }
        if let Some(font) = fonts.sans() {
            render_text_left(
                canvas,
                font,
                &err.message,
                left + pad,
                cursor_y,
                13.5 * s,
                palette.text,
            );
            cursor_y += 28.0 * s;
            if let Some(ref hint) = err.hint {
                render_text_left(
                    canvas,
                    font,
                    hint,
                    left + pad,
                    cursor_y,
                    12.5 * s,
                    palette.red_text,
                );
            }
        }
    }

    fn draw_pill(
        &mut self,
        canvas: &mut Canvas<'_>,
        s: f32,
        palette: Palette,
        text: &str,
        bottom: bool,
        neutral: bool,
    ) {
        let w = canvas.width as f32;
        let h = canvas.height as f32;
        let font_px = 13.5 * s;
        let text_w = self
            .ensure_fonts()
            .sans()
            .map(|f| measure_text_width(f, text, font_px))
            .unwrap_or((text.len() as f32) * 7.5 * s);

        let dot_w = if neutral { 0.0 } else { 14.0 * s };
        let pad_x = 16.0 * s;
        let pill_w = (text_w + pad_x * 2.0 + dot_w).clamp(76.0 * s, w - 32.0 * s);
        let pill_h = 34.0 * s;
        let left = (w - pill_w) * 0.5;
        let top = if bottom {
            (h - pill_h - 24.0 * s).max(16.0 * s)
        } else {
            (h - pill_h) * 0.5
        };

        canvas.fill_rounded_rect(
            left,
            top,
            left + pill_w,
            top + pill_h,
            pill_h * 0.5,
            palette.panel.with_alpha(242),
            Some((palette.line, 1.0 * s)),
        );

        let text_x = if neutral {
            left + (pill_w - text_w) * 0.5
        } else {
            let dot_cx = left + pad_x + 3.0 * s;
            let dot_cy = top + pill_h * 0.5;
            canvas.fill_circle(dot_cx, dot_cy, 3.0 * s, palette.red);
            left + pad_x + dot_w
        };

        let fonts = self.ensure_fonts();
        if let Some(font) = fonts.sans() {
            let scaled = font.as_scaled(PxScale::from(font_px));
            let text_h = scaled.ascent() - scaled.descent();
            let y_top = top + (pill_h - text_h) * 0.5;
            render_text_left(canvas, font, text, text_x, y_top, font_px, palette.text);
        }
    }

    fn draw_help_sheet(&mut self, canvas: &mut Canvas<'_>, s: f32, palette: Palette) {
        let w = canvas.width as f32;
        let h = canvas.height as f32;

        canvas.fill_rect(
            0,
            0,
            canvas.width as i32,
            canvas.height as i32,
            palette.scrim,
        );

        let rows = help_rows();
        let pad = 24.0 * s;
        let title_h = 34.0 * s;
        let row_h = 26.0 * s;
        let footer_h = 24.0 * s;
        let card_w = (480.0 * s).min(w - 32.0 * s).max(280.0 * s);
        let card_h = (pad * 2.0 + title_h + rows.len() as f32 * row_h + footer_h + 10.0 * s)
            .min(h - 32.0 * s);
        let left = (w - card_w) * 0.5;
        let top = (h - card_h) * 0.5;

        canvas.fill_rounded_rect(
            left,
            top,
            left + card_w,
            top + card_h,
            16.0 * s,
            palette.panel.with_alpha(250),
            Some((palette.line_strong, 1.0 * s)),
        );

        canvas.fill_rounded_rect(
            left + pad,
            top + pad + 2.0 * s,
            left + pad + 3.0 * s,
            top + pad + 21.0 * s,
            1.5 * s,
            palette.red,
            None,
        );

        let fonts = self.ensure_fonts();
        if let Some(font) = fonts.sans_bold() {
            render_text_left(
                canvas,
                font,
                "Keyboard Shortcuts",
                left + pad + 11.0 * s,
                top + pad,
                16.5 * s,
                palette.text,
            );
        }

        let key_col_w = 166.0 * s;
        let desc_x = left + pad + key_col_w + 14.0 * s;
        let mut y = top + pad + title_h + 4.0 * s;
        let max_y = top + card_h - pad - footer_h;

        let key_font_px = 12.0 * s;
        let desc_font_px = 13.0 * s;

        for &(key_label, desc) in rows {
            if y + row_h > max_y + 4.0 * s {
                break;
            }
            let cap_top = y + 2.0 * s;
            let cap_bot = y + row_h - 3.0 * s;
            canvas.fill_rounded_rect(
                left + pad,
                cap_top,
                left + pad + key_col_w,
                cap_bot,
                6.0 * s,
                palette.surface,
                Some((palette.line, 1.0 * s)),
            );

            let fonts = self.ensure_fonts();
            if let Some(font) = fonts.mono() {
                let scaled = font.as_scaled(PxScale::from(key_font_px));
                let th = scaled.ascent() - scaled.descent();
                let ty = cap_top + ((cap_bot - cap_top) - th) * 0.5;
                render_text_centered(
                    canvas,
                    font,
                    key_label,
                    left + pad + key_col_w * 0.5,
                    ty,
                    key_font_px,
                    palette.text,
                );
            }
            if let Some(font) = fonts.sans() {
                let scaled = font.as_scaled(PxScale::from(desc_font_px));
                let th = scaled.ascent() - scaled.descent();
                let ty = y + (row_h - th) * 0.5;
                render_text_left(canvas, font, desc, desc_x, ty, desc_font_px, palette.text);
            }

            y += row_h;
        }

        let fonts = self.ensure_fonts();
        if let Some(font) = fonts.sans() {
            render_text_centered(
                canvas,
                font,
                "Press ? or Esc to close",
                w * 0.5,
                top + card_h - pad - footer_h + 6.0 * s,
                12.0 * s,
                palette.muted,
            );
        }
    }
}

fn measure_text_width(font: &FontVec, text: &str, size_px: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size_px));
    let mut width = 0.0f32;
    let mut prev = None;
    for ch in text.chars() {
        let gid = scaled.glyph_id(ch);
        if let Some(p) = prev {
            width += scaled.kern(p, gid);
        }
        width += scaled.h_advance(gid);
        prev = Some(gid);
    }
    width
}

fn render_text_centered(
    canvas: &mut Canvas<'_>,
    font: &FontVec,
    text: &str,
    center_x: f32,
    y_top: f32,
    size_px: f32,
    color: Rgba,
) {
    let w = measure_text_width(font, text, size_px);
    render_text_left(
        canvas,
        font,
        text,
        center_x - w * 0.5,
        y_top,
        size_px,
        color,
    );
}

fn render_text_left(
    canvas: &mut Canvas<'_>,
    font: &FontVec,
    text: &str,
    x_start: f32,
    y_top: f32,
    size_px: f32,
    color: Rgba,
) {
    let scaled = font.as_scaled(PxScale::from(size_px));
    let baseline = y_top + scaled.ascent();
    let mut pen_x = x_start;
    let mut prev = None;

    for ch in text.chars() {
        let gid = scaled.glyph_id(ch);
        if let Some(p) = prev {
            pen_x += scaled.kern(p, gid);
        }
        let glyph = gid.with_scale_and_position(PxScale::from(size_px), point(pen_x, baseline));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            let ox = bounds.min.x.floor() as i32;
            let oy = bounds.min.y.floor() as i32;
            outlined.draw(|gx, gy, cov| {
                if cov > 0.004 {
                    canvas.blend_pixel(ox + gx as i32, oy + gy as i32, color, cov);
                }
            });
        }
        pen_x += scaled.h_advance(gid);
        prev = Some(gid);
    }
}
