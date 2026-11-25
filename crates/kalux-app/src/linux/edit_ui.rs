//! Linux Wayland (`wl_shm`) Edit Mode UI renderer: crop overlay + floating ka-suite dock.
//!
//! Consumes the shared [`kalux_core::DockLayout`] geometry so visual rendering and pointer
//! hit-testing match pixel-for-pixel across Windows and Linux.

use kalux_core::edit::EditDrag;
use kalux_core::{
    CropHandle, DockButton, DockLayout, EditPrompt, EditSession, EditTab, Palette, Rect, Rgba,
};

use crate::linux::overlay::LinuxOverlay;
use crate::linux::render::Canvas;

/// Draw the crop frame (dimmed scrim, border, rule-of-thirds, corner L-brackets, dimension pill)
/// and the floating bottom Edit Dock + optional inline Save/Exit prompt.
#[allow(clippy::too_many_arguments)]
pub fn draw_edit_ui(
    canvas: &mut Canvas<'_>,
    overlay: &mut LinuxOverlay,
    scale: f32,
    palette: Palette,
    image_dest: Rect,
    source_dims: (u32, u32),
    session: &EditSession,
    layout: &DockLayout,
) {
    let s = scale.max(1.0);
    let recipe = session.recipe();

    // 1. Crop frame overlay (hidden while holding `\` compare to show clean original).
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
            canvas,
            overlay,
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
        let bx = (canvas.width as f32 - badge_w) * 0.5;
        let by = 14.0 * s;
        canvas.fill_rounded_rect(
            bx,
            by,
            bx + badge_w,
            by + badge_h,
            13.0 * s,
            palette.panel.with_alpha(235),
            Some((palette.red, 1.0 * s)),
        );
        overlay.draw_text_centered(
            canvas,
            "Original",
            bx + badge_w * 0.5,
            by + 6.0 * s,
            12.0 * s,
            palette.text,
            true,
            false,
        );
    }

    // Animated source static-frame notice (top-left pill).
    if session.is_animated_source {
        let msg = format!(
            "Static frame {} / {} · saves as copy",
            session.frame_index + 1,
            session.total_frames
        );
        let tw = overlay.measure_text_line(&msg, 11.5 * s, false, false);
        let pw = tw + 22.0 * s;
        let ph = 26.0 * s;
        let px = 14.0 * s;
        let py = 14.0 * s;
        canvas.fill_rounded_rect(
            px,
            py,
            px + pw,
            py + ph,
            13.0 * s,
            palette.panel.with_alpha(235),
            Some((palette.line, 1.0 * s)),
        );
        overlay.draw_text_line(
            canvas,
            &msg,
            px + 11.0 * s,
            py + 6.0 * s,
            11.5 * s,
            palette.muted,
            false,
            false,
        );
    }

    // 2. Floating bottom Edit Dock.
    draw_dock(canvas, overlay, s, palette, session, layout);
}

