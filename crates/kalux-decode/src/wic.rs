//! Windows Imaging Component (WIC) backend for non-JPEG formats, DCT-scaled large JPEGs,
//! and animated GIF / animated WebP decoding.

use std::cell::RefCell;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use kalux_core::{DecodedImage, Frame, ImageFormat};
use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, GENERIC_READ, RPC_E_CHANGED_MODE, S_FALSE, S_OK,
    WINCODEC_ERR_COMPONENTNOTFOUND,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatBmp, GUID_ContainerFormatGif,
    GUID_ContainerFormatHeif, GUID_ContainerFormatIco, GUID_ContainerFormatJpeg,
    GUID_ContainerFormatPng, GUID_ContainerFormatTiff, GUID_ContainerFormatWebp,
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder, IWICBitmapFrameDecode, IWICBitmapSource,
    IWICImagingFactory, IWICMetadataQueryReader, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICBitmapTransformFlipHorizontal,
    WICBitmapTransformFlipVertical, WICBitmapTransformOptions, WICBitmapTransformRotate0,
    WICBitmapTransformRotate90, WICBitmapTransformRotate180, WICBitmapTransformRotate270,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::{VT_I4, VT_UI1, VT_UI2, VT_UI4, VT_VECTOR};
use windows::core::{GUID, HRESULT, Interface, PCWSTR};

use crate::DecodeError;
use crate::anim::{
    AnimatedCompositor, Disposal, FrameMeta, MAX_ANIMATED_BYTES, capped_animation_dimensions,
    normalize_gif_delay_ms, parse_netscape_loop_count,
};
use crate::orient::Orientation;
use crate::scale::fit_dimensions;

/// RAII guard for per-thread COM initialization.
struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn init_mta() -> Result<Self, DecodeError> {
        // SAFETY: Initializing COM on the current thread with MTA; balanced in Drop if S_OK/S_FALSE.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self {
                should_uninit: true,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            // Thread already initialized COM (e.g. STA on the UI thread); do not call CoUninitialize.
            Ok(Self {
                should_uninit: false,
            })
        } else {
            Err(DecodeError::Io(format!(
                "CoInitializeEx failed: 0x{:08X}",
                hr.0 as u32
            )))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            // SAFETY: Paired with a successful S_OK/S_FALSE CoInitializeEx on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Per-thread WIC factory + COM guard.
/// Field order is load-bearing: `factory` is dropped before `com_guard` calls `CoUninitialize`.
struct ThreadWic {
    factory: IWICImagingFactory,
    _com_guard: ComGuard,
}

thread_local! {
    static THREAD_WIC: RefCell<Option<ThreadWic>> = const { RefCell::new(None) };
}

fn with_factory<T>(
    f: impl FnOnce(&IWICImagingFactory) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
    THREAD_WIC.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            let com_guard = ComGuard::init_mta()?;
            // SAFETY: COM is initialized on this thread; CLSID_WICImagingFactory is an in-proc COM class.
            let factory: IWICImagingFactory = unsafe {
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
            }
            .map_err(|e| DecodeError::Io(format!("failed to create WICImagingFactory: {e}")))?;
            *slot = Some(ThreadWic {
                factory,
                _com_guard: com_guard,
            });
        }
        let state = slot
            .as_ref()
            .ok_or_else(|| DecodeError::Io("WIC thread state unexpectedly uninitialized".into()))?;
        f(&state.factory)
    })
}

/// RAII wrapper around Win32 `PROPVARIANT` ensuring `PropVariantClear` runs on drop.
struct PropVar(PROPVARIANT);

impl PropVar {
    fn new() -> Self {
        Self(PROPVARIANT::default())
    }

    fn as_mut_ptr(&mut self) -> *mut PROPVARIANT {
        &mut self.0
    }

