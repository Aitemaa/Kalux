//! Animated image (GIF / animated WebP) compositing, disposal, delay normalization,
//! loop-count parsing, and total-memory capping.
//!
//! Kept platform-independent so all animation logic is unit-tested on Linux and shared by WIC on Windows.

#![cfg_attr(not(windows), allow(dead_code))]

use kalux_core::Frame;

use crate::scale::{downscale_bgra, fit_dimensions};

/// Maximum total pixel bytes across all frames of an animated image (256 MiB).
pub const MAX_ANIMATED_BYTES: usize = 256 * 1024 * 1024;

/// GIF graphic control extension disposal method.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Disposal {
    /// 0: Unspecified (keep canvas as-is).
    #[default]
    None,
    /// 1: Do not dispose (keep canvas as-is).
    Keep,
    /// 2: Restore frame's bounding box to transparent background.
    RestoreBackground,
    /// 3: Restore canvas to state prior to drawing this frame.
    RestorePrevious,
}

impl Disposal {
    pub fn from_gif_code(code: u8) -> Self {
        match code {
            1 => Self::Keep,
            2 => Self::RestoreBackground,
            3 => Self::RestorePrevious,
            _ => Self::None,
        }
    }
}

/// Metadata for one raw animation frame prior to compositing.
#[derive(Copy, Clone, Debug)]
pub struct FrameMeta {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    pub delay_ms: u32,
    pub disposal: Disposal,
}

/// Convert a GIF delay in centiseconds (1/100 s) to milliseconds.
/// Matches browser convention: 0 ms and 10 ms both clamp to 100 ms.
pub fn normalize_gif_delay_ms(delay_cs: u16) -> u32 {
    let ms = u32::from(delay_cs) * 10;
    if ms <= 10 { 100 } else { ms }
}

/// Parse loop count from GIF `NETSCAPE2.0` / `ANIMEXTS1.0` application extension.
///
/// WIC returns the application identifier (11 bytes) and data sub-block, which is usually
/// `[3, 1, lo, hi]` (with sub-block length prefix) or `[1, lo, hi]`.
/// Returns `Some(0)` for infinite loop, `Some(n)` for `n` repeats, or `None` if unrecognized.
pub fn parse_netscape_loop_count(app_id: &[u8], data: &[u8]) -> Option<u32> {
    if app_id != b"NETSCAPE2.0" && app_id != b"ANIMEXTS1.0" {
        return None;
    }
    if data.len() >= 4 && data[0] == 3 && data[1] == 1 {
        Some(u32::from(u16::from_le_bytes([data[2], data[3]])))
    } else if data.len() >= 3 && data[0] == 1 {
        Some(u32::from(u16::from_le_bytes([data[1], data[2]])))
    } else {
        None
    }
}

/// Compute output dimensions for an animation so it fits `max_size` (never upscaled) AND
/// `frame_count * width * height * 4 <= max_total_bytes`.
pub fn capped_animation_dimensions(
    canvas_w: u32,
    canvas_h: u32,
    frame_count: u32,
    max_size: Option<(u32, u32)>,
    max_total_bytes: usize,
) -> (u32, u32) {
    let (mut w, mut h) = match max_size {
        Some((mw, mh)) => fit_dimensions(canvas_w, canvas_h, mw, mh),
        None => (canvas_w, canvas_h),
    };
    if w == 0 || h == 0 || frame_count == 0 {
        return (w, h);
    }

    let cap = max_total_bytes as u128;
    let total = |tw: u32, th: u32| -> u128 {
        u128::from(tw) * u128::from(th) * 4 * u128::from(frame_count)
    };
    if total(w, h) <= cap {
        return (w, h);
    }

    let factor = (cap as f64 / total(w, h) as f64).sqrt();
    w = ((w as f64 * factor).floor() as u32).max(1);
    h = ((h as f64 * factor).floor() as u32).max(1);
    while total(w, h) > cap && (w > 1 || h > 1) {
        if w >= h && w > 1 {
            w -= 1;
        } else if h > 1 {
            h -= 1;
        } else {
            break;
        }
    }
    (w, h)
}

