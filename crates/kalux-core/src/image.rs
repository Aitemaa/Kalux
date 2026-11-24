//! Decoded image representation shared by the decoder and the renderer.

use std::path::Path;

/// One frame of pixels. Layout: **BGRA8, premultiplied alpha**, tightly packed
/// (`stride == width * 4`). This matches `DXGI_FORMAT_B8G8R8A8_UNORM` +
/// `D2D1_ALPHA_MODE_PREMULTIPLIED`, so the renderer can upload without conversion.
#[derive(Clone, Debug)]
pub struct Frame {
    pub pixels: Vec<u8>,
    /// Display duration for animated images; 0 for still images.
    pub delay_ms: u32,
}

/// A decoded, display-ready image (EXIF orientation already applied).
#[derive(Clone, Debug)]
pub struct DecodedImage {
    /// Dimensions of `frames[*].pixels` (may be smaller than the source when downscaled at decode time).
    pub width: u32,
    pub height: u32,
    /// Dimensions of the original file after orientation, used for the title bar and 100% zoom.
    pub source_width: u32,
    pub source_height: u32,
    pub format: ImageFormat,
    /// At least one frame. More than one means animated.
    pub frames: Vec<Frame>,
    /// 0 = loop forever.
    pub loop_count: u32,
}

impl DecodedImage {
    /// Total pixel bytes across frames (used for the byte-budget cache).
    pub fn byte_size(&self) -> usize {
        todo!("kalux-core: sum of frame pixel lengths")
    }

    pub fn is_animated(&self) -> bool {
        todo!("kalux-core: frames.len() > 1")
    }

    /// True when `width/height` are smaller than `source_width/source_height`.
    pub fn is_downscaled(&self) -> bool {
        todo!("kalux-core")
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Ico,
    WebP,
    Heif,
    Avif,
    Tiff,
    Jxl,
}

/// Lower-case extensions (without dot) Kalux lists in a folder playlist.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jfif", "jpe", "png", "gif", "bmp", "dib", "ico", "webp", "heic", "heif",
    "avif", "tif", "tiff", "jxl",
];

impl ImageFormat {
    /// Case-insensitive lookup by file extension. `None` if not an image Kalux handles.
    pub fn from_path(path: &Path) -> Option<Self> {
        let _ = path;
        todo!("kalux-core")
    }
}
