//! Fast software BGRA8 premultiplied framebuffer renderer for Wayland `wl_shm`.
//!
//! Wayland `wl_shm::Format::Argb8888` on little-endian architectures is `[B, G, R, A]` in memory —
//! the exact same byte layout as `kalux_core::DecodedImage`, so there is zero channel swizzling.

use kalux_core::{DecodedImage, Palette, Rect, Rgba};

/// Borrowed BGRA8 premultiplied pixel canvas (`width * height * 4` bytes).
pub struct Canvas<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a mut [u8],
}

impl<'a> Canvas<'a> {
    pub fn new(width: u32, height: u32, pixels: &'a mut [u8]) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Fill the entire canvas with a solid opaque `Rgba` color.
    pub fn clear(&mut self, color: Rgba) {
        let px = [color.b, color.g, color.r, 255];
        let total = (self.width as usize)
            .saturating_mul(self.height as usize)
            .min(self.pixels.len() / 4);
        for dst in self.pixels[..total * 4].as_chunks_mut::<4>().0 {
            *dst = px;
        }
    }

    /// Blend a straight-alpha `Rgba` color over a single pixel at `(x, y)` with extra coverage `cov` in `[0, 1]`.
    #[inline]
    pub fn blend_pixel(&mut self, x: i32, y: i32, color: Rgba, cov: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let sa = ((f32::from(color.a) * cov).clamp(0.0, 255.0) + 0.5) as u16;
        if sa == 0 {
            return;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        if idx + 4 > self.pixels.len() {
            return;
        }
        let dst = &mut self.pixels[idx..idx + 4];
        if sa >= 255 {
            dst[0] = color.b;
            dst[1] = color.g;
            dst[2] = color.r;
            dst[3] = 255;
            return;
        }
        let inv = 255 - sa;
        dst[0] = ((u16::from(color.b) * sa + u16::from(dst[0]) * inv + 127) / 255) as u8;
        dst[1] = ((u16::from(color.g) * sa + u16::from(dst[1]) * inv + 127) / 255) as u8;
        dst[2] = ((u16::from(color.r) * sa + u16::from(dst[2]) * inv + 127) / 255) as u8;
        dst[3] = 255;
    }

    /// Fill an axis-aligned rectangle with straight-alpha blending.
    pub fn fill_rect(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba) {
        let min_x = x0.max(0).min(self.width as i32);
        let max_x = x1.max(0).min(self.width as i32);
        let min_y = y0.max(0).min(self.height as i32);
        let max_y = y1.max(0).min(self.height as i32);
        if min_x >= max_x || min_y >= max_y || color.a == 0 {
            return;
        }

        let sa = u16::from(color.a);
        let inv = 255 - sa;
        let sb = u16::from(color.b);
        let sg = u16::from(color.g);
        let sr = u16::from(color.r);
        let w = self.width as usize;

        for y in min_y as usize..max_y as usize {
            let row_start = (y * w + min_x as usize) * 4;
            let row_end = (y * w + max_x as usize) * 4;
            if row_end > self.pixels.len() {
                break;
            }
            if sa == 255 {
                let px = [color.b, color.g, color.r, 255];
                for dst in self.pixels[row_start..row_end].as_chunks_mut::<4>().0 {
                    *dst = px;
                }
            } else {
                for dst in self.pixels[row_start..row_end].as_chunks_mut::<4>().0 {
                    dst[0] = ((sb * sa + u16::from(dst[0]) * inv + 127) / 255) as u8;
                    dst[1] = ((sg * sa + u16::from(dst[1]) * inv + 127) / 255) as u8;
                    dst[2] = ((sr * sa + u16::from(dst[2]) * inv + 127) / 255) as u8;
                    dst[3] = 255;
                }
            }
        }
    }

    /// Draw an anti-aliased rounded rectangle with optional border.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rounded_rect(
        &mut self,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
        radius: f32,
        fill: Rgba,
        border: Option<(Rgba, f32)>,
    ) {
        let w = (right - left).max(0.0);
        let h = (bottom - top).max(0.0);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let r = radius.clamp(0.0, (w * 0.5).min(h * 0.5));
        let cx = (left + right) * 0.5;
        let cy = (top + bottom) * 0.5;
        let hx = w * 0.5 - r;
        let hy = h * 0.5 - r;

        let min_x = (left.floor() as i32 - 1).max(0);
        let max_x = (right.ceil() as i32 + 1).min(self.width as i32);
        let min_y = (top.floor() as i32 - 1).max(0);
        let max_y = (bottom.ceil() as i32 + 1).min(self.height as i32);

        for y in min_y..max_y {
            let py = y as f32 + 0.5;
            let qy = (py - cy).abs() - hy;
            for x in min_x..max_x {
                let px = x as f32 + 0.5;
                let qx = (px - cx).abs() - hx;
                let dx = qx.max(0.0);
                let dy = qy.max(0.0);
                let dist = (dx * dx + dy * dy).sqrt() + qx.max(qy).min(0.0) - r;

                let outer_cov = (0.5 - dist).clamp(0.0, 1.0);
                if outer_cov <= 0.001 {
                    continue;
                }

                if let Some((border_col, bw)) = border {
                    let inner_cov = (0.5 - (dist + bw)).clamp(0.0, 1.0);
                    let border_cov = (outer_cov - inner_cov).clamp(0.0, 1.0);
                    if inner_cov > 0.001 {
                        self.blend_pixel(x, y, fill, inner_cov);
                    }
                    if border_cov > 0.001 {
                        self.blend_pixel(x, y, border_col, border_cov);
                    }
                } else {
                    self.blend_pixel(x, y, fill, outer_cov);
                }
            }
        }
    }

    /// Draw an anti-aliased filled circle.
    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, color: Rgba) {
        let min_x = ((cx - radius).floor() as i32 - 1).max(0);
        let max_x = ((cx + radius).ceil() as i32 + 1).min(self.width as i32);
        let min_y = ((cy - radius).floor() as i32 - 1).max(0);
        let max_y = ((cy + radius).ceil() as i32 + 1).min(self.height as i32);
        for y in min_y..max_y {
            let dy = (y as f32 + 0.5) - cy;
            for x in min_x..max_x {
                let dx = (x as f32 + 0.5) - cx;
                let dist = (dx * dx + dy * dy).sqrt() - radius;
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.001 {
                    self.blend_pixel(x, y, color, cov);
                }
            }
        }
    }

