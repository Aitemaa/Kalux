//! Decoding and preloading for Kalux.
//!
//! * [`decode_file`]: synchronous decode of one file (used by workers and tests).
//! * [`ImageStore`]: owns a worker pool + byte-budget cache; the app tells it what is
//!   *focused* and which neighbors to preload, and drains results with [`ImageStore::poll`].
//!
//! Backends:
//! * JPEG: `zune-jpeg` (pure Rust) — on all platforms. EXIF orientation via `kamadak-exif`.
//!   On Windows, when `max_size` requires ≥ 2× downscaling on both axes, we route large JPEGs
//!   through WIC so `IWICBitmapScaler` engages native JPEG DCT scaling (`IWICBitmapSourceTransform`
//!   1/2, 1/4, 1/8 IDCT), avoiding a full-resolution decode.
//! * Everything else (and JPEG fallback): WIC on Windows (`cfg(windows)`), including codec
//!   extensions (HEIF/AVIF/RAW/JXL) when installed. On non-Windows, non-JPEG returns
//!   `DecodeError::Unsupported` (Linux is only a test host).
//!
//! Output is always [`kalux_core::DecodedImage`] in BGRA8 premultiplied, orientation applied,
//! optionally downscaled to fit `max_size` (never upscaled; aspect preserved).

mod anim;
mod jpeg;
mod orient;
#[cfg(not(windows))]
mod portable;
mod scale;
mod store;
#[cfg(windows)]
mod wic;

use std::fs::File;
use std::io::Read;
use std::path::Path;

use kalux_core::DecodedImage;

pub use store::{ImageStore, StoreConfig, StoreEvent};

#[derive(Debug, Clone)]
pub enum DecodeError {
    NotFound,
    /// Format recognised but no decoder available (e.g. HEIF extension not installed).
    /// `hint` is a user-facing next step, e.g. "Install HEIF Image Extensions from the Microsoft Store".
    Unsupported {
        hint: Option<String>,
    },
    Corrupt(String),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "File not found."),
            Self::Unsupported { hint: Some(hint) } => {
                write!(f, "This image format is not supported yet. {hint}.")
            }
            Self::Unsupported { hint: None } => {
                write!(f, "This image format is not supported.")
            }
            Self::Corrupt(detail) => {
                if detail.is_empty() {
                    write!(f, "This image file appears to be damaged.")
                } else {
                    write!(f, "Could not decode image: {detail}.")
                }
            }
            Self::Io(detail) => write!(f, "Could not read file: {detail}."),
            Self::Cancelled => write!(f, "Image decode was cancelled."),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Read up to 16 leading bytes of `path` to sniff the container magic and detect missing files.
fn read_header(path: &Path) -> Result<(usize, [u8; 16]), DecodeError> {
    let mut file = File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => DecodeError::NotFound,
        _ => DecodeError::Io(e.to_string()),
    })?;
    let mut buf = [0u8; 16];
    let n = file
        .read(&mut buf)
        .map_err(|e| DecodeError::Io(e.to_string()))?;
    Ok((n, buf))
}

/// On Windows, decide whether WIC's native DCT-scaled JPEG decode (`1/2`, `1/4`, `1/8` IDCT via
/// `IWICBitmapSourceTransform`) beats `zune-jpeg` full decode + CPU box downscale.
///
/// WIC DCT scaling skips at least 75% of IDCT & color-conversion work once the source is ≥ 2×
/// larger than `max_size` on both axes (e.g. 12 MP 4000×3000 -> 1920×1080). For full-resolution
/// decodes (`max_size == None`) or images close to the target size, `zune-jpeg` stays faster and
/// avoids COM/WIC overhead.
#[cfg(windows)]
fn prefer_wic_for_jpeg(path: &Path, max_size: Option<(u32, u32)>) -> bool {
    let Some((mw, mh)) = max_size else {
        return false;
    };
    if mw == 0 || mh == 0 {
        return false;
    }
    let Some((raw_w, raw_h)) = jpeg::probe_jpeg_dimensions(path) else {
        return false;
    };
    // Compare conservatively against max(mw, mh) so 90°/270° rotated JPEGs still qualify accurately.
    let target_long = mw.max(mh);
    let target_short = mw.min(mh);
    let src_long = raw_w.max(raw_h);
    let src_short = raw_w.min(raw_h);
    src_long >= target_long.saturating_mul(2) && src_short >= target_short.saturating_mul(2)
}

/// Decode `path`. When `max_size` is `Some((w, h))`, the result fits inside `w×h`
/// (use decoder-native scaling where possible: zune/WIC DCT scaling, `IWICBitmapScaler`).
/// Must be callable from any thread (initialise COM per thread on Windows, MTA).
pub fn decode_file(path: &Path, max_size: Option<(u32, u32)>) -> Result<DecodedImage, DecodeError> {
    let (n, header) = read_header(path)?;
    if n == 0 {
        return Err(DecodeError::Corrupt("file is empty".into()));
    }

    let is_jpeg = jpeg::is_jpeg_magic(&header[..n]);

    #[cfg(windows)]
    {
        if is_jpeg {
            if prefer_wic_for_jpeg(path, max_size) {
                match wic::decode_file_wic(path, max_size) {
                    Ok(img) => return Ok(img),
                    Err(_) => return jpeg::decode_jpeg_file(path, max_size),
                }
            }
            match jpeg::decode_jpeg_file(path, max_size) {
                Ok(img) => return Ok(img),
                Err(_) => return wic::decode_file_wic(path, max_size),
            }
        }
        wic::decode_file_wic(path, max_size)
    }

    #[cfg(not(windows))]
    {
        if is_jpeg {
            return jpeg::decode_jpeg_file(path, max_size);
        }
        portable::decode_file_portable(path, &header[..n], max_size)
    }
}
