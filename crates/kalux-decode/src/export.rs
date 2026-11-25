//! Memory-bounded full-resolution Edit Mode export pipeline (Edit Mode PRD §5.4).
//!
//! Guarantees:
//! 1. Lossless JPEG EXIF orientation fast-path when only 90° turns/flips are applied.
//! 2. Full-resolution decode runs only on export, extracts strictly the cropped region into the
//!    minimal output buffer (RGB/BGR 3 B/px for JPEG, RGBA/BGRA 4 B/px for PNG/TIFF/BMP),
//!    drops the full-res source immediately, and applies [`ToneLut`] **in-place**.
//! 3. Writes atomically via a hidden sibling temp file + `sync_all` + `rename` so crashes or
//!    permission failures never corrupt the original image.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use kalux_core::{DecodedImage, EditRecipe, Frame, ImageFormat, ToneLut};

use crate::exif_orient::{lossless_jpeg_orient_file, sibling_temp_path};
use crate::{DecodeError, decode_file};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Jpeg,
    Png,
    Bmp,
    Tiff,
}

impl ExportFormat {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
        }
    }

    pub const fn from_image_format(fmt: ImageFormat) -> Option<Self> {
        match fmt {
            ImageFormat::Jpeg => Some(Self::Jpeg),
            ImageFormat::Png => Some(Self::Png),
            ImageFormat::Bmp => Some(Self::Bmp),
            ImageFormat::Tiff => Some(Self::Tiff),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ExportOptions {
    /// JPEG quality in `1..=100` (default `92`).
    pub jpeg_quality: u8,
    /// When `true`, writes a non-destructive copy (`<stem>_edited.<ext>`) instead of overwriting.
    pub save_copy: bool,
    /// Optional explicit output format override.
    pub format_override: Option<ExportFormat>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            jpeg_quality: 92,
            save_copy: true,
            format_override: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportResult {
    pub saved_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub lossless: bool,
    /// True when overwrite was requested on a read-only/animated format and safely redirected to a copy.
    pub redirected_to_copy: bool,
}

#[derive(Debug, Clone)]
pub enum ExportError {
    Decode(DecodeError),
    Encode(String),
    Io(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "Decode failed: {e}"),
            Self::Encode(msg) => write!(f, "Encode failed: {msg}"),
            Self::Io(msg) => write!(f, "File save failed: {msg}"),
        }
    }
}

impl std::error::Error for ExportError {}

/// Choose a non-clobbering sibling copy path: `<stem>_edited.<ext>`, `<stem>_edited_2.<ext>`, ...
pub fn next_copy_path(original: &Path, ext: &str) -> PathBuf {
    let dir = original
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");

    let first = dir.join(format!("{stem}_edited.{ext}"));
    if !first.exists() {
        return first;
    }
    for idx in 2u32..10_000 {
        let candidate = dir.join(format!("{stem}_edited_{idx}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}_edited_{}.{ext}", std::process::id()))
}

/// True if `path`'s format supports direct in-place overwrite (not animated, writable codec).
pub fn supports_in_place_overwrite(path: &Path, is_animated: bool) -> bool {
    if is_animated {
        return false;
    }
    ImageFormat::from_path(path)
        .and_then(ExportFormat::from_image_format)
        .is_some()
}

/// Render `recipe` against the full-resolution `src_path` into a BGRA8 premultiplied
/// [`DecodedImage`] (used for `Ctrl+C` Copy Edited Image and deterministic testing).
pub fn render_edited_image(
    src_path: &Path,
    recipe: &EditRecipe,
    frame_idx: usize,
) -> Result<DecodedImage, ExportError> {
    let source = decode_file(src_path, None).map_err(ExportError::Decode)?;
    let fmt = source.format;
    let (mut bgra, out_w, out_h) = extract_cropped_oriented_bgra(&source, frame_idx, recipe)?;
    // Drop full-resolution source immediately before applying tone LUT in-place.
    drop(source);

    let lut = ToneLut::from_adjustments(&recipe.tone);
    lut.apply_bgra_premul_slice(&mut bgra);

    Ok(DecodedImage {
        width: out_w,
        height: out_h,
        source_width: out_w,
        source_height: out_h,
        format: fmt,
        frames: vec![Frame {
            pixels: bgra,
            delay_ms: 0,
        }],
        loop_count: 0,
    })
}

/// Export `src_path` with `recipe` according to `opts`.
///
/// Handles:
/// - Lossless EXIF orientation fast-path for orient-only JPEG saves.
/// - Safe automatic redirection to Save Copy for animated or read-only formats (`HEIC`/`GIF`/`WebP`/etc.).
/// - Immediate drop of full-resolution source buffer after crop extraction.
/// - Atomic temp-file + rename write.
pub fn export_image(
    src_path: &Path,
    recipe: &EditRecipe,
    opts: ExportOptions,
    frame_idx: usize,
) -> Result<ExportResult, ExportError> {
    let src_fmt = ImageFormat::from_path(src_path);
    let direct_fmt = opts
        .format_override
        .or_else(|| src_fmt.and_then(ExportFormat::from_image_format));

    // Fast Path A: Lossless EXIF orientation update for orient-only JPEG edits.
    if recipe.is_orientation_only()
        && src_fmt == Some(ImageFormat::Jpeg)
        && direct_fmt == Some(ExportFormat::Jpeg)
    {
        let dst_path = if opts.save_copy {
            let ext = src_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("jpg");
            next_copy_path(src_path, ext)
        } else {
            src_path.to_path_buf()
        };

        if lossless_jpeg_orient_file(
            src_path,
            &dst_path,
            recipe.turns,
            recipe.flip_h,
            recipe.flip_v,
        )
        .is_ok()
        {
            // Probe oriented dimensions cheaply from header (or fallback to decode metadata).
            let (out_w, out_h) = decode_file(&dst_path, Some((64, 64)))
                .map(|d| (d.source_width, d.source_height))
                .unwrap_or((0, 0));
            return Ok(ExportResult {
                saved_path: dst_path,
                width: out_w,
                height: out_h,
                lossless: true,
                redirected_to_copy: false,
            });
        }
    }

    // Standard Path: decode full-resolution once.
    let source = decode_file(src_path, None).map_err(ExportError::Decode)?;
    let is_anim = source.is_animated();
    let has_alpha = frame_has_transparency(&source, frame_idx);

    let (export_fmt, redirected_to_copy) = match direct_fmt {
        Some(fmt) if !is_anim => (fmt, false),
        Some(fmt) => (fmt, !opts.save_copy),
        None => {
            let fallback = if has_alpha {
                ExportFormat::Png
            } else {
                ExportFormat::Jpeg
            };
            (fallback, !opts.save_copy)
        }
    };

    let dst_path = if opts.save_copy || redirected_to_copy {
        let ext = if opts.format_override.is_none() && !redirected_to_copy {
            src_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(export_fmt.extension())
        } else {
            export_fmt.extension()
        };
        next_copy_path(src_path, ext)
    } else {
        src_path.to_path_buf()
    };

    let tmp_path = sibling_temp_path(&dst_path);
    let lut = ToneLut::from_adjustments(&recipe.tone);
    let quality = opts.jpeg_quality.clamp(1, 100);

    let (out_w, out_h) = match export_fmt {
        ExportFormat::Jpeg => {
            let (mut rgb_or_bgr, w, h) = extract_cropped_oriented_opaque(
                &source,
                frame_idx,
                recipe,
                cfg!(windows), // Windows WIC takes 24bppBGR; Linux jpeg-encoder takes RGB8
            )?;
            drop(source); // Release full-res decode before tone + encode!
            apply_lut_24bpp(&lut, &mut rgb_or_bgr, cfg!(windows));
            if let Err(err) = encode_jpeg_to_file(&tmp_path, &rgb_or_bgr, w, h, quality) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }
            (w, h)
        }
        ExportFormat::Png | ExportFormat::Bmp | ExportFormat::Tiff => {
            let (mut pixels, w, h) = extract_cropped_oriented_straight_4bpp(
                &source,
                frame_idx,
                recipe,
                cfg!(windows), // Windows WIC takes BGRA8; Linux png/image takes RGBA8
            )?;
            drop(source); // Release full-res decode before tone + encode!
            apply_lut_straight_4bpp(&lut, &mut pixels, cfg!(windows));
            if let Err(err) = encode_raster_to_file(&tmp_path, &pixels, w, h, export_fmt) {
                let _ = fs::remove_file(&tmp_path);
                return Err(err);
            }
            (w, h)
        }
    };

    if let Err(err) = fs::rename(&tmp_path, &dst_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(ExportError::Io(format!(
            "replace {}: {err}",
            dst_path.display()
        )));
    }

    Ok(ExportResult {
        saved_path: dst_path,
        width: out_w,
        height: out_h,
        lossless: false,
        redirected_to_copy,
    })
}

/// Encode a BGRA8 premultiplied [`DecodedImage`] to in-memory PNG bytes (for Wayland clipboard).
#[cfg(not(windows))]
pub fn encode_png_bytes(img: &DecodedImage, frame_idx: usize) -> Result<Vec<u8>, ExportError> {
    let frame = img
        .frames
        .get(frame_idx.min(img.frames.len().saturating_sub(1)))
        .ok_or_else(|| ExportError::Encode("image has no frames".into()))?;
    let w = img.width;
    let h = img.height;
    let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for px in frame.pixels.as_chunks::<4>().0 {
        let [r, g, b, a] = unpremul_bgra_to_rgba(*px);
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc
        .write_header()
        .map_err(|e| ExportError::Encode(e.to_string()))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| ExportError::Encode(e.to_string()))?;
    drop(writer);
    Ok(out)
}

fn frame_has_transparency(source: &DecodedImage, frame_idx: usize) -> bool {
    let Some(frame) = source
        .frames
        .get(frame_idx.min(source.frames.len().saturating_sub(1)))
    else {
        return false;
    };
    frame.pixels.as_chunks::<4>().0.iter().any(|px| px[3] < 255)
}

#[inline(always)]
fn unpremul_bgra_to_rgba(px: [u8; 4]) -> [u8; 4] {
    let a = px[3];
    if a == 255 {
        [px[2], px[1], px[0], 255]
    } else if a == 0 {
        [0, 0, 0, 0]
    } else {
        let a32 = u32::from(a);
        let r = ((u32::from(px[2]) * 255 + a32 / 2) / a32).min(255) as u8;
        let g = ((u32::from(px[1]) * 255 + a32 / 2) / a32).min(255) as u8;
        let b = ((u32::from(px[0]) * 255 + a32 / 2) / a32).min(255) as u8;
        [r, g, b, a]
    }
}

fn extract_cropped_oriented_bgra(
    source: &DecodedImage,
    frame_idx: usize,
    recipe: &EditRecipe,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    let frame = source
        .frames
        .get(frame_idx.min(source.frames.len().saturating_sub(1)))
        .ok_or_else(|| ExportError::Encode("image has no frames".into()))?;
    let sw = source.width;
    let sh = source.height;
    let rect = recipe.output_pixel_rect(sw, sh);
    if rect.w == 0 || rect.h == 0 {
        return Err(ExportError::Encode("empty crop dimensions".into()));
    }

    let mut out = vec![0u8; (rect.w as usize) * (rect.h as usize) * 4];
    let sw_u = sw as usize;
    let ow_u = rect.w as usize;

    for dy in 0..rect.h {
        let oy = rect.y + dy;
        let dst_row = &mut out[(dy as usize) * ow_u * 4..(dy as usize + 1) * ow_u * 4];
        for dx in 0..rect.w {
            let ox = rect.x + dx;
            let (sx, sy) = recipe
                .turns
                .inverse_pixel(ox, oy, sw, sh, recipe.flip_h, recipe.flip_v);
            let s = ((sy as usize) * sw_u + (sx as usize)) * 4;
            let d = (dx as usize) * 4;
            dst_row[d..d + 4].copy_from_slice(&frame.pixels[s..s + 4]);
        }
    }

    Ok((out, rect.w, rect.h))
}

fn extract_cropped_oriented_opaque(
    source: &DecodedImage,
    frame_idx: usize,
    recipe: &EditRecipe,
    bgr_order: bool,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    let frame = source
        .frames
        .get(frame_idx.min(source.frames.len().saturating_sub(1)))
        .ok_or_else(|| ExportError::Encode("image has no frames".into()))?;
    let sw = source.width;
    let sh = source.height;
    let rect = recipe.output_pixel_rect(sw, sh);
    if rect.w == 0 || rect.h == 0 {
        return Err(ExportError::Encode("empty crop dimensions".into()));
    }

    let mut out = vec![0u8; (rect.w as usize) * (rect.h as usize) * 3];
    let sw_u = sw as usize;
    let ow_u = rect.w as usize;

    for dy in 0..rect.h {
        let oy = rect.y + dy;
        let dst_row = &mut out[(dy as usize) * ow_u * 3..(dy as usize + 1) * ow_u * 3];
        for dx in 0..rect.w {
            let ox = rect.x + dx;
            let (sx, sy) = recipe
                .turns
                .inverse_pixel(ox, oy, sw, sh, recipe.flip_h, recipe.flip_v);
            let s = ((sy as usize) * sw_u + (sx as usize)) * 4;
            let px = [
                frame.pixels[s],
                frame.pixels[s + 1],
                frame.pixels[s + 2],
                frame.pixels[s + 3],
            ];
            // Composite premultiplied pixel over solid black for opaque JPEG output.
            let b = px[0];
            let g = px[1];
            let r = px[2];
            let d = (dx as usize) * 3;
            if bgr_order {
                dst_row[d] = b;
                dst_row[d + 1] = g;
                dst_row[d + 2] = r;
            } else {
                dst_row[d] = r;
                dst_row[d + 1] = g;
                dst_row[d + 2] = b;
            }
        }
    }

    Ok((out, rect.w, rect.h))
}

fn extract_cropped_oriented_straight_4bpp(
    source: &DecodedImage,
    frame_idx: usize,
    recipe: &EditRecipe,
    bgra_order: bool,
) -> Result<(Vec<u8>, u32, u32), ExportError> {
    let frame = source
        .frames
        .get(frame_idx.min(source.frames.len().saturating_sub(1)))
        .ok_or_else(|| ExportError::Encode("image has no frames".into()))?;
    let sw = source.width;
    let sh = source.height;
    let rect = recipe.output_pixel_rect(sw, sh);
    if rect.w == 0 || rect.h == 0 {
        return Err(ExportError::Encode("empty crop dimensions".into()));
    }

    let mut out = vec![0u8; (rect.w as usize) * (rect.h as usize) * 4];
    let sw_u = sw as usize;
    let ow_u = rect.w as usize;

    for dy in 0..rect.h {
        let oy = rect.y + dy;
        let dst_row = &mut out[(dy as usize) * ow_u * 4..(dy as usize + 1) * ow_u * 4];
        for dx in 0..rect.w {
            let ox = rect.x + dx;
            let (sx, sy) = recipe
                .turns
                .inverse_pixel(ox, oy, sw, sh, recipe.flip_h, recipe.flip_v);
            let s = ((sy as usize) * sw_u + (sx as usize)) * 4;
            let [r, g, b, a] = unpremul_bgra_to_rgba([
                frame.pixels[s],
                frame.pixels[s + 1],
                frame.pixels[s + 2],
                frame.pixels[s + 3],
            ]);
            let d = (dx as usize) * 4;
            if bgra_order {
                dst_row[d] = b;
                dst_row[d + 1] = g;
                dst_row[d + 2] = r;
                dst_row[d + 3] = a;
            } else {
                dst_row[d] = r;
                dst_row[d + 1] = g;
                dst_row[d + 2] = b;
                dst_row[d + 3] = a;
            }
        }
    }

    Ok((out, rect.w, rect.h))
}

fn apply_lut_24bpp(lut: &ToneLut, pixels: &mut [u8], bgr_order: bool) {
    if lut.is_identity() {
        return;
    }
    if bgr_order {
        for px in pixels.as_chunks_mut::<3>().0 {
            let [r, g, b] = lut.apply_rgb(px[2], px[1], px[0]);
            px[0] = b;
            px[1] = g;
            px[2] = r;
        }
    } else {
        lut.apply_rgb_slice(pixels);
    }
}

fn apply_lut_straight_4bpp(lut: &ToneLut, pixels: &mut [u8], bgra_order: bool) {
    if lut.is_identity() {
        return;
    }
    if bgra_order {
        for px in pixels.as_chunks_mut::<4>().0 {
            if px[3] == 0 {
                continue;
            }
            let [r, g, b] = lut.apply_rgb(px[2], px[1], px[0]);
            px[0] = b;
            px[1] = g;
            px[2] = r;
        }
    } else {
        lut.apply_rgba_straight_slice(pixels);
    }
}

// ============================================================================
// Non-Windows (Linux) pure-Rust encoders (reusing workspace crates)
// ============================================================================

#[cfg(not(windows))]
fn encode_jpeg_to_file(
    tmp_path: &Path,
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<(), ExportError> {
    use std::fs::File;
    use std::io::BufWriter;

    let file = File::create(tmp_path).map_err(|e| ExportError::Io(e.to_string()))?;
    let mut writer = BufWriter::new(file);
    let enc = jpeg_encoder::Encoder::new(&mut writer, quality);
    enc.encode(
        rgb,
        width as u16,
        height as u16,
        jpeg_encoder::ColorType::Rgb,
    )
    .map_err(|e| ExportError::Encode(e.to_string()))?;
    let file = writer
        .into_inner()
        .map_err(|e| ExportError::Io(e.to_string()))?;
    file.sync_all().map_err(|e| ExportError::Io(e.to_string()))
}

#[cfg(not(windows))]
fn encode_raster_to_file(
    tmp_path: &Path,
    rgba: &[u8],
    width: u32,
    height: u32,
    format: ExportFormat,
) -> Result<(), ExportError> {
    use std::fs::File;
    use std::io::BufWriter;

    let file = File::create(tmp_path).map_err(|e| ExportError::Io(e.to_string()))?;
    let mut writer = BufWriter::new(file);

    match format {
        ExportFormat::Png => {
            let mut enc = png::Encoder::new(&mut writer, width, height);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut png_w = enc
                .write_header()
                .map_err(|e| ExportError::Encode(e.to_string()))?;
            png_w
                .write_image_data(rgba)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Bmp => {
            use image::ImageEncoder as _;
            let enc = image::codecs::bmp::BmpEncoder::new(&mut writer);
            enc.write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Tiff => {
            use image::ImageEncoder as _;
            let enc = image::codecs::tiff::TiffEncoder::new(&mut writer);
            enc.write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
                .map_err(|e| ExportError::Encode(e.to_string()))?;
        }
        ExportFormat::Jpeg => unreachable!(),
    }

    let file = writer
        .into_inner()
        .map_err(|e| ExportError::Io(e.to_string()))?;
    file.sync_all().map_err(|e| ExportError::Io(e.to_string()))
}

// ============================================================================
// Windows native WIC encoders (zero extra binary bloat!)
// ============================================================================

#[cfg(windows)]
fn encode_jpeg_to_file(
    tmp_path: &Path,
    bgr: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<(), ExportError> {
    use windows::Win32::Graphics::Imaging::{
        GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
    };
    encode_wic_to_file(
        tmp_path,
        bgr,
        width,
        height,
        width.saturating_mul(3),
        &GUID_ContainerFormatJpeg,
        &GUID_WICPixelFormat24bppBGR,
        Some(f32::from(quality) / 100.0),
    )
}

#[cfg(windows)]
fn encode_raster_to_file(
    tmp_path: &Path,
    bgra: &[u8],
    width: u32,
    height: u32,
    format: ExportFormat,
) -> Result<(), ExportError> {
    use windows::Win32::Graphics::Imaging::{
        GUID_ContainerFormatBmp, GUID_ContainerFormatPng, GUID_ContainerFormatTiff,
        GUID_WICPixelFormat32bppBGRA,
    };
    let container = match format {
        ExportFormat::Png => &GUID_ContainerFormatPng,
        ExportFormat::Bmp => &GUID_ContainerFormatBmp,
        ExportFormat::Tiff => &GUID_ContainerFormatTiff,
        ExportFormat::Jpeg => unreachable!(),
    };
    encode_wic_to_file(
        tmp_path,
        bgra,
        width,
        height,
        width.saturating_mul(4),
        container,
        &GUID_WICPixelFormat32bppBGRA,
        None,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn encode_wic_to_file(
    tmp_path: &Path,
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    container_guid: &windows::core::GUID,
    pixel_fmt_guid: &windows::core::GUID,
    jpeg_quality: Option<f32>,
) -> Result<(), ExportError> {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, STGC_DEFAULT,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{PCWSTR, PWSTR};

    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Paired with successful `CoInitializeEx` on this worker thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    // SAFETY: Initialize COM MTA on the export worker thread.
    let com_ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let _com = ComGuard(com_ok);

    let wide_path: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: Standard in-proc WIC factory creation.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| ExportError::Encode(format!("WIC factory: {e}")))?;

    // SAFETY: Valid factory.
    let stream = unsafe { factory.CreateStream() }
        .map_err(|e| ExportError::Io(format!("WIC stream: {e}")))?;

    // SAFETY: `wide_path` is NUL-terminated UTF-16.
    unsafe { stream.InitializeFromFilename(PCWSTR(wide_path.as_ptr()), GENERIC_WRITE.0) }
        .map_err(|e| ExportError::Io(format!("open {}: {e}", tmp_path.display())))?;

    // SAFETY: Standard container GUID with default vendor.
    let encoder = unsafe { factory.CreateEncoder(container_guid, std::ptr::null()) }
        .map_err(|e| ExportError::Encode(format!("WIC encoder: {e}")))?;

    // SAFETY: Stream is open for write.
    unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }
        .map_err(|e| ExportError::Encode(format!("WIC encoder init: {e}")))?;

    let mut frame_opt: Option<IWICBitmapFrameEncode> = None;
    let mut props_opt: Option<IPropertyBag2> = None;
    // SAFETY: Out-pointers for newly created frame and property bag.
    unsafe { encoder.CreateNewFrame(&mut frame_opt, &mut props_opt) }
        .map_err(|e| ExportError::Encode(format!("WIC frame: {e}")))?;
    let frame = frame_opt.ok_or_else(|| ExportError::Encode("null WIC frame".into()))?;

    if let (Some(q), Some(props)) = (jpeg_quality, props_opt.as_ref()) {
        let mut name: Vec<u16> = "ImageQuality"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let prop = PROPBAG2 {
            pstrName: PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        let val = VARIANT::from(q.clamp(0.01, 1.0));
        // SAFETY: `prop` and `val` are valid single-element arrays.
        unsafe { props.Write(1, &prop, &val) }
            .map_err(|e| ExportError::Encode(format!("WIC quality: {e}")))?;
    }

    // SAFETY: `props_opt` belongs to this frame.
    unsafe { frame.Initialize(props_opt.as_ref()) }
        .map_err(|e| ExportError::Encode(format!("WIC frame init: {e}")))?;
    // SAFETY: Valid non-zero dimensions.
    unsafe { frame.SetSize(width, height) }
        .map_err(|e| ExportError::Encode(format!("WIC SetSize: {e}")))?;

    let mut pf = *pixel_fmt_guid;
    // SAFETY: Valid mutable pixel format GUID.
    unsafe { frame.SetPixelFormat(&mut pf) }
        .map_err(|e| ExportError::Encode(format!("WIC SetPixelFormat: {e}")))?;

    // SAFETY: `pixels` holds `height * stride` bytes in the requested pixel layout.
    unsafe { frame.WritePixels(height, stride, pixels) }
        .map_err(|e| ExportError::Encode(format!("WIC WritePixels: {e}")))?;
    // SAFETY: All rows written.
    unsafe { frame.Commit() }.map_err(|e| ExportError::Encode(format!("WIC frame commit: {e}")))?;
    // SAFETY: Frame committed.
    unsafe { encoder.Commit() }
        .map_err(|e| ExportError::Encode(format!("WIC encoder commit: {e}")))?;
    // SAFETY: Encoder committed.
    unsafe { stream.Commit(STGC_DEFAULT) }
        .map_err(|e| ExportError::Io(format!("WIC stream flush: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kalux_core::{CropRect, QuarterTurns, ToneAdjustments};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("kalux_export_{tag}_{}_{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn next_copy_path_increments_without_clobbering() {
        let dir = temp_dir("copy_name");
        let orig = dir.join("photo.jpg");
        fs::write(&orig, b"a").unwrap();

        let p1 = next_copy_path(&orig, "jpg");
        assert_eq!(p1, dir.join("photo_edited.jpg"));
        fs::write(&p1, b"b").unwrap();

        let p2 = next_copy_path(&orig, "jpg");
        assert_eq!(p2, dir.join("photo_edited_2.jpg"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_crop_rotate_and_tone_on_png_and_jpeg() {
        let dir = temp_dir("crop_tone");

        // 1. Copy alpha.png fixture and export cropped + monochrome copy.
        let alpha_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alpha.png");
        let alpha_work = dir.join("alpha.png");
        fs::copy(&alpha_src, &alpha_work).unwrap();
        let orig_img = decode_file(&alpha_work, None).unwrap();

        let recipe = EditRecipe {
            crop: CropRect {
                x: 0.25,
                y: 0.25,
                w: 0.5,
                h: 0.5,
            },
            turns: QuarterTurns::R90,
            flip_h: false,
            flip_v: false,
            tone: ToneAdjustments {
                monochrome: true,
                ..Default::default()
            },
        };
        let res = export_image(
            &alpha_work,
            &recipe,
            ExportOptions {
                save_copy: true,
                ..Default::default()
            },
            0,
        )
        .unwrap();
        assert!(!res.lossless);
        assert_eq!(res.saved_path, dir.join("alpha_edited.png"));
        let decoded = decode_file(&res.saved_path, None).unwrap();
        assert_eq!(decoded.width, orig_img.height / 2);
        assert_eq!(decoded.height, orig_img.width / 2);

        // 2. Lossless EXIF orient-only save on orient_6.jpg.
        let jpg_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/orient_6.jpg");
        let jpg_work = dir.join("orient_6.jpg");
        fs::copy(&jpg_src, &jpg_work).unwrap();
        let orient_recipe = EditRecipe {
            turns: QuarterTurns::R90,
            ..Default::default()
        };
        let jpg_res = export_image(
            &jpg_work,
            &orient_recipe,
            ExportOptions {
                save_copy: false,
                ..Default::default()
            },
            0,
        )
        .unwrap();
        assert!(jpg_res.lossless);
        assert_eq!(jpg_res.saved_path, jpg_work);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_animated_gif_redirects_overwrite_to_png_copy() {
        let dir = temp_dir("gif_copy");
        let gif_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/animated.gif");
        let gif_work = dir.join("anim.gif");
        fs::copy(&gif_src, &gif_work).unwrap();

        let res = export_image(
            &gif_work,
            &EditRecipe {
                flip_h: true,
                ..Default::default()
            },
            ExportOptions {
                save_copy: false, // Even if overwrite requested, animated GIF must redirect to copy!
                ..Default::default()
            },
            0,
        )
        .unwrap();
        assert!(res.redirected_to_copy);
        assert!(
            res.saved_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "png" || e == "jpg")
        );
        assert!(res.saved_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
