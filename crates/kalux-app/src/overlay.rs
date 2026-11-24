//! Lazy DirectWrite text overlays (help sheet, toasts, loading pill, error card, empty hint).
//!
//! Uses the shared ka-suite `Palette` tokens (Kavox / Kadex / Kapix: warm charcoal in dark mode,
//! warm paper in light mode, one red `#E8262D` accent).
//!
//! The `IDWriteFactory` is created on first overlay draw — **never** on the startup critical path
//! when opening an image file normally.

use kalux_core::action::help_rows;
use kalux_core::{Palette, Rgba};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_ROUNDED_RECT, ID2D1RenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::core::w;
use windows_numerics::Vector2;

/// Structured error card data shown in-window when an image or command fails.
#[derive(Clone, Debug)]
pub struct ErrorCard {
    pub title: String,
    pub subject: String,
    pub message: String,
    pub hint: Option<String>,
}

/// Snapshot of overlay state rendered on top of the canvas.
pub struct OverlayView<'a> {
    pub empty_hint: bool,
    pub error: Option<&'a ErrorCard>,
    pub loading_label: Option<&'a str>,
    pub toast: Option<&'a str>,
    pub show_help: bool,
}

impl OverlayView<'_> {
    pub fn is_anything_visible(&self) -> bool {
        self.empty_hint
            || self.error.is_some()
            || self.loading_label.is_some()
            || self.toast.is_some()
            || self.show_help
    }
}

struct TextFormats {
    dpi: u32,
    title: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_center: IDWriteTextFormat,
    small_center: IDWriteTextFormat,
    key: IDWriteTextFormat,
}

struct TargetBrushes {
    is_dark: bool,
    panel_bg: ID2D1SolidColorBrush,
    panel_border: ID2D1SolidColorBrush,
    key_bg: ID2D1SolidColorBrush,
    key_border: ID2D1SolidColorBrush,
    scrim: ID2D1SolidColorBrush,
    text_primary: ID2D1SolidColorBrush,
    text_muted: ID2D1SolidColorBrush,
    accent_red: ID2D1SolidColorBrush,
    accent_red_text: ID2D1SolidColorBrush,
}

/// Manages lazily initialized DirectWrite resources and target-bound brushes.
#[derive(Default)]
pub struct OverlayRenderer {
    dwrite: Option<IDWriteFactory>,
    formats: Option<TextFormats>,
    brushes: Option<TargetBrushes>,
}

impl OverlayRenderer {
    /// Drop device-dependent brushes (call when recreating the D2D render target).
    pub fn discard_target_resources(&mut self) {
        self.brushes = None;
    }

    /// Draw any active overlays onto `target` (which operates in physical pixels at 96 DPI).
    pub fn draw(
        &mut self,
        target: &ID2D1RenderTarget,
        client: (u32, u32),
        dpi: u32,
        palette: Palette,
        view: &OverlayView<'_>,
    ) {
        if !view.is_anything_visible() || client.0 == 0 || client.1 == 0 {
            return;
        }

        if self.ensure_dwrite(dpi).is_err() || self.ensure_brushes(target, palette).is_err() {
            return;
        }

        let scale = (dpi.max(96) as f32) / 96.0;
        let w = client.0 as f32;
        let h = client.1 as f32;

        if view.empty_hint && view.error.is_none() {
            self.draw_empty_hint(target, w, h, scale);
        }

        if let Some(err) = view.error {
            self.draw_error_card(target, w, h, scale, err);
        }

        if let Some(label) = view.loading_label {
            self.draw_loading_overlay(target, w, h, scale, label);
        }

        if let Some(toast) = view.toast {
            self.draw_toast(target, w, h, scale, toast);
        }

        if view.show_help {
            self.draw_help_overlay(target, w, h, scale);
        }
    }