#[allow(clippy::too_many_arguments)]
fn draw_crop_overlay(
    canvas: &mut Canvas<'_>,
    overlay: &mut LinuxOverlay,
    s: f32,
    palette: Palette,
    img_rect: Rect,
    crop: Rect,
    out_px: kalux_core::PixelRect,
    active_handle: Option<CropHandle>,
    show_thirds: bool,
    show_handles: bool,
) {
    let il = img_rect.x.round() as i32;
    let it = img_rect.y.round() as i32;
    let ir = (img_rect.x + img_rect.w).round() as i32;
    let ib = (img_rect.y + img_rect.h).round() as i32;

    let cl = crop.x.round() as i32;
    let ct = crop.y.round() as i32;
    let cr = (crop.x + crop.w).round() as i32;
    let cb = (crop.y + crop.h).round() as i32;

    // Dim outside the crop rect using ka-suite scrim.
    let scrim = Rgba::rgb(0, 0, 0).with_alpha(if palette.is_dark { 155 } else { 115 });
    canvas.fill_rect(il, it, ir, ct, scrim);
    canvas.fill_rect(il, cb, ir, ib, scrim);
    canvas.fill_rect(il, ct, cl, cb, scrim);
    canvas.fill_rect(cr, ct, ir, cb, scrim);

    if !show_handles {
        return;
    }

    // Rule-of-thirds hairlines while actively dragging.
    if show_thirds && (cr - cl) > 24 && (cb - ct) > 24 {
        let grid_col = Rgba::rgb(242, 241, 238).with_alpha(110);
        for k in 1..3 {
            let gx = cl + ((cr - cl) * k) / 3;
            let gy = ct + ((cb - ct) * k) / 3;
            canvas.fill_rect(gx, ct, gx + 1, cb, grid_col);
            canvas.fill_rect(cl, gy, cr, gy + 1, grid_col);
        }
    }

    // Crisp 1px border around crop rectangle.
    let border_col = Rgba::rgb(242, 241, 238).with_alpha(230);
    let bw = (1.0 * s).round().max(1.0) as i32;
    canvas.fill_rect(cl, ct, cr, ct + bw, border_col);
    canvas.fill_rect(cl, cb - bw, cr, cb, border_col);
    canvas.fill_rect(cl, ct, cl + bw, cb, border_col);
    canvas.fill_rect(cr - bw, ct, cr, cb, border_col);

    // 4 Corner L-brackets + 4 edge midpoint bars.
    let arm = (16.0 * s).round().min((cr - cl) as f32 * 0.35).max(6.0) as i32;
    let thick = (3.0 * s).round().max(2.0) as i32;
    let handle_col = |h: CropHandle| -> Rgba {
        if active_handle == Some(h) {
            palette.red
        } else {
            Rgba::rgb(245, 244, 240)
        }
    };

    // NW
    let c = handle_col(CropHandle::Nw);
    canvas.fill_rect(cl - 1, ct - 1, cl + arm, ct - 1 + thick, c);
    canvas.fill_rect(cl - 1, ct - 1, cl - 1 + thick, ct + arm, c);
    // NE
    let c = handle_col(CropHandle::Ne);
    canvas.fill_rect(cr - arm, ct - 1, cr + 1, ct - 1 + thick, c);
    canvas.fill_rect(cr + 1 - thick, ct - 1, cr + 1, ct + arm, c);
    // SW
    let c = handle_col(CropHandle::Sw);
    canvas.fill_rect(cl - 1, cb + 1 - thick, cl + arm, cb + 1, c);
    canvas.fill_rect(cl - 1, cb - arm, cl - 1 + thick, cb + 1, c);
    // SE
    let c = handle_col(CropHandle::Se);
    canvas.fill_rect(cr - arm, cb + 1 - thick, cr + 1, cb + 1, c);
    canvas.fill_rect(cr + 1 - thick, cb - arm, cr + 1, cb + 1, c);

    // Edge midpoint bars
    let mid_x = (cl + cr) / 2;
    let mid_y = (ct + cb) / 2;
    let bar = (10.0 * s).round().min((cr - cl) as f32 * 0.22).max(4.0) as i32;
    canvas.fill_rect(
        mid_x - bar,
        ct - 1,
        mid_x + bar,
        ct - 1 + thick,
        handle_col(CropHandle::N),
    );
    canvas.fill_rect(
        mid_x - bar,
        cb + 1 - thick,
        mid_x + bar,
        cb + 1,
        handle_col(CropHandle::S),
    );
    canvas.fill_rect(
        cl - 1,
        mid_y - bar,
        cl - 1 + thick,
        mid_y + bar,
        handle_col(CropHandle::W),
    );
    canvas.fill_rect(
        cr + 1 - thick,
        mid_y - bar,
        cr + 1,
        mid_y + bar,
        handle_col(CropHandle::E),
    );

    // Live full-resolution source dimension badge at top-left of crop frame.
    let dim_label = format!("{} × {}", out_px.w, out_px.h);
    let tw = overlay.measure_text_line(&dim_label, 11.5 * s, false, true);
    let bw_px = tw + 16.0 * s;
    let bh_px = 22.0 * s;
    let bx = (crop.x + 8.0 * s)
        .min(canvas.width as f32 - bw_px - 8.0 * s)
        .max(8.0 * s);
    let by = if crop.y >= bh_px + 10.0 * s {
        crop.y - bh_px - 6.0 * s
    } else {
        crop.y + 8.0 * s
    };
    canvas.fill_rounded_rect(
        bx,
        by,
        bx + bw_px,
        by + bh_px,
        6.0 * s,
        palette.panel.with_alpha(232),
        Some((palette.line, 1.0 * s)),
    );
    overlay.draw_text_centered(
        canvas,
        &dim_label,
        bx + bw_px * 0.5,
        by + 4.5 * s,
        11.5 * s,
        palette.text,
        false,
        true,
    );
}

