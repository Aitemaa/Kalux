//! Lossless JPEG EXIF orientation updater (Edit Mode PRD §5.4 Fast Path A).
//!
//! When the user only rotates 90° and/or flips a JPEG (no crop, no tone edits), we update
//! the 2-byte EXIF Orientation tag directly without decoding or re-encoding DCT coefficients.
//! If the JPEG has no APP1 EXIF segment, a minimal valid 32-byte APP1 Exif header is inserted
//! right after the SOI (`FF D8`) marker — still 100% lossless with zero pixel re-encoding.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use kalux_core::QuarterTurns;

use crate::orient::{Orientation, read_exif_orientation};

/// Convert an [`Orientation`] to its TIFF EXIF tag 274 value (`1..=8`).
pub const fn orientation_to_exif(orient: Orientation) -> u16 {
    match orient {
        Orientation::Normal => 1,
        Orientation::FlipHorizontal => 2,
        Orientation::Rotate180 => 3,
        Orientation::FlipVertical => 4,
        Orientation::Transpose => 5,
        Orientation::Rotate90 => 6,
        Orientation::Transverse => 7,
        Orientation::Rotate270 => 8,
    }
}

/// Represent an orientation as a 2×2 signed permutation matrix acting on centered coordinates
/// `(x, y)` with +X right and +Y down.
const fn orient_to_matrix(o: Orientation) -> [i8; 4] {
    match o {
        Orientation::Normal => [1, 0, 0, 1],
        Orientation::FlipHorizontal => [-1, 0, 0, 1],
        Orientation::Rotate180 => [-1, 0, 0, -1],
        Orientation::FlipVertical => [1, 0, 0, -1],
        // (x, y) -> (y, x)
        Orientation::Transpose => [0, 1, 1, 0],
        // (x, y) -> (-y, x)
        Orientation::Rotate90 => [0, -1, 1, 0],
        // (x, y) -> (-y, -x)
        Orientation::Transverse => [0, -1, -1, 0],
        // (x, y) -> (y, -x)
        Orientation::Rotate270 => [0, 1, -1, 0],
    }
}

const fn matrix_to_orient(m: [i8; 4]) -> Orientation {
    match m {
        [1, 0, 0, 1] => Orientation::Normal,
        [-1, 0, 0, 1] => Orientation::FlipHorizontal,
        [-1, 0, 0, -1] => Orientation::Rotate180,
        [1, 0, 0, -1] => Orientation::FlipVertical,
        [0, 1, 1, 0] => Orientation::Transpose,
        [0, -1, 1, 0] => Orientation::Rotate90,
        [0, -1, -1, 0] => Orientation::Transverse,
        [0, 1, -1, 0] => Orientation::Rotate270,
        _ => Orientation::Normal,
    }
}

const fn mat_mul(b: [i8; 4], a: [i8; 4]) -> [i8; 4] {
    [
        b[0] * a[0] + b[1] * a[2],
        b[0] * a[1] + b[1] * a[3],
        b[2] * a[0] + b[3] * a[2],
        b[2] * a[1] + b[3] * a[3],
    ]
}

/// Compose an existing EXIF [`Orientation`] with user edit transforms `(turns, flip_h, flip_v)`
/// applied in EditRecipe order: first rotate CW by `turns`, then `flip_h`, then `flip_v`.
pub const fn compose_orientation(
    existing: Orientation,
    turns: QuarterTurns,
    flip_h: bool,
    flip_v: bool,
) -> Orientation {
    let mut m = orient_to_matrix(existing);
    let rot = match turns {
        QuarterTurns::R0 => [1, 0, 0, 1],
        QuarterTurns::R90 => [0, -1, 1, 0],
        QuarterTurns::R180 => [-1, 0, 0, -1],
        QuarterTurns::R270 => [0, 1, -1, 0],
    };
    m = mat_mul(rot, m);
    if flip_h {
        m = mat_mul([-1, 0, 0, 1], m);
    }
    if flip_v {
        m = mat_mul([1, 0, 0, -1], m);
    }
    matrix_to_orient(m)
}

