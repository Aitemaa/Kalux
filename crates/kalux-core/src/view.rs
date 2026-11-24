//! Zoom / pan math. Pure functions over sizes in **physical pixels**.

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ZoomMode {
    /// Fit inside the client area, never upscale beyond 100% (small images stay 1:1, centered).
    Fit,
    /// 1 source pixel = 1 screen pixel.
    Actual,
    /// Free zoom from wheel / +/-.
    Custom,
}

/// View state for one image. `scale` is screen px per **source** px
/// (so 1.0 == 100% regardless of whether the decoded bitmap was downscaled).
#[derive(Clone, Debug)]
pub struct View {
    pub mode: ZoomMode,
    pub scale: f32,
    /// Offset of the image center from the client center, in screen px.
    pub pan: (f32, f32),
}

impl View {
    pub const MIN_SCALE: f32 = 0.02;
    pub const MAX_SCALE: f32 = 64.0;

    fn fit_scale(source: (u32, u32), client: (u32, u32)) -> f32 {
        if source.0 == 0 || source.1 == 0 || client.0 == 0 || client.1 == 0 {
            return 1.0;
        }
        let sx = client.0 as f32 / source.0 as f32;
        let sy = client.1 as f32 / source.1 as f32;
        sx.min(sy).min(1.0)
    }

    fn clamp_pan(&mut self, source: (u32, u32), client: (u32, u32)) {
        let img_w = source.0 as f32 * self.scale;
        let img_h = source.1 as f32 * self.scale;
        let cli_w = client.0 as f32;
        let cli_h = client.1 as f32;

        self.pan.0 = if !self.pan.0.is_finite() || img_w <= cli_w {
            0.0
        } else {
            let max_x = (img_w - cli_w) * 0.5;
            self.pan.0.clamp(-max_x, max_x)
        };

        self.pan.1 = if !self.pan.1.is_finite() || img_h <= cli_h {
            0.0
        } else {
            let max_y = (img_h - cli_h) * 0.5;
            self.pan.1.clamp(-max_y, max_y)
        };
    }

    /// Fit view for an image of `source` size in a client of `client` size.
    pub fn fit(source: (u32, u32), client: (u32, u32)) -> Self {
        Self {
            mode: ZoomMode::Fit,
            scale: Self::fit_scale(source, client),
            pan: (0.0, 0.0),
        }
    }

    /// Recompute `scale` after a resize when in `Fit`/`Actual` mode; clamp pan otherwise.
    pub fn relayout(&mut self, source: (u32, u32), client: (u32, u32)) {
        match self.mode {
            ZoomMode::Fit => {
                self.scale = Self::fit_scale(source, client);
                self.pan = (0.0, 0.0);
            }
            ZoomMode::Actual => {
                self.scale = 1.0;
                self.clamp_pan(source, client);
            }
            ZoomMode::Custom => {
                self.clamp_pan(source, client);
            }
        }
    }

    /// Multiply scale by `factor`, keeping the source point under `cursor` (client coords) fixed.
    /// Switches mode to `Custom`, clamps to [MIN_SCALE, MAX_SCALE], then clamps pan.
    pub fn zoom_at(
        &mut self,
        factor: f32,
        cursor: (f32, f32),
        source: (u32, u32),
        client: (u32, u32),
    ) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let old_scale = if self.scale.is_finite() && self.scale > 0.0 {
            self.scale
        } else {
            1.0
        };
        let new_scale = (old_scale * factor).clamp(Self::MIN_SCALE, Self::MAX_SCALE);
        let ratio = new_scale / old_scale;