fn draw_dock(
    canvas: &mut Canvas<'_>,
    overlay: &mut LinuxOverlay,
    s: f32,
    palette: Palette,
    session: &EditSession,
    layout: &DockLayout,
) {
    let p = layout.panel;
    // Main floating dock container.
    canvas.fill_rounded_rect(
        p.x,
        p.y,
        p.x + p.w,
        p.y + p.h,
        11.0 * s,
        palette.panel.with_alpha(244),
        Some((palette.line, 1.0 * s)),
    );

    // Optional inline prompt bar floating above the dock.
    if let Some(pr) = layout.prompt_panel {
        canvas.fill_rounded_rect(
            pr.x,
            pr.y,
            pr.x + pr.w,
            pr.y + pr.h,
            10.0 * s,
            palette.panel.with_alpha(248),
            Some((palette.red, 1.0 * s)),
        );
        let msg = match session.prompt {
            EditPrompt::SaveChoice => "Save edits?",
            EditPrompt::UnsavedExit => "Unsaved edits",
            EditPrompt::None => "",
        };
        overlay.draw_text_line(
            canvas,
            msg,
            pr.x + 12.0 * s,
            pr.y + 11.5 * s,
            12.0 * s,
            palette.text,
            true,
            false,
        );
    }

    let recipe = session.recipe();

    for item in &layout.items {
        let hovered = session.hovered_button == Some(item.button);
        let r = item.rect;
        match item.button {
            DockButton::TabCrop => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Crop",
                    session.tab == EditTab::Crop,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::TabAdjust => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Adjust",
                    session.tab == EditTab::Adjust,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::Aspect(preset) => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    preset.label(session.aspect_swapped),
                    session.aspect == preset,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::SwapAspect => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::SwapAspect,
                    false,
                    hovered,
                    true,
                );
            }
            DockButton::RotateCcw => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::RotateCcw,
                    false,
                    hovered,
                    true,
                );
            }
            DockButton::RotateCw => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::RotateCw,
                    false,
                    hovered,
                    true,
                );
            }
            DockButton::FlipH => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::FlipH,
                    recipe.flip_h,
                    hovered,
                    true,
                );
            }
            DockButton::FlipV => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::FlipV,
                    recipe.flip_v,
                    hovered,
                    true,
                );
            }
            DockButton::Slider(slider) => {
                draw_slider(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
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
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Mono (M)",
                    recipe.tone.monochrome,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::Reset => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Reset",
                    false,
                    hovered,
                    false,
                    session.is_dirty(),
                );
            }
            DockButton::Compare => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "\\",
                    session.comparing_original,
                    hovered,
                    false,
                    session.is_dirty(),
                );
            }
            DockButton::Undo => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::Undo,
                    false,
                    hovered,
                    session.history.can_undo(),
                );
            }
            DockButton::Redo => {
                draw_icon_chip(
                    canvas,
                    s,
                    palette,
                    r,
                    DockIcon::Redo,
                    false,
                    hovered,
                    session.history.can_redo(),
                );
            }
            DockButton::QualityCycle => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    session.quality.label(),
                    false,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::CopyEdited => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Copy",
                    false,
                    hovered,
                    false,
                    !session.exporting,
                );
            }
            DockButton::SaveOverwrite => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Save",
                    false,
                    hovered,
                    false,
                    session.is_dirty() && !session.exporting,
                );
            }
            DockButton::SaveCopy => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    if session.exporting {
                        "Saving..."
                    } else {
                        "Save Copy"
                    },
                    false,
                    hovered,
                    true,
                    !session.exporting,
                );
            }
            DockButton::Done => {
                draw_chip(
                    canvas, overlay, s, palette, r, "Done", false, hovered, false, true,
                );
            }
            DockButton::PromptSaveCopy => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Save Copy",
                    false,
                    hovered,
                    true,
                    true,
                );
            }
            DockButton::PromptOverwrite => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Overwrite (O)",
                    false,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::PromptDiscard => {
                draw_chip(
                    canvas,
                    overlay,
                    s,
                    palette,
                    r,
                    "Discard (D)",
                    false,
                    hovered,
                    false,
                    true,
                );
            }
            DockButton::PromptCancel => {
                draw_chip(
                    canvas, overlay, s, palette, r, "Cancel", false, hovered, false, true,
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
    canvas: &mut Canvas<'_>,
    s: f32,
    palette: Palette,
    r: Rect,
    icon: DockIcon,
    active: bool,
    hovered: bool,
    enabled: bool,
) {
    let (fill, border, fg) = chip_colors(palette, s, active, hovered, false, enabled);
    canvas.fill_rounded_rect(r.x, r.y, r.x + r.w, r.y + r.h, 6.0 * s, fill, border);

    let cx = r.x + r.w * 0.5;
    let cy = r.y + r.h * 0.5;
    let sw = 1.45 * s;

    let seg = |canvas: &mut Canvas<'_>, x0: f32, y0: f32, x1: f32, y1: f32| {
        draw_aa_segment(
            canvas,
            cx + x0 * s,
            cy + y0 * s,
            cx + x1 * s,
            cy + y1 * s,
            sw,
            fg,
        );
    };

    match icon {
        DockIcon::SwapAspect => {
            // Opposing horizontal arrows (swap width <-> height).
            seg(canvas, -4.5, -2.5, 4.5, -2.5);
            seg(canvas, 2.0, -5.0, 4.5, -2.5);
            seg(canvas, 2.0, 0.0, 4.5, -2.5);
            seg(canvas, 4.5, 2.5, -4.5, 2.5);
            seg(canvas, -2.0, 0.0, -4.5, 2.5);
            seg(canvas, -2.0, 5.0, -4.5, 2.5);
        }
        DockIcon::RotateCcw => {
            // Open counter-clockwise 270-deg loop with top-left arrowhead.
            seg(canvas, -4.0, -3.5, 3.5, -3.5);
            seg(canvas, 3.5, -3.5, 3.5, 3.5);
            seg(canvas, 3.5, 3.5, -3.5, 3.5);
            seg(canvas, -3.5, 3.5, -3.5, 0.8);
            seg(canvas, -4.2, -3.5, -1.2, -6.0);
            seg(canvas, -4.2, -3.5, -1.2, -1.0);
        }
        DockIcon::RotateCw => {
            // Open clockwise 270-deg loop with top-right arrowhead.
            seg(canvas, 4.0, -3.5, -3.5, -3.5);
            seg(canvas, -3.5, -3.5, -3.5, 3.5);
            seg(canvas, -3.5, 3.5, 3.5, 3.5);
            seg(canvas, 3.5, 3.5, 3.5, 0.8);
            seg(canvas, 4.2, -3.5, 1.2, -6.0);
            seg(canvas, 4.2, -3.5, 1.2, -1.0);
        }
        DockIcon::FlipH => {
            // Vertical mirror axis + left/right mirror wedges.
            seg(canvas, 0.0, -5.0, 0.0, 5.0);
            seg(canvas, -2.2, -3.5, -5.5, 0.0);
            seg(canvas, -5.5, 0.0, -2.2, 3.5);
            seg(canvas, 2.2, -3.5, 5.5, 0.0);
            seg(canvas, 5.5, 0.0, 2.2, 3.5);
        }
        DockIcon::FlipV => {
            // Horizontal mirror axis + top/bottom mirror wedges.
            seg(canvas, -5.0, 0.0, 5.0, 0.0);
            seg(canvas, -3.5, -2.2, 0.0, -5.5);
            seg(canvas, 0.0, -5.5, 3.5, -2.2);
            seg(canvas, -3.5, 2.2, 0.0, 5.5);
            seg(canvas, 0.0, 5.5, 3.5, 2.2);
        }
        DockIcon::Undo => {
            // Left U-turn undo arrow.
            seg(canvas, -4.5, -1.5, 2.2, -1.5);
            seg(canvas, 2.2, -1.5, 4.2, 0.5);
            seg(canvas, 4.2, 0.5, 4.2, 2.0);
            seg(canvas, 4.2, 2.0, 2.2, 4.0);
            seg(canvas, 2.2, 4.0, -1.5, 4.0);
            seg(canvas, -4.5, -1.5, -1.8, -4.2);
            seg(canvas, -4.5, -1.5, -1.8, 1.2);
        }
        DockIcon::Redo => {
            // Right U-turn redo arrow.
            seg(canvas, 4.5, -1.5, -2.2, -1.5);
            seg(canvas, -2.2, -1.5, -4.2, 0.5);
            seg(canvas, -4.2, 0.5, -4.2, 2.0);
            seg(canvas, -4.2, 2.0, -2.2, 4.0);
            seg(canvas, -2.2, 4.0, 1.5, 4.0);
            seg(canvas, 4.5, -1.5, 1.8, -4.2);
            seg(canvas, 4.5, -1.5, 1.8, 1.2);
        }
    }
}

