//! Windows Direct2D + DirectWrite Edit Mode UI renderer: crop overlay + floating ka-suite dock.
//!
//! Consumes the shared [`kalux_core::DockLayout`] geometry so visual rendering and pointer
//! hit-testing match pixel-for-pixel across Windows and Linux.

use kalux_core::edit::EditDrag;
use kalux_core::{
    CropHandle, DockButton, DockLayout, EditPrompt, EditSession, EditTab, Palette, Rect, Rgba,
};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE, D2D1_ROUNDED_RECT, ID2D1RenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::core::w;
use windows_numerics::Vector2;

use crate::overlay::rgba_to_d2d;

struct EditFormats {
    scale_key: u32,
    label_center: IDWriteTextFormat,
    label_center_bold: IDWriteTextFormat,
    label_left: IDWriteTextFormat,
    value_right: IDWriteTextFormat,
    mono_center: IDWriteTextFormat,
}

#[derive(Default)]
pub struct EditUiRenderer {
    dwrite: Option<IDWriteFactory>,
    formats: Option<EditFormats>,
}

impl EditUiRenderer {
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        target: &ID2D1RenderTarget,
        client: (u32, u32),
        scale: f32,
        palette: Palette,
        image_dest: Rect,
        source_dims: (u32, u32),
        session: &EditSession,
        layout: &DockLayout,
    ) {
        let s = scale.max(1.0);
        if self.ensure_formats(s).is_err() {
            return;
        }
        let Some(ref fmts) = self.formats else {
            return;
        };

        let recipe = session.recipe();

        // 1. Crop frame overlay (hidden while holding `\` compare).
        if !session.comparing_original {
            let crop_rect = recipe.crop.to_screen_rect(image_dest);
            let active_handle = match session.drag {
                Some(EditDrag::CropHandle { handle, .. }) => Some(handle),
                Some(EditDrag::NewCrop { .. }) => Some(CropHandle::Se),
                _ => session.hovered_handle,
            };
            let is_dragging_crop = matches!(
                session.drag,
                Some(EditDrag::CropHandle { .. } | EditDrag::NewCrop { .. })
            );
            draw_crop_overlay(
                target,
                fmts,
                s,
                palette,
                image_dest,
                crop_rect,
                recipe.output_pixel_rect(source_dims.0, source_dims.1),
                active_handle,
                is_dragging_crop,
                session.tab == EditTab::Crop || !recipe.crop.is_full(),
            );
        } else {
            // Subtle "Original" badge top-center while holding compare key.
            let badge_w = 96.0 * s;
            let badge_h = 26.0 * s;
            let bx = (client.0 as f32 - badge_w) * 0.5;
            let by = 14.0 * s;
            fill_rounded(
                target,
                Rect {
                    x: bx,
                    y: by,
                    w: badge_w,
                    h: badge_h,
                },
                13.0 * s,
                palette.panel.with_alpha(235),
                Some((palette.red, 1.0 * s)),
            );
            draw_text(
                target,
                "Original",
                &fmts.label_center_bold,
                Rect {
                    x: bx,
                    y: by,
                    w: badge_w,
                    h: badge_h,
                },
                palette.text,
            );
        }

        // Animated source static-frame notice (top-left pill).
        if session.is_animated_source {
            let msg = format!(
                "Static frame {} / {} · saves as copy",
                session.frame_index + 1,
                session.total_frames
            );
            let pw = (msg.len() as f32 * 6.5 + 24.0) * s;
            let ph = 26.0 * s;
            let px = 14.0 * s;
            let py = 14.0 * s;
            fill_rounded(
                target,
                Rect {
                    x: px,
                    y: py,
                    w: pw,
                    h: ph,
                },
                13.0 * s,
                palette.panel.with_alpha(235),
                Some((palette.line, 1.0 * s)),
            );
            draw_text(
                target,
                &msg,
                &fmts.label_center,
                Rect {
                    x: px,
                    y: py,
                    w: pw,
                    h: ph,
                },
                palette.muted,
            );
        }

        // 2. Floating bottom Edit Dock + optional inline prompt bar.
        draw_dock(target, fmts, s, palette, session, layout);
    }

    fn ensure_formats(&mut self, scale: f32) -> windows::core::Result<()> {
        let factory = match self.dwrite.as_ref() {
            Some(f) => f,
            None => {
                // SAFETY: Creating a shared DirectWrite factory has no preconditions.
                let f: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }?;
                self.dwrite.insert(f)
            }
        };
        let scale_key = (scale * 100.0).round() as u32;
        if self
            .formats
            .as_ref()
            .is_some_and(|f| f.scale_key == scale_key)
        {
            return Ok(());
        }
        let label_center = make_fmt(
            factory,
            w!("Segoe UI"),
            DWRITE_FONT_WEIGHT_NORMAL,
            11.5 * scale,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?;
        let label_center_bold = make_fmt(
            factory,
            w!("Segoe UI"),
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            11.5 * scale,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?;
        let label_left = make_fmt(
            factory,
            w!("Segoe UI"),
            DWRITE_FONT_WEIGHT_NORMAL,
            11.5 * scale,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        )?;
        let value_right = make_fmt(
            factory,
            w!("Consolas"),
            DWRITE_FONT_WEIGHT_NORMAL,
            10.5 * scale,
            DWRITE_TEXT_ALIGNMENT_TRAILING,
        )?;
        let mono_center = make_fmt(
            factory,
            w!("Consolas"),
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            11.0 * scale,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        )?;
        self.formats = Some(EditFormats {
            scale_key,
            label_center,
            label_center_bold,
            label_left,
            value_right,
            mono_center,
        });
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_crop_overlay(
    target: &ID2D1RenderTarget,
    fmts: &EditFormats,
    s: f32,
    palette: Palette,
    image_dest: Rect,
    crop: Rect,
    out_px: kalux_core::PixelRect,
    active_handle: Option<CropHandle>,
    show_thirds: bool,
    show_frame: bool,
) {
    if !show_frame {
        return;
    }

    let ix0 = image_dest.x;
    let iy0 = image_dest.y;
    let ix1 = image_dest.x + image_dest.w;
    let iy1 = image_dest.y + image_dest.h;

    let cx0 = crop.x.clamp(ix0, ix1);
    let cy0 = crop.y.clamp(iy0, iy1);
    let cx1 = (crop.x + crop.w).clamp(cx0, ix1);
    let cy1 = (crop.y + crop.h).clamp(cy0, iy1);

    // Dimmed outer scrim covering only the cropped-out regions.
    let scrim = Rgba::rgb(0x00, 0x00, 0x00).with_alpha(158);
    if cy0 > iy0 {
        fill_rect(
            target,
            Rect {
                x: ix0,
                y: iy0,
                w: ix1 - ix0,
                h: cy0 - iy0,
            },
            scrim,
        );
    }
    if cy1 < iy1 {
        fill_rect(
            target,
            Rect {
                x: ix0,
                y: cy1,
                w: ix1 - ix0,
                h: iy1 - cy1,
            },
            scrim,
        );
    }
    if cx0 > ix0 {
        fill_rect(
            target,
            Rect {
                x: ix0,
                y: cy0,
                w: cx0 - ix0,
                h: cy1 - cy0,
            },
            scrim,
        );
    }
    if cx1 < ix1 {
        fill_rect(
            target,
            Rect {
                x: cx1,
                y: cy0,
                w: ix1 - cx1,
                h: cy1 - cy0,
            },
            scrim,
        );
    }

    // Rule-of-thirds grid while dragging.
    if show_thirds && (cx1 - cx0) > 24.0 && (cy1 - cy0) > 24.0 {
        let grid = Rgba::rgb(0xff, 0xff, 0xff).with_alpha(110);
        for k in 1..=2 {
            let gx = cx0 + (cx1 - cx0) * (k as f32 / 3.0);
            let gy = cy0 + (cy1 - cy0) * (k as f32 / 3.0);
            draw_line(target, (gx, cy0), (gx, cy1), grid, 1.0);
            draw_line(target, (cx0, gy), (cx1, gy), grid, 1.0);
        }
    }

    // 1 px crop rectangle border.
    let border = Rgba::rgb(0xff, 0xff, 0xff).with_alpha(235);
    if let Some(brush) = make_brush(target, border) {
        let r = D2D_RECT_F {
            left: cx0 + 0.5,
            top: cy0 + 0.5,
            right: cx1 - 0.5,
            bottom: cy1 - 0.5,
        };
        // SAFETY: Valid D2D target and brush inside BeginDraw..EndDraw.
        unsafe { target.DrawRectangle(&r, &brush, 1.0, None) };
    }

    // Corner L-brackets + edge mid-bars.
    let arm = (16.0 * s).min((cx1 - cx0) * 0.35).min((cy1 - cy0) * 0.35);
    let thick = (3.0 * s).max(2.0);
    let color_for = |h: CropHandle| {
        if active_handle == Some(h) {
            palette.red
        } else {
            Rgba::rgb(0xff, 0xff, 0xff)
        }
    };

    // NW
    let nw = color_for(CropHandle::Nw);
    fill_rect(
        target,
        Rect {
            x: cx0 - 1.0,
            y: cy0 - 1.0,
            w: arm,
            h: thick,
        },
        nw,
    );
    fill_rect(
        target,
        Rect {
            x: cx0 - 1.0,
            y: cy0 - 1.0,
            w: thick,
            h: arm,
        },
        nw,
    );
    // NE
    let ne = color_for(CropHandle::Ne);
    fill_rect(
        target,
        Rect {
            x: cx1 - arm + 1.0,
            y: cy0 - 1.0,
            w: arm,
            h: thick,
        },
        ne,
    );
    fill_rect(
        target,
        Rect {
            x: cx1 - thick + 1.0,
            y: cy0 - 1.0,
            w: thick,
            h: arm,
        },
        ne,
    );
    // SW
    let sw = color_for(CropHandle::Sw);
    fill_rect(
        target,
        Rect {
            x: cx0 - 1.0,
            y: cy1 - thick + 1.0,
            w: arm,
            h: thick,
        },
        sw,
    );
    fill_rect(
        target,
        Rect {
            x: cx0 - 1.0,
            y: cy1 - arm + 1.0,
            w: thick,
            h: arm,
        },
        sw,
    );
    // SE
    let se = color_for(CropHandle::Se);
    fill_rect(
        target,
        Rect {
            x: cx1 - arm + 1.0,
            y: cy1 - thick + 1.0,
            w: arm,
            h: thick,
        },
        se,
    );
    fill_rect(
        target,
        Rect {
            x: cx1 - thick + 1.0,
            y: cy1 - arm + 1.0,
            w: thick,
            h: arm,
        },
        se,
    );

    // Mid-edge grab bars.
    let bar_len = (22.0 * s).min((cx1 - cx0) * 0.28);
    if bar_len >= 8.0 {
        let mx = (cx0 + cx1) * 0.5;
        fill_rect(
            target,
            Rect {
                x: mx - bar_len * 0.5,
                y: cy0 - 1.0,
                w: bar_len,
                h: thick,
            },
            color_for(CropHandle::N),
        );
        fill_rect(
            target,
            Rect {
                x: mx - bar_len * 0.5,
                y: cy1 - thick + 1.0,
                w: bar_len,
                h: thick,
            },
            color_for(CropHandle::S),
        );
    }
    let vbar_len = (22.0 * s).min((cy1 - cy0) * 0.28);
    if vbar_len >= 8.0 {
        let my = (cy0 + cy1) * 0.5;
        fill_rect(
            target,
            Rect {
                x: cx0 - 1.0,
                y: my - vbar_len * 0.5,
                w: thick,
                h: vbar_len,
            },
            color_for(CropHandle::W),
        );
        fill_rect(
            target,
            Rect {
                x: cx1 - thick + 1.0,
                y: my - vbar_len * 0.5,
                w: thick,
                h: vbar_len,
            },
            color_for(CropHandle::E),
        );
    }

    // Live source-pixel dimension readout pill at bottom-right of crop frame.
    let label = format!("{} \u{00D7} {}", out_px.w, out_px.h);
    let pw = (label.len() as f32 * 6.8 + 18.0) * s;
    let ph = 22.0 * s;
    if (cx1 - cx0) > pw + 16.0 * s && (cy1 - cy0) > ph + 16.0 * s {
        let px = cx1 - pw - 8.0 * s;
        let py = cy1 - ph - 8.0 * s;
        let pill = Rect {
            x: px,
            y: py,
            w: pw,
            h: ph,
        };
        fill_rounded(
            target,
            pill,
            11.0 * s,
            Rgba::rgb(0x16, 0x16, 0x16).with_alpha(215),
            Some((Rgba::rgb(0xff, 0xff, 0xff).with_alpha(45), 1.0)),
        );
        draw_text(
            target,
            &label,
            &fmts.mono_center,
            pill,
            Rgba::rgb(0xf4, 0xf3, 0xef),
        );
    }
}

fn draw_dock(
    target: &ID2D1RenderTarget,
    fmts: &EditFormats,
    s: f32,
    palette: Palette,
    session: &EditSession,
    layout: &DockLayout,
) {
    let p = layout.panel;
    let panel_bg = palette.panel.with_alpha(244);
    fill_rounded(target, p, 10.0 * s, panel_bg, Some((palette.line, 1.0)));

    // Divider line between row 0 (tabs/actions) and tool row(s).
    let div_y = p.y + 8.0 * s + 28.0 * s + 3.0 * s;
    draw_line(
        target,
        (p.x + 10.0 * s, div_y),
        (p.x + p.w - 10.0 * s, div_y),
        palette.line,
        1.0,
    );

    // Optional inline prompt bar floating above dock.
    if let Some(pr) = layout.prompt_panel {
        fill_rounded(target, pr, 9.0 * s, panel_bg, Some((palette.red, 1.2)));
        let prompt_msg = match session.prompt {
            EditPrompt::SaveChoice => "Save changes?",
            EditPrompt::UnsavedExit => "Unsaved edits",
            EditPrompt::None => "",
        };
        draw_text(
            target,
            prompt_msg,
            &fmts.label_left,
            Rect {
                x: pr.x + 14.0 * s,
                y: pr.y,
                w: 140.0 * s,
                h: pr.h,
            },
            palette.text,
        );
    }

    let recipe = session.recipe();
    for item in &layout.items {
        let hovered = session.hovered_button == Some(item.button);
        match item.button {
            DockButton::TabCrop => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Crop",
                    session.tab == EditTab::Crop,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::TabAdjust => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Adjust",
                    session.tab == EditTab::Adjust,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::Aspect(preset) => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    preset.label(session.aspect_swapped),
                    session.aspect == preset,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::SwapAspect => {
                let enabled = !matches!(
                    session.aspect,
                    kalux_core::AspectPreset::Free | kalux_core::AspectPreset::Square
                );
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::SwapAspect,
                    false,
                    hovered && enabled,
                    enabled,
                );
            }
            DockButton::RotateCcw => {
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::RotateCcw,
                    false,
                    hovered,
                    true,
                );
            }
            DockButton::RotateCw => {
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::RotateCw,
                    false,
                    hovered,
                    true,
                );
            }
            DockButton::FlipH => {
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::FlipH,
                    recipe.flip_h,
                    hovered,
                    true,
                );
            }
            DockButton::FlipV => {
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::FlipV,
                    recipe.flip_v,
                    hovered,
                    true,
                );
            }
            DockButton::Slider(slider) => {
                draw_slider(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    slider.label(),
                    recipe.tone.get(slider),
                    hovered
                        || matches!(
                            session.drag,
                            Some(EditDrag::Slider { slider: active, .. }) if active == slider
                        ),
                );
            }
            DockButton::Monochrome => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Mono (M)",
                    recipe.tone.monochrome,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::Reset => {
                let enabled = session.is_dirty();
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Reset",
                    false,
                    hovered && enabled,
                    false,
                    enabled,
                );
            }
            DockButton::Compare => {
                let enabled = session.is_dirty();
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "\\",
                    session.comparing_original,
                    hovered && enabled,
                    false,
                    enabled,
                );
            }
            DockButton::Undo => {
                let enabled = session.history.can_undo();
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::Undo,
                    false,
                    hovered && enabled,
                    enabled,
                );
            }
            DockButton::Redo => {
                let enabled = session.history.can_redo();
                draw_icon_chip(
                    target,
                    s,
                    palette,
                    item.rect,
                    DockIcon::Redo,
                    false,
                    hovered && enabled,
                    enabled,
                );
            }
            DockButton::QualityCycle => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    session.quality.label(),
                    false,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::CopyEdited => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Copy",
                    false,
                    hovered,
                    false,
                    !session.exporting,
                );
            }
            DockButton::SaveOverwrite => {
                let enabled = session.is_dirty() && !session.exporting;
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Save",
                    false,
                    hovered && enabled,
                    false,
                    enabled,
                );
            }
            DockButton::SaveCopy => {
                let enabled = session.is_dirty() && !session.exporting;
                let label = if session.exporting {
                    "Saving…"
                } else {
                    "Save Copy"
                };
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    label,
                    false,
                    hovered && enabled,
                    true,
                    enabled,
                );
            }
            DockButton::Done => {
                draw_chip(
                    target, fmts, s, palette, item.rect, "Done", false, hovered, false, true,
                );
            }
            DockButton::PromptSaveCopy => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Save Copy",
                    false,
                    hovered,
                    true,
                    true,
                );
            }
            DockButton::PromptOverwrite => {
                draw_chip(
                    target,
                    fmts,
                    s,
                    palette,
                    item.rect,
                    "Overwrite",
                    false,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::PromptDiscard => {
                draw_chip(
                    target, fmts, s, palette, item.rect, "Discard", false, hovered, false, true,
                );
            }
            DockButton::PromptCancel => {
                draw_chip(
                    target, fmts, s, palette, item.rect, "Cancel", false, hovered, false, true,
                );
            }
        }
    }
}