    fn ensure_dwrite(&mut self, dpi: u32) -> windows::core::Result<()> {
        let factory = match self.dwrite.as_ref() {
            Some(f) => f,
            None => {
                // SAFETY: Creating a shared DirectWrite factory has no special preconditions.
                let f: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;
                self.dwrite.insert(f)
            }
        };

        let needs_formats = self.formats.as_ref().is_none_or(|f| f.dpi != dpi);
        if needs_formats {
            let scale = (dpi.max(96) as f32) / 96.0;

            let title = create_format(
                factory,
                w!("Segoe UI"),
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                18.0 * scale,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_WORD_WRAPPING_WRAP,
            )?;
            let body = create_format(
                factory,
                w!("Segoe UI"),
                DWRITE_FONT_WEIGHT_NORMAL,
                14.0 * scale,
                DWRITE_TEXT_ALIGNMENT_LEADING,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_WORD_WRAPPING_WRAP,
            )?;
            let body_center = create_format(
                factory,
                w!("Segoe UI"),
                DWRITE_FONT_WEIGHT_NORMAL,
                14.5 * scale,
                DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_WORD_WRAPPING_WRAP,
            )?;
            let small_center = create_format(
                factory,
                w!("Segoe UI"),
                DWRITE_FONT_WEIGHT_NORMAL,
                12.5 * scale,
                DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_WORD_WRAPPING_WRAP,
            )?;
            let key = create_format(
                factory,
                w!("Consolas"),
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                12.5 * scale,
                DWRITE_TEXT_ALIGNMENT_CENTER,
                DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_WORD_WRAPPING_NO_WRAP,
            )?;

            self.formats = Some(TextFormats {
                dpi,
                title,
                body,
                body_center,
                small_center,
                key,
            });
        }

        Ok(())
    }

    fn ensure_brushes(
        &mut self,
        target: &ID2D1RenderTarget,
        p: Palette,
    ) -> windows::core::Result<()> {
        if self
            .brushes
            .as_ref()
            .is_some_and(|b| b.is_dark == p.is_dark)
        {
            return Ok(());
        }

        let make = |rgba: Rgba| {
            let color = rgba_to_d2d(rgba);
            // SAFETY: `target` is a valid D2D render target.
            unsafe { target.CreateSolidColorBrush(&color, None) }
        };

        let panel_alpha = if p.is_dark { 244 } else { 248 };

        self.brushes = Some(TargetBrushes {
            is_dark: p.is_dark,
            panel_bg: make(p.panel.with_alpha(panel_alpha))?,
            panel_border: make(p.line)?,
            key_bg: make(p.surface)?,
            key_border: make(p.line_strong)?,
            scrim: make(p.scrim)?,
            text_primary: make(p.text)?,
            text_muted: make(p.muted)?,
            accent_red: make(p.red)?,
            accent_red_text: make(p.red_text)?,
        });

        Ok(())
    }