/// Rewrite or insert the EXIF Orientation tag in raw JPEG bytes without touching scan data.
/// Returns `Ok(new_bytes)` on success or `Err(reason)` if `bytes` is not a valid JPEG.
pub fn patch_jpeg_exif_orientation(
    bytes: &[u8],
    turns: QuarterTurns,
    flip_h: bool,
    flip_v: bool,
) -> Result<Vec<u8>, String> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err("Not a JPEG file".to_string());
    }

    let current = read_exif_orientation(bytes);
    let target = compose_orientation(current, turns, flip_h, flip_v);
    let target_val = orientation_to_exif(target);

    // Scan JPEG markers until SOS (0xDA) looking for an APP1 (0xE1) Exif\0\0 segment.
    let mut pos = 2usize;
    let mut app1_range: Option<(usize, usize)> = None;

    while pos + 4 <= bytes.len() {
        if bytes[pos] != 0xFF {
            break;
        }
        // Skip fill 0xFF bytes.
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let marker = bytes[pos];
        pos += 1;

        // SOS (0xDA) or EOI (0xD9) ends the header segment list.
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        // Standalone markers (RST0..7, TEM).
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if pos + 2 > bytes.len() {
            break;
        }
        let seg_len = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        if seg_len < 2 || pos + seg_len > bytes.len() {
            break;
        }
        let payload_start = pos + 2;
        let payload_end = pos + seg_len;

        if marker == 0xE1
            && payload_end - payload_start >= 14
            && &bytes[payload_start..payload_start + 6] == b"Exif\0\0"
        {
            app1_range = Some((payload_start, payload_end));
            break;
        }
        pos = payload_end;
    }

    // Case 1: Existing APP1 Exif segment contains an Orientation (0x0112) SHORT tag -> patch 2 bytes in place!
    if let Some((p_start, p_end)) = app1_range
        && let Some((val_offset, big_endian)) =
            find_orientation_value_offset(&bytes[p_start + 6..p_end])
    {
        let abs = p_start + 6 + val_offset;
        let mut out = bytes.to_vec();
        let encoded = if big_endian {
            target_val.to_be_bytes()
        } else {
            target_val.to_le_bytes()
        };
        out[abs] = encoded[0];
        out[abs + 1] = encoded[1];
        return Ok(out);
    }

    // Case 2: No APP1 Exif segment (or APP1 lacked an Orientation tag) -> insert a minimal
    // 32-byte APP1 Exif segment right after SOI (FF D8). Note: if an APP1 Exif segment existed
    // without an Orientation tag, inserting before it makes IFD0 orientation discoverable cleanly.
    let minimal_app1 = build_minimal_exif_app1(target_val);
    let mut out = Vec::with_capacity(bytes.len() + minimal_app1.len());
    out.extend_from_slice(&bytes[..2]);
    out.extend_from_slice(&minimal_app1);
    out.extend_from_slice(&bytes[2..]);
    Ok(out)
}