#[derive(Copy, Clone)]
enum DockIcon {
    SwapAspect,
    RotateCcw,
    RotateCw,
    FlipH,
    FlipV,
    Undo,
    Redo,
}

#[allow(clippy::too_many_arguments)]
fn draw_icon_chip(
    target: &ID2D1RenderTarget,
    s: f32,
    palette: Palette,
    r: Rect,
    icon: DockIcon,
    active: bool,
    hovered: bool,
    enabled: bool,
) {
    let (bg, border, fg) = if active {
        let fill = if palette.is_dark {
            Rgba::rgb(0x2c, 0x2a, 0x28)
        } else {
            Rgba::rgb(0xea, 0xe7, 0xe1)
        };
        (fill, Some((palette.red, 1.2 * s)), palette.text)
    } else if hovered && enabled {
        let fill = if palette.is_dark {
            Rgba::rgb(0x28, 0x26, 0x24)
        } else {
            Rgba::rgb(0xee, 0xeb, 0xe5)
        };
        (fill, Some((palette.line_strong, 1.0 * s)), palette.text)
    } else {
        (
            palette.surface,
            Some((palette.line, 1.0 * s)),
            if enabled { palette.text } else { palette.faint },
        )
    };
    fill_rounded(target, r, 6.0 * s, bg, border);

    let cx = r.x + r.w * 0.5;
    let cy = r.y + r.h * 0.5;
    let sw = 1.45 * s;
    let seg = |x0: f32, y0: f32, x1: f32, y1: f32| {
        draw_line(
            target,
            (cx + x0 * s, cy + y0 * s),
            (cx + x1 * s, cy + y1 * s),
            fg,
            sw,
        );
    };

    match icon {
        DockIcon::SwapAspect => {
            seg(-4.5, -2.5, 4.5, -2.5);
            seg(2.0, -5.0, 4.5, -2.5);
            seg(2.0, 0.0, 4.5, -2.5);
            seg(4.5, 2.5, -4.5, 2.5);
            seg(-2.0, 0.0, -4.5, 2.5);
            seg(-2.0, 5.0, -4.5, 2.5);
        }
        DockIcon::RotateCcw => {
            seg(-4.0, -3.5, 3.5, -3.5);
            seg(3.5, -3.5, 3.5, 3.5);
            seg(3.5, 3.5, -3.5, 3.5);
            seg(-3.5, 3.5, -3.5, 0.8);
            seg(-4.2, -3.5, -1.2, -6.0);
            seg(-4.2, -3.5, -1.2, -1.0);
        }
        DockIcon::RotateCw => {
            seg(4.0, -3.5, -3.5, -3.5);
            seg(-3.5, -3.5, -3.5, 3.5);
            seg(-3.5, 3.5, 3.5, 3.5);
            seg(3.5, 3.5, 3.5, 0.8);
            seg(4.2, -3.5, 1.2, -6.0);
            seg(4.2, -3.5, 1.2, -1.0);
        }
        DockIcon::FlipH => {
            seg(0.0, -5.0, 0.0, 5.0);
            seg(-2.2, -3.5, -5.5, 0.0);
            seg(-5.5, 0.0, -2.2, 3.5);
            seg(2.2, -3.5, 5.5, 0.0);
            seg(5.5, 0.0, 2.2, 3.5);
        }
        DockIcon::FlipV => {
            seg(-5.0, 0.0, 5.0, 0.0);
            seg(-3.5, -2.2, 0.0, -5.5);
            seg(0.0, -5.5, 3.5, -2.2);
            seg(-3.5, 2.2, 0.0, 5.5);
            seg(0.0, 5.5, 3.5, 2.2);
        }
        DockIcon::Undo => {
            seg(-4.5, -1.5, 2.2, -1.5);
            seg(2.2, -1.5, 4.2, 0.5);
            seg(4.2, 0.5, 4.2, 2.0);
            seg(4.2, 2.0, 2.2, 4.0);
            seg(2.2, 4.0, -1.5, 4.0);
            seg(-4.5, -1.5, -1.8, -4.2);
            seg(-4.5, -1.5, -1.8, 1.2);
        }
        DockIcon::Redo => {
            seg(4.5, -1.5, -2.2, -1.5);
            seg(-2.2, -1.5, -4.2, 0.5);
            seg(-4.2, 0.5, -4.2, 2.0);
            seg(-4.2, 2.0, -2.2, 4.0);
            seg(-2.2, 4.0, 1.5, 4.0);
            seg(4.5, -1.5, 1.8, -4.2);
            seg(4.5, -1.5, 1.8, 1.2);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_chip(
    target: &ID2D1RenderTarget,
    fmts: &EditFormats,
    s: f32,
    palette: Palette,
    r: Rect,
    label: &str,
    active: bool,
    hovered: bool,
    primary_red: bool,
    enabled: bool,
) {
    let (bg, border, fg) = if primary_red {
        if enabled {
            let fill = if hovered {
                palette.red_text
            } else {
                palette.red
            };
            (fill, None, Rgba::rgb(0xff, 0xff, 0xff))
        } else {
            (
                palette.red.with_alpha(95),
                None,
                Rgba::rgb(0xff, 0xff, 0xff).with_alpha(165),
            )
        }
    } else if active {
        let fill = if palette.is_dark {
            Rgba::rgb(0x2c, 0x2a, 0x28)
        } else {
            Rgba::rgb(0xea, 0xe7, 0xe1)
        };
        (fill, Some((palette.red, 1.2)), palette.text)
    } else if hovered && enabled {
        let fill = if palette.is_dark {
            Rgba::rgb(0x28, 0x26, 0x24)
        } else {
            Rgba::rgb(0xee, 0xeb, 0xe5)
        };
        (fill, Some((palette.line, 1.0)), palette.text)
    } else {
        let fill = if palette.is_dark {
            Rgba::rgb(0x1f, 0x1e, 0x1c)
        } else {
            Rgba::rgb(0xf6, 0xf4, 0xef)
        };
        let fg = if enabled {
            palette.text
        } else {
            palette.muted.with_alpha(115)
        };
        (fill, Some((palette.line, 1.0)), fg)
    };

    fill_rounded(target, r, 6.0 * s, bg, border);
    let fmt = if primary_red || active {
        &fmts.label_center_bold
    } else {
        &fmts.label_center
    };
    draw_text(target, label, fmt, r, fg);
}

#[allow(clippy::too_many_arguments)]
fn draw_slider(
    target: &ID2D1RenderTarget,
    fmts: &EditFormats,
    s: f32,
    palette: Palette,
    r: Rect,
    label: &str,
    value: f32,
    active: bool,
) {
    let (tx0, tx1) = DockLayout::slider_track(r, s);
    let cy = r.y + r.h * 0.5;
    let track_h = (3.0 * s).max(2.0);

    draw_text(
        target,
        label,
        &fmts.label_left,
        Rect {
            x: r.x + 2.0 * s,
            y: r.y,
            w: (tx0 - r.x - 4.0 * s).max(16.0),
            h: r.h,
        },
        if active { palette.text } else { palette.muted },
    );

    // Track rail.
    let rail = if palette.is_dark {
        Rgba::rgb(0x30, 0x2e, 0x2b)
    } else {
        Rgba::rgb(0xdf, 0xdc, 0xd5)
    };
    fill_rounded(
        target,
        Rect {
            x: tx0,
            y: cy - track_h * 0.5,
            w: tx1 - tx0,
            h: track_h,
        },
        track_h * 0.5,
        rail,
        None,
    );

    // Center tick (0.0) and active fill.
    let mid_x = (tx0 + tx1) * 0.5;
    let t = ((value + 1.0) * 0.5).clamp(0.0, 1.0);
    let knob_x = tx0 + t * (tx1 - tx0);
    let fill_l = mid_x.min(knob_x);
    let fill_r = mid_x.max(knob_x);
    if fill_r - fill_l > 0.5 {
        fill_rounded(
            target,
            Rect {
                x: fill_l,
                y: cy - track_h * 0.5,
                w: fill_r - fill_l,
                h: track_h,
            },
            track_h * 0.5,
            palette.red,
            None,
        );
    }

    // Center notch.
    draw_line(
        target,
        (mid_x, cy - 4.0 * s),
        (mid_x, cy + 4.0 * s),
        palette.line,
        1.0,
    );

    // Thumb circle.
    let knob_r = if active { 6.0 * s } else { 5.0 * s };
    let thumb_color = if active || value.abs() > 0.01 {
        palette.red
    } else {
        palette.text
    };
    if let Some(brush) = make_brush(target, thumb_color) {
        let ell = D2D1_ELLIPSE {
            point: Vector2 { X: knob_x, Y: cy },
            radiusX: knob_r,
            radiusY: knob_r,
        };
        // SAFETY: Valid D2D target and brush inside BeginDraw..EndDraw.
        unsafe { target.FillEllipse(&ell, &brush) };
    }

    // Numeric readout (`+24`, `-12`, `0`).
    let pct = (value * 100.0).round() as i32;
    let val_str = if pct > 0 {
        format!("+{pct}")
    } else {
        format!("{pct}")
    };
    draw_text(
        target,
        &val_str,
        &fmts.value_right,
        Rect {
            x: tx1 + 2.0 * s,
            y: r.y,
            w: (r.x + r.w - tx1 - 4.0 * s).max(20.0),
            h: r.h,
        },
        if pct != 0 {
            palette.text
        } else {
            palette.muted
        },
    );
}

fn make_fmt(
    factory: &IDWriteFactory,
    family: windows::core::PCWSTR,
    weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT,
    size: f32,
    align: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
) -> windows::core::Result<IDWriteTextFormat> {
    // SAFETY: Valid DirectWrite factory and static UTF-16 family/locale names.
    let fmt = unsafe {
        factory.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!("en-US"),
        )
    }?;
    // SAFETY: Valid text format.
    unsafe {
        let _ = fmt.SetTextAlignment(align);
        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    }
    Ok(fmt)
}

fn make_brush(target: &ID2D1RenderTarget, color: Rgba) -> Option<ID2D1SolidColorBrush> {
    let c = rgba_to_d2d(color);
    // SAFETY: Valid D2D render target inside BeginDraw..EndDraw.
    unsafe { target.CreateSolidColorBrush(&c, None) }.ok()
}

fn fill_rect(target: &ID2D1RenderTarget, r: Rect, color: Rgba) {
    if let Some(brush) = make_brush(target, color) {
        let dr = D2D_RECT_F {
            left: r.x,
            top: r.y,
            right: r.x + r.w,
            bottom: r.y + r.h,
        };
        // SAFETY: Valid target and brush inside BeginDraw..EndDraw.
        unsafe { target.FillRectangle(&dr, &brush) };
    }
}

fn fill_rounded(
    target: &ID2D1RenderTarget,
    r: Rect,
    radius: f32,
    fill: Rgba,
    stroke: Option<(Rgba, f32)>,
) {
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: r.x,
            top: r.y,
            right: r.x + r.w,
            bottom: r.y + r.h,
        },
        radiusX: radius,
        radiusY: radius,
    };
    if let Some(brush) = make_brush(target, fill) {
        // SAFETY: Valid target and brush inside BeginDraw..EndDraw.
        unsafe { target.FillRoundedRectangle(&rr, &brush) };
    }
    if let Some((sc, sw)) = stroke
        && let Some(brush) = make_brush(target, sc)
    {
        // SAFETY: Valid target and brush inside BeginDraw..EndDraw.
        unsafe { target.DrawRoundedRectangle(&rr, &brush, sw, None) };
    }
}

fn draw_line(target: &ID2D1RenderTarget, p0: (f32, f32), p1: (f32, f32), color: Rgba, width: f32) {
    if let Some(brush) = make_brush(target, color) {
        // SAFETY: Valid target and brush inside BeginDraw..EndDraw.
        unsafe {
            target.DrawLine(
                Vector2 { X: p0.0, Y: p0.1 },
                Vector2 { X: p1.0, Y: p1.1 },
                &brush,
                width,
                None,
            );
        }
    }
}

fn draw_text(
    target: &ID2D1RenderTarget,
    text: &str,
    fmt: &IDWriteTextFormat,
    r: Rect,
    color: Rgba,
) {
    if text.is_empty() {
        return;
    }
    let Some(brush) = make_brush(target, color) else {
        return;
    };
    let wide: Vec<u16> = text.encode_utf16().collect();
    let dr = D2D_RECT_F {
        left: r.x,
        top: r.y,
        right: r.x + r.w,
        bottom: r.y + r.h,
    };
    // SAFETY: Valid target, format, brush, and `wide` UTF-16 slice inside BeginDraw..EndDraw.
    unsafe {
        target.DrawText(
            &wide,
            fmt,
            &dr,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}