/// Stateful canvas compositor for animated BGRA8 premultiplied frames.
pub struct AnimatedCompositor {
    canvas_w: u32,
    canvas_h: u32,
    out_w: u32,
    out_h: u32,
    canvas: Vec<u8>,
    frames: Vec<Frame>,
}

impl AnimatedCompositor {
    pub fn new(canvas_w: u32, canvas_h: u32, out_w: u32, out_h: u32, frame_hint: usize) -> Self {
        let len = (canvas_w as usize)
            .saturating_mul(canvas_h as usize)
            .saturating_mul(4);
        Self {
            canvas_w,
            canvas_h,
            out_w,
            out_h,
            canvas: vec![0u8; len],
            frames: Vec::with_capacity(frame_hint),
        }
    }

    /// Composite one premultiplied BGRA8 sub-frame onto the canvas, emit the composited frame
    /// (downscaled to `out_w × out_h` if smaller than the canvas), and apply its disposal rule.
    pub fn push_frame(&mut self, pixels: &[u8], meta: FrameMeta) {
        let cw = self.canvas_w as usize;
        let ch = self.canvas_h as usize;
        let fw = meta.width as usize;
        let fh = meta.height as usize;
        let left = meta.left as usize;
        let top = meta.top as usize;

        let copy_w = fw.min(cw.saturating_sub(left));
        let copy_h = fh.min(ch.saturating_sub(top));

        // Snapshot region before drawing when disposal is RestorePrevious.
        let saved_rect: Option<Vec<u8>> = if meta.disposal == Disposal::RestorePrevious {
            let mut snap = Vec::with_capacity(copy_w * copy_h * 4);
            for row in 0..copy_h {
                let start = ((top + row) * cw + left) * 4;
                snap.extend_from_slice(&self.canvas[start..start + copy_w * 4]);
            }
            Some(snap)
        } else {
            None
        };

        // Premultiplied BGRA8 source-over blend onto the canvas.
        for row in 0..copy_h {
            let src_row_off = row * fw * 4;
            let dst_row_off = ((top + row) * cw + left) * 4;
            if src_row_off + copy_w * 4 > pixels.len() {
                break;
            }
            let src_slice = &pixels[src_row_off..src_row_off + copy_w * 4];
            let dst_slice = &mut self.canvas[dst_row_off..dst_row_off + copy_w * 4];
            for (s, d) in src_slice
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst_slice.as_chunks_mut::<4>().0.iter_mut())
            {
                let sa = s[3];
                if sa == 255 {
                    *d = *s;
                } else if sa > 0 {
                    let inv_a = u16::from(255 - sa);
                    for i in 0..4 {
                        let blended = ((u16::from(d[i]) * inv_a + 127) / 255) as u8;
                        d[i] = s[i].saturating_add(blended);
                    }
                }
            }
        }

        // Capture full composited frame (downscaled if needed).
        let frame_pixels = if self.out_w == self.canvas_w && self.out_h == self.canvas_h {
            self.canvas.clone()
        } else {
            downscale_bgra(
                &self.canvas,
                self.canvas_w,
                self.canvas_h,
                self.out_w,
                self.out_h,
            )
        };
        self.frames.push(Frame {
            pixels: frame_pixels,
            delay_ms: meta.delay_ms,
        });

