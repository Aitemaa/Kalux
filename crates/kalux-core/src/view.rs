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

    /// Fit view for an image of `source` size in a client of `client` size.
    pub fn fit(source: (u32, u32), client: (u32, u32)) -> Self {
        let _ = (source, client);
        todo!("kalux-core")
    }

    /// Recompute `scale` after a resize when in `Fit`/`Actual` mode; clamp pan otherwise.
    pub fn relayout(&mut self, source: (u32, u32), client: (u32, u32)) {
        let _ = (source, client);
        todo!("kalux-core")
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
        let _ = (factor, cursor, source, client);
        todo!("kalux-core")
    }

    /// Set 100% centered on `cursor` (or client center when `None`).
    pub fn actual_size(
        &mut self,
        cursor: Option<(f32, f32)>,
        source: (u32, u32),
        client: (u32, u32),
    ) {
        let _ = (cursor, source, client);
        todo!("kalux-core")
    }

    /// Pan by a screen-px delta, then clamp so the image can't be dragged fully off-screen
    /// (an axis smaller than the client stays centered).
    pub fn pan_by(&mut self, dx: f32, dy: f32, source: (u32, u32), client: (u32, u32)) {
        let _ = (dx, dy, source, client);
        todo!("kalux-core")
    }

    /// Destination rectangle of the whole image in client coordinates.
    pub fn dest_rect(&self, source: (u32, u32), client: (u32, u32)) -> Rect {
        let _ = (source, client);
        todo!("kalux-core")
    }

    /// True when the visible scale needs more source detail than a downscaled bitmap of
    /// `decoded` size provides (i.e. the app should request a full-resolution decode).
    pub fn needs_full_resolution(&self, source: (u32, u32), decoded: (u32, u32)) -> bool {
        let _ = (source, decoded);
        todo!("kalux-core")
    }
}