        let cx = cursor.0 - client.0 as f32 * 0.5;
        let cy = cursor.1 - client.1 as f32 * 0.5;
        self.pan.0 = cx - (cx - self.pan.0) * ratio;
        self.pan.1 = cy - (cy - self.pan.1) * ratio;
        self.scale = new_scale;
        self.mode = ZoomMode::Custom;
        self.clamp_pan(source, client);
    }

    /// Set 100% centered on `cursor` (or client center when `None`).
    pub fn actual_size(
        &mut self,
        cursor: Option<(f32, f32)>,
        source: (u32, u32),
        client: (u32, u32),
    ) {
        let anchor = cursor.unwrap_or((client.0 as f32 * 0.5, client.1 as f32 * 0.5));
        if self.scale.is_finite() && self.scale > 0.0 {
            let ratio = 1.0 / self.scale;
            let cx = anchor.0 - client.0 as f32 * 0.5;
            let cy = anchor.1 - client.1 as f32 * 0.5;
            self.pan.0 = cx - (cx - self.pan.0) * ratio;
            self.pan.1 = cy - (cy - self.pan.1) * ratio;
        } else {
            self.pan = (0.0, 0.0);
        }
        self.scale = 1.0;
        self.mode = ZoomMode::Actual;
        self.clamp_pan(source, client);
    }

    /// Pan by a screen-px delta, then clamp so the image can't be dragged fully off-screen
    /// (an axis smaller than the client stays centered).
    pub fn pan_by(&mut self, dx: f32, dy: f32, source: (u32, u32), client: (u32, u32)) {
        if dx.is_finite() {
            self.pan.0 += dx;
        }
        if dy.is_finite() {
            self.pan.1 += dy;
        }
        self.clamp_pan(source, client);
    }

    /// Destination rectangle of the whole image in client coordinates.
    pub fn dest_rect(&self, source: (u32, u32), client: (u32, u32)) -> Rect {
        let w = source.0 as f32 * self.scale;
        let h = source.1 as f32 * self.scale;
        let x = client.0 as f32 * 0.5 + self.pan.0 - w * 0.5;
        let y = client.1 as f32 * 0.5 + self.pan.1 - h * 0.5;
        Rect { x, y, w, h }
    }

    /// True when the visible scale needs more source detail than a downscaled bitmap of
    /// `decoded` size provides (i.e. the app should request a full-resolution decode).
    pub fn needs_full_resolution(&self, source: (u32, u32), decoded: (u32, u32)) -> bool {
        if decoded.0 >= source.0 && decoded.1 >= source.1 {
            return false;
        }
        const EPS_PX: f32 = 1.0;
        let vis_w = source.0 as f32 * self.scale;
        let vis_h = source.1 as f32 * self.scale;
        (decoded.0 < source.0 && vis_w > decoded.0 as f32 + EPS_PX)
            || (decoded.1 < source.1 && vis_h > decoded.1 as f32 + EPS_PX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "expected {a} ≈ {b}");
    }

    #[test]
    fn fit_downscales_large_and_never_upscales_small() {
        let v = View::fit((4000, 3000), (1920, 1080));
        assert_eq!(v.mode, ZoomMode::Fit);
        approx_eq(v.scale, 1080.0 / 3000.0);
        assert_eq!(v.pan, (0.0, 0.0));
        let r = v.dest_rect((4000, 3000), (1920, 1080));
        approx_eq(r.w, 1440.0);
        approx_eq(r.h, 1080.0);
        approx_eq(r.x, 240.0);
        approx_eq(r.y, 0.0);

        // Small image stays 1:1, centered
        let small = View::fit((200, 100), (1920, 1080));
        approx_eq(small.scale, 1.0);
        let rs = small.dest_rect((200, 100), (1920, 1080));
        approx_eq(rs.x, 860.0);
        approx_eq(rs.y, 490.0);
        approx_eq(rs.w, 200.0);
        approx_eq(rs.h, 100.0);
    }

    #[test]
    fn zoom_at_keeps_source_point_under_cursor_fixed() {
        let source = (2000, 2000);
        let client = (1000, 1000);
        let mut v = View::fit(source, client); // scale = 0.5, pan = (0, 0)
        let cursor = (700.0, 400.0); // 200 right, 100 up of center -> source offset (+400, -200)

        let before = v.dest_rect(source, client);
        let src_u = (cursor.0 - before.x) / before.w;
        let src_v = (cursor.1 - before.y) / before.h;

        v.zoom_at(2.0, cursor, source, client); // scale = 1.0, img = 2000x2000 > client 1000x1000
        assert_eq!(v.mode, ZoomMode::Custom);
        approx_eq(v.scale, 1.0);

        let after = v.dest_rect(source, client);
        let screen_x = after.x + src_u * after.w;
        let screen_y = after.y + src_v * after.h;
        approx_eq(screen_x, cursor.0);
        approx_eq(screen_y, cursor.1);
    }

    #[test]
    fn zoom_clamps_to_min_and_max() {
        let mut v = View::fit((1000, 1000), (1000, 1000));
        v.zoom_at(1000.0, (500.0, 500.0), (1000, 1000), (1000, 1000));
        approx_eq(v.scale, View::MAX_SCALE);
        v.zoom_at(1e-6, (500.0, 500.0), (1000, 1000), (1000, 1000));
        approx_eq(v.scale, View::MIN_SCALE);
    }

    #[test]
    fn pan_clamping_centers_small_axis_and_bounds_large_axis() {
        // Wide image at 1:1 in 1000x1000 client: X can pan ±500, Y (600 <= 1000) stays centered
        let source = (2000, 600);
        let client = (1000, 1000);
        let mut v = View::fit(source, client);
        v.actual_size(None, source, client);
        assert_eq!(v.mode, ZoomMode::Actual);
        approx_eq(v.scale, 1.0);

        v.pan_by(9999.0, 9999.0, source, client);
        approx_eq(v.pan.0, 500.0);
        approx_eq(v.pan.1, 0.0);
        let r = v.dest_rect(source, client);
        approx_eq(r.x, 0.0); // left edge flush with client left edge

        v.pan_by(-9999.0, -9999.0, source, client);
        approx_eq(v.pan.0, -500.0);
        approx_eq(v.pan.1, 0.0);
        let r2 = v.dest_rect(source, client);
        approx_eq(r2.x + r2.w, 1000.0); // right edge flush with client right edge
    }

    #[test]
    fn relayout_updates_fit_and_clamps_custom() {
        let source = (2000, 1000);
        let mut v = View::fit(source, (1000, 1000));
        approx_eq(v.scale, 0.5);
        v.relayout(source, (500, 500));
        approx_eq(v.scale, 0.25);

        v.actual_size(None, source, (500, 500));
        v.pan_by(1000.0, 1000.0, source, (500, 500));
        approx_eq(v.pan.0, 750.0);
        // Grow client so image fits: pan snaps to 0
        v.relayout(source, (2500, 1500));
        approx_eq(v.scale, 1.0);
        assert_eq!(v.pan, (0.0, 0.0));
    }

    #[test]
    fn needs_full_resolution_checks_scale_and_downscale() {
        let source = (4000, 3000);
        let decoded = (1440, 1080);

        // At fit scale (0.36): visible is 1440x1080 == decoded -> false
        let mut v = View::fit(source, (1920, 1080));
        assert!(!v.needs_full_resolution(source, decoded));

        // Slightly zoomed in -> needs full resolution
        v.zoom_at(1.1, (960.0, 540.0), source, (1920, 1080));
        assert!(v.needs_full_resolution(source, decoded));

        // Already full resolution: even at 4x zoom -> false
        v.zoom_at(10.0, (960.0, 540.0), source, (1920, 1080));
        assert!(!v.needs_full_resolution(source, source));
    }
}