/// Locate the byte offset (within `tiff`) of the 2-byte Orientation (tag `0x0112`, type SHORT=3,
/// count=1) value in IFD0, along with `is_big_endian`.
fn find_orientation_value_offset(tiff: &[u8]) -> Option<(usize, bool)> {
    if tiff.len() < 8 {
        return None;
    }
    let big_endian = match &tiff[0..2] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let read_u16 = |off: usize| -> Option<u16> {
        let b = tiff.get(off..off + 2)?;
        Some(if big_endian {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    };
    let read_u32 = |off: usize| -> Option<u32> {
        let b = tiff.get(off..off + 4)?;
        Some(if big_endian {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        })
    };

    if read_u16(2)? != 42 {
        return None;
    }
    let ifd0 = read_u32(4)? as usize;
    let count = read_u16(ifd0)? as usize;
    let entries_start = ifd0.checked_add(2)?;

    for i in 0..count {
        let entry = entries_start.checked_add(i.checked_mul(12)?)?;
        if entry + 12 > tiff.len() {
            break;
        }
        let tag = read_u16(entry)?;
        let ty = read_u16(entry + 2)?;
        let num = read_u32(entry + 4)?;
        // Tag 0x0112 (Orientation), Type 3 (SHORT = 2 bytes), Count 1 -> stored inline at entry + 8.
        if tag == 0x0112 && ty == 3 && num == 1 {
            return Some((entry + 8, big_endian));
        }
    }
    None
}

/// Build a minimal valid JPEG APP1 segment containing one IFD0 entry: Orientation = `val`.
fn build_minimal_exif_app1(val: u16) -> Vec<u8> {
    // TIFF payload (big-endian "MM"):
    //   8 B header: "MM", 0x002A, offset=8
    //   2 B IFD0 count = 1
    //  12 B entry: tag=0x0112, type=3 (SHORT), count=1, value=(val, 0x0000)
    //   4 B next IFD offset = 0
    // Total TIFF = 26 bytes; + "Exif\0\0" (6) = 32 bytes; + length field (2) = 34.
    let mut seg = Vec::with_capacity(36);
    seg.extend_from_slice(&[0xFF, 0xE1]);
    seg.extend_from_slice(&(34u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    // TIFF header
    seg.extend_from_slice(b"MM");
    seg.extend_from_slice(&42u16.to_be_bytes());
    seg.extend_from_slice(&8u32.to_be_bytes());
    // IFD0: 1 entry
    seg.extend_from_slice(&1u16.to_be_bytes());
    seg.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    seg.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    seg.extend_from_slice(&1u32.to_be_bytes()); // count = 1
    seg.extend_from_slice(&val.to_be_bytes()); // inline value
    seg.extend_from_slice(&0u16.to_be_bytes()); // padding
    seg.extend_from_slice(&0u32.to_be_bytes()); // next IFD = 0
    seg
}

/// Atomically write `data` to `dst_path` via a sibling temporary file + `sync_all` + rename.
pub(crate) fn atomic_write_bytes(dst_path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp_path = sibling_temp_path(dst_path);
    let write_res = (|| -> Result<(), String> {
        let mut f = File::create(&tmp_path)
            .map_err(|e| format!("create temp file {}: {e}", tmp_path.display()))?;
        f.write_all(data)
            .map_err(|e| format!("write temp file {}: {e}", tmp_path.display()))?;
        f.sync_all()
            .map_err(|e| format!("flush temp file {}: {e}", tmp_path.display()))?;
        Ok(())
    })();

    if let Err(err) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&tmp_path, dst_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("replace {}: {err}", dst_path.display()));
    }
    Ok(())
}

/// Generate a collision-free hidden temp path in the same directory as `dst_path`.
pub(crate) fn sibling_temp_path(dst_path: &Path) -> std::path::PathBuf {
    let dir = dst_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = dst_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(format!(".{stem}.{pid}.{nanos}.kalux.tmp"))
}

/// Perform a lossless JPEG orientation save from `src_path` to `dst_path`.
pub fn lossless_jpeg_orient_file(
    src_path: &Path,
    dst_path: &Path,
    turns: QuarterTurns,
    flip_h: bool,
    flip_v: bool,
) -> Result<(), String> {
    let raw = fs::read(src_path).map_err(|e| format!("read {}: {e}", src_path.display()))?;
    let patched = patch_jpeg_exif_orientation(&raw, turns, flip_h, flip_v)?;
    atomic_write_bytes(dst_path, &patched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_orientation_matches_pixel_simulation_for_all_128_combinations() {
        let all_orients = [
            Orientation::Normal,
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Transpose,
            Orientation::Rotate90,
            Orientation::Transverse,
            Orientation::Rotate270,
        ];
        // 3x2 distinct pixels: IDs 1..=6.
        let base_pixels: Vec<u8> = (1u8..=6)
            .flat_map(|id| [id, id * 10, id * 20, 255])
            .collect();

        for orig in all_orients {
            let (after_orig, ow, oh) = orig.apply_bgra(base_pixels.clone(), 3, 2);
            for turns in [
                QuarterTurns::R0,
                QuarterTurns::R90,
                QuarterTurns::R180,
                QuarterTurns::R270,
            ] {
                for flip_h in [false, true] {
                    for flip_v in [false, true] {
                        // Simulate EditRecipe directly on `after_orig`:
                        let (ew, eh) = turns.oriented_dims(ow, oh);
                        let mut expected = vec![0u8; (ew * eh * 4) as usize];
                        for y in 0..eh {
                            for x in 0..ew {
                                let (sx, sy) = turns.inverse_pixel(x, y, ow, oh, flip_h, flip_v);
                                let s = ((sy * ow + sx) * 4) as usize;
                                let d = ((y * ew + x) * 4) as usize;
                                expected[d..d + 4].copy_from_slice(&after_orig[s..s + 4]);
                            }
                        }

                        // Compare with applying the composed single Orientation to `base_pixels`:
                        let composed = compose_orientation(orig, turns, flip_h, flip_v);
                        let (actual, aw, ah) = composed.apply_bgra(base_pixels.clone(), 3, 2);
                        assert_eq!(
                            (aw, ah),
                            (ew, eh),
                            "dim mismatch for {orig:?} + ({turns:?}, {flip_h}, {flip_v})"
                        );
                        assert_eq!(
                            actual, expected,
                            "pixel mismatch for {orig:?} + ({turns:?}, {flip_h}, {flip_v}) -> {composed:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn patch_exif_updates_existing_fixture_losslessly() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/orient_6.jpg");
        let raw = fs::read(&fixture).unwrap();
        assert_eq!(read_exif_orientation(&raw), Orientation::Rotate90);

        // Rotate 90° CCW should bring orient_6 back to Normal (1) with identical byte length!
        let patched = patch_jpeg_exif_orientation(&raw, QuarterTurns::R270, false, false).unwrap();
        assert_eq!(patched.len(), raw.len());
        assert_eq!(read_exif_orientation(&patched), Orientation::Normal);
    }

    #[test]
    fn patch_exif_inserts_app1_when_missing() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/grayscale.jpg");
        let raw = fs::read(&fixture).unwrap();
        let patched = patch_jpeg_exif_orientation(&raw, QuarterTurns::R90, false, false).unwrap();
        assert_eq!(read_exif_orientation(&patched), Orientation::Rotate90);
    }
}