fn draw_aa_segment(
    canvas: &mut Canvas<'_>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    width: f32,
    color: Rgba,
) {
    let half_w = (width * 0.5).max(0.5);
    let pad = half_w + 1.5;
    let min_x = (x0.min(x1) - pad).floor() as i32;
    let max_x = (x0.max(x1) + pad).ceil() as i32;
    let min_y = (y0.min(y1) - pad).floor() as i32;
    let max_y = (y0.max(y1) + pad).ceil() as i32;

    let vx = x1 - x0;
    let vy = y1 - y0;
    let len_sq = vx * vx + vy * vy;

    for y in min_y..=max_y {
        let py = y as f32 + 0.5;
        for x in min_x..=max_x {
            let px = x as f32 + 0.5;
            let t = if len_sq > 1e-6 {
                (((px - x0) * vx + (py - y0) * vy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let dx = px - (x0 + vx * t);
            let dy = py - (y0 + vy * t);
            let dist = (dx * dx + dy * dy).sqrt() - half_w;
            let cov = (0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.01 {
                canvas.blend_pixel(x, y, color, cov);
            }
        }
    }
}

fn chip_colors(
    palette: Palette,
    s: f32,
    active: bool,
    hovered: bool,
    primary_red: bool,
    enabled: bool,
) -> (Rgba, Option<(Rgba, f32)>, Rgba) {
    if primary_red {
        let bg = if !enabled {
            palette.surface
        } else if hovered {
            Rgba::rgb(0xF0, 0x35, 0x3C)
        } else {
            palette.red
        };
        let fg = if enabled {
            Rgba::rgb(0xFF, 0xFF, 0xFF)
        } else {
            palette.faint
        };
        (bg, None, fg)
    } else if active {
        (
            palette.surface_2,
            Some((palette.red, 1.0 * s)),
            palette.text,
        )
    } else if hovered && enabled {
        (
            palette.surface_2,
            Some((palette.line_strong, 1.0 * s)),
            palette.text,
        )
    } else {
        (
            palette.surface,
            Some((palette.line, 1.0 * s)),
            if enabled { palette.text } else { palette.faint },
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_chip(
    canvas: &mut Canvas<'_>,
    overlay: &mut LinuxOverlay,
    s: f32,
    palette: Palette,
    r: Rect,
    label: &str,
    active: bool,
    hovered: bool,
    primary_red: bool,
    enabled: bool,
) {
    let (fill, border, text_col) = chip_colors(palette, s, active, hovered, primary_red, enabled);

    canvas.fill_rounded_rect(r.x, r.y, r.x + r.w, r.y + r.h, 6.0 * s, fill, border);

    let font_sz = 11.5 * s;
    let y_text = r.y + (r.h - font_sz) * 0.5 - 0.5 * s;
    overlay.draw_text_centered(
        canvas,
        label,
        r.x + r.w * 0.5,
        y_text,
        font_sz,
        text_col,
        primary_red || active,
        false,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_slider(
    canvas: &mut Canvas<'_>,
    overlay: &mut LinuxOverlay,
    s: f32,
    palette: Palette,
    r: Rect,
    label: &str,
    value: f32,
    active: bool,
) {
    let font_sz = 11.0 * s;
    let y_text = r.y + (r.h - font_sz) * 0.5 - 0.5 * s;
    overlay.draw_text_line(
        canvas,
        label,
        r.x + 2.0 * s,
        y_text,
        font_sz,
        if active { palette.text } else { palette.muted },
        active,
        false,
    );

    let (x0, x1) = DockLayout::slider_track(r, s);
    let cy = r.y + r.h * 0.5;
    let track_h = 3.0 * s;
    canvas.fill_rounded_rect(
        x0,
        cy - track_h * 0.5,
        x1,
        cy + track_h * 0.5,
        1.5 * s,
        palette.surface_2,
        None,
    );

    // Center zero tick.
    let mid_x = (x0 + x1) * 0.5;
    canvas.fill_rect(
        mid_x.round() as i32,
        (cy - 4.0 * s).round() as i32,
        mid_x.round() as i32 + 1,
        (cy + 4.0 * s).round() as i32,
        palette.line_strong,
    );

    // Filled span from center to current value.
    let t = (value.clamp(-1.0, 1.0) + 1.0) * 0.5;
    let knob_x = x0 + (x1 - x0) * t;
    let span_l = mid_x.min(knob_x);
    let span_r = mid_x.max(knob_x);
    if span_r - span_l > 1.0 {
        canvas.fill_rounded_rect(
            span_l,
            cy - track_h * 0.5,
            span_r,
            cy + track_h * 0.5,
            1.5 * s,
            if active { palette.red } else { palette.muted },
            None,
        );
    }

    // Thumb circle.
    let thumb_col = if active || value.abs() > 1e-3 {
        palette.red
    } else {
        palette.text
    };
    canvas.fill_circle(knob_x, cy, 5.5 * s, thumb_col);

    // Numeric readout on the right.
    let pct = (value * 100.0).round() as i32;
    let val_str = if pct > 0 {
        format!("+{pct}")
    } else {
        format!("{pct}")
    };
    overlay.draw_text_centered(
        canvas,
        &val_str,
        r.x + r.w - 15.0 * s,
        y_text,
        10.5 * s,
        if pct != 0 {
            palette.text
        } else {
            palette.faint
        },
        false,
        true,
    );
}
