//! EXIF orientation parsing and BGRA8 pixel transforms (all 8 orientations).

use std::io::Cursor;

/// Standard EXIF orientation values (TIFF tag 274).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    /// 1: Normal (top-left).
    #[default]
    Normal,
    /// 2: Flip horizontally.
    FlipHorizontal,
    /// 3: Rotate 180°.
    Rotate180,
    /// 4: Flip vertically.
    FlipVertical,
    /// 5: Transpose (swap X and Y; rotate 90° CW then flip horizontally).
    Transpose,
    /// 6: Rotate 90° clockwise.
    Rotate90,
    /// 7: Transverse (rotate 270° CW then flip horizontally).
    Transverse,
    /// 8: Rotate 270° clockwise (90° CCW).
    Rotate270,
}

impl Orientation {
    /// Convert raw EXIF tag value (1..=8) to `Orientation`. Unknown values map to `Normal`.
    pub fn from_exif(value: u32) -> Self {
        match value {
            2 => Self::FlipHorizontal,
            3 => Self::Rotate180,
            4 => Self::FlipVertical,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => Self::Normal,
        }
    }

    /// True if width and height are swapped after applying this orientation (orientations 5..=8).
    pub fn swaps_axes(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270
        )
    }

    /// Output dimensions after applying this orientation (involution for swapping axes).
    pub fn dimensions(self, width: u32, height: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// Apply this orientation to a tightly-packed BGRA8 buffer (`width * height * 4` bytes).
    /// Returns `(pixels, out_width, out_height)`.
    pub fn apply_bgra(self, src: Vec<u8>, width: u32, height: u32) -> (Vec<u8>, u32, u32) {
        if self == Self::Normal || width == 0 || height == 0 {
            return (src, width, height);
        }

        let w = width as usize;
        let h = height as usize;
        let expected = w.saturating_mul(h).saturating_mul(4);
        if src.len() < expected {
            return (src, width, height);
        }

        let (out_w, out_h) = self.dimensions(width, height);
        let ow = out_w as usize;
        let mut dst = vec![0u8; expected];

        match self {
            Self::Normal => unreachable!(),
            Self::FlipHorizontal => {
                for y in 0..h {
                    let src_row = &src[y * w * 4..(y + 1) * w * 4];
                    let dst_row = &mut dst[y * w * 4..(y + 1) * w * 4];
                    for x in 0..w {
                        let s = x * 4;
                        let d = (w - 1 - x) * 4;
                        dst_row[d..d + 4].copy_from_slice(&src_row[s..s + 4]);
                    }
                }
            }
            Self::Rotate180 => {
                for y in 0..h {
                    let src_row = &src[y * w * 4..(y + 1) * w * 4];
                    let dst_row = &mut dst[(h - 1 - y) * w * 4..(h - y) * w * 4];
                    for x in 0..w {
                        let s = x * 4;
                        let d = (w - 1 - x) * 4;
                        dst_row[d..d + 4].copy_from_slice(&src_row[s..s + 4]);
                    }
                }
            }
            Self::FlipVertical => {
                let row_bytes = w * 4;
                for y in 0..h {
                    let s = y * row_bytes;
                    let d = (h - 1 - y) * row_bytes;
                    dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
                }
            }
            Self::Transpose => {
                // (x, y) -> (y, x)
                for y in 0..h {
                    let src_row = &src[y * w * 4..(y + 1) * w * 4];
                    for x in 0..w {
                        let s = x * 4;
                        let d = (x * ow + y) * 4;
                        dst[d..d + 4].copy_from_slice(&src_row[s..s + 4]);
                    }
                }
            }
            Self::Rotate90 => {
                // (x, y) -> (h - 1 - y, x)
                for y in 0..h {
                    let src_row = &src[y * w * 4..(y + 1) * w * 4];
                    let dx = h - 1 - y;
                    for x in 0..w {
                        let s = x * 4;
                        let d = (x * ow + dx) * 4;
                        dst[d..d + 4].copy_from_slice(&src_row[s..s + 4]);
                    }
                }
            }
            Self::Transverse => {
                // (x, y) -> (h - 1 - y, w - 1 - x)
                for y in 0..h {
                    let src_row = &src[y * w * 4..(y + 1) * w * 4];
                    let dx = h - 1 - y;
                    for x in 0..w {
                        let s = x * 4;
                        let dy = w - 1 - x;
                        let d = (dy * ow + dx) * 4;
                        dst[d..d + 4].copy_from_slice(&src_row[s..s + 4]);
                    }
                }
            }
            Self::Rotate270 => {
                // (x, y) -> (y, w - 1 - x)
                for y in 0..h {
                    let src_row = &src[y * w * 4..(y + 1) * w * 4];
                    for x in 0..w {
                        let s = x * 4;
                        let dy = w - 1 - x;
                        let d = (dy * ow + y) * 4;
                        dst[d..d + 4].copy_from_slice(&src_row[s..s + 4]);
                    }
                }
            }
        }

        (dst, out_w, out_h)
    }
}

