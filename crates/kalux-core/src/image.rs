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
        self.frames
            .iter()
            .fold(0usize, |acc, f| acc.saturating_add(f.pixels.len()))
    }

    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// True when `width/height` are smaller than `source_width/source_height`.
    pub fn is_downscaled(&self) -> bool {
        self.width < self.source_width || self.height < self.source_height
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
        let ext = path.extension()?.to_str()?;
        let mut buf = [0u8; 8];
        if ext.len() > buf.len() {
            return None;
        }
        for (dst, &src) in buf.iter_mut().zip(ext.as_bytes()) {
            *dst = src.to_ascii_lowercase();
        }
        match &buf[..ext.len()] {
            b"jpg" | b"jpeg" | b"jfif" | b"jpe" => Some(Self::Jpeg),
            b"png" => Some(Self::Png),
            b"gif" => Some(Self::Gif),
            b"bmp" | b"dib" => Some(Self::Bmp),
            b"ico" => Some(Self::Ico),
            b"webp" => Some(Self::WebP),
            b"heic" | b"heif" => Some(Self::Heif),
            b"avif" => Some(Self::Avif),
            b"tif" | b"tiff" => Some(Self::Tiff),
            b"jxl" => Some(Self::Jxl),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_image(
        width: u32,
        height: u32,
        source_width: u32,
        source_height: u32,
        frame_count: usize,
    ) -> DecodedImage {
        let len = (width as usize) * (height as usize) * 4;
        let frames = (0..frame_count)
            .map(|_| Frame {
                pixels: vec![0u8; len],
                delay_ms: if frame_count > 1 { 100 } else { 0 },
            })
            .collect();
        DecodedImage {
            width,
            height,
            source_width,
            source_height,
            format: ImageFormat::Png,
            frames,
            loop_count: 0,
        }
    }

    #[test]
    fn byte_size_sums_all_frames() {
        let still = sample_image(10, 20, 10, 20, 1);
        assert_eq!(still.byte_size(), 10 * 20 * 4);

        let anim = sample_image(10, 20, 10, 20, 3);
        assert_eq!(anim.byte_size(), 3 * 10 * 20 * 4);
    }

    #[test]
    fn is_animated_checks_frame_count() {
        assert!(!sample_image(4, 4, 4, 4, 0).is_animated());
        assert!(!sample_image(4, 4, 4, 4, 1).is_animated());
        assert!(sample_image(4, 4, 4, 4, 2).is_animated());
    }

    #[test]
    fn is_downscaled_checks_both_axes() {
        assert!(!sample_image(100, 100, 100, 100, 1).is_downscaled());
        assert!(sample_image(50, 100, 100, 100, 1).is_downscaled());
        assert!(sample_image(100, 50, 100, 100, 1).is_downscaled());
        assert!(sample_image(50, 50, 100, 100, 1).is_downscaled());
    }

    #[test]
    fn every_supported_extension_maps_case_insensitively() {
        for &ext in SUPPORTED_EXTENSIONS {
            let lower = format!("photo.{ext}");
            let upper = format!("photo.{}", ext.to_ascii_uppercase());
            let mixed = format!(
                "dir/sub/photo.{}{}",
                ext[..1].to_ascii_uppercase(),
                &ext[1..]
            );
            let f_lower = ImageFormat::from_path(Path::new(&lower));
            assert!(f_lower.is_some(), "missing mapping for {ext}");
            assert_eq!(ImageFormat::from_path(Path::new(&upper)), f_lower);
            assert_eq!(ImageFormat::from_path(Path::new(&mixed)), f_lower);
        }
    }

    #[test]
    fn specific_extension_mappings_and_unknowns() {
        assert_eq!(
            ImageFormat::from_path(Path::new("a.JPG")),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            ImageFormat::from_path(Path::new("a.jfif")),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(
            ImageFormat::from_path(Path::new("a.dib")),
            Some(ImageFormat::Bmp)
        );
        assert_eq!(
            ImageFormat::from_path(Path::new("a.HEIC")),
            Some(ImageFormat::Heif)
        );
        assert_eq!(
            ImageFormat::from_path(Path::new("a.tif")),
            Some(ImageFormat::Tiff)
        );
        assert_eq!(
            ImageFormat::from_path(Path::new("a.jxl")),
            Some(ImageFormat::Jxl)
        );
        assert_eq!(ImageFormat::from_path(Path::new("readme.txt")), None);
        assert_eq!(ImageFormat::from_path(Path::new("no_extension")), None);
        assert_eq!(ImageFormat::from_path(Path::new("file.verylongext")), None);
    }
}