    fn draw_empty_hint(&self, target: &ID2D1RenderTarget, w: f32, h: f32, s: f32) {
        let Some(formats) = self.formats.as_ref() else {
            return;
        };
        let Some(brushes) = self.brushes.as_ref() else {
            return;
        };

        let box_w = (360.0 * s).min(w - 32.0 * s).max(160.0);
        let box_h = 128.0 * s;
        let left = (w - box_w) * 0.5;
        let top = (h - box_h) * 0.5;

        let card = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left,
                top,
                right: left + box_w,
                bottom: top + box_h,
            },
            radiusX: 16.0 * s,
            radiusY: 16.0 * s,
        };

        // SAFETY: Valid D2D render target and brushes.
        unsafe {
            target.FillRoundedRectangle(&card, &brushes.panel_bg);
            target.DrawRoundedRectangle(&card, &brushes.panel_border, 1.0 * s, None);
        }

        // Signature ka-suite red accent dot centered above the hint text.
        let dot = D2D1_ELLIPSE {
            point: Vector2 {
                X: w * 0.5,
                Y: top + 24.0 * s,
            },
            radiusX: 3.5 * s,
            radiusY: 3.5 * s,
        };
        // SAFETY: Valid D2D render target and brush.
        unsafe { target.FillEllipse(&dot, &brushes.accent_red) };

        let main_rect = D2D_RECT_F {
            left: left + 16.0 * s,
            top: top + 36.0 * s,
            right: left + box_w - 16.0 * s,
            bottom: top + 74.0 * s,
        };
        draw_text(
            target,
            "Drop an image here",
            &formats.body_center,
            main_rect,
            &brushes.text_primary,
        );

        let sub_rect = D2D_RECT_F {
            left: left + 16.0 * s,
            top: top + 74.0 * s,
            right: left + box_w - 16.0 * s,
            bottom: top + 108.0 * s,
        };
        draw_text(
            target,
            "Press ? for keyboard shortcuts",
            &formats.small_center,
            sub_rect,
            &brushes.text_muted,
        );
    }

    fn draw_error_card(&self, target: &ID2D1RenderTarget, w: f32, h: f32, s: f32, err: &ErrorCard) {
        let Some(formats) = self.formats.as_ref() else {
            return;
        };
        let Some(brushes) = self.brushes.as_ref() else {
            return;
        };

        let card_w = (540.0 * s).min(w - 40.0 * s).max(240.0);
        let pad = 24.0 * s;
        let has_hint = err.hint.as_deref().is_some_and(|t| !t.is_empty());
        let card_h = (if has_hint { 216.0 } else { 176.0 } * s).min(h - 40.0 * s);
        let left = (w - card_w) * 0.5;
        let top = (h - card_h) * 0.5;

        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left,
                top,
                right: left + card_w,
                bottom: top + card_h,
            },
            radiusX: 16.0 * s,
            radiusY: 16.0 * s,
        };

        // SAFETY: Valid D2D render target and brushes.
        unsafe {
            target.FillRoundedRectangle(&rr, &brushes.panel_bg);
            target.DrawRoundedRectangle(&rr, &brushes.panel_border, 1.0 * s, None);
        }

        let inner_l = left + pad;
        let inner_r = left + card_w - pad;
        let mut y = top + pad;

        let accent_bar = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: inner_l,
                top: y + 4.0 * s,
                right: inner_l + 3.5 * s,
                bottom: y + 24.0 * s,
            },
            radiusX: 1.75 * s,
            radiusY: 1.75 * s,
        };
        // SAFETY: Valid D2D render target and brush.
        unsafe { target.FillRoundedRectangle(&accent_bar, &brushes.accent_red) };

        draw_text(
            target,
            &err.title,
            &formats.title,
            D2D_RECT_F {
                left: inner_l + 12.0 * s,
                top: y,
                right: inner_r,
                bottom: y + 28.0 * s,
            },
            &brushes.text_primary,
        );
        y += 34.0 * s;

        if !err.subject.is_empty() {
            draw_text(
                target,
                &err.subject,
                &formats.body,
                D2D_RECT_F {
                    left: inner_l,
                    top: y,
                    right: inner_r,
                    bottom: y + 24.0 * s,
                },
                &brushes.text_muted,
            );
            y += 28.0 * s;
        }

        draw_text(
            target,
            &err.message,
            &formats.body,
            D2D_RECT_F {
                left: inner_l,
                top: y,
                right: inner_r,
                bottom: y + 52.0 * s,
            },
            &brushes.text_primary,
        );
        y += 56.0 * s;

        if let Some(hint) = err.hint.as_deref().filter(|t| !t.is_empty()) {
            draw_text(
                target,
                hint,
                &formats.body,
                D2D_RECT_F {
                    left: inner_l,
                    top: y,
                    right: inner_r,
                    bottom: (top + card_h - pad).max(y + 24.0 * s),
                },
                &brushes.accent_red_text,
            );
        }
    }

    fn draw_loading_overlay(
        &self,
        target: &ID2D1RenderTarget,
        w: f32,
        h: f32,
        s: f32,
        label: &str,
    ) {
        let Some(brushes) = self.brushes.as_ref() else {
            return;
        };

        let full = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: w,
            bottom: h,
        };
        // SAFETY: Valid D2D render target and brush.
        unsafe { target.FillRectangle(&full, &brushes.scrim) };

        self.draw_pill(target, w, h * 0.5, s, label, true);
    }

    fn draw_toast(&self, target: &ID2D1RenderTarget, w: f32, h: f32, s: f32, text: &str) {
        let center_y = (h - 42.0 * s).max(32.0 * s);
        self.draw_pill(target, w, center_y, s, text, true);
    }

    fn draw_pill(
        &self,
        target: &ID2D1RenderTarget,
        w: f32,
        center_y: f32,
        s: f32,
        text: &str,
        red_dot: bool,
    ) {
        let Some(factory) = self.dwrite.as_ref() else {
            return;
        };
        let Some(formats) = self.formats.as_ref() else {
            return;
        };
        let Some(brushes) = self.brushes.as_ref() else {
            return;
        };

        let dot_space = if red_dot { 14.0 * s } else { 0.0 };
        let max_w = (w - 48.0 * s).max(120.0);
        let (text_w, text_h) = measure_text(
            factory,
            &formats.body_center,
            text,
            (max_w - dot_space).max(60.0),
            40.0 * s,
        );
        let pill_w = (text_w + 32.0 * s + dot_space).clamp(76.0 * s, max_w);
        let pill_h = (text_h + 16.0 * s).max(34.0 * s);

        let left = (w - pill_w) * 0.5;
        let top = center_y - pill_h * 0.5;
        let rect = D2D_RECT_F {
            left,
            top,
            right: left + pill_w,
            bottom: top + pill_h,
        };
        let rr = D2D1_ROUNDED_RECT {
            rect,
            radiusX: pill_h * 0.5,
            radiusY: pill_h * 0.5,
        };

        // SAFETY: Valid D2D render target and brushes.
        unsafe {
            target.FillRoundedRectangle(&rr, &brushes.panel_bg);
            target.DrawRoundedRectangle(&rr, &brushes.panel_border, 1.0 * s, None);
        }

        let text_rect = if red_dot {
            let dot = D2D1_ELLIPSE {
                point: Vector2 {
                    X: left + 16.0 * s,
                    Y: center_y,
                },
                radiusX: 3.0 * s,
                radiusY: 3.0 * s,
            };
            // SAFETY: Valid D2D render target and brush.
            unsafe { target.FillEllipse(&dot, &brushes.accent_red) };
            D2D_RECT_F {
                left: left + dot_space * 0.75,
                top,
                right: left + pill_w,
                bottom: top + pill_h,
            }
        } else {
            rect
        };

        draw_text(
            target,
            text,
            &formats.body_center,
            text_rect,
            &brushes.text_primary,
        );
    }

    fn draw_help_overlay(&self, target: &ID2D1RenderTarget, w: f32, h: f32, s: f32) {
        let Some(formats) = self.formats.as_ref() else {
            return;
        };
        let Some(brushes) = self.brushes.as_ref() else {
            return;
        };

        let full = D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: w,
            bottom: h,
        };
        // SAFETY: Valid D2D render target and brush.
        unsafe { target.FillRectangle(&full, &brushes.scrim) };

        let rows = help_rows();
        let pad = 22.0 * s;
        let header_h = 34.0 * s;
        let footer_h = 26.0 * s;
        let row_h = 28.0 * s;
        let max_panel_h = (h - 48.0 * s).max(160.0);
        let desired_h = pad * 2.0 + header_h + footer_h + (rows.len() as f32) * row_h + 12.0 * s;
        let panel_h = desired_h.min(max_panel_h);
        let panel_w = (480.0 * s).min(w - 40.0 * s).max(260.0);

        let left = (w - panel_w) * 0.5;
        let top = (h - panel_h) * 0.5;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left,
                top,
                right: left + panel_w,
                bottom: top + panel_h,
            },
            radiusX: 16.0 * s,
            radiusY: 16.0 * s,
        };

        // SAFETY: Valid D2D render target and brushes.
        unsafe {
            target.FillRoundedRectangle(&rr, &brushes.panel_bg);
            target.DrawRoundedRectangle(&rr, &brushes.panel_border, 1.0 * s, None);
        }

        // Signature ka-suite red accent bar beside the modal title.
        let accent_bar = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: left + pad,
                top: top + pad + 7.0 * s,
                right: left + pad + 3.5 * s,
                bottom: top + pad + header_h - 7.0 * s,
            },
            radiusX: 1.75 * s,
            radiusY: 1.75 * s,
        };
        // SAFETY: Valid D2D render target and brush.
        unsafe { target.FillRoundedRectangle(&accent_bar, &brushes.accent_red) };

        draw_text(
            target,
            "Keyboard Shortcuts",
            &formats.title,
            D2D_RECT_F {
                left: left + pad + 12.0 * s,
                top: top + pad,
                right: left + panel_w - pad,
                bottom: top + pad + header_h,
            },
            &brushes.text_primary,
        );

        let key_col_w = (165.0 * s).min(panel_w * 0.42);
        let content_bottom = top + panel_h - pad - footer_h;
        let mut y = top + pad + header_h + 8.0 * s;

        for &(keys, desc) in rows {
            if y + row_h > content_bottom + 2.0 * s {
                break;
            }

            let badge = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: left + pad,
                    top: y + 3.0 * s,
                    right: left + pad + key_col_w,
                    bottom: y + row_h - 3.0 * s,
                },
                radiusX: 6.0 * s,
                radiusY: 6.0 * s,
            };
            // SAFETY: Valid D2D render target and brushes.
            unsafe {
                target.FillRoundedRectangle(&badge, &brushes.key_bg);
                target.DrawRoundedRectangle(&badge, &brushes.key_border, 1.0 * s, None);
            }

            draw_text(
                target,
                keys,
                &formats.key,
                badge.rect,
                &brushes.text_primary,
            );

            draw_text(
                target,
                desc,
                &formats.body,
                D2D_RECT_F {
                    left: left + pad + key_col_w + 14.0 * s,
                    top: y,
                    right: left + panel_w - pad,
                    bottom: y + row_h,
                },
                &brushes.text_primary,
            );

            y += row_h;
        }

        draw_text(
            target,
            "Press ? or Esc to close",
            &formats.small_center,
            D2D_RECT_F {
                left: left + pad,
                top: top + panel_h - pad - footer_h + 4.0 * s,
                right: left + panel_w - pad,
                bottom: top + panel_h - pad + 4.0 * s,
            },
            &brushes.text_muted,
        );
    }
}