    fn to_u32(&self) -> Option<u32> {
        // SAFETY: Inspecting the discriminant `vt` before reading the matching union field.
        unsafe {
            let inner = &*self.0.Anonymous.Anonymous;
            match inner.vt {
                VT_UI1 => Some(u32::from(inner.Anonymous.bVal)),
                VT_UI2 => Some(u32::from(inner.Anonymous.uiVal)),
                VT_UI4 => Some(inner.Anonymous.ulVal),
                VT_I4 => u32::try_from(inner.Anonymous.lVal).ok(),
                _ => None,
            }
        }
    }

    fn to_bytes(&self) -> Option<Vec<u8>> {
        // SAFETY: Only reads `caub` when `vt == VT_UI1 | VT_VECTOR` and pointer is non-null.
        unsafe {
            let inner = &*self.0.Anonymous.Anonymous;
            if inner.vt.0 == (VT_UI1.0 | VT_VECTOR.0) {
                let caub = inner.Anonymous.caub;
                if caub.cElems == 0 {
                    return Some(Vec::new());
                }
                if !caub.pElems.is_null() {
                    let slice = std::slice::from_raw_parts(caub.pElems, caub.cElems as usize);
                    return Some(slice.to_vec());
                }
            }
            None
        }
    }
}

impl Drop for PropVar {
    fn drop(&mut self) {
        // SAFETY: Clearing an initialized or zeroed PROPVARIANT owned by `self`.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_to_wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn query_u32(reader: &IWICMetadataQueryReader, query: &str) -> Option<u32> {
    let wq = wide_null(query);
    let mut pv = PropVar::new();
    // SAFETY: `wq` is NUL-terminated UTF-16 and `pv` is a valid PROPVARIANT out-pointer.
    unsafe {
        reader
            .GetMetadataByName(PCWSTR(wq.as_ptr()), pv.as_mut_ptr())
            .ok()?
    };
    pv.to_u32()
}

fn query_bytes(reader: &IWICMetadataQueryReader, query: &str) -> Option<Vec<u8>> {
    let wq = wide_null(query);
    let mut pv = PropVar::new();
    // SAFETY: `wq` is NUL-terminated UTF-16 and `pv` is a valid PROPVARIANT out-pointer.
    unsafe {
        reader
            .GetMetadataByName(PCWSTR(wq.as_ptr()), pv.as_mut_ptr())
            .ok()?
    };
    pv.to_bytes()
}

fn read_wic_orientation(frame: &IWICBitmapFrameDecode) -> Orientation {
    // SAFETY: Standard WIC COM method returning an optional metadata query reader.
    let Ok(reader) = (unsafe { frame.GetMetadataQueryReader() }) else {
        return Orientation::Normal;
    };
    // Try Windows Property System canonical query first (works across JPEG, HEIF, AVIF, TIFF, WebP, RAW),
    // then explicit EXIF IFD tag 274 fallbacks.
    for query in [
        "System.Photo.Orientation",
        "/app1/ifd/{ushort=274}",
        "/xmp/tiff:Orientation",
        "/ifd/{ushort=274}",
    ] {
        if let Some(val) = query_u32(&reader, query) {
            return Orientation::from_exif(val);
        }
    }
    Orientation::Normal
}

fn wic_transform_for_orientation(orient: Orientation) -> WICBitmapTransformOptions {
    // In WIC, rotation is applied BEFORE horizontal/vertical flipping.
    match orient {
        Orientation::Normal => WICBitmapTransformRotate0,
        Orientation::FlipHorizontal => WICBitmapTransformFlipHorizontal,
        Orientation::Rotate180 => WICBitmapTransformRotate180,
        Orientation::FlipVertical => WICBitmapTransformFlipVertical,
        Orientation::Transpose => WICBitmapTransformOptions(
            WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        Orientation::Rotate90 => WICBitmapTransformRotate90,
        Orientation::Transverse => WICBitmapTransformOptions(
            WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
        ),
        Orientation::Rotate270 => WICBitmapTransformRotate270,
    }
}

fn format_from_container_guid(guid: GUID, path: &Path) -> ImageFormat {
    if guid == GUID_ContainerFormatJpeg {
        ImageFormat::Jpeg
    } else if guid == GUID_ContainerFormatPng {
        ImageFormat::Png
    } else if guid == GUID_ContainerFormatGif {
        ImageFormat::Gif
    } else if guid == GUID_ContainerFormatBmp {
        ImageFormat::Bmp
    } else if guid == GUID_ContainerFormatIco {
        ImageFormat::Ico
    } else if guid == GUID_ContainerFormatWebp {
        ImageFormat::WebP
    } else if guid == GUID_ContainerFormatHeif {
        // AVIF and HEIC can both report HEIF container on some codec versions; prefer extension if AVIF.
        if ImageFormat::from_path(path) == Some(ImageFormat::Avif) {
            ImageFormat::Avif
        } else {
            ImageFormat::Heif
        }
    } else if guid == GUID_ContainerFormatTiff {
        ImageFormat::Tiff
    } else {
        ImageFormat::from_path(path).unwrap_or(ImageFormat::Png)
    }
}

fn is_raw_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "cr2" | "cr3" | "nef" | "nrw" | "arw" | "dng" | "orf" | "rw2" | "raf" | "pef" | "srw"
    )
}