        // Apply post-frame disposal for the subsequent frame.
        match meta.disposal {
            Disposal::None | Disposal::Keep => {}
            Disposal::RestoreBackground => {
                for row in 0..copy_h {
                    let start = ((top + row) * cw + left) * 4;
                    self.canvas[start..start + copy_w * 4].fill(0);
                }
            }
            Disposal::RestorePrevious => {
                if let Some(snap) = saved_rect {
                    for row in 0..copy_h {
                        let dst_start = ((top + row) * cw + left) * 4;
                        let src_start = row * copy_w * 4;
                        self.canvas[dst_start..dst_start + copy_w * 4]
                            .copy_from_slice(&snap[src_start..src_start + copy_w * 4]);
                    }
                }
            }
        }
    }

    pub fn finish(self) -> Vec<Frame> {
        self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_bgra(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            v.extend_from_slice(&px);
        }
        v
    }

    #[test]
    fn delay_normalization_matches_browsers() {
        assert_eq!(normalize_gif_delay_ms(0), 100);
        assert_eq!(normalize_gif_delay_ms(1), 100);
        assert_eq!(normalize_gif_delay_ms(2), 20);
        assert_eq!(normalize_gif_delay_ms(5), 50);
        assert_eq!(normalize_gif_delay_ms(10), 100);
    }

    #[test]
    fn netscape_loop_count_parsing() {
        assert_eq!(
            parse_netscape_loop_count(b"NETSCAPE2.0", &[3, 1, 0, 0]),
            Some(0)
        );
        assert_eq!(
            parse_netscape_loop_count(b"NETSCAPE2.0", &[3, 1, 5, 0]),
            Some(5)
        );
        assert_eq!(
            parse_netscape_loop_count(b"ANIMEXTS1.0", &[1, 2, 1]),
            Some(258)
        );
        assert_eq!(
            parse_netscape_loop_count(b"OTHEREXT1.0", &[3, 1, 0, 0]),
            None
        );
    }

    #[test]
    fn memory_cap_downscales_huge_animation() {
        // 1000 frames of 1000x1000 = 4 GB -> must fit <= 256 MiB
        let (w, h) = capped_animation_dimensions(1000, 1000, 1000, None, MAX_ANIMATED_BYTES);
        assert!(w < 1000 && h < 1000);
        let total = (w as usize) * (h as usize) * 4 * 1000;
        assert!(total <= MAX_ANIMATED_BYTES);
        // Should be close to sqrt(256 MiB / 4000) ≈ 259
        assert!((250..=260).contains(&w));
    }

    #[test]
    fn compositor_handles_offsets_restore_background_and_restore_previous() {
        let red = [0, 0, 255, 255];
        let green = [0, 255, 0, 255];
        let blue = [255, 0, 0, 255];
        let clear = [0, 0, 0, 0];

        let mut comp = AnimatedCompositor::new(4, 4, 4, 4, 4);

        // Frame 0: fill full 4x4 with red, Keep.
        comp.push_frame(
            &solid_bgra(4, 4, red),
            FrameMeta {
                left: 0,
                top: 0,
                width: 4,
                height: 4,
                delay_ms: 100,
                disposal: Disposal::Keep,
            },
        );

        // Frame 1: draw 2x2 green at (1, 1), RestorePrevious!
        comp.push_frame(
            &solid_bgra(2, 2, green),
            FrameMeta {
                left: 1,
                top: 1,
                width: 2,
                height: 2,
                delay_ms: 50,
                disposal: Disposal::RestorePrevious,
            },
        );

        // Frame 2: draw 1x1 blue at (0, 0), RestoreBackground.
        // Because Frame 1 had RestorePrevious, (1, 1) must be RED again, not green!
        comp.push_frame(
            &solid_bgra(1, 1, blue),
            FrameMeta {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
                delay_ms: 80,
                disposal: Disposal::RestoreBackground,
            },
        );

        // Frame 3: draw 1x1 transparent pixel at (3, 3), Keep.
        // Because Frame 2 had RestoreBackground on (0, 0), (0, 0) must now be CLEAR!
        comp.push_frame(
            &solid_bgra(1, 1, clear),
            FrameMeta {
                left: 3,
                top: 3,
                width: 1,
                height: 1,
                delay_ms: 60,
                disposal: Disposal::Keep,
            },
        );

        let frames = comp.finish();
        assert_eq!(frames.len(), 4);

        let px = |f: usize, x: usize, y: usize| -> [u8; 4] {
            let o = (y * 4 + x) * 4;
            frames[f].pixels[o..o + 4].try_into().unwrap()
        };

        // Frame 0: all red
        assert_eq!(px(0, 0, 0), red);
        assert_eq!(px(0, 1, 1), red);

        // Frame 1: (0,0) red, (1,1) green
        assert_eq!(px(1, 0, 0), red);
        assert_eq!(px(1, 1, 1), green);

        // Frame 2: (0,0) blue, (1,1) restored back to red!
        assert_eq!(px(2, 0, 0), blue);
        assert_eq!(px(2, 1, 1), red);

        // Frame 3: (0,0) cleared to transparent by RestoreBackground, (1,1) still red!
        assert_eq!(px(3, 0, 0), clear);
        assert_eq!(px(3, 1, 1), red);
    }
}
