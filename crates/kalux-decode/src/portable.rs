//! Pure-Rust image decoders for non-Windows platforms (Linux / Wayland).
//!
//! Covers PNG, GIF (static + animated with disposal & loop count), WebP (static + animated),
//! BMP, ICO, and TIFF in 100% pure Rust, always outputting premultiplied BGRA8 with EXIF
//! orientation applied and optional `max_size` downscaling.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use kalux_core::{DecodedImage, Frame, ImageFormat};

use crate::DecodeError;
use crate::anim::{
    AnimatedCompositor, Disposal, FrameMeta, MAX_ANIMATED_BYTES, capped_animation_dimensions,
    normalize_gif_delay_ms,
};
use crate::orient::{Orientation, read_exif_orientation};
use crate::scale::{downscale_bgra, fit_dimensions};

/// Decode any non-JPEG image file using pure-Rust decoders.
pub fn decode_file_portable(
    path: &Path,
    header: &[u8],
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    let format = sniff_format(header).or_else(|| ImageFormat::from_path(path));
    let Some(format) = format else {
        return Err(DecodeError::Unsupported { hint: None });
    };

    let bytes = fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => DecodeError::NotFound,
        _ => DecodeError::Io(e.to_string()),
    })?;
    if bytes.is_empty() {
        return Err(DecodeError::Corrupt("file is empty".into()));
    }

    match format {
        ImageFormat::Png => decode_png(&bytes, max_size),
        ImageFormat::Gif => decode_gif(&bytes, max_size),
        ImageFormat::WebP => decode_webp(&bytes, max_size),
        ImageFormat::Bmp => decode_via_image(&bytes, image::ImageFormat::Bmp, format, max_size),
        ImageFormat::Ico => decode_via_image(&bytes, image::ImageFormat::Ico, format, max_size),
        ImageFormat::Tiff => decode_via_image(&bytes, image::ImageFormat::Tiff, format, max_size),
        ImageFormat::Jpeg => Err(DecodeError::Corrupt("invalid JPEG header".into())),
        _ => Err(DecodeError::Unsupported { hint: None }),
    }
}

fn sniff_format(h: &[u8]) -> Option<ImageFormat> {
    if h.len() >= 8 && h[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return Some(ImageFormat::Png);
    }
    if h.len() >= 6 && (&h[..6] == b"GIF87a" || &h[..6] == b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if h.len() >= 12 && &h[..4] == b"RIFF" && &h[8..12] == b"WEBP" {
        return Some(ImageFormat::WebP);
    }
    if h.len() >= 2 && &h[..2] == b"BM" {
        return Some(ImageFormat::Bmp);
    }
    if h.len() >= 4 && (h[..4] == [0, 0, 1, 0] || h[..4] == [0, 0, 2, 0]) {
        return Some(ImageFormat::Ico);
    }
    if h.len() >= 4 && (&h[..4] == b"II*\0" || &h[..4] == b"MM\0*") {
        return Some(ImageFormat::Tiff);
    }
    None
}

/// Premultiply straight RGBA8 pixels in-place into BGRA8 premultiplied.
fn rgba_to_premul_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for (src, dst) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        let (r, g, b, a) = (src[0], src[1], src[2], src[3]);
        if a == 255 {
            dst[0] = b;
            dst[1] = g;
            dst[2] = r;
            dst[3] = 255;
        } else if a == 0 {
            *dst = [0, 0, 0, 0];
        } else {
            let a16 = u16::from(a);
            dst[0] = ((u16::from(b) * a16 + 127) / 255) as u8;
            dst[1] = ((u16::from(g) * a16 + 127) / 255) as u8;
            dst[2] = ((u16::from(r) * a16 + 127) / 255) as u8;
            dst[3] = a;
        }
    }
    out
}

fn rgb_to_bgra(rgb: &[u8]) -> Vec<u8> {
    let px = rgb.len() / 3;
    let mut out = vec![0u8; px * 4];
    for (src, dst) in rgb
        .as_chunks::<3>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = 255;
    }
    out
}

fn gray_to_bgra(gray: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; gray.len() * 4];
    for (&v, dst) in gray.iter().zip(out.as_chunks_mut::<4>().0.iter_mut()) {
        dst[0] = v;
        dst[1] = v;
        dst[2] = v;
        dst[3] = 255;
    }
    out
}