fn missing_codec_error(path: &Path) -> DecodeError {
    if is_raw_extension(path) {
        return DecodeError::Unsupported {
            hint: Some("Install Raw Image Extension from the Microsoft Store".into()),
        };
    }
    match ImageFormat::from_path(path) {
        Some(ImageFormat::Heif) => DecodeError::Unsupported {
            hint: Some("Install HEIF Image Extensions from the Microsoft Store".into()),
        },
        Some(ImageFormat::Avif) => DecodeError::Unsupported {
            hint: Some("Install AV1 Video Extension from the Microsoft Store".into()),
        },
        Some(ImageFormat::Jxl) => DecodeError::Unsupported {
            hint: Some("Install JPEG XL Image Extension from the Microsoft Store".into()),
        },
        Some(ImageFormat::WebP) => DecodeError::Unsupported {
            hint: Some("Install WebP Image Extensions from the Microsoft Store".into()),
        },
        Some(
            ImageFormat::Jpeg
            | ImageFormat::Png
            | ImageFormat::Gif
            | ImageFormat::Bmp
            | ImageFormat::Ico
            | ImageFormat::Tiff,
        ) => {
            // Built-in Windows codecs always exist for these formats; COMPONENTNOTFOUND means invalid header.
            DecodeError::Corrupt("unrecognized or corrupt image header".into())
        }
        None => DecodeError::Unsupported { hint: None },
    }
}

fn map_wic_error(err: windows::core::Error, path: &Path) -> DecodeError {
    let hr = err.code();
    let h_not_found = HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0);
    let h_path_not_found = HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0);
    if hr == h_not_found || hr == h_path_not_found {
        DecodeError::NotFound
    } else if hr == WINCODEC_ERR_COMPONENTNOTFOUND {
        missing_codec_error(path)
    } else {
        DecodeError::Corrupt(format!(
            "WIC error (0x{:08X}): {}",
            hr.0 as u32,
            err.message()
        ))
    }
}