    /// Blit a premultiplied BGRA8 `DecodedImage` frame into `dest` on this canvas.
    ///
    /// Matches Windows Direct2D interpolation rules (PRD §7.1):
    /// - Fast 1:1 row copy when `dest` matches source dimensions exactly
    /// - Nearest-neighbor when zoomed ≥ 300% (`scale >= 3.0`)
    /// - Fixed-point bilinear interpolation otherwise (with premultiplied alpha-over blending)
    pub fn draw_image(&mut self, image: &DecodedImage, frame_idx: usize, dest: Rect, scale: f32) {
        if image.frames.is_empty() || image.width == 0 || image.height == 0 {
            return;
        }
        let frame = &image.frames[frame_idx.min(image.frames.len() - 1)];
        let sw = image.width as usize;
        let sh = image.height as usize;
        if frame.pixels.len() < sw * sh * 4 || dest.w <= 0.0 || dest.h <= 0.0 {
            return;
        }

        let dst_w = self.width as i32;
        let dst_h = self.height as i32;
        let x0 = (dest.x.floor() as i32).max(0);
        let y0 = (dest.y.floor() as i32).max(0);
        let x1 = ((dest.x + dest.w).ceil() as i32).min(dst_w);
        let y1 = ((dest.y + dest.h).ceil() as i32).min(dst_h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let src = &frame.pixels;
        let cw = self.width as usize;

        // Fast exact 1:1 path.
        let exact_size = (dest.w - image.width as f32).abs() < 0.01
            && (dest.h - image.height as f32).abs() < 0.01;
        let aligned =
            (dest.x - dest.x.round()).abs() < 0.01 && (dest.y - dest.y.round()).abs() < 0.01;
        if exact_size && aligned {
            let ox = dest.x.round() as i32;
            let oy = dest.y.round() as i32;
            for y in y0..y1 {
                let sy = (y - oy) as usize;
                if sy >= sh {
                    continue;
                }
                let sx_start = (x0 - ox) as usize;
                let count = ((x1 - x0) as usize).min(sw.saturating_sub(sx_start));
                if count == 0 {
                    continue;
                }
                let s_off = (sy * sw + sx_start) * 4;
                let d_off = ((y as usize) * cw + (x0 as usize)) * 4;
                let s_slice = &src[s_off..s_off + count * 4];
                let d_slice = &mut self.pixels[d_off..d_off + count * 4];
                for (s, d) in s_slice
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(d_slice.as_chunks_mut::<4>().0.iter_mut())
                {
                    blend_premul_over(*s, d);
                }
            }
            return;
        }

        if scale >= 3.0 {
            let inv_w = image.width as f64 / f64::from(dest.w);
            let inv_h = image.height as f64 / f64::from(dest.h);
            let dx_origin = f64::from(dest.x);
            let dy_origin = f64::from(dest.y);

            let span_w = (x1 - x0) as usize;
            let mut sx_lut = Vec::with_capacity(span_w);
            for x in x0..x1 {
                let sx = (((f64::from(x) + 0.5) - dx_origin) * inv_w).floor() as isize;
                sx_lut.push(sx.clamp(0, sw as isize - 1) as usize);
            }

            for y in y0..y1 {
                let sy = (((f64::from(y) + 0.5) - dy_origin) * inv_h).floor() as isize;
                let sy = sy.clamp(0, sh as isize - 1) as usize;
                let s_row = &src[sy * sw * 4..(sy + 1) * sw * 4];
                let d_off = ((y as usize) * cw + (x0 as usize)) * 4;
                let d_row = &mut self.pixels[d_off..d_off + span_w * 4];
                for (&sx, d) in sx_lut.iter().zip(d_row.as_chunks_mut::<4>().0.iter_mut()) {
                    let o = sx * 4;
                    let s = [s_row[o], s_row[o + 1], s_row[o + 2], s_row[o + 3]];
                    blend_premul_over(s, d);
                }
            }
            return;
        }

        // Fixed-point 8-bit fractional bilinear resampling.
        let inv_w = image.width as f64 / f64::from(dest.w);
        let inv_h = image.height as f64 / f64::from(dest.h);
        let dx_origin = f64::from(dest.x);
        let dy_origin = f64::from(dest.y);
        let max_sx = (sw - 1) as f64;
        let max_sy = (sh - 1) as f64;

        let span_w = (x1 - x0) as usize;
        let mut x_table: Vec<(usize, usize, u32)> = Vec::with_capacity(span_w);
        for x in x0..x1 {
            let fx = (((f64::from(x) + 0.5) - dx_origin) * inv_w - 0.5).clamp(0.0, max_sx);
            let ix0 = fx.floor() as usize;
            let ix1 = (ix0 + 1).min(sw - 1);
            let wx = ((fx - ix0 as f64) * 256.0 + 0.5) as u32;
            x_table.push((ix0 * 4, ix1 * 4, wx.min(256)));
        }

        for y in y0..y1 {
            let fy = (((f64::from(y) + 0.5) - dy_origin) * inv_h - 0.5).clamp(0.0, max_sy);
            let iy0 = fy.floor() as usize;
            let iy1 = (iy0 + 1).min(sh - 1);
            let wy = (((fy - iy0 as f64) * 256.0 + 0.5) as u32).min(256);
            let inv_wy = 256 - wy;

            let row0 = &src[iy0 * sw * 4..(iy0 + 1) * sw * 4];
            let row1 = &src[iy1 * sw * 4..(iy1 + 1) * sw * 4];
            let d_off = ((y as usize) * cw + (x0 as usize)) * 4;
            let d_row = &mut self.pixels[d_off..d_off + span_w * 4];

            for (&(o0, o1, wx), d) in x_table.iter().zip(d_row.as_chunks_mut::<4>().0.iter_mut()) {
                let inv_wx = 256 - wx;
                let w00 = inv_wx * inv_wy;
                let w10 = wx * inv_wy;
                let w01 = inv_wx * wy;
                let w11 = wx * wy;

                let mut sampled = [0u8; 4];
                for c in 0..4 {
                    let v = u32::from(row0[o0 + c]) * w00
                        + u32::from(row0[o1 + c]) * w10
                        + u32::from(row1[o0 + c]) * w01
                        + u32::from(row1[o1 + c]) * w11;
                    sampled[c] = ((v + 32_768) >> 16) as u8;
                }
                blend_premul_over(sampled, d);
            }
        }
    }
}

/// Premultiplied BGRA8 source-over onto opaque BGRA8 destination.
#[inline(always)]
fn blend_premul_over(s: [u8; 4], d: &mut [u8; 4]) {
    let sa = s[3];
    if sa == 255 {
        *d = s;
    } else if sa > 0 {
        let inv = u16::from(255 - sa);
        d[0] = s[0].saturating_add(((u16::from(d[0]) * inv + 127) / 255) as u8);
        d[1] = s[1].saturating_add(((u16::from(d[1]) * inv + 127) / 255) as u8);
        d[2] = s[2].saturating_add(((u16::from(d[2]) * inv + 127) / 255) as u8);
        d[3] = 255;
    }
}

/// Draw a subtle 1px hairline horizontal divider.
pub fn draw_hline(canvas: &mut Canvas<'_>, x0: i32, x1: i32, y: i32, color: Rgba) {
    canvas.fill_rect(x0, y, x1, y + 1, color);
}

/// Helper to draw the signature mini Kalux brand mark inside a square `(x, y, size)`.
pub fn draw_mini_icon(canvas: &mut Canvas<'_>, x: f32, y: f32, size: f32, palette: Palette) {
    let tile_bg = if palette.is_dark {
        Rgba::rgb(0x22, 0x22, 0x22)
    } else {
        Rgba::rgb(0x19, 0x19, 0x19)
    };
    let ink = Rgba::rgb(0xF5, 0xF5, 0xF5);
    let accent = Rgba::rgb(0xE8, 0x26, 0x2D);

    canvas.fill_rounded_rect(x, y, x + size, y + size, size * 0.22, tile_bg, None);

    let s = size / 18.0;
    canvas.fill_rect(
        (x + 4.5 * s).round() as i32,
        (y + 4.0 * s).round() as i32,
        (x + 6.7 * s).round() as i32,
        (y + 14.0 * s).round() as i32,
        ink,
    );
    for i in 0..5 {
        let t = i as f32 / 4.0;
        let rx = x + (8.4 + t * 3.8) * s;
        let ry_up = y + (8.2 - t * 3.8) * s;
        let ry_dn = y + (9.4 + t * 3.8) * s;
        canvas.fill_rect(
            rx.round() as i32,
            ry_up.round() as i32,
            (rx + 2.0 * s).round() as i32,
            (ry_up + 1.8 * s).round() as i32,
            ink,
        );
        canvas.fill_rect(
            rx.round() as i32,
            ry_dn.round() as i32,
            (rx + 2.0 * s).round() as i32,
            (ry_dn + 1.8 * s).round() as i32,
            ink,
        );
    }
    canvas.fill_circle(x + 12.6 * s, y + 9.0 * s, 1.5 * s, accent);
}
