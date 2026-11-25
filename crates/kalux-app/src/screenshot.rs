//! Offscreen Direct2D + DirectWrite screenshot renderer (`--screenshot`).
//!
//! Uses the exact same `kalux_decode`, `kalux_core::View`, `kalux_core::Palette`, and
//! `OverlayRenderer` pipeline as the live window, writing a PNG via WIC so README screenshots
//! are rendered directly by Kalux's own code.

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use kalux_core::edit::edit_image_dest_rect;
use kalux_core::{
    AspectPreset, CropRect, DockLayout, EditRecipe, EditSession, EditTab, Palette, ToneAdjustments,
    View,
};
use kalux_decode::decode_file;
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppPBGRA,
    IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapCacheOnDemand, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, STGC_DEFAULT};
use windows::core::{Interface, PCWSTR, w};
use windows_numerics::{Matrix3x2, Vector2};

use crate::args::ScreenshotEditMode;
use crate::edit_ui::EditUiRenderer;
use crate::overlay::{OverlayRenderer, OverlayView, rgba_to_d2d};
use crate::render::{choose_interpolation, create_d2d_bitmap};

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

/// Render an offscreen screenshot of Kalux showing `opts.image` to `opts.out` (PNG).
///
/// Requires COM to be initialized on the calling thread.
pub fn render_screenshot(opts: &ScreenshotOptions) -> Result<(), String> {
    let w = opts.width.max(320);
    let h = opts.height.max(240);
    let client_h = h.saturating_sub(TITLEBAR_H).max(1);
    let palette = Palette::for_light(opts.light);

    let decoded = decode_file(&opts.image, Some((w, client_h)))
        .map_err(|e| format!("Failed to decode '{}': {e}", opts.image.display()))?;

    // SAFETY: COM is initialized by caller; creating standard in-proc WIC factory.
    let wic: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("Failed to create WIC factory: {e}"))?;

    // SAFETY: Valid WIC factory and non-zero dimensions.
    let wic_bmp =
        unsafe { wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand) }
            .map_err(|e| format!("Failed to create WIC target bitmap: {e}"))?;

    // SAFETY: Single-threaded D2D factory on calling thread.
    let d2d: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }
        .map_err(|e| format!("Failed to create D2D factory: {e}"))?;

    let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        ..Default::default()
    };

    // SAFETY: `wic_bmp` is `32bppPBGRA`, matching `DXGI_FORMAT_B8G8R8A8_UNORM` + `PREMULTIPLIED`.
    let target: ID2D1RenderTarget = unsafe { d2d.CreateWicBitmapRenderTarget(&wic_bmp, &rt_props) }
        .map_err(|e| format!("Failed to create WIC D2D render target: {e}"))?;

    let frame = decoded
        .frames
        .first()
        .ok_or_else(|| "Decoded image has no frames".to_string())?;
    let source_dims = (decoded.source_width, decoded.source_height);
    let client_dims = (w, client_h);

    let edit_state = match opts.edit_mode {
        ScreenshotEditMode::None => None,
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
            let oriented = session.recipe().oriented_dims(source_dims.0, source_dims.1);
            let (dest, img_scale) =
                edit_image_dest_rect(oriented, client_dims, layout.reserved_bottom_px, 1.0);
            Some((session, layout, dest, img_scale))
        }
    };

    let toned_pixels: Option<Vec<u8>> = edit_state.as_ref().and_then(|(session, _, _, _)| {
        let lut = session.preview_lut();
        if lut.is_identity() {
            None
        } else {
            let mut buf = frame.pixels.clone();
            lut.apply_bgra_premul_slice(&mut buf);
            Some(buf)
        }
    });
    let upload_pixels = toned_pixels.as_deref().unwrap_or(&frame.pixels);
    let pitch = decoded.width.saturating_mul(4);
    let d2d_bmp = create_d2d_bitmap(&target, decoded.width, decoded.height, pitch, upload_pixels)
        .map_err(|e| format!("Failed to create D2D bitmap: {e}"))?;

    let (dest, img_scale) = if let Some((_, _, d, s)) = edit_state.as_ref() {
        (*d, *s)
    } else {
        let view = View::fit(source_dims, client_dims);
        (view.dest_rect(source_dims, client_dims), view.scale)
    };

    let mut overlays = OverlayRenderer::default();
    let mut edit_ui = EditUiRenderer::default();
    let overlay_view = OverlayView {
        empty_hint: false,
        error: None,
        loading_label: None,
        toast: opts.toast.as_deref(),
        show_help: opts.show_help,
    };

    let bg = rgba_to_d2d(palette.bg);
    let dc = target.cast::<ID2D1DeviceContext>().ok();

    let title_text = opts.title.clone().unwrap_or_else(|| {
        let name = opts
            .image
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| opts.image.to_string_lossy().into_owned());
        let suffix = match opts.edit_mode {
            ScreenshotEditMode::None => "",
            ScreenshotEditMode::Crop | ScreenshotEditMode::Adjust => " — Editing",
        };
        format!(
            "{name} — 1 / 12 — {}×{}{suffix} — Kalux",
            decoded.source_width, decoded.source_height
        )
    });

    // SAFETY: Valid D2D render target; paired with `EndDraw` below.
    unsafe { target.BeginDraw() };

    // SAFETY: Inside BeginDraw..EndDraw.
    unsafe { target.Clear(Some(&bg)) };

    // Translate into the client area beneath the 36 px title bar.
    let client_xf = Matrix3x2::translation(0.0, TITLEBAR_H as f32);
    // SAFETY: Inside BeginDraw..EndDraw.
    unsafe { target.SetTransform(&client_xf) };

    let d2d_rect = D2D_RECT_F {
        left: dest.x,
        top: dest.y,
        right: dest.x + dest.w,
        bottom: dest.y + dest.h,
    };
    if let Some(dc_ref) = dc.as_ref() {
        let mode = choose_interpolation(dest, (decoded.width, decoded.height), img_scale);
        // SAFETY: `dc_ref` is the same target via QI inside BeginDraw..EndDraw.
        unsafe { dc_ref.DrawBitmap(&d2d_bmp, Some(&d2d_rect), 1.0, mode, None, None) };
    } else {
        // SAFETY: Inside BeginDraw..EndDraw with a bitmap created from this target.
        unsafe {
            target.DrawBitmap(
                &d2d_bmp,
                Some(&d2d_rect),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    if let Some((ref session, ref layout, edit_dest, _)) = edit_state {
        edit_ui.draw(
            &target,
            client_dims,
            1.0,
            palette,
            edit_dest,
            source_dims,
            session,
            layout,
        );
    }

    overlays.draw(&target, client_dims, 96, palette, &overlay_view);

    // Reset transform and paint the matching native-style title bar + crisp 1 px window frame.
    let identity = Matrix3x2::identity();
    // SAFETY: Inside BeginDraw..EndDraw.
    unsafe { target.SetTransform(&identity) };
    draw_window_chrome(&target, w as f32, h as f32, palette, &title_text);

    // SAFETY: Pairs with `BeginDraw` above.
    unsafe { target.EndDraw(None, None) }.map_err(|e| format!("D2D EndDraw failed: {e}"))?;

    save_wic_bitmap_as_png(&wic, &wic_bmp, &opts.out)
}

fn draw_window_chrome(target: &ID2D1RenderTarget, w: f32, h: f32, palette: Palette, title: &str) {
    let make = |rgba| {
        let c = rgba_to_d2d(rgba);
        // SAFETY: Valid D2D render target inside BeginDraw..EndDraw.
        unsafe { target.CreateSolidColorBrush(&c, None) }.ok()
    };

    let Some(bar_brush) = make(palette.panel) else {
        return;
    };
    let Some(line_brush) = make(palette.line) else {
        return;
    };
    let Some(text_brush) = make(palette.text) else {
        return;
    };
    let Some(muted_brush) = make(palette.muted) else {
        return;
    };
    let Some(red_brush) = make(palette.red) else {
        return;
    };
    let Some(tile_brush) = make(kalux_core::Rgba::rgb(0x22, 0x22, 0x22)) else {
        return;
    };
    let Some(white_brush) = make(kalux_core::Rgba::rgb(0xff, 0xff, 0xff)) else {
        return;
    };

    let bar_h = TITLEBAR_H as f32;
    let bar_rect = D2D_RECT_F {
        left: 0.0,
        top: 0.0,
        right: w,
        bottom: bar_h,
    };

    // SAFETY: All brushes and target are valid inside BeginDraw..EndDraw.
    unsafe {
        target.FillRectangle(&bar_rect, &bar_brush);
        target.DrawLine(
            Vector2 {
                X: 0.0,
                Y: bar_h - 0.5,
            },
            Vector2 {
                X: w,
                Y: bar_h - 0.5,
            },
            &line_brush,
            1.0,
            None,
        );

        // Mini Kalux icon (18x18 rounded dark tile + white split K + red glint).
        let icon_rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 12.0,
                top: 9.0,
                right: 30.0,
                bottom: 27.0,
            },
            radiusX: 4.0,
            radiusY: 4.0,
        };
        target.FillRoundedRectangle(&icon_rr, &tile_brush);
        target.DrawLine(
            Vector2 { X: 17.4, Y: 13.4 },
            Vector2 { X: 17.4, Y: 22.6 },
            &white_brush,
            1.6,
            None,
        );
        target.DrawLine(
            Vector2 { X: 20.4, Y: 16.9 },
            Vector2 { X: 24.4, Y: 13.4 },
            &white_brush,
            1.6,
            None,
        );
        target.DrawLine(
            Vector2 { X: 20.4, Y: 19.1 },
            Vector2 { X: 24.4, Y: 22.6 },
            &white_brush,
            1.6,
            None,
        );
        let glint = D2D1_ELLIPSE {
            point: Vector2 { X: 19.2, Y: 18.0 },
            radiusX: 1.25,
            radiusY: 1.25,
        };
        target.FillEllipse(&glint, &red_brush);

        // Windows 11 style caption glyphs on the right: minimize, maximize, close.
        let cy = bar_h * 0.5;
        let min_x = w - 114.0;
        target.DrawLine(
            Vector2 { X: min_x, Y: cy },
            Vector2 {
                X: min_x + 10.0,
                Y: cy,
            },
            &muted_brush,
            1.0,
            None,
        );
        let max_x = w - 70.0;
        target.DrawRectangle(
            &D2D_RECT_F {
                left: max_x,
                top: cy - 4.5,
                right: max_x + 9.0,
                bottom: cy + 4.5,
            },
            &muted_brush,
            1.0,
            None,
        );
        let cls_x = w - 28.0;
        target.DrawLine(
            Vector2 {
                X: cls_x,
                Y: cy - 4.5,
            },
            Vector2 {
                X: cls_x + 9.0,
                Y: cy + 4.5,
            },
            &muted_brush,
            1.1,
            None,
        );
        target.DrawLine(
            Vector2 {
                X: cls_x + 9.0,
                Y: cy - 4.5,
            },
            Vector2 {
                X: cls_x,
                Y: cy + 4.5,
            },
            &muted_brush,
            1.1,
            None,
        );

        // Crisp 1 px outer window border.
        target.DrawRectangle(
            &D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: w - 0.5,
                bottom: h - 0.5,
            },
            &line_brush,
            1.0,
            None,
        );
    }

    // Window title text via DirectWrite.
    // SAFETY: Creating a shared DirectWrite factory has no special preconditions.
    let Ok(dwrite): windows::core::Result<IDWriteFactory> =
        (unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) })
    else {
        return;
    };
    // SAFETY: Valid DirectWrite factory and static UTF-16 family/locale names.
    let Ok(fmt) = (unsafe {
        dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            12.5,
            w!("en-US"),
        )
    }) else {
        return;
    };
    // SAFETY: Valid text format.
    unsafe {
        let _ = fmt.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        let _ = fmt.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        let _ = fmt.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    }

    let wide: Vec<u16> = title.encode_utf16().collect();
    let title_rect = D2D_RECT_F {
        left: 40.0,
        top: 0.0,
        right: (w - 140.0).max(60.0),
        bottom: bar_h,
    };
    // SAFETY: Valid target, format, brush, and `wide` buffer.
    unsafe {
        target.DrawText(
            &wide,
            &fmt,
            &title_rect,
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

fn save_wic_bitmap_as_png(
    wic: &IWICImagingFactory,
    bitmap: &windows::Win32::Graphics::Imaging::IWICBitmap,
    out_path: &Path,
) -> Result<(), String> {
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create screenshot directory: {e}"))?;
    }

    let wide_path: Vec<u16> = out_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: Valid WIC factory.
    let stream =
        unsafe { wic.CreateStream() }.map_err(|e| format!("Failed to create WIC stream: {e}"))?;
    // SAFETY: `wide_path` is NUL-terminated UTF-16.
    unsafe { stream.InitializeFromFilename(PCWSTR(wide_path.as_ptr()), GENERIC_WRITE.0) }
        .map_err(|e| format!("Failed to open '{}' for writing: {e}", out_path.display()))?;

    // SAFETY: Standard PNG container GUID.
    let encoder = unsafe { wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null()) }
        .map_err(|e| format!("Failed to create WIC PNG encoder: {e}"))?;
    // SAFETY: Valid writable stream.
    unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }
        .map_err(|e| format!("Failed to initialize PNG encoder: {e}"))?;

    let mut frame_opt: Option<IWICBitmapFrameEncode> = None;
    // SAFETY: Valid initialized encoder.
    unsafe { encoder.CreateNewFrame(&mut frame_opt, std::ptr::null_mut()) }
        .map_err(|e| format!("Failed to create PNG frame: {e}"))?;
    let frame = frame_opt.ok_or_else(|| "WIC returned null PNG frame".to_string())?;

    // SAFETY: Valid frame.
    unsafe { frame.Initialize(None) }
        .map_err(|e| format!("Failed to initialize PNG frame: {e}"))?;
    // SAFETY: `bitmap` implements `IWICBitmapSource`.
    unsafe { frame.WriteSource(bitmap, std::ptr::null()) }
        .map_err(|e| format!("Failed to encode PNG pixels: {e}"))?;
    // SAFETY: Frame pixels written.
    unsafe { frame.Commit() }.map_err(|e| format!("Failed to commit PNG frame: {e}"))?;
    // SAFETY: Frame committed.
    unsafe { encoder.Commit() }.map_err(|e| format!("Failed to commit PNG encoder: {e}"))?;
    // SAFETY: Encoder committed.
    unsafe { stream.Commit(STGC_DEFAULT) }
        .map_err(|e| format!("Failed to flush PNG stream: {e}"))?;

    Ok(())
}
