//! Aspect-preserving target sizing and fast separable area/box downscaler for BGRA8 premultiplied pixels.

/// Compute aspect-preserving dimensions that fit within `(max_w, max_h)`.
/// Never upscales; always returns dimensions >= (1, 1) for non-zero inputs.
pub fn fit_dimensions(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || max_w == 0 || max_h == 0 {
        return (src_w, src_h);
    }
    if src_w <= max_w && src_h <= max_h {
        return (src_w, src_h);
    }
    let scale = (max_w as f64 / src_w as f64).min(max_h as f64 / src_h as f64);
    let dst_w = ((src_w as f64 * scale).round() as u32).clamp(1, max_w.min(src_w));
    let dst_h = ((src_h as f64 * scale).round() as u32).clamp(1, max_h.min(src_h));
    (dst_w, dst_h)
}

const FP_SHIFT: u32 = 22;
const FP_SCALE: u64 = 1 << FP_SHIFT;
const FP_HALF: u32 = 1 << (FP_SHIFT - 1);

#[derive(Copy, Clone)]
struct Span {
    start: usize,
    /// Exclusive end index.
    end: usize,
    first_weight: u32,
    last_weight: u32,
}

/// Precompute fixed-point (`1 << 22`) area-overlap spans mapping `src_len` pixels down to `dst_len`.
///
/// Every span's weights sum to exactly `1 << 22`, guaranteeing exact preservation of solid colors
/// while allowing the inner resampling loops to use `u32` accumulation and a bitshift instead of
/// 64-bit integer division.
fn build_spans(src_len: usize, dst_len: usize) -> (Vec<Span>, u32) {
    let mut spans = Vec::with_capacity(dst_len);
    let s_u64 = src_len as u64;
    let d_u64 = dst_len as u64;
    let mid_weight = ((d_u64 * FP_SCALE) / s_u64) as u32;

    for d in 0..dst_len {
        let out_start = (d as u64) * s_u64;
        let out_end = ((d as u64) + 1) * s_u64;

        let start = (out_start / d_u64) as usize;
        let end = (out_end.div_ceil(d_u64) as usize).min(src_len);

        if end <= start + 1 {
            spans.push(Span {
                start,
                end,
                first_weight: FP_SCALE as u32,
                last_weight: 0,
            });
            continue;
        }

        let first_src_end = ((start as u64) + 1) * d_u64;
        let first_raw = first_src_end.min(out_end) - out_start;
        let last_src_start = ((end - 1) as u64) * d_u64;
        let last_raw = out_end - last_src_start;

        let mut first_w = (first_raw * FP_SCALE) / s_u64;
        let mut last_w = (last_raw * FP_SCALE) / s_u64;
        let n_mid = (end - start - 2) as u64;
        let sum = first_w + n_mid * u64::from(mid_weight) + last_w;
        if let Some(rem) = FP_SCALE.checked_sub(sum) {
            let add_first = rem / 2;
            first_w += add_first;
            last_w += rem - add_first;
        }

        spans.push(Span {
            start,
            end,
            first_weight: first_w as u32,
            last_weight: last_w as u32,
        });
    }
    (spans, mid_weight)
}

