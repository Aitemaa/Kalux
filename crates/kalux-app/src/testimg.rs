//! Deterministic non-trivial JPEG test image generator using the Windows Imaging Component (WIC).

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
    IWICBitmapFrameEncode, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, STGC_DEFAULT};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{PCWSTR, PWSTR};

const STRIP_ROWS: u32 = 64;
const JPEG_QUALITY: f32 = 0.9;

/// Generate a deterministic gradient + noise JPEG at `out_path` with size `width × height`.
///
/// Requires COM to be initialized on the calling thread.
pub fn generate_test_jpeg(out_path: &Path, width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("Dimensions must be non-zero".to_string());
    }

    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {e}"))?;
    }

    let wide_path: Vec<u16> = out_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: COM is initialized by caller; creating standard in-proc WIC factory.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("Failed to create WIC factory: {e}"))?;

    // SAFETY: Valid COM factory.
    let stream = unsafe { factory.CreateStream() }
        .map_err(|e| format!("Failed to create WIC stream: {e}"))?;

    // SAFETY: `wide_path` is NUL-terminated UTF-16.
    unsafe { stream.InitializeFromFilename(PCWSTR(wide_path.as_ptr()), GENERIC_WRITE.0) }
        .map_err(|e| format!("Failed to open '{}' for writing: {e}", out_path.display()))?;

    // SAFETY: Standard JPEG container GUID; null vendor uses default WIC JPEG encoder.
    let encoder = unsafe { factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()) }
        .map_err(|e| format!("Failed to create WIC JPEG encoder: {e}"))?;

    // SAFETY: `stream` is initialized and writable.
    unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }
        .map_err(|e| format!("Failed to initialize JPEG encoder: {e}"))?;

    let mut frame_opt: Option<IWICBitmapFrameEncode> = None;
    let mut props_opt: Option<IPropertyBag2> = None;
    // SAFETY: Valid initialized encoder and out-pointers.
    unsafe { encoder.CreateNewFrame(&mut frame_opt, &mut props_opt) }
        .map_err(|e| format!("Failed to create JPEG frame: {e}"))?;

    let frame = frame_opt.ok_or_else(|| "WIC returned null frame encoder".to_string())?;

    if let Some(ref props) = props_opt {
        set_jpeg_quality(props, JPEG_QUALITY)?;
    }

    // SAFETY: `props_opt` was produced by `CreateNewFrame` for this frame.
    unsafe { frame.Initialize(props_opt.as_ref()) }
        .map_err(|e| format!("Failed to initialize JPEG frame: {e}"))?;

    // SAFETY: Valid initialized frame.
    unsafe { frame.SetSize(width, height) }.map_err(|e| format!("Failed to set JPEG size: {e}"))?;

    let mut pixel_format = GUID_WICPixelFormat24bppBGR;
    // SAFETY: Passing mutable pixel format GUID as required by WIC.
    unsafe { frame.SetPixelFormat(&mut pixel_format) }
        .map_err(|e| format!("Failed to set JPEG pixel format: {e}"))?;

    if pixel_format != GUID_WICPixelFormat24bppBGR {
        return Err("WIC JPEG encoder refused 24bppBGR pixel format".to_string());
    }

    let stride = width
        .checked_mul(3)
        .ok_or_else(|| "Image stride overflow".to_string())?;
    let mut strip = vec![0u8; (stride as usize) * (STRIP_ROWS.min(height) as usize)];
    let mut rng_state: u64 = 0x4B41_4C55_585F_3031;

    let mut y = 0u32;
    while y < height {
        let rows = STRIP_ROWS.min(height - y);
        let bytes = (rows as usize) * (stride as usize);
        fill_pattern_strip(&mut strip[..bytes], width, height, y, rows, &mut rng_state);

        // SAFETY: `strip[..bytes]` holds exactly `rows * stride` bytes of 24bppBGR pixels.
        unsafe { frame.WritePixels(rows, stride, &strip[..bytes]) }
            .map_err(|e| format!("Failed to write JPEG pixels at row {y}: {e}"))?;

        y += rows;
    }

    // SAFETY: All `height` scanlines have been written.
    unsafe { frame.Commit() }.map_err(|e| format!("Failed to commit JPEG frame: {e}"))?;
    // SAFETY: Frame is committed.
    unsafe { encoder.Commit() }.map_err(|e| format!("Failed to commit JPEG encoder: {e}"))?;
    // SAFETY: Encoder is committed.
    unsafe { stream.Commit(STGC_DEFAULT) }
        .map_err(|e| format!("Failed to flush JPEG stream: {e}"))?;

    Ok(())
}

fn set_jpeg_quality(props: &IPropertyBag2, quality: f32) -> Result<(), String> {
    let mut name: Vec<u16> = "ImageQuality"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let prop = PROPBAG2 {
        pstrName: PWSTR(name.as_mut_ptr()),
        ..Default::default()
    };
    let value = VARIANT::from(quality);
    // SAFETY: `prop` references live NUL-terminated UTF-16 `name` and valid `VARIANT` for this call.
    unsafe { props.Write(1, &prop, &value) }
        .map_err(|e| format!("Failed to set JPEG ImageQuality: {e}"))
}

/// Fill `rows` scanlines of 24bppBGR starting at `y_start` with multi-frequency gradients + noise
/// so JPEG compression sees realistic photographic entropy.
fn fill_pattern_strip(
    buf: &mut [u8],
    width: u32,
    height: u32,
    y_start: u32,
    rows: u32,
    rng: &mut u64,
) {
    let inv_w = 1.0 / (width.max(1) as f32);
    let inv_h = 1.0 / (height.max(1) as f32);
    let stride = (width as usize) * 3;

    for row in 0..rows {
        let y = y_start + row;
        let fy = y as f32 * inv_h;
        let row_slice = &mut buf[(row as usize) * stride..(row as usize + 1) * stride];
        let wave_y = ((y as f32 * 0.031).sin() * 28.0) as i32;

        for x in 0..width {
            let fx = x as f32 * inv_w;
            let wave_x = ((x as f32 * 0.023 + fy * 14.0).cos() * 24.0) as i32;
            let fine = (((x ^ y) & 31) as i32) - 16;

            // SplitMix64 step for fast deterministic high-frequency grain.
            *rng = rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *rng;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            let z = z ^ (z >> 31);

            let n_b = ((z & 0x1F) as i32) - 16;
            let n_g = (((z >> 8) & 0x1F) as i32) - 16;
            let n_r = (((z >> 16) & 0x1F) as i32) - 16;

            let base_r = (fx * 210.0 + fy * 30.0) as i32 + wave_x + fine / 2 + n_r;
            let base_g = ((1.0 - fx) * 140.0 + fy * 100.0) as i32 + wave_y + n_g;
            let base_b = (fy * 200.0 + (1.0 - fx) * 40.0) as i32 - wave_x / 2 - fine / 2 + n_b;

            let idx = (x as usize) * 3;
            row_slice[idx] = base_b.clamp(0, 255) as u8;
            row_slice[idx + 1] = base_g.clamp(0, 255) as u8;
            row_slice[idx + 2] = base_r.clamp(0, 255) as u8;
        }
    }
}