fn gray_alpha_to_premul_bgra(ga: &[u8]) -> Vec<u8> {
    let px = ga.len() / 2;
    let mut out = vec![0u8; px * 4];
    for (src, dst) in ga
        .as_chunks::<2>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0.iter_mut())
    {
        let (v, a) = (src[0], src[1]);
        if a == 255 {
            *dst = [v, v, v, 255];
        } else if a == 0 {
            *dst = [0, 0, 0, 0];
        } else {
            let p = ((u16::from(v) * u16::from(a) + 127) / 255) as u8;
            *dst = [p, p, p, a];
        }
    }
    out
}

/// Apply EXIF orientation + optional `max_size` downscale and wrap into a single-frame `DecodedImage`.
fn finalize_static_bgra(
    bgra: Vec<u8>,
    raw_w: u32,
    raw_h: u32,
    orientation: Orientation,
    max_size: Option<(u32, u32)>,
    format: ImageFormat,
) -> Result<DecodedImage, DecodeError> {
    if raw_w == 0 || raw_h == 0 {
        return Err(DecodeError::Corrupt("image has zero dimensions".into()));
    }
    let (oriented, src_w, src_h) = orientation.apply_bgra(bgra, raw_w, raw_h);
    let (dst_w, dst_h) = match max_size {
        Some((mw, mh)) => fit_dimensions(src_w, src_h, mw, mh),
        None => (src_w, src_h),
    };
    let pixels = if dst_w == src_w && dst_h == src_h {
        oriented
    } else {
        downscale_bgra(&oriented, src_w, src_h, dst_w, dst_h)
    };
    Ok(DecodedImage {
        width: dst_w,
        height: dst_h,
        source_width: src_w,
        source_height: src_h,
        frames: vec![Frame {
            pixels,
            delay_ms: 0,
        }],
        loop_count: 0,
        format,
    })
}

fn decode_png(bytes: &[u8], max_size: Option<(u32, u32)>) -> Result<DecodedImage, DecodeError> {
    let orientation = read_exif_orientation(bytes);

    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?;

    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| DecodeError::Corrupt("PNG dimensions overflow".into()))?;
    let mut buf = vec![0u8; buf_size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let data = &buf[..info.buffer_size()];

    let bgra = match info.color_type {
        png::ColorType::Rgba => rgba_to_premul_bgra(data),
        png::ColorType::Rgb => rgb_to_bgra(data),
        png::ColorType::Grayscale => gray_to_bgra(data),
        png::ColorType::GrayscaleAlpha => gray_alpha_to_premul_bgra(data),
        png::ColorType::Indexed => {
            return Err(DecodeError::Corrupt(
                "unexpected unexpanded indexed PNG".into(),
            ));
        }
    };

    finalize_static_bgra(
        bgra,
        info.width,
        info.height,
        orientation,
        max_size,
        ImageFormat::Png,
    )
}

fn decode_gif(bytes: &[u8], max_size: Option<(u32, u32)>) -> Result<DecodedImage, DecodeError> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);

    // First pass: collect raw frames so we know exact frame count for the 256 MiB memory cap.
    let mut decoder = opts
        .read_info(Cursor::new(bytes))
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?;

    let canvas_w = u32::from(decoder.width());
    let canvas_h = u32::from(decoder.height());
    if canvas_w == 0 || canvas_h == 0 {
        return Err(DecodeError::Corrupt("GIF has zero dimensions".into()));
    }

    let loop_count = match decoder.repeat() {
        gif::Repeat::Infinite => 0,
        gif::Repeat::Finite(n) => u32::from(n),
    };

    struct RawGifFrame {
        bgra: Vec<u8>,
        meta: FrameMeta,
    }

    let mut raw_frames: Vec<RawGifFrame> = Vec::new();
    while let Some(frame) = decoder
        .read_next_frame()
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?
    {
        let disposal = match frame.dispose {
            gif::DisposalMethod::Keep => Disposal::Keep,
            gif::DisposalMethod::Background => Disposal::RestoreBackground,
            gif::DisposalMethod::Previous => Disposal::RestorePrevious,
            gif::DisposalMethod::Any => Disposal::None,
        };
        let meta = FrameMeta {
            left: u32::from(frame.left),
            top: u32::from(frame.top),
            width: u32::from(frame.width),
            height: u32::from(frame.height),
            delay_ms: normalize_gif_delay_ms(frame.delay),
            disposal,
        };
        let bgra = rgba_to_premul_bgra(&frame.buffer);
        raw_frames.push(RawGifFrame { bgra, meta });
    }

    if raw_frames.is_empty() {
        return Err(DecodeError::Corrupt("GIF contains no frames".into()));
    }

    let frame_count = raw_frames.len() as u32;
    let (out_w, out_h) = capped_animation_dimensions(
        canvas_w,
        canvas_h,
        frame_count,
        max_size,
        MAX_ANIMATED_BYTES,
    );

    let mut compositor =
        AnimatedCompositor::new(canvas_w, canvas_h, out_w, out_h, raw_frames.len());
    for rf in raw_frames {
        compositor.push_frame(&rf.bgra, rf.meta);
    }
    let mut frames = compositor.finish();
    if frames.len() == 1 {
        frames[0].delay_ms = 0;
    }

    Ok(DecodedImage {
        width: out_w,
        height: out_h,
        source_width: canvas_w,
        source_height: canvas_h,
        frames,
        loop_count: if frame_count > 1 { loop_count } else { 0 },
        format: ImageFormat::Gif,
    })
}