/// Downscale a tightly-packed BGRA8 premultiplied image from `(src_w, src_h)` to `(dst_w, dst_h)`
/// using an exact separable 2-pass area/box filter.
///
/// If `dst_w >= src_w && dst_h >= src_h`, returns a clone of `src` (never upscales).
pub fn downscale_bgra(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let dst_w = dst_w.min(src_w);
    let dst_h = dst_h.min(src_h);
    if dst_w == src_w && dst_h == src_h {
        return src.to_vec();
    }

    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;

    // Pass 1: horizontal (sw × sh -> dw × sh)
    let horiz: Vec<u8> = if dw == sw {
        src.to_vec()
    } else {
        let (spans, mid_w) = build_spans(sw, dw);
        let mut tmp = vec![0u8; dw * sh * 4];

        for y in 0..sh {
            let srow = &src[y * sw * 4..(y + 1) * sw * 4];
            let drow = &mut tmp[y * dw * 4..(y + 1) * dw * 4];
            for (dx, span) in spans.iter().enumerate() {
                let s0 = span.start * 4;
                let fw = span.first_weight;
                let mut acc0 = u32::from(srow[s0]) * fw;
                let mut acc1 = u32::from(srow[s0 + 1]) * fw;
                let mut acc2 = u32::from(srow[s0 + 2]) * fw;
                let mut acc3 = u32::from(srow[s0 + 3]) * fw;

                if span.end > span.start + 1 {
                    if span.end > span.start + 2 {
                        let mut m0 = 0u32;
                        let mut m1 = 0u32;
                        let mut m2 = 0u32;
                        let mut m3 = 0u32;
                        for sx in (span.start + 1)..(span.end - 1) {
                            let o = sx * 4;
                            m0 += u32::from(srow[o]);
                            m1 += u32::from(srow[o + 1]);
                            m2 += u32::from(srow[o + 2]);
                            m3 += u32::from(srow[o + 3]);
                        }
                        acc0 += m0 * mid_w;
                        acc1 += m1 * mid_w;
                        acc2 += m2 * mid_w;
                        acc3 += m3 * mid_w;
                    }

                    let sl = (span.end - 1) * 4;
                    let lw = span.last_weight;
                    acc0 += u32::from(srow[sl]) * lw;
                    acc1 += u32::from(srow[sl + 1]) * lw;
                    acc2 += u32::from(srow[sl + 2]) * lw;
                    acc3 += u32::from(srow[sl + 3]) * lw;
                }

                let o = dx * 4;
                drow[o] = ((acc0 + FP_HALF) >> FP_SHIFT) as u8;
                drow[o + 1] = ((acc1 + FP_HALF) >> FP_SHIFT) as u8;
                drow[o + 2] = ((acc2 + FP_HALF) >> FP_SHIFT) as u8;
                drow[o + 3] = ((acc3 + FP_HALF) >> FP_SHIFT) as u8;
            }
        }
        tmp
    };

    if dh == sh {
        return horiz;
    }

    // Pass 2: vertical (dw × sh -> dw × dh)
    let (spans, mid_w) = build_spans(sh, dh);
    let row_bytes = dw * 4;
    let mut out = vec![0u8; dw * dh * 4];
    let mut accum = vec![0u32; row_bytes];

    for (dy, span) in spans.iter().enumerate() {
        let fw = span.first_weight;
        let first_row = &horiz[span.start * row_bytes..(span.start + 1) * row_bytes];
        for (a, &b) in accum.iter_mut().zip(first_row) {
            *a = u32::from(b) * fw;
        }

        if span.end > span.start + 1 {
            if span.end > span.start + 2 {
                for sy in (span.start + 1)..(span.end - 1) {
                    let row = &horiz[sy * row_bytes..(sy + 1) * row_bytes];
                    for (a, &b) in accum.iter_mut().zip(row) {
                        *a += u32::from(b) * mid_w;
                    }
                }
            }
            let lw = span.last_weight;
            let last_row = &horiz[(span.end - 1) * row_bytes..span.end * row_bytes];
            for (a, &b) in accum.iter_mut().zip(last_row) {
                *a += u32::from(b) * lw;
            }
        }

        let drow = &mut out[dy * row_bytes..(dy + 1) * row_bytes];
        for (dst_b, &a) in drow.iter_mut().zip(&accum) {
            *dst_b = ((a + FP_HALF) >> FP_SHIFT) as u8;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_dimensions_preserves_aspect_and_never_upscales() {
        assert_eq!(fit_dimensions(4000, 3000, 1920, 1080), (1440, 1080));
        assert_eq!(fit_dimensions(3000, 4000, 1920, 1080), (810, 1080));
        assert_eq!(fit_dimensions(800, 600, 1920, 1080), (800, 600));
        assert_eq!(fit_dimensions(10000, 2, 1000, 1000), (1000, 1));
    }

    #[test]
    fn downscale_preserves_solid_color_exactly() {
        let color = [17u8, 123, 231, 255];
        let mut src = Vec::with_capacity(137 * 89 * 4);
        for _ in 0..(137 * 89) {
            src.extend_from_slice(&color);
        }
        let dst = downscale_bgra(&src, 137, 89, 53, 31);
        assert_eq!(dst.len(), 53 * 31 * 4);
        for px in dst.as_chunks::<4>().0 {
            assert_eq!(*px, color);
        }
    }

    #[test]
    fn downscale_2x2_box_average_is_exact() {
        // 4x4 divided into 2x2 quadrants: TL=0, TR=100, BL=200, BR=250
        let mut src = vec![0u8; 4 * 4 * 4];
        for y in 0..4 {
            for x in 0..4 {
                let v = match (x >= 2, y >= 2) {
                    (false, false) => 0u8,
                    (true, false) => 100,
                    (false, true) => 200,
                    (true, true) => 250,
                };
                let o = (y * 4 + x) * 4;
                src[o..o + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let dst = downscale_bgra(&src, 4, 4, 2, 2);
        assert_eq!(&dst[0..4], &[0, 0, 0, 255]);
        assert_eq!(&dst[4..8], &[100, 100, 100, 255]);
        assert_eq!(&dst[8..12], &[200, 200, 200, 255]);
        assert_eq!(&dst[12..16], &[250, 250, 250, 255]);

        // Downscaling all the way to 1x1 averages the four quadrants: (0+100+200+250)/4 = 137.5 -> 138
        let one = downscale_bgra(&src, 4, 4, 1, 1);
        assert_eq!(one, vec![138, 138, 138, 255]);
    }
}
