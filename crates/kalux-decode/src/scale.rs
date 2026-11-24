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

#[derive(Copy, Clone)]
struct Span {
    start: usize,
    /// Exclusive end index.
    end: usize,
    first_weight: u64,
    last_weight: u64,
}

/// Precompute exact integer area-overlap spans mapping `src_len` pixels down to `dst_len` pixels.
///
/// In units of `1 / dst_len`, output pixel `d` covers `[d * src_len, (d + 1) * src_len]`
/// (total weight `src_len`). Each source pixel `s` covers `[s * dst_len, (s + 1) * dst_len]`.
/// Interior pixels have exact weight `dst_len`; boundary pixels have `first_weight` / `last_weight`.
fn build_spans(src_len: usize, dst_len: usize) -> Vec<Span> {
    let mut spans = Vec::with_capacity(dst_len);
    let s_u64 = src_len as u64;
    let d_u64 = dst_len as u64;

    for d in 0..dst_len {
        let out_start = (d as u64) * s_u64;
        let out_end = ((d as u64) + 1) * s_u64;

        let start = (out_start / d_u64) as usize;
        let end = (out_end.div_ceil(d_u64) as usize).min(src_len);

        let first_src_end = ((start as u64) + 1) * d_u64;
        let first_weight = first_src_end.min(out_end) - out_start;

        let last_weight = if end <= start + 1 {
            0
        } else {
            let last_src_start = ((end - 1) as u64) * d_u64;
            out_end - last_src_start
        };

        spans.push(Span {
            start,
            end,
            first_weight,
            last_weight,
        });
    }
    spans
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
        let spans = build_spans(sw, dw);
        let mid_w = dw as u64;
        let denom = sw as u64;
        let half = denom / 2;
        let mut tmp = vec![0u8; dw * sh * 4];

        for y in 0..sh {
            let srow = &src[y * sw * 4..(y + 1) * sw * 4];
            let drow = &mut tmp[y * dw * 4..(y + 1) * dw * 4];
            for (dx, span) in spans.iter().enumerate() {
                let s0 = span.start * 4;
                let mut acc0 = srow[s0] as u64 * span.first_weight;
                let mut acc1 = srow[s0 + 1] as u64 * span.first_weight;
                let mut acc2 = srow[s0 + 2] as u64 * span.first_weight;
                let mut acc3 = srow[s0 + 3] as u64 * span.first_weight;

                if span.end > span.start + 1 {
                    let mut m0 = 0u64;
                    let mut m1 = 0u64;
                    let mut m2 = 0u64;
                    let mut m3 = 0u64;
                    for sx in (span.start + 1)..(span.end - 1) {
                        let o = sx * 4;
                        m0 += srow[o] as u64;
                        m1 += srow[o + 1] as u64;
                        m2 += srow[o + 2] as u64;
                        m3 += srow[o + 3] as u64;
                    }
                    acc0 += m0 * mid_w;
                    acc1 += m1 * mid_w;
                    acc2 += m2 * mid_w;
                    acc3 += m3 * mid_w;

                    let sl = (span.end - 1) * 4;
                    acc0 += srow[sl] as u64 * span.last_weight;
                    acc1 += srow[sl + 1] as u64 * span.last_weight;
                    acc2 += srow[sl + 2] as u64 * span.last_weight;
                    acc3 += srow[sl + 3] as u64 * span.last_weight;
                }

                let o = dx * 4;
                drow[o] = ((acc0 + half) / denom) as u8;
                drow[o + 1] = ((acc1 + half) / denom) as u8;
                drow[o + 2] = ((acc2 + half) / denom) as u8;
                drow[o + 3] = ((acc3 + half) / denom) as u8;
            }
        }
        tmp
    };

    if dh == sh {
        return horiz;
    }

    // Pass 2: vertical (dw × sh -> dw × dh)
    let spans = build_spans(sh, dh);
    let mid_w = dh as u64;
    let denom = sh as u64;
    let half = denom / 2;
    let row_bytes = dw * 4;
    let mut out = vec![0u8; dw * dh * 4];
    let mut accum = vec![0u64; row_bytes];

    for (dy, span) in spans.iter().enumerate() {
        let first_row = &horiz[span.start * row_bytes..(span.start + 1) * row_bytes];
        for (a, &b) in accum.iter_mut().zip(first_row) {
            *a = b as u64 * span.first_weight;
        }

        if span.end > span.start + 1 {
            if span.end > span.start + 2 {
                // Accumulate interior rows separately so we only multiply by mid_w once per channel.
                // Actually multiplying directly avoids a second buffer and is linear in memory.
                for sy in (span.start + 1)..(span.end - 1) {
                    let row = &horiz[sy * row_bytes..(sy + 1) * row_bytes];
                    for (a, &b) in accum.iter_mut().zip(row) {
                        *a += b as u64 * mid_w;
                    }
                }
            }
            let last_row = &horiz[(span.end - 1) * row_bytes..span.end * row_bytes];
            for (a, &b) in accum.iter_mut().zip(last_row) {
                *a += b as u64 * span.last_weight;
            }
        }

        let drow = &mut out[dy * row_bytes..(dy + 1) * row_bytes];
        for (dst_b, &a) in drow.iter_mut().zip(&accum) {
            *dst_b = ((a + half) / denom) as u8;
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
