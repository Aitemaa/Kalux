//! Deterministic JPEG generator for `--gen-test-image <out.jpg> <w> <h>` on Linux.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use jpeg_encoder::{ColorType, Encoder};

/// Generate a deterministic gradient + checkerboard JPEG (identical pattern to Windows WIC).
pub fn generate_test_jpeg(out_path: &Path, width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("image dimensions must be non-zero".to_string());
    }
    let w = u16::try_from(width).map_err(|_| "width exceeds 65535".to_string())?;
    let h = u16::try_from(height).map_err(|_| "height exceeds 65535".to_string())?;

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| format!("create output directory: {e}"))?;
    }

    let mut rgb = vec![0u8; (width as usize) * (height as usize) * 3];
    for y in 0..height {
        let fy = ((y * 255) / height.max(1)) as u8;
        let row_off = (y as usize) * (width as usize) * 3;
        for x in 0..width {
            let fx = ((x * 255) / width.max(1)) as u8;
            let checker = if ((x / 64) + (y / 64)).is_multiple_of(2) {
                24u8
            } else {
                0u8
            };
            let o = row_off + (x as usize) * 3;
            rgb[o] = fx.saturating_add(checker);
            rgb[o + 1] = fy.saturating_add(checker / 2);
            rgb[o + 2] = (255 - fx).saturating_add(checker / 2);
        }
    }

    let file = File::create(out_path).map_err(|e| format!("create {}: {e}", out_path.display()))?;
    let writer = BufWriter::new(file);
    let encoder = Encoder::new(writer, 88);
    encoder
        .encode(&rgb, w, h, ColorType::Rgb)
        .map_err(|e| format!("encode JPEG: {e}"))
}