pub fn rgba_to_d2d(rgba: Rgba) -> D2D1_COLOR_F {
    let [r, g, b, a] = rgba.to_f32_rgba();
    D2D1_COLOR_F { r, g, b, a }
}

fn create_format(
    factory: &IDWriteFactory,
    font_family: windows::core::PCWSTR,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    font_size: f32,
    text_align: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
    para_align: windows::Win32::Graphics::DirectWrite::DWRITE_PARAGRAPH_ALIGNMENT,
    wrapping: windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING,
) -> windows::core::Result<IDWriteTextFormat> {
    // SAFETY: `factory` is valid and `font_family` is a static NUL-terminated UTF-16 string.
    let fmt = unsafe {
        factory.CreateTextFormat(
            font_family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            font_size,
            w!("en-US"),
        )
    }?;
    // SAFETY: `fmt` is a valid `IDWriteTextFormat`.
    unsafe {
        fmt.SetTextAlignment(text_align)?;
        fmt.SetParagraphAlignment(para_align)?;
        fmt.SetWordWrapping(wrapping)?;
    }
    Ok(fmt)
}

fn measure_text(
    factory: &IDWriteFactory,
    format: &IDWriteTextFormat,
    text: &str,
    max_width: f32,
    max_height: f32,
) -> (f32, f32) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `factory` and `format` are valid; `wide` is live for the call.
    let Ok(layout) = (unsafe { factory.CreateTextLayout(&wide, format, max_width, max_height) })
    else {
        return (120.0, 24.0);
    };
    let mut metrics = DWRITE_TEXT_METRICS::default();
    // SAFETY: `layout` is valid and `metrics` is a valid stack pointer.
    if unsafe { layout.GetMetrics(&mut metrics) }.is_ok() {
        (metrics.widthIncludingTrailingWhitespace, metrics.height)
    } else {
        (120.0, 24.0)
    }
}

fn draw_text(
    target: &ID2D1RenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `target`, `format`, `brush`, and `wide` are valid for the duration of the draw call.
    unsafe {
        target.DrawText(
            &wide,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}