/// Read EXIF orientation from a JPEG (or TIFF/PNG/HEIF container supported by `kamadak-exif`).
/// Returns `Orientation::Normal` if no EXIF block or Orientation tag is present.
pub fn read_exif_orientation(bytes: &[u8]) -> Orientation {
    let mut cursor = Cursor::new(bytes);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor) else {
        return Orientation::Normal;
    };
    let Some(field) = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY) else {
        return Orientation::Normal;
    };
    field
        .value
        .get_uint(0)
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 3×2 BGRA image where pixel (x, y) has B = x + 1, G = y + 1, R = x * 10 + y, A = 255.
    fn make_3x2() -> Vec<u8> {
        // Layout (w=3, h=2):
        // (0,0)=A  (1,0)=B  (2,0)=C
        // (0,1)=D  (1,1)=E  (2,1)=F
        let ids: [u8; 6] = [1, 2, 3, 4, 5, 6];
        let mut out = Vec::with_capacity(6 * 4);
        for id in ids {
            out.extend_from_slice(&[id, id * 10, id * 20, 255]);
        }
        out
    }

    fn pixel_ids(buf: &[u8]) -> Vec<u8> {
        buf.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
    }

    #[test]
    fn all_eight_orientations_match_exif_spec() {
        // Original 3x2:
        // [1, 2, 3]
        // [4, 5, 6]
        let (p, w, h) = Orientation::Normal.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (3, 2));
        assert_eq!(pixel_ids(&p), vec![1, 2, 3, 4, 5, 6]);

        // 2: FlipHorizontal ->
        // [3, 2, 1]
        // [6, 5, 4]
        let (p, w, h) = Orientation::FlipHorizontal.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (3, 2));
        assert_eq!(pixel_ids(&p), vec![3, 2, 1, 6, 5, 4]);

        // 3: Rotate180 ->
        // [6, 5, 4]
        // [3, 2, 1]
        let (p, w, h) = Orientation::Rotate180.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (3, 2));
        assert_eq!(pixel_ids(&p), vec![6, 5, 4, 3, 2, 1]);

        // 4: FlipVertical ->
        // [4, 5, 6]
        // [1, 2, 3]
        let (p, w, h) = Orientation::FlipVertical.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (3, 2));
        assert_eq!(pixel_ids(&p), vec![4, 5, 6, 1, 2, 3]);

        // 5: Transpose (2x3) ->
        // [1, 4]
        // [2, 5]
        // [3, 6]
        let (p, w, h) = Orientation::Transpose.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (2, 3));
        assert_eq!(pixel_ids(&p), vec![1, 4, 2, 5, 3, 6]);

        // 6: Rotate90 CW (2x3) ->
        // [4, 1]
        // [5, 2]
        // [6, 3]
        let (p, w, h) = Orientation::Rotate90.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (2, 3));
        assert_eq!(pixel_ids(&p), vec![4, 1, 5, 2, 6, 3]);

        // 7: Transverse (2x3) ->
        // [6, 3]
        // [5, 2]
        // [4, 1]
        let (p, w, h) = Orientation::Transverse.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (2, 3));
        assert_eq!(pixel_ids(&p), vec![6, 3, 5, 2, 4, 1]);

        // 8: Rotate270 CW / 90 CCW (2x3) ->
        // [3, 6]
        // [2, 5]
        // [1, 4]
        let (p, w, h) = Orientation::Rotate270.apply_bgra(make_3x2(), 3, 2);
        assert_eq!((w, h), (2, 3));
        assert_eq!(pixel_ids(&p), vec![3, 6, 2, 5, 1, 4]);
    }

    #[test]
    fn from_exif_handles_unknown_values() {
        assert_eq!(Orientation::from_exif(0), Orientation::Normal);
        assert_eq!(Orientation::from_exif(1), Orientation::Normal);
        assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
        assert_eq!(Orientation::from_exif(99), Orientation::Normal);
    }
}
