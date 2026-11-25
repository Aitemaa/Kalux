//! Offscreen PNG screenshot renderer (`--screenshot`) for Linux.
//!
//! Uses the exact same `kalux_decode`, `kalux_core::View`, `kalux_core::Palette`,
//! `Canvas`, and `LinuxOverlay` pipeline as the live Wayland window.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

use kalux_core::edit::edit_image_dest_rect;
use kalux_core::{
    AspectPreset, CropRect, DockLayout, EditRecipe, EditSession, EditTab, Palette, ToneAdjustments,
    View,
};
use kalux_decode::decode_file;

use crate::args::ScreenshotEditMode;
use crate::linux::edit_ui::draw_edit_ui;
use crate::linux::overlay::{LinuxOverlay, OverlayView};
use crate::linux::render::{Canvas, draw_hline, draw_mini_icon};

const TITLEBAR_H: u32 = 36;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotOptions {
    pub image: PathBuf,
    pub out: PathBuf,
    pub light: bool,
    pub show_help: bool,
    pub edit_mode: ScreenshotEditMode,
    pub toast: Option<String>,
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
}

pub fn render_screenshot(opts: &ScreenshotOptions) -> Result<(), String> {
    let w = opts.width.max(320);
    let h = opts.height.max(240);
    let client_h = h.saturating_sub(TITLEBAR_H).max(1);
    let palette = if opts.light {
        Palette::LIGHT
    } else {
        Palette::DARK
    };

    let decoded = decode_file(&opts.image, Some((w, client_h)))
        .map_err(|e| format!("decode {}: {e}", opts.image.display()))?;

    let mut full_pixels = vec![0u8; (w as usize) * (h as usize) * 4];

    // 1. Render client viewport (below the 36px chrome bar)
    let client_offset = (TITLEBAR_H as usize) * (w as usize) * 4;
    let mut overlay = LinuxOverlay::new();
    {
        let mut client_canvas = Canvas::new(w, client_h, &mut full_pixels[client_offset..]);
        client_canvas.clear(palette.bg);

        let src_sz = (decoded.source_width, decoded.source_height);
        match opts.edit_mode {
            ScreenshotEditMode::None => {
                let view = View::fit(src_sz, (w, client_h));
                let dest = view.dest_rect(src_sz, (w, client_h));
                client_canvas.draw_image(&decoded, 0, dest, view.scale);
            }
            ScreenshotEditMode::Crop | ScreenshotEditMode::Adjust => {
                let tab = if opts.edit_mode == ScreenshotEditMode::Adjust {
                    EditTab::Adjust
                } else {
                    EditTab::Crop
                };
                let mut session = EditSession::new(tab, false, 0, 1);
                if opts.edit_mode == ScreenshotEditMode::Crop {
                    session.aspect = AspectPreset::ThreeTwo;
                    session.history.push(EditRecipe {
                        crop: CropRect {
                            x: 0.14,
                            y: 0.12,
                            w: 0.72,
                            h: 0.74,
                        },
                        ..EditRecipe::default()
                    });
                } else {
                    session.history.push(EditRecipe {
                        tone: ToneAdjustments {
                            exposure: 0.18,
                            contrast: 0.24,
                            saturation: 0.16,
                            warmth: 0.22,
                            monochrome: false,
                        },
                        ..EditRecipe::default()
                    });
                }
                let layout = DockLayout::compute(w, client_h, 1.0, &session);
                let recipe = session.recipe();
                let oriented = recipe.oriented_dims(src_sz.0, src_sz.1);
                let (dest, img_scale) =
                    edit_image_dest_rect(oriented, (w, client_h), layout.reserved_bottom_px, 1.0);
                let lut = session.preview_lut();
                client_canvas.draw_image_edited(
                    &decoded,
                    0,
                    dest,
                    img_scale,
                    recipe.turns,
                    recipe.flip_h,
                    recipe.flip_v,
                    &lut,
                );
                draw_edit_ui(
                    &mut client_canvas,
                    &mut overlay,
                    1.0,
                    palette,
                    dest,
                    src_sz,
                    &session,
                    &layout,
                );
            }
        }

        let ov = OverlayView {
            empty_hint: false,
            error: None,
            loading_label: None,
            toast: opts.toast.as_deref(),
            show_help: opts.show_help,
        };
        overlay.draw(&mut client_canvas, 1.0, palette, &ov);
    }

    // 2. Render titlebar strip at top matching the ka-suite chrome
    {
        let mut canvas = Canvas::new(w, h, &mut full_pixels);
        canvas.fill_rect(0, 0, w as i32, TITLEBAR_H as i32, palette.panel);
        draw_hline(
            &mut canvas,
            0,
            w as i32,
            (TITLEBAR_H - 1) as i32,
            palette.line,
        );
        draw_mini_icon(&mut canvas, 12.0, 9.0, 18.0, palette);

        let title = opts.title.clone().unwrap_or_else(|| {
            let name = opts
                .image
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "image".to_string());
            let suffix = match opts.edit_mode {
                ScreenshotEditMode::None => String::new(),
                ScreenshotEditMode::Crop | ScreenshotEditMode::Adjust => " — Editing".to_string(),
            };
            format!(
                "{name} — 1 / 1 — {}×{}{suffix} — Kalux",
                decoded.source_width, decoded.source_height
            )
        });
        overlay.draw_text_line(
            &mut canvas,
            &title,
            40.0,
            11.0,
            13.0,
            palette.text,
            false,
            false,
        );
    }

    // 3. Convert BGRA8 -> RGBA8 and write PNG
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for (src, dst) in full_pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgba.as_chunks_mut::<4>().0.iter_mut())
    {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = 255;
    }

    if let Some(parent) = opts.out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| format!("create output directory: {e}"))?;
    }

    let file =
        File::create(&opts.out).map_err(|e| format!("create {}: {e}", opts.out.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("PNG header: {e}"))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| format!("PNG write: {e}"))
}