fn decode_webp(bytes: &[u8], max_size: Option<(u32, u32)>) -> Result<DecodedImage, DecodeError> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?;

    let (raw_w, raw_h) = decoder.dimensions();
    if raw_w == 0 || raw_h == 0 {
        return Err(DecodeError::Corrupt("WebP has zero dimensions".into()));
    }

    let orientation = decoder
        .exif_metadata()
        .ok()
        .flatten()
        .map(|exif_bytes| read_exif_orientation(&exif_bytes))
        .unwrap_or(Orientation::Normal);

    let has_alpha = decoder.has_alpha();
    let bpp = if has_alpha { 4usize } else { 3usize };
    let frame_buf_len = (raw_w as usize)
        .checked_mul(raw_h as usize)
        .and_then(|n| n.checked_mul(bpp))
        .ok_or_else(|| DecodeError::Corrupt("WebP dimensions overflow".into()))?;

    if !decoder.is_animated() {
        let mut buf = vec![0u8; frame_buf_len];
        decoder
            .read_image(&mut buf)
            .map_err(|e| DecodeError::Corrupt(e.to_string()))?;
        let bgra = if has_alpha {
            rgba_to_premul_bgra(&buf)
        } else {
            rgb_to_bgra(&buf)
        };
        return finalize_static_bgra(bgra, raw_w, raw_h, orientation, max_size, ImageFormat::WebP);
    }

    // Animated WebP: `image-webp` already handles canvas compositing & disposal per frame!
    let frame_count = decoder.num_frames();
    let loop_count = match decoder.loop_count() {
        image_webp::LoopCount::Forever => 0,
        image_webp::LoopCount::Times(n) => u32::from(n.get()),
    };
    let (out_w, out_h) = capped_animation_dimensions(
        raw_w,
        raw_h,
        frame_count.max(1),
        max_size,
        MAX_ANIMATED_BYTES,
    );

    let mut frames = Vec::with_capacity(frame_count as usize);
    let mut buf = vec![0u8; frame_buf_len];
    for _ in 0..frame_count {
        let delay_ms = match decoder.read_frame(&mut buf) {
            Ok(d) => {
                if d <= 10 {
                    100
                } else {
                    d
                }
            }
            Err(image_webp::DecodingError::NoMoreFrames) => break,
            Err(e) => return Err(DecodeError::Corrupt(e.to_string())),
        };
        let bgra = if has_alpha {
            rgba_to_premul_bgra(&buf)
        } else {
            rgb_to_bgra(&buf)
        };
        let pixels = if out_w == raw_w && out_h == raw_h {
            bgra
        } else {
            downscale_bgra(&bgra, raw_w, raw_h, out_w, out_h)
        };
        frames.push(Frame { pixels, delay_ms });
    }

    if frames.is_empty() {
        return Err(DecodeError::Corrupt("animated WebP has no frames".into()));
    }

    Ok(DecodedImage {
        width: out_w,
        height: out_h,
        source_width: raw_w,
        source_height: raw_h,
        frames,
        loop_count,
        format: ImageFormat::WebP,
    })
}

fn decode_via_image(
    bytes: &[u8],
    img_fmt: image::ImageFormat,
    format: ImageFormat,
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    let orientation = read_exif_orientation(bytes);
    let dyn_img = image::load_from_memory_with_format(bytes, img_fmt)
        .map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let rgba = dyn_img.to_rgba8();
    let (raw_w, raw_h) = rgba.dimensions();
    let bgra = rgba_to_premul_bgra(rgba.as_raw());
    finalize_static_bgra(bgra, raw_w, raw_h, orientation, max_size, format)
}