/// Convert any `IWICBitmapSource` to tightly-packed 32bpp PBGRA bytes.
fn copy_source_pbgra(
    factory: &IWICImagingFactory,
    source: &IWICBitmapSource,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, DecodeError> {
    // SAFETY: Querying pixel format of a valid WIC bitmap source.
    let fmt = unsafe { source.GetPixelFormat() }
        .map_err(|e| DecodeError::Corrupt(format!("WIC GetPixelFormat failed: {e}")))?;

    let final_source: IWICBitmapSource = if fmt == GUID_WICPixelFormat32bppPBGRA {
        source.clone()
    } else {
        // SAFETY: Creating and initializing a WIC format converter targeting 32bppPBGRA.
        let converter = unsafe { factory.CreateFormatConverter() }
            .map_err(|e| DecodeError::Io(format!("CreateFormatConverter failed: {e}")))?;
        // SAFETY: Converting `source` to 32bpp premultiplied BGRA with no palette/dither.
        unsafe {
            converter.Initialize(
                source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .map_err(|e| DecodeError::Corrupt(format!("WIC FormatConverter failed: {e}")))?;
        converter
            .cast()
            .map_err(|e| DecodeError::Io(format!("FormatConverter cast failed: {e}")))?
    };

    let stride = width
        .checked_mul(4)
        .ok_or_else(|| DecodeError::Corrupt("image stride overflow".into()))?;
    let buf_len = (stride as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| DecodeError::Corrupt("image buffer size overflow".into()))?;
    let mut pixels = vec![0u8; buf_len];

    // SAFETY: `pixels` holds `stride * height` bytes; passing null rect copies the full image.
    unsafe { final_source.CopyPixels(std::ptr::null(), stride, &mut pixels) }
        .map_err(|e| DecodeError::Corrupt(format!("WIC CopyPixels failed: {e}")))?;
    Ok(pixels)
}

fn decode_still_frame(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    format: ImageFormat,
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    let mut raw_w = 0u32;
    let mut raw_h = 0u32;
    // SAFETY: Querying frame dimensions into valid stack pointers.
    unsafe { frame.GetSize(&mut raw_w, &mut raw_h) }
        .map_err(|e| DecodeError::Corrupt(format!("WIC GetSize failed: {e}")))?;
    if raw_w == 0 || raw_h == 0 {
        return Err(DecodeError::Corrupt("image has zero dimensions".into()));
    }

    let orient = read_wic_orientation(frame);
    let (source_width, source_height) = orient.dimensions(raw_w, raw_h);
    let (target_w, target_h) = match max_size {
        Some((mw, mh)) => fit_dimensions(source_width, source_height, mw, mh),
        None => (source_width, source_height),
    };
    let (raw_target_w, raw_target_h) = orient.dimensions(target_w, target_h);

    let mut source: IWICBitmapSource = frame
        .cast()
        .map_err(|e| DecodeError::Io(format!("Frame cast to IWICBitmapSource failed: {e}")))?;

    // Fast path: if downscaling is needed, place IWICBitmapScaler directly on the frame so
    // WIC automatically engages IWICBitmapSourceTransform (JPEG 1/2, 1/4, 1/8 DCT scaling).
    let needs_scale = raw_target_w < raw_w || raw_target_h < raw_h;
    if needs_scale {
        // SAFETY: Creating a WIC bitmap scaler from the factory.
        let scaler = unsafe { factory.CreateBitmapScaler() }
            .map_err(|e| DecodeError::Io(format!("CreateBitmapScaler failed: {e}")))?;
        // SAFETY: Initializing scaler directly on `source` (frame) to preserve DCT scaling fast path.
        let init_res = unsafe {
            scaler.Initialize(
                &source,
                raw_target_w,
                raw_target_h,
                WICBitmapInterpolationModeFant,
            )
        };
        if init_res.is_ok() {
            source = scaler
                .cast()
                .map_err(|e| DecodeError::Io(format!("Scaler cast failed: {e}")))?;
        } else {
            // Some rare pixel formats cannot be scaled before format conversion; convert first then scale.
            // SAFETY: Creating a format converter to convert to 32bppPBGRA before scaling.
            let converter = unsafe { factory.CreateFormatConverter() }
                .map_err(|e| DecodeError::Io(format!("CreateFormatConverter failed: {e}")))?;
            // SAFETY: Converting `source` to 32bppPBGRA.
            unsafe {
                converter.Initialize(
                    &source,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
            }
            .map_err(|e| DecodeError::Corrupt(format!("WIC FormatConverter failed: {e}")))?;
            // SAFETY: Scaling the converted 32bppPBGRA bitmap.
            unsafe {
                scaler.Initialize(
                    &converter,
                    raw_target_w,
                    raw_target_h,
                    WICBitmapInterpolationModeFant,
                )
            }
            .map_err(|e| DecodeError::Corrupt(format!("WIC BitmapScaler failed: {e}")))?;
            source = scaler
                .cast()
                .map_err(|e| DecodeError::Io(format!("Scaler cast failed: {e}")))?;
        }
    }

    // Apply EXIF orientation on the already-downscaled source.
    if orient != Orientation::Normal {
        // SAFETY: Creating a WIC flip/rotator from the factory.
        let rotator = unsafe { factory.CreateBitmapFlipRotator() }
            .map_err(|e| DecodeError::Io(format!("CreateBitmapFlipRotator failed: {e}")))?;
        // SAFETY: Applying EXIF orientation transform to `source`.
        unsafe { rotator.Initialize(&source, wic_transform_for_orientation(orient)) }
            .map_err(|e| DecodeError::Corrupt(format!("WIC FlipRotator failed: {e}")))?;
        source = rotator
            .cast()
            .map_err(|e| DecodeError::Io(format!("FlipRotator cast failed: {e}")))?;
    }

    let mut out_w = 0u32;
    let mut out_h = 0u32;
    // SAFETY: Querying final output dimensions after scaling and orientation.
    unsafe { source.GetSize(&mut out_w, &mut out_h) }
        .map_err(|e| DecodeError::Corrupt(format!("WIC GetSize failed: {e}")))?;

    let pixels = copy_source_pbgra(factory, &source, out_w, out_h)?;

    Ok(DecodedImage {
        width: out_w,
        height: out_h,
        source_width,
        source_height,
        format,
        frames: vec![Frame {
            pixels,
            delay_ms: 0,
        }],
        loop_count: 0,
    })
}

fn decode_animated(
    factory: &IWICImagingFactory,
    decoder: &IWICBitmapDecoder,
    frame_count: u32,
    format: ImageFormat,
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    // Query container-level logical screen dimensions and loop count (GIF NETSCAPE2.0 / WebP).
    // SAFETY: Querying optional container metadata reader from WIC decoder.
    let container_meta = unsafe { decoder.GetMetadataQueryReader() }.ok();

    let mut canvas_w = container_meta
        .as_ref()
        .and_then(|r| query_u32(r, "/logscrdesc/Width"))
        .unwrap_or(0);
    let mut canvas_h = container_meta
        .as_ref()
        .and_then(|r| query_u32(r, "/logscrdesc/Height"))
        .unwrap_or(0);

    let loop_count = if format == ImageFormat::Gif {
        container_meta
            .as_ref()
            .and_then(|r| {
                let app = query_bytes(r, "/appext/application")?;
                let data = query_bytes(r, "/appext/data")?;
                parse_netscape_loop_count(&app, &data)
            })
            .unwrap_or(1)
    } else {
        // Animated WebP: default to infinite loop if loop count metadata is absent.
        container_meta
            .as_ref()
            .and_then(|r| query_u32(r, "/anim/LoopCount"))
            .unwrap_or(0)
    };

    // Fallback to frame 0 size if container logical screen dimensions are missing.
    if canvas_w == 0 || canvas_h == 0 {
        // SAFETY: Frame 0 exists because `frame_count > 1`.
        let f0 = unsafe { decoder.GetFrame(0) }
            .map_err(|e| DecodeError::Corrupt(format!("WIC GetFrame(0) failed: {e}")))?;
        let mut w0 = 0u32;
        let mut h0 = 0u32;
        // SAFETY: Querying frame 0 dimensions.
        unsafe { f0.GetSize(&mut w0, &mut h0) }
            .map_err(|e| DecodeError::Corrupt(format!("WIC GetSize failed: {e}")))?;
        if canvas_w == 0 {
            canvas_w = w0;
        }
        if canvas_h == 0 {
            canvas_h = h0;
        }
    }

    if canvas_w == 0 || canvas_h == 0 {
        return Err(DecodeError::Corrupt(
            "animated image has zero dimensions".into(),
        ));
    }

    let (out_w, out_h) = capped_animation_dimensions(
        canvas_w,
        canvas_h,
        frame_count,
        max_size,
        MAX_ANIMATED_BYTES,
    );
    let mut compositor =
        AnimatedCompositor::new(canvas_w, canvas_h, out_w, out_h, frame_count as usize);

    for idx in 0..frame_count {
        // SAFETY: `idx < frame_count`.
        let frame = unsafe { decoder.GetFrame(idx) }
            .map_err(|e| DecodeError::Corrupt(format!("WIC GetFrame({idx}) failed: {e}")))?;
        let mut fw = 0u32;
        let mut fh = 0u32;
        // SAFETY: Querying frame dimensions into stack pointers.
        unsafe { frame.GetSize(&mut fw, &mut fh) }
            .map_err(|e| DecodeError::Corrupt(format!("WIC GetSize failed: {e}")))?;

        // SAFETY: Querying optional per-frame metadata reader.
        let meta_reader = unsafe { frame.GetMetadataQueryReader() }.ok();

        let (left, top, delay_ms, disposal) = if format == ImageFormat::Gif {
            let left = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/imgdesc/Left"))
                .unwrap_or(0);
            let top = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/imgdesc/Top"))
                .unwrap_or(0);
            let delay_cs = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/grctlext/Delay"))
                .unwrap_or(0) as u16;
            let disp_code = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/grctlext/Disposal"))
                .unwrap_or(0) as u8;
            (
                left,
                top,
                normalize_gif_delay_ms(delay_cs),
                Disposal::from_gif_code(disp_code),
            )
        } else {
            // Animated WebP metadata fallback (if WIC exposes multiple WebP frames).
            let left = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/anmf/FrameX"))
                .unwrap_or(0);
            let top = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/anmf/FrameY"))
                .unwrap_or(0);
            let raw_ms = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/anmf/FrameDuration"))
                .unwrap_or(100);
            let delay_ms = if raw_ms <= 10 { 100 } else { raw_ms };
            let disp_code = meta_reader
                .as_ref()
                .and_then(|r| query_u32(r, "/anmf/DisposeToBackgroundColor"))
                .map(|v| if v != 0 { 2u8 } else { 1u8 })
                .unwrap_or(1);
            (left, top, delay_ms, Disposal::from_gif_code(disp_code))
        };

        let source: IWICBitmapSource = frame
            .cast()
            .map_err(|e| DecodeError::Io(format!("Frame cast failed: {e}")))?;
        let raw_pixels = copy_source_pbgra(factory, &source, fw, fh)?;

        compositor.push_frame(
            &raw_pixels,
            FrameMeta {
                left,
                top,
                width: fw,
                height: fh,
                delay_ms,
                disposal,
            },
        );
    }

    Ok(DecodedImage {
        width: out_w,
        height: out_h,
        source_width: canvas_w,
        source_height: canvas_h,
        format,
        frames: compositor.finish(),
        loop_count,
    })
}

/// Decode `path` through WIC (handles PNG, GIF, BMP, ICO, WebP, TIFF, HEIF, AVIF, JXL, RAW,
/// and DCT-scaled JPEGs).
pub fn decode_file_wic(
    path: &Path,
    max_size: Option<(u32, u32)>,
) -> Result<DecodedImage, DecodeError> {
    with_factory(|factory| {
        let wide_path = path_to_wide(path);
        // SAFETY: `wide_path` is a NUL-terminated UTF-16 filename; read-only decode.
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                PCWSTR(wide_path.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        }
        .map_err(|e| map_wic_error(e, path))?;

        // SAFETY: Querying container GUID from a valid WIC decoder.
        let container_guid =
            unsafe { decoder.GetContainerFormat() }.map_err(|e| map_wic_error(e, path))?;
        let format = format_from_container_guid(container_guid, path);

        // SAFETY: Querying frame count from a valid WIC decoder.
        let frame_count = unsafe { decoder.GetFrameCount() }.map_err(|e| map_wic_error(e, path))?;
        if frame_count == 0 {
            return Err(DecodeError::Corrupt("image contains zero frames".into()));
        }

        if frame_count > 1 && matches!(format, ImageFormat::Gif | ImageFormat::WebP) {
            return decode_animated(factory, &decoder, frame_count, format, max_size);
        }

        // SAFETY: `frame_count >= 1`, so frame 0 exists.
        let frame = unsafe { decoder.GetFrame(0) }.map_err(|e| map_wic_error(e, path))?;
        decode_still_frame(factory, &frame, format, max_size)
    })
}
