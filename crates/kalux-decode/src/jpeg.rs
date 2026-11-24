//! Pure-Rust JPEG decoder using `zune-jpeg` + `kamadak-exif` EXIF orientation.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use kalux_core::{DecodedImage, Frame, ImageFormat};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::DecodeError;
use crate::orient::read_exif_orientation;
use crate::scale::{downscale_bgra, fit_dimensions};

/// True if `header` begins with a JPEG SOI + marker prefix (`FF D8 FF`).
pub fn is_jpeg_magic(header: &[u8]) -> bool {
    header.len() >= 3 && header[0] == 0xFF && header[1] == 0xD8 && header[2] == 0xFF
}

/// Cheaply read raw (pre-orientation) JPEG dimensions from file headers without decoding pixels.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn probe_jpeg_dimensions(path: &Path) -> Option<(u32, u32)> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut decoder = JpegDecoder::new(reader);
    decoder.decode_headers().ok()?;
    let info = decoder.info()?;
    Some((u32::from(info.width), u32::from(info.height)))
}

/// Decode a JPEG file via `zune-jpeg`, applying EXIF orientation and optional aspect-preserving
/// downscaling. Output is tightly-packed BGRA8 (premultiplied; alpha = 255).
pub fn decode_jpeg_file(
    path: &Path,
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => DecodeError::NotFound,
        _ => DecodeError::Io(e.to_string()),
    })?;
    decode_jpeg_bytes(&bytes, max_size)
}

/// Decode in-memory JPEG bytes into a display-ready BGRA8 `DecodedImage`.
pub fn decode_jpeg_bytes(
    bytes: &[u8],
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    let orientation = read_exif_orientation(bytes);

    // Construct with BGRA options BEFORE decode_headers() because zune-jpeg selects its
    // color-conversion kernel inside decode_headers() rather than in set_options().
    let bgra_opts = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::BGRA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), bgra_opts);
    decoder
        .decode_headers()
        .map_err(|e| DecodeError::Corrupt(format!("JPEG header error: {e}")))?;

    let info = decoder
        .info()
        .ok_or_else(|| DecodeError::Corrupt("missing JPEG dimensions".into()))?;
    let raw_w = u32::from(info.width);
    let raw_h = u32::from(info.height);
    if raw_w == 0 || raw_h == 0 {
        return Err(DecodeError::Corrupt("JPEG has zero dimensions".into()));
    }

    // zune-jpeg supports YCbCr -> BGRA directly, while Luma/CMYK/YCCK only support RGBA.
    let (mut pixels, need_rb_swap) = match decoder.input_colorspace() {
        Some(ColorSpace::Luma) => {
            // Grayscale RGBA has R == G == B, so it is already valid BGRA without swizzling.
            let rgba_opts = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGBA);
            let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), rgba_opts);
            let px = dec
                .decode()
                .map_err(|e| DecodeError::Corrupt(format!("JPEG decode error: {e}")))?;
            (px, false)
        }
        Some(ColorSpace::CMYK | ColorSpace::YCCK) => {
            let rgba_opts = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGBA);
            let mut dec = JpegDecoder::new_with_options(ZCursor::new(bytes), rgba_opts);
            let px = dec
                .decode()
                .map_err(|e| DecodeError::Corrupt(format!("JPEG decode error: {e}")))?;
            (px, true)
        }
        _ => {
            let px = decoder
                .decode()
                .map_err(|e| DecodeError::Corrupt(format!("JPEG decode error: {e}")))?;
            (px, false)
        }
    };

    let expected_len = (raw_w as usize)
        .checked_mul(raw_h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| DecodeError::Corrupt("JPEG dimensions overflow".into()))?;
    if pixels.len() != expected_len {
        return Err(DecodeError::Corrupt(format!(
            "unexpected JPEG buffer size: got {}, expected {expected_len}",
            pixels.len()
        )));
    }

    if need_rb_swap {
        for px in pixels.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }

    // Source dimensions are post-orientation (matching DecodedImage contract).
    let (source_width, source_height) = orientation.dimensions(raw_w, raw_h);

    // Downscale BEFORE orientation so rotation touches far fewer pixels when shrinking.
    let (target_w, target_h) = match max_size {
        Some((mw, mh)) => fit_dimensions(source_width, source_height, mw, mh),
        None => (source_width, source_height),
    };
    let (raw_target_w, raw_target_h) = orientation.dimensions(target_w, target_h);

    let (scaled_pixels, cur_w, cur_h) = if raw_target_w < raw_w || raw_target_h < raw_h {
        let scaled = downscale_bgra(&pixels, raw_w, raw_h, raw_target_w, raw_target_h);
        (scaled, raw_target_w, raw_target_h)
    } else {
        (pixels, raw_w, raw_h)
    };

    let (final_pixels, width, height) = orientation.apply_bgra(scaled_pixels, cur_w, cur_h);

    Ok(DecodedImage {
        width,
        height,
        source_width,
        source_height,
        format: ImageFormat::Jpeg,
        frames: vec![Frame {
            pixels: final_pixels,
            delay_ms: 0,
        }],
        loop_count: 0,
    })
}
