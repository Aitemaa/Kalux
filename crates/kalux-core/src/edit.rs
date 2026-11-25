//! Parametric non-destructive Edit Mode core (zero external dependencies).
//!
//! Everything here is pure data + math:
//! - [`EditRecipe`] (~56 bytes) describes crop, 90° turns, flips, and tone adjustments.
//! - [`EditHistory`] caps undo/redo at 64 recipe snapshots (< 4 KB total RAM).
//! - [`ToneLut`] folds exposure, contrast, warmth, saturation, and monochrome into a
//!   < 1 KB lookup table + 3×3 matrix shared identically by preview renderers and export.
//! - [`DockLayout`] computes exact hit-testable UI rectangles shared across Windows and Linux.

use crate::Rect;

/// Maximum undo/redo history entries kept in memory (64 * ~56 B < 4 KB).
pub const MAX_UNDO_STEPS: usize = 64;

/// Minimum output crop dimension in full-resolution source pixels.
pub const MIN_CROP_SOURCE_PX: u32 = 16;

/// Normalized `[0.0..=1.0]` crop rectangle in **oriented** image coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for CropRect {
    fn default() -> Self {
        Self::FULL
    }
}

/// Integer pixel crop rectangle in oriented full-resolution source coordinates.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl CropRect {
    pub const FULL: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    pub fn is_full(self) -> bool {
        self.x.abs() < 1e-4
            && self.y.abs() < 1e-4
            && (self.w - 1.0).abs() < 1e-4
            && (self.h - 1.0).abs() < 1e-4
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    /// Clamp rectangle to `[0, 1] x [0, 1]` while preserving minimum normalized size `(min_w, min_h)`.
    pub fn clamped(self, min_w: f32, min_h: f32) -> Self {
        let mw = min_w.clamp(1e-4, 1.0);
        let mh = min_h.clamp(1e-4, 1.0);
        let w = if self.w.is_finite() {
            self.w.clamp(mw, 1.0)
        } else {
            1.0
        };
        let h = if self.h.is_finite() {
            self.h.clamp(mh, 1.0)
        } else {
            1.0
        };
        let x = if self.x.is_finite() {
            self.x.clamp(0.0, 1.0 - w)
        } else {
            0.0
        };
        let y = if self.y.is_finite() {
            self.y.clamp(0.0, 1.0 - h)
        } else {
            0.0
        };
        Self { x, y, w, h }
    }

    /// Convert to integer source pixels for an oriented image of `(img_w, img_h)`.
    pub fn to_pixel_rect(self, img_w: u32, img_h: u32) -> PixelRect {
        if img_w == 0 || img_h == 0 {
            return PixelRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            };
        }
        let x0 = ((self.x * img_w as f32).round() as i64).clamp(0, (img_w - 1) as i64) as u32;
        let y0 = ((self.y * img_h as f32).round() as i64).clamp(0, (img_h - 1) as i64) as u32;
        let x1 = ((self.right() * img_w as f32).round() as i64).clamp((x0 + 1) as i64, img_w as i64)
            as u32;
        let y1 = ((self.bottom() * img_h as f32).round() as i64)
            .clamp((y0 + 1) as i64, img_h as i64) as u32;
        PixelRect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(1),
            h: (y1 - y0).max(1),
        }
    }

    /// Project this normalized crop rect onto screen coordinates given the image's `dest` rect.
    pub fn to_screen_rect(self, image_dest: Rect) -> Rect {
        Rect {
            x: image_dest.x + self.x * image_dest.w,
            y: image_dest.y + self.y * image_dest.h,
            w: self.w * image_dest.w,
            h: self.h * image_dest.h,
        }
    }

    /// Transform crop rect when the oriented image rotates 90° clockwise: `(x, y) -> (1 - y, x)`.
    pub fn rotate_cw(self) -> Self {
        Self {
            x: (1.0 - (self.y + self.h)).clamp(0.0, 1.0),
            y: self.x.clamp(0.0, 1.0),
            w: self.h.clamp(0.0, 1.0),
            h: self.w.clamp(0.0, 1.0),
        }
    }

    /// Transform crop rect when the oriented image rotates 90° counter-clockwise: `(x, y) -> (y, 1 - x)`.
    pub fn rotate_ccw(self) -> Self {
        Self {
            x: self.y.clamp(0.0, 1.0),
            y: (1.0 - (self.x + self.w)).clamp(0.0, 1.0),
            w: self.h.clamp(0.0, 1.0),
            h: self.w.clamp(0.0, 1.0),
        }
    }

    /// Mirror crop rect horizontally.
    pub fn flip_h(self) -> Self {
        Self {
            x: (1.0 - (self.x + self.w)).clamp(0.0, 1.0),
            ..self
        }
    }

    /// Mirror crop rect vertically.
    pub fn flip_v(self) -> Self {
        Self {
            y: (1.0 - (self.y + self.h)).clamp(0.0, 1.0),
            ..self
        }
    }

    /// Minimum normalized `(min_w, min_h)` so the crop stays `>= MIN_CROP_SOURCE_PX` source px.
    pub fn min_norm_for(img_w: u32, img_h: u32) -> (f32, f32) {
        let mw = if img_w <= MIN_CROP_SOURCE_PX {
            1.0
        } else {
            (MIN_CROP_SOURCE_PX as f32 / img_w as f32).min(1.0)
        };
        let mh = if img_h <= MIN_CROP_SOURCE_PX {
            1.0
        } else {
            (MIN_CROP_SOURCE_PX as f32 / img_h as f32).min(1.0)
        };
        (mw, mh)
    }

    /// Center the largest crop matching `target_ratio` (`width / height` in pixels) inside `self`.
    pub fn fit_aspect(self, target_ratio: f32, img_w: u32, img_h: u32) -> Self {
        if !target_ratio.is_finite() || target_ratio <= 0.0 || img_w == 0 || img_h == 0 {
            return self;
        }
        let img_aspect = img_w as f32 / img_h as f32;
        // Desired normalized ratio: nw / nh = target_ratio / img_aspect.
        let norm_ratio = target_ratio / img_aspect;
        let cx = self.x + self.w * 0.5;
        let cy = self.y + self.h * 0.5;

        let (mut nw, mut nh) = if self.w / self.h > norm_ratio {
            (self.h * norm_ratio, self.h)
        } else {
            (self.w, self.w / norm_ratio)
        };

        let (min_w, min_h) = Self::min_norm_for(img_w, img_h);
        if nw < min_w {
            nw = min_w;
            nh = (nw / norm_ratio).min(1.0);
        }
        if nh < min_h {
            nh = min_h;
            nw = (nh * norm_ratio).min(1.0);
        }
        if nw > 1.0 {
            nw = 1.0;
            nh = (nw / norm_ratio).min(1.0);
        }
        if nh > 1.0 {
            nh = 1.0;
            nw = (nh * norm_ratio).min(1.0);
        }

        let x = (cx - nw * 0.5).clamp(0.0, 1.0 - nw);
        let y = (cy - nh * 0.5).clamp(0.0, 1.0 - nh);
        Self { x, y, w: nw, h: nh }
    }

    /// Apply a pointer drag delta `(dx, dy)` in normalized image coordinates to `self` (the
    /// snapshot captured at pointer-down).
    pub fn apply_drag(
        self,
        handle: CropHandle,
        dx: f32,
        dy: f32,
        locked_ratio: Option<f32>,
        img_w: u32,
        img_h: u32,
    ) -> Self {
        let (min_w, min_h) = Self::min_norm_for(img_w, img_h);

        if handle == CropHandle::Move {
            let x = (self.x + dx).clamp(0.0, 1.0 - self.w);
            let y = (self.y + dy).clamp(0.0, 1.0 - self.h);
            return Self { x, y, ..self };
        }

        let mut l = self.x;
        let mut t = self.y;
        let mut r = self.right();
        let mut b = self.bottom();

        match handle {
            CropHandle::Nw => {
                l = (l + dx).clamp(0.0, r - min_w);
                t = (t + dy).clamp(0.0, b - min_h);
            }
            CropHandle::Ne => {
                r = (r + dx).clamp(l + min_w, 1.0);
                t = (t + dy).clamp(0.0, b - min_h);
            }
            CropHandle::Sw => {
                l = (l + dx).clamp(0.0, r - min_w);
                b = (b + dy).clamp(t + min_h, 1.0);
            }
            CropHandle::Se => {
                r = (r + dx).clamp(l + min_w, 1.0);
                b = (b + dy).clamp(t + min_h, 1.0);
            }
            CropHandle::N => {
                t = (t + dy).clamp(0.0, b - min_h);
            }
            CropHandle::S => {
                b = (b + dy).clamp(t + min_h, 1.0);
            }
            CropHandle::W => {
                l = (l + dx).clamp(0.0, r - min_w);
            }
            CropHandle::E => {
                r = (r + dx).clamp(l + min_w, 1.0);
            }
            CropHandle::Move => unreachable!(),
        }

        let Some(target_ratio) = locked_ratio.filter(|v| v.is_finite() && *v > 0.0) else {
            return Self {
                x: l,
                y: t,
                w: r - l,
                h: b - t,
            }
            .clamped(min_w, min_h);
        };

        if img_w == 0 || img_h == 0 {
            return Self {
                x: l,
                y: t,
                w: r - l,
                h: b - t,
            }
            .clamped(min_w, min_h);
        }

        let img_aspect = img_w as f32 / img_h as f32;
        let norm_ratio = target_ratio / img_aspect;
        let req_w = (r - l).max(min_w);
        let req_h = (b - t).max(min_h);

        // Anchor the opposite corner/edge and solve the largest valid rectangle with `norm_ratio`.
        match handle {
            CropHandle::Nw => {
                let (w, h) = solve_aspect_in_bounds(req_w, req_h, norm_ratio, r, b, min_w, min_h);
                Self {
                    x: (r - w).clamp(0.0, 1.0 - w),
                    y: (b - h).clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::Ne => {
                let (w, h) =
                    solve_aspect_in_bounds(req_w, req_h, norm_ratio, 1.0 - l, b, min_w, min_h);
                Self {
                    x: l.clamp(0.0, 1.0 - w),
                    y: (b - h).clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::Sw => {
                let (w, h) =
                    solve_aspect_in_bounds(req_w, req_h, norm_ratio, r, 1.0 - t, min_w, min_h);
                Self {
                    x: (r - w).clamp(0.0, 1.0 - w),
                    y: t.clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::Se => {
                let (w, h) = solve_aspect_in_bounds(
                    req_w,
                    req_h,
                    norm_ratio,
                    1.0 - l,
                    1.0 - t,
                    min_w,
                    min_h,
                );
                Self {
                    x: l.clamp(0.0, 1.0 - w),
                    y: t.clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::N => {
                let cx = self.x + self.w * 0.5;
                let max_h = b.min((cx.min(1.0 - cx) * 2.0) / norm_ratio).max(min_h);
                let h = req_h.clamp(min_h, max_h);
                let w = (h * norm_ratio).clamp(min_w, 1.0);
                Self {
                    x: (cx - w * 0.5).clamp(0.0, 1.0 - w),
                    y: (b - h).clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::S => {
                let cx = self.x + self.w * 0.5;
                let max_h = (1.0 - t)
                    .min((cx.min(1.0 - cx) * 2.0) / norm_ratio)
                    .max(min_h);
                let h = req_h.clamp(min_h, max_h);
                let w = (h * norm_ratio).clamp(min_w, 1.0);
                Self {
                    x: (cx - w * 0.5).clamp(0.0, 1.0 - w),
                    y: t.clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::W => {
                let cy = self.y + self.h * 0.5;
                let max_w = r.min((cy.min(1.0 - cy) * 2.0) * norm_ratio).max(min_w);
                let w = req_w.clamp(min_w, max_w);
                let h = (w / norm_ratio).clamp(min_h, 1.0);
                Self {
                    x: (r - w).clamp(0.0, 1.0 - w),
                    y: (cy - h * 0.5).clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::E => {
                let cy = self.y + self.h * 0.5;
                let max_w = (1.0 - l)
                    .min((cy.min(1.0 - cy) * 2.0) * norm_ratio)
                    .max(min_w);
                let w = req_w.clamp(min_w, max_w);
                let h = (w / norm_ratio).clamp(min_h, 1.0);
                Self {
                    x: l.clamp(0.0, 1.0 - w),
                    y: (cy - h * 0.5).clamp(0.0, 1.0 - h),
                    w,
                    h,
                }
            }
            CropHandle::Move => unreachable!(),
        }
    }

    /// Nudge crop box by `(dx_px, dy_px)` source pixels; when `resize` is true, adjusts bottom-right.
    pub fn nudge_px(self, dx_px: i32, dy_px: i32, resize: bool, img_w: u32, img_h: u32) -> Self {
        if img_w == 0 || img_h == 0 {
            return self;
        }
        let dx = dx_px as f32 / img_w as f32;
        let dy = dy_px as f32 / img_h as f32;
        let (min_w, min_h) = Self::min_norm_for(img_w, img_h);
        if resize {
            Self {
                x: self.x,
                y: self.y,
                w: (self.w + dx).clamp(min_w, 1.0 - self.x),
                h: (self.h + dy).clamp(min_h, 1.0 - self.y),
            }
        } else {
            Self {
                x: (self.x + dx).clamp(0.0, 1.0 - self.w),
                y: (self.y + dy).clamp(0.0, 1.0 - self.h),
                w: self.w,
                h: self.h,
            }
        }
    }
}

fn solve_aspect_in_bounds(
    req_w: f32,
    req_h: f32,
    norm_ratio: f32,
    max_w: f32,
    max_h: f32,
    min_w: f32,
    min_h: f32,
) -> (f32, f32) {
    // Pick dominant axis from the user's drag, then clamp cleanly to available bounds.
    let (mut w, mut h) = if req_w / norm_ratio >= req_h {
        (req_w, req_w / norm_ratio)
    } else {
        (req_h * norm_ratio, req_h)
    };

    if w > max_w {
        w = max_w;
        h = w / norm_ratio;
    }
    if h > max_h {
        h = max_h;
        w = h * norm_ratio;
    }
    if w < min_w {
        w = min_w.min(max_w);
        h = (w / norm_ratio).clamp(min_h.min(max_h), max_h);
    }
    if h < min_h {
        h = min_h.min(max_h);
        w = (h * norm_ratio).clamp(min_w.min(max_w), max_w);
    }
    (
        w.clamp(1e-4, max_w.max(1e-4)),
        h.clamp(1e-4, max_h.max(1e-4)),
    )
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CropHandle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
    Move,
}

/// Hit-test a pointer position against the screen-space crop rectangle handles and interior.
pub fn hit_test_crop(pt: (f32, f32), crop: Rect, hit_radius: f32) -> Option<CropHandle> {
    let (px, py) = pt;
    let r = hit_radius.max(6.0);
    let l = crop.x;
    let t = crop.y;
    let ri = crop.x + crop.w;
    let b = crop.y + crop.h;

    let near = |ax: f32, ay: f32| (px - ax).abs() <= r && (py - ay).abs() <= r;

    // Corners take highest priority.
    if near(l, t) {
        return Some(CropHandle::Nw);
    }
    if near(ri, t) {
        return Some(CropHandle::Ne);
    }
    if near(l, b) {
        return Some(CropHandle::Sw);
    }
    if near(ri, b) {
        return Some(CropHandle::Se);
    }

    // Edges next.
    if px >= l + r * 0.5 && px <= ri - r * 0.5 {
        if (py - t).abs() <= r {
            return Some(CropHandle::N);
        }
        if (py - b).abs() <= r {
            return Some(CropHandle::S);
        }
    }
    if py >= t + r * 0.5 && py <= b - r * 0.5 {
        if (px - l).abs() <= r {
            return Some(CropHandle::W);
        }
        if (px - ri).abs() <= r {
            return Some(CropHandle::E);
        }
    }

    // Interior drag moves the crop rectangle.
    if px >= l && px <= ri && py >= t && py <= b {
        return Some(CropHandle::Move);
    }

    None
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum AspectPreset {
    #[default]
    Free,
    Original,
    Square,
    FourThree,
    ThreeTwo,
    SixteenNine,
}

impl AspectPreset {
    pub const ALL: [Self; 6] = [
        Self::Free,
        Self::Original,
        Self::Square,
        Self::FourThree,
        Self::ThreeTwo,
        Self::SixteenNine,
    ];

    pub const fn label(self, swapped: bool) -> &'static str {
        match (self, swapped) {
            (Self::Free, _) => "Free",
            (Self::Original, _) => "Orig",
            (Self::Square, _) => "1:1",
            (Self::FourThree, false) => "4:3",
            (Self::FourThree, true) => "3:4",
            (Self::ThreeTwo, false) => "3:2",
            (Self::ThreeTwo, true) => "2:3",
            (Self::SixteenNine, false) => "16:9",
            (Self::SixteenNine, true) => "9:16",
        }
    }

    /// Pixel aspect ratio `width / height` for this preset, or `None` for `Free`.
    pub fn ratio(self, img_w: u32, img_h: u32, swapped: bool) -> Option<f32> {
        let base = match self {
            Self::Free => return None,
            Self::Original => {
                if img_w == 0 || img_h == 0 {
                    return None;
                }
                img_w as f32 / img_h as f32
            }
            Self::Square => 1.0,
            Self::FourThree => 4.0 / 3.0,
            Self::ThreeTwo => 3.0 / 2.0,
            Self::SixteenNine => 16.0 / 9.0,
        };
        Some(if swapped && self != Self::Square {
            1.0 / base
        } else {
            base
        })
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum QuarterTurns {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl QuarterTurns {
    pub const fn cw(self) -> Self {
        match self {
            Self::R0 => Self::R90,
            Self::R90 => Self::R180,
            Self::R180 => Self::R270,
            Self::R270 => Self::R0,
        }
    }

    pub const fn ccw(self) -> Self {
        match self {
            Self::R0 => Self::R270,
            Self::R90 => Self::R0,
            Self::R180 => Self::R90,
            Self::R270 => Self::R180,
        }
    }

    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::R90 | Self::R270)
    }

    pub const fn oriented_dims(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps_axes() { (h, w) } else { (w, h) }
    }

    /// Degrees clockwise: `0.0`, `90.0`, `180.0`, or `270.0`.
    pub const fn degrees_cw(self) -> f32 {
        match self {
            Self::R0 => 0.0,
            Self::R90 => 90.0,
            Self::R180 => 180.0,
            Self::R270 => 270.0,
        }
    }

    /// Map a normalized oriented coordinate `(u, v)` in `[0, 1] x [0, 1]` back to the
    /// unrotated/unflipped source image coordinate `(su, sv)`.
    ///
    /// Semantic order of operations applied to the source image:
    /// 1. Rotate CW by `self` (`R0` / `R90` / `R180` / `R270`)
    /// 2. Flip horizontally if `flip_h` (in the post-rotation frame)
    /// 3. Flip vertically if `flip_v` (in the post-rotation frame)
    #[inline(always)]
    pub fn inverse_uv(self, u: f32, v: f32, flip_h: bool, flip_v: bool) -> (f32, f32) {
        let u1 = if flip_h { 1.0 - u } else { u };
        let v1 = if flip_v { 1.0 - v } else { v };
        match self {
            Self::R0 => (u1, v1),
            Self::R90 => (v1, 1.0 - u1),
            Self::R180 => (1.0 - u1, 1.0 - v1),
            Self::R270 => (1.0 - v1, u1),
        }
    }

    /// Map an integer pixel `(ox, oy)` in oriented image space back to `(sx, sy)` in the
    /// original unrotated `(src_w, src_h)` bitmap.
    #[inline(always)]
    pub fn inverse_pixel(
        self,
        ox: u32,
        oy: u32,
        src_w: u32,
        src_h: u32,
        flip_h: bool,
        flip_v: bool,
    ) -> (u32, u32) {
        let (ow, oh) = self.oriented_dims(src_w, src_h);
        let x1 = if flip_h { ow - 1 - ox } else { ox };
        let y1 = if flip_v { oh - 1 - oy } else { oy };
        match self {
            Self::R0 => (x1, y1),
            Self::R90 => (y1, src_h - 1 - x1),
            Self::R180 => (src_w - 1 - x1, src_h - 1 - y1),
            Self::R270 => (src_w - 1 - y1, x1),
        }
    }
}

/// Parametric tone & color controls in `[-1.0..=1.0]`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct ToneAdjustments {
    /// `-1.0..=+1.0` (maps to ±2.0 EV with soft highlight roll-off).
    pub exposure: f32,
    /// `-1.0..=+1.0` (midpoint-pivoted contrast).
    pub contrast: f32,
    /// `-1.0..=+1.0` (`-1.0` = grayscale).
    pub saturation: f32,
    /// `-1.0..=+1.0` (cool blue ↔ warm amber).
    pub warmth: f32,
    /// One-click Rec.709 monochrome toggle.
    pub monochrome: bool,
}

impl ToneAdjustments {
    pub fn is_identity(&self) -> bool {
        !self.monochrome
            && self.exposure.abs() < 1e-4
            && self.contrast.abs() < 1e-4
            && self.saturation.abs() < 1e-4
            && self.warmth.abs() < 1e-4
    }

    pub fn get(&self, slider: ToneSlider) -> f32 {
        match slider {
            ToneSlider::Exposure => self.exposure,
            ToneSlider::Contrast => self.contrast,
            ToneSlider::Saturation => self.saturation,
            ToneSlider::Warmth => self.warmth,
        }
    }

    pub fn set(&mut self, slider: ToneSlider, value: f32) {
        let v = if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match slider {
            ToneSlider::Exposure => self.exposure = v,
            ToneSlider::Contrast => self.contrast = v,
            ToneSlider::Saturation => self.saturation = v,
            ToneSlider::Warmth => self.warmth = v,
        }
    }

    /// Compact integer fingerprint for cache invalidation in renderers.
    pub fn quantized_key(&self) -> [i32; 5] {
        [
            (self.exposure * 1000.0).round() as i32,
            (self.contrast * 1000.0).round() as i32,
            (self.saturation * 1000.0).round() as i32,
            (self.warmth * 1000.0).round() as i32,
            i32::from(self.monochrome),
        ]
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ToneSlider {
    Exposure,
    Contrast,
    Saturation,
    Warmth,
}

impl ToneSlider {
    pub const ALL: [Self; 4] = [
        Self::Exposure,
        Self::Contrast,
        Self::Saturation,
        Self::Warmth,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Exposure => "Exposure",
            Self::Contrast => "Contrast",
            Self::Saturation => "Saturation",
            Self::Warmth => "Warmth",
        }
    }
}

/// Fused `< 1 KB` tone/color lookup operator shared by preview renderers and full-res export.
///
/// Pipeline per straight-alpha sRGB pixel:
/// 1. Per-channel 1D LUT (`r_lut`, `g_lut`, `b_lut`): Exposure (linear light with soft highlight
///    roll-off) + Contrast (midpoint S-curve) + Warmth (luminance-balanced R/B temperature shift).
/// 2. Optional 3×3 Rec.709 saturation / monochrome matrix (integer fixed-point `Q10`).
#[derive(Clone, Debug)]
pub struct ToneLut {
    pub identity: bool,
    pub r_lut: [u8; 256],
    pub g_lut: [u8; 256],
    pub b_lut: [u8; 256],
    /// Fixed-point (`* 1024`) saturation factor `s` in `0..=2048` (`1024` = identity, `0` = mono).
    sat_q10: i32,
}

impl ToneLut {
    pub fn from_adjustments(adj: &ToneAdjustments) -> Self {
        let mut r_lut = [0u8; 256];
        let mut g_lut = [0u8; 256];
        let mut b_lut = [0u8; 256];
        if adj.is_identity() {
            for i in 0..256 {
                let v = i as u8;
                r_lut[i] = v;
                g_lut[i] = v;
                b_lut[i] = v;
            }
            return Self {
                identity: true,
                r_lut,
                g_lut,
                b_lut,
                sat_q10: 1024,
            };
        }

        let exp = adj.exposure.clamp(-1.0, 1.0);
        let con = adj.contrast.clamp(-1.0, 1.0);
        let warm = adj.warmth.clamp(-1.0, 1.0);
        let exp_gain = 2.0f32.powf(exp * 1.8);
        // Contrast slope around 0.5: smooth range [0.35 .. 2.35].
        let con_slope = if con >= 0.0 {
            1.0 + con * 1.35
        } else {
            1.0 + con * 0.65
        };

        // Warmth channel multipliers preserving perceived luminance.
        let r_warm = 1.0 + warm * 0.16;
        let g_warm = 1.0 + warm * 0.02;
        let b_warm = 1.0 - warm * 0.18;

        for i in 0..256 {
            let srgb = i as f32 / 255.0;
            // Decode approximate sRGB -> linear for photographic exposure gain.
            let lin = srgb_to_linear(srgb);
            let mut exposed = lin * exp_gain;
            // Soft highlight shoulder when lifting exposure so bright skies roll off smoothly.
            if exp > 0.0 && exposed > 0.82 {
                let excess = exposed - 0.82;
                exposed = 0.82 + 0.18 * (1.0 - (-excess / 0.35).exp());
            }
            let base = linear_to_srgb(exposed.clamp(0.0, 1.0));
            // Pivot contrast smoothly around 0.5.
            let contrasted = ((base - 0.5) * con_slope + 0.5).clamp(0.0, 1.0);

            r_lut[i] = ((contrasted * r_warm).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            g_lut[i] = ((contrasted * g_warm).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            b_lut[i] = ((contrasted * b_warm).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }

        let sat_f = if adj.monochrome {
            0.0
        } else {
            (1.0 + adj.saturation.clamp(-1.0, 1.0)).clamp(0.0, 2.0)
        };
        let sat_q10 = (sat_f * 1024.0 + 0.5) as i32;

        Self {
            identity: false,
            r_lut,
            g_lut,
            b_lut,
            sat_q10,
        }
    }

    #[inline(always)]
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    /// Apply fused LUT + Rec.709 saturation/monochrome to one straight-alpha RGB triple.
    #[inline(always)]
    pub fn apply_rgb(&self, r: u8, g: u8, b: u8) -> [u8; 3] {
        if self.identity {
            return [r, g, b];
        }
        let rr = self.r_lut[r as usize];
        let gg = self.g_lut[g as usize];
        let bb = self.b_lut[b as usize];

        if self.sat_q10 == 1024 {
            return [rr, gg, bb];
        }

        // Rec.709 luma in Q10: 0.2126*1024≈218, 0.7152*1024≈732, 0.0722*1024≈74 (sum = 1024).
        let luma = (218 * i32::from(rr) + 732 * i32::from(gg) + 74 * i32::from(bb) + 512) >> 10;
        if self.sat_q10 == 0 {
            let y = luma.clamp(0, 255) as u8;
            return [y, y, y];
        }
        let s = self.sat_q10;
        let out_r = luma + (((i32::from(rr) - luma) * s + 512) >> 10);
        let out_g = luma + (((i32::from(gg) - luma) * s + 512) >> 10);
        let out_b = luma + (((i32::from(bb) - luma) * s + 512) >> 10);
        [
            out_r.clamp(0, 255) as u8,
            out_g.clamp(0, 255) as u8,
            out_b.clamp(0, 255) as u8,
        ]
    }

    /// Apply fused LUT + saturation to one **BGRA8 premultiplied** pixel.
    #[inline(always)]
    pub fn apply_bgra_premul(&self, px: [u8; 4]) -> [u8; 4] {
        if self.identity {
            return px;
        }
        let a = px[3];
        if a == 255 {
            let [r, g, b] = self.apply_rgb(px[2], px[1], px[0]);
            return [b, g, r, 255];
        }
        if a == 0 {
            return [0, 0, 0, 0];
        }
        let a32 = u32::from(a);
        let sr = ((u32::from(px[2]) * 255 + a32 / 2) / a32).min(255) as u8;
        let sg = ((u32::from(px[1]) * 255 + a32 / 2) / a32).min(255) as u8;
        let sb = ((u32::from(px[0]) * 255 + a32 / 2) / a32).min(255) as u8;
        let [r, g, b] = self.apply_rgb(sr, sg, sb);
        let pr = ((u32::from(r) * a32 + 127) / 255) as u8;
        let pg = ((u32::from(g) * a32 + 127) / 255) as u8;
        let pb = ((u32::from(b) * a32 + 127) / 255) as u8;
        [pb, pg, pr, a]
    }

    /// Apply in-place to a tightly packed **BGRA8 premultiplied** pixel buffer.
    pub fn apply_bgra_premul_slice(&self, pixels: &mut [u8]) {
        if self.identity {
            return;
        }
        for px in pixels.as_chunks_mut::<4>().0 {
            *px = self.apply_bgra_premul(*px);
        }
    }

    /// Apply in-place to a tightly packed straight-alpha **RGB8** buffer (e.g. JPEG export).
    pub fn apply_rgb_slice(&self, pixels: &mut [u8]) {
        if self.identity {
            return;
        }
        for px in pixels.as_chunks_mut::<3>().0 {
            *px = self.apply_rgb(px[0], px[1], px[2]);
        }
    }

    /// Apply in-place to a tightly packed straight-alpha **RGBA8** buffer (e.g. PNG/TIFF export).
    pub fn apply_rgba_straight_slice(&self, pixels: &mut [u8]) {
        if self.identity {
            return;
        }
        for px in pixels.as_chunks_mut::<4>().0 {
            if px[3] == 0 {
                continue;
            }
            let [r, g, b] = self.apply_rgb(px[0], px[1], px[2]);
            px[0] = r;
            px[1] = g;
            px[2] = b;
        }
    }
}

#[inline]
fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Complete non-destructive edit recipe (~56 bytes, `Copy`, zero heap allocations).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct EditRecipe {
    pub crop: CropRect,
    pub turns: QuarterTurns,
    pub flip_h: bool,
    pub flip_v: bool,
    pub tone: ToneAdjustments,
}

impl EditRecipe {
    pub fn is_identity(&self) -> bool {
        self.crop.is_full()
            && self.turns == QuarterTurns::R0
            && !self.flip_h
            && !self.flip_v
            && self.tone.is_identity()
    }

    /// True when the only edits are 90° turns and/or flips (eligible for lossless JPEG EXIF fast-path).
    pub fn is_orientation_only(&self) -> bool {
        !self.is_identity() && self.crop.is_full() && self.tone.is_identity()
    }

    /// Dimensions of the oriented (pre-crop) image.
    pub const fn oriented_dims(&self, src_w: u32, src_h: u32) -> (u32, u32) {
        self.turns.oriented_dims(src_w, src_h)
    }

    /// Output pixel rectangle in oriented full-resolution coordinates.
    pub fn output_pixel_rect(&self, src_w: u32, src_h: u32) -> PixelRect {
        let (ow, oh) = self.oriented_dims(src_w, src_h);
        self.crop.to_pixel_rect(ow, oh)
    }
}

/// Bounded undo/redo stack of [`EditRecipe`] snapshots (64 entries < 4 KB).
#[derive(Clone, Debug)]
pub struct EditHistory {
    entries: Vec<EditRecipe>,
    cursor: usize,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl EditHistory {
    pub fn new() -> Self {
        Self {
            entries: vec![EditRecipe::default()],
            cursor: 0,
        }
    }

    pub fn current(&self) -> EditRecipe {
        self.entries[self.cursor]
    }

    pub fn current_mut(&mut self) -> &mut EditRecipe {
        &mut self.entries[self.cursor]
    }

    /// Record a new committed recipe state (no-op if equal to current).
    pub fn push(&mut self, recipe: EditRecipe) {
        if self.entries[self.cursor] == recipe {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(recipe);
        if self.entries.len() > MAX_UNDO_STEPS {
            let excess = self.entries.len() - MAX_UNDO_STEPS;
            self.entries.drain(0..excess);
        }
        self.cursor = self.entries.len() - 1;
    }

    /// Record a committed change starting from a pre-drag `before` snapshot, ensuring
    /// `before` is at the top of the undo stack and `after` becomes the new current state.
    pub fn commit_from(&mut self, before: EditRecipe, after: EditRecipe) {
        if before == after {
            self.entries[self.cursor] = before;
            return;
        }
        self.entries[self.cursor] = before;
        self.push(after);
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn undo(&mut self) -> Option<EditRecipe> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        Some(self.entries[self.cursor])
    }

    pub fn redo(&mut self) -> Option<EditRecipe> {
        if !self.can_redo() {
            return None;
        }
        self.cursor += 1;
        Some(self.entries[self.cursor])
    }

    /// Reset all edits back to identity (recorded as an undoable step).
    pub fn reset(&mut self) {
        self.push(EditRecipe::default());
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum EditTab {
    #[default]
    Crop,
    Adjust,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum EditPrompt {
    #[default]
    None,
    /// Inline Save bar (`Ctrl+S` on pixel edits): `[Save Copy (Enter)] [Overwrite (O)] [Cancel (Esc)]`.
    SaveChoice,
    /// Inline exit guard (`Esc`/`E` with unsaved edits): `[Save Copy (Enter)] [Discard (D)] [Cancel (Esc)]`.
    UnsavedExit,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum JpegQuality {
    Compact,
    #[default]
    Default,
    High,
}

impl JpegQuality {
    pub const ALL: [Self; 3] = [Self::Compact, Self::Default, Self::High];

    pub const fn quality(self) -> u8 {
        match self {
            Self::Compact => 80,
            Self::Default => 92,
            Self::High => 98,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Q80",
            Self::Default => "Q92",
            Self::High => "Q98",
        }
    }

    pub const fn cycle(self) -> Self {
        match self {
            Self::Compact => Self::Default,
            Self::Default => Self::High,
            Self::High => Self::Compact,
        }
    }
}

/// Active pointer drag state inside Edit Mode.
#[derive(Copy, Clone, Debug)]
pub enum EditDrag {
    CropHandle {
        handle: CropHandle,
        start_recipe: EditRecipe,
        start_norm: (f32, f32),
    },
    NewCrop {
        start_recipe: EditRecipe,
        anchor_norm: (f32, f32),
    },
    Slider {
        slider: ToneSlider,
        start_recipe: EditRecipe,
    },
}

/// Platform-independent Edit Mode session state (stored as `Option<EditSession>` on the app).
#[derive(Clone, Debug)]
pub struct EditSession {
    pub history: EditHistory,
    pub tab: EditTab,
    pub aspect: AspectPreset,
    pub aspect_swapped: bool,
    pub quality: JpegQuality,
    pub show_quality_bar: bool,
    pub comparing_original: bool,
    pub prompt: EditPrompt,
    pub drag: Option<EditDrag>,
    pub hovered_handle: Option<CropHandle>,
    pub hovered_button: Option<DockButton>,
    pub is_animated_source: bool,
    pub frame_index: usize,
    pub total_frames: usize,
    pub exporting: bool,
}

impl EditSession {
    pub fn new(
        tab: EditTab,
        is_animated_source: bool,
        frame_index: usize,
        total_frames: usize,
    ) -> Self {
        Self {
            history: EditHistory::new(),
            tab,
            aspect: AspectPreset::Free,
            aspect_swapped: false,
            quality: JpegQuality::Default,
            show_quality_bar: false,
            comparing_original: false,
            prompt: EditPrompt::None,
            drag: None,
            hovered_handle: None,
            hovered_button: None,
            is_animated_source,
            frame_index,
            total_frames,
            exporting: false,
        }
    }

    pub fn recipe(&self) -> EditRecipe {
        self.history.current()
    }

    pub fn is_dirty(&self) -> bool {
        !self.recipe().is_identity()
    }

    /// Active tone LUT for preview rendering (identity while holding `\` compare).
    pub fn preview_lut(&self) -> ToneLut {
        if self.comparing_original {
            ToneLut::from_adjustments(&ToneAdjustments::default())
        } else {
            ToneLut::from_adjustments(&self.recipe().tone)
        }
    }

    /// Rotate 90° CW and rotate the crop box along with the image.
    pub fn rotate_cw(&mut self) {
        let mut r = self.recipe();
        r.turns = r.turns.cw();
        r.crop = r.crop.rotate_cw();
        self.history.push(r);
    }

    /// Rotate 90° CCW and rotate the crop box along with the image.
    pub fn rotate_ccw(&mut self) {
        let mut r = self.recipe();
        r.turns = r.turns.ccw();
        r.crop = r.crop.rotate_ccw();
        self.history.push(r);
    }

    /// Flip horizontally and mirror the crop box.
    pub fn flip_h(&mut self) {
        let mut r = self.recipe();
        r.flip_h = !r.flip_h;
        r.crop = r.crop.flip_h();
        self.history.push(r);
    }

    /// Flip vertically and mirror the crop box.
    pub fn flip_v(&mut self) {
        let mut r = self.recipe();
        r.flip_v = !r.flip_v;
        r.crop = r.crop.flip_v();
        self.history.push(r);
    }

    /// Toggle monochrome and record an undo step.
    pub fn toggle_monochrome(&mut self) {
        let mut r = self.recipe();
        r.tone.monochrome = !r.tone.monochrome;
        self.history.push(r);
    }

    /// Apply an aspect ratio preset, adjusting the current crop rectangle immediately.
    pub fn set_aspect(&mut self, preset: AspectPreset, src_w: u32, src_h: u32) {
        self.aspect = preset;
        if preset == AspectPreset::Free {
            return;
        }
        let mut r = self.recipe();
        let (ow, oh) = r.oriented_dims(src_w, src_h);
        if let Some(ratio) = preset.ratio(ow, oh, self.aspect_swapped) {
            r.crop = r.crop.fit_aspect(ratio, ow, oh);
            self.history.push(r);
        }
    }

    /// Swap aspect orientation (`16:9` ↔ `9:16`) if a non-free/non-square preset is selected.
    pub fn swap_aspect(&mut self, src_w: u32, src_h: u32) {
        if matches!(self.aspect, AspectPreset::Free | AspectPreset::Square) {
            return;
        }
        self.aspect_swapped = !self.aspect_swapped;
        self.set_aspect(self.aspect, src_w, src_h);
    }

    /// Nudge crop box in source pixels.
    pub fn nudge_crop(&mut self, dx_px: i32, dy_px: i32, resize: bool, src_w: u32, src_h: u32) {
        let mut r = self.recipe();
        let (ow, oh) = r.oriented_dims(src_w, src_h);
        r.crop = r.crop.nudge_px(dx_px, dy_px, resize, ow, oh);
        self.history.push(r);
    }

    /// Reset all edits (undoable).
    pub fn reset_all(&mut self) {
        self.aspect = AspectPreset::Free;
        self.aspect_swapped = false;
        self.prompt = EditPrompt::None;
        self.history.reset();
    }

    /// Effective locked aspect ratio (considering `shift_held` temporary lock during drag).
    pub fn effective_aspect_ratio(&self, src_w: u32, src_h: u32, shift_held: bool) -> Option<f32> {
        let r = self.recipe();
        let (ow, oh) = r.oriented_dims(src_w, src_h);
        if let Some(ratio) = self.aspect.ratio(ow, oh, self.aspect_swapped) {
            return Some(ratio);
        }
        if shift_held && ow > 0 && oh > 0 {
            let cur_w = r.crop.w * ow as f32;
            let cur_h = r.crop.h * oh as f32;
            if cur_h > 0.0 {
                return Some(cur_w / cur_h);
            }
        }
        None
    }
}

/// Interactive controls inside the floating Edit Dock.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DockButton {
    TabCrop,
    TabAdjust,
    Aspect(AspectPreset),
    SwapAspect,
    RotateCcw,
    RotateCw,
    FlipH,
    FlipV,
    Slider(ToneSlider),
    Monochrome,
    Reset,
    Compare,
    Undo,
    Redo,
    QualityCycle,
    CopyEdited,
    SaveOverwrite,
    SaveCopy,
    Done,
    // Inline prompt actions
    PromptSaveCopy,
    PromptOverwrite,
    PromptDiscard,
    PromptCancel,
}

#[derive(Copy, Clone, Debug)]
pub struct DockItem {
    pub button: DockButton,
    pub rect: Rect,
}

/// Complete physical-pixel layout of the floating Edit Dock & optional inline prompt bar.
#[derive(Clone, Debug)]
pub struct DockLayout {
    /// Total bottom reserved height (physical px) so the fitted image clears the dock cleanly.
    pub reserved_bottom_px: f32,
    pub panel: Rect,
    pub prompt_panel: Option<Rect>,
    pub items: Vec<DockItem>,
}

impl DockLayout {
    /// Compute the dock geometry for a client window `(client_w, client_h)` at scale `s >= 1.0`.
    pub fn compute(client_w: u32, client_h: u32, scale: f32, session: &EditSession) -> Self {
        let s = scale.max(1.0);
        let cw = client_w as f32;
        let ch = client_h as f32;

        let margin_b = 14.0 * s;
        let pad_x = 12.0 * s;
        let pad_y = 8.0 * s;
        let row_h = 28.0 * s;
        let gap = 6.0 * s;

        let tool_rows = match session.tab {
            EditTab::Crop => 1,
            EditTab::Adjust => 2,
        };
        // Top row: tabs + actions; bottom row(s): tool controls.
        let panel_h = pad_y * 2.0 + row_h + (tool_rows as f32) * (row_h + gap);
        let max_w = (cw - 24.0 * s).max(280.0 * s);
        let panel_w = (700.0 * s).min(max_w);
        let panel_x = (cw - panel_w) * 0.5;
        let panel_y = (ch - panel_h - margin_b).max(8.0 * s);

        let panel = Rect {
            x: panel_x,
            y: panel_y,
            w: panel_w,
            h: panel_h,
        };

        let mut items = Vec::with_capacity(24);

        // --- Row 0 (header/actions row): Tabs on left, Actions on right ---
        let r0_y = panel_y + pad_y;
        let mut lx = panel_x + pad_x;
        let tab_w = 60.0 * s;
        items.push(DockItem {
            button: DockButton::TabCrop,
            rect: Rect {
                x: lx,
                y: r0_y,
                w: tab_w,
                h: row_h,
            },
        });
        lx += tab_w + 4.0 * s;
        items.push(DockItem {
            button: DockButton::TabAdjust,
            rect: Rect {
                x: lx,
                y: r0_y,
                w: 66.0 * s,
                h: row_h,
            },
        });

        // Right-aligned action buttons in row 0.
        let mut rx = panel_x + panel_w - pad_x;
        let push_right = |items: &mut Vec<DockItem>, rx: &mut f32, w: f32, btn: DockButton| {
            *rx -= w;
            items.push(DockItem {
                button: btn,
                rect: Rect {
                    x: *rx,
                    y: r0_y,
                    w,
                    h: row_h,
                },
            });
            *rx -= gap;
        };

        push_right(&mut items, &mut rx, 52.0 * s, DockButton::Done);
        push_right(&mut items, &mut rx, 84.0 * s, DockButton::SaveCopy);
        if !session.is_animated_source {
            push_right(&mut items, &mut rx, 52.0 * s, DockButton::SaveOverwrite);
        }
        push_right(&mut items, &mut rx, 52.0 * s, DockButton::CopyEdited);
        push_right(&mut items, &mut rx, 44.0 * s, DockButton::QualityCycle);
        push_right(&mut items, &mut rx, 32.0 * s, DockButton::Redo);
        push_right(&mut items, &mut rx, 32.0 * s, DockButton::Undo);
        push_right(&mut items, &mut rx, 32.0 * s, DockButton::Compare);

        // --- Tool row(s) ---
        let r1_y = r0_y + row_h + gap;
        match session.tab {
            EditTab::Crop => {
                let mut cx = panel_x + pad_x;
                let chip_w = 46.0 * s;
                for preset in AspectPreset::ALL {
                    items.push(DockItem {
                        button: DockButton::Aspect(preset),
                        rect: Rect {
                            x: cx,
                            y: r1_y,
                            w: chip_w,
                            h: row_h,
                        },
                    });
                    cx += chip_w + 4.0 * s;
                }
                items.push(DockItem {
                    button: DockButton::SwapAspect,
                    rect: Rect {
                        x: cx,
                        y: r1_y,
                        w: 34.0 * s,
                        h: row_h,
                    },
                });

                // Transform + Reset buttons right-aligned on row 1.
                let mut tx = panel_x + panel_w - pad_x;
                let push_t = |items: &mut Vec<DockItem>, tx: &mut f32, w: f32, btn: DockButton| {
                    *tx -= w;
                    items.push(DockItem {
                        button: btn,
                        rect: Rect {
                            x: *tx,
                            y: r1_y,
                            w,
                            h: row_h,
                        },
                    });
                    *tx -= 4.0 * s;
                };
                push_t(&mut items, &mut tx, 50.0 * s, DockButton::Reset);
                push_t(&mut items, &mut tx, 34.0 * s, DockButton::FlipV);
                push_t(&mut items, &mut tx, 34.0 * s, DockButton::FlipH);
                push_t(&mut items, &mut tx, 34.0 * s, DockButton::RotateCw);
                push_t(&mut items, &mut tx, 34.0 * s, DockButton::RotateCcw);
            }
            EditTab::Adjust => {
                // 2x2 grid of sliders on rows 1 & 2, with Mono + Reset on the right of row 2.
                let inner_w = panel_w - pad_x * 2.0;
                let right_col_w = 122.0 * s;
                let sliders_area_w = (inner_w - right_col_w - 12.0 * s).max(160.0 * s);
                let col_w = (sliders_area_w - 14.0 * s) * 0.5;
                let r2_y = r1_y + row_h + gap;

                let positions = [
                    (ToneSlider::Exposure, panel_x + pad_x, r1_y),
                    (
                        ToneSlider::Contrast,
                        panel_x + pad_x + col_w + 14.0 * s,
                        r1_y,
                    ),
                    (ToneSlider::Saturation, panel_x + pad_x, r2_y),
                    (ToneSlider::Warmth, panel_x + pad_x + col_w + 14.0 * s, r2_y),
                ];
                for (slider, sx, sy) in positions {
                    items.push(DockItem {
                        button: DockButton::Slider(slider),
                        rect: Rect {
                            x: sx,
                            y: sy,
                            w: col_w,
                            h: row_h,
                        },
                    });
                }

                let rx_box = panel_x + panel_w - pad_x - right_col_w;
                items.push(DockItem {
                    button: DockButton::Monochrome,
                    rect: Rect {
                        x: rx_box,
                        y: r1_y,
                        w: right_col_w,
                        h: row_h,
                    },
                });
                items.push(DockItem {
                    button: DockButton::Reset,
                    rect: Rect {
                        x: rx_box,
                        y: r2_y,
                        w: right_col_w,
                        h: row_h,
                    },
                });
            }
        }

        // --- Optional inline confirmation prompt pill floating above dock ---
        let prompt_panel = match session.prompt {
            EditPrompt::None => None,
            EditPrompt::SaveChoice | EditPrompt::UnsavedExit => {
                let pw = (430.0 * s).min(max_w);
                let ph = 38.0 * s;
                let px = (cw - pw) * 0.5;
                let py = (panel_y - ph - 10.0 * s).max(8.0 * s);
                let p_rect = Rect {
                    x: px,
                    y: py,
                    w: pw,
                    h: ph,
                };
                let by = py + 5.0 * s;
                let bh = ph - 10.0 * s;
                let mut prx = px + pw - 8.0 * s;
                let mut add_p = |w: f32, btn: DockButton| {
                    prx -= w;
                    items.push(DockItem {
                        button: btn,
                        rect: Rect {
                            x: prx,
                            y: by,
                            w,
                            h: bh,
                        },
                    });
                    prx -= 6.0 * s;
                };
                add_p(64.0 * s, DockButton::PromptCancel);
                match session.prompt {
                    EditPrompt::SaveChoice => {
                        add_p(90.0 * s, DockButton::PromptOverwrite);
                        add_p(106.0 * s, DockButton::PromptSaveCopy);
                    }
                    EditPrompt::UnsavedExit => {
                        add_p(78.0 * s, DockButton::PromptDiscard);
                        add_p(106.0 * s, DockButton::PromptSaveCopy);
                    }
                    EditPrompt::None => {}
                }
                Some(p_rect)
            }
        };

        let reserved_bottom_px = panel_h + margin_b + 16.0 * s;

        Self {
            reserved_bottom_px,
            panel,
            prompt_panel,
            items,
        }
    }

    pub fn hit_test(&self, pt: (f32, f32)) -> Option<DockButton> {
        // Check prompt items first (rendered above panel).
        for item in self.items.iter().rev() {
            if point_in_rect(pt, item.rect) {
                return Some(item.button);
            }
        }
        None
    }

    pub fn contains_point(&self, pt: (f32, f32)) -> bool {
        point_in_rect(pt, self.panel) || self.prompt_panel.is_some_and(|r| point_in_rect(pt, r))
    }

    pub fn rect_of(&self, button: DockButton) -> Option<Rect> {
        self.items
            .iter()
            .find(|i| i.button == button)
            .map(|i| i.rect)
    }

    /// Track geometry `(track_x0, track_x1)` for a tone slider item rect.
    pub fn slider_track(item_rect: Rect, scale: f32) -> (f32, f32) {
        let s = scale.max(1.0);
        let label_w = 66.0 * s;
        let val_w = 34.0 * s;
        let x0 = item_rect.x + label_w;
        let x1 = (item_rect.x + item_rect.w - val_w).max(x0 + 24.0 * s);
        (x0, x1)
    }

    /// Map pointer X coordinate to slider value in `[-1.0..=1.0]` (with gentle snap to `0.0`).
    pub fn slider_value_at(item_rect: Rect, scale: f32, px: f32) -> f32 {
        let (x0, x1) = Self::slider_track(item_rect, scale);
        let t = ((px - x0) / (x1 - x0).max(1.0)).clamp(0.0, 1.0);
        let raw = t * 2.0 - 1.0;
        if raw.abs() < 0.03 { 0.0 } else { raw }
    }
}

/// Compute the fitted display rectangle of an oriented image inside the Edit Mode canvas
/// (leaving breathing room around the image and clearing the bottom floating dock).
pub fn edit_image_dest_rect(
    oriented_src: (u32, u32),
    client: (u32, u32),
    reserved_bottom_px: f32,
    scale: f32,
) -> (Rect, f32) {
    let s = scale.max(1.0);
    let pad = 20.0 * s;
    let avail_w = (client.0 as f32 - pad * 2.0).max(32.0);
    let avail_h = (client.1 as f32 - pad - reserved_bottom_px).max(32.0);
    if oriented_src.0 == 0 || oriented_src.1 == 0 {
        return (
            Rect {
                x: pad,
                y: pad,
                w: avail_w,
                h: avail_h,
            },
            1.0,
        );
    }
    let sx = avail_w / oriented_src.0 as f32;
    let sy = avail_h / oriented_src.1 as f32;
    // Allow modest upscaling up to 8x for tiny icons in Edit Mode so crop handles remain usable.
    let fit_scale = sx.min(sy).min(8.0);
    let w = oriented_src.0 as f32 * fit_scale;
    let h = oriented_src.1 as f32 * fit_scale;
    let x = pad + (avail_w - w) * 0.5;
    let y = pad + (avail_h - h) * 0.5;
    (Rect { x, y, w, h }, fit_scale)
}

#[inline]
pub fn point_in_rect(pt: (f32, f32), r: Rect) -> bool {
    pt.0 >= r.x && pt.0 <= r.x + r.w && pt.1 >= r.y && pt.1 <= r.y + r.h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_size_is_tiny_and_copy() {
        assert!(std::mem::size_of::<EditRecipe>() <= 64);
        assert!(std::mem::size_of::<ToneLut>() <= 1024);
    }

    #[test]
    fn crop_rect_rotations_and_flips_are_invertible() {
        let orig = CropRect {
            x: 0.1,
            y: 0.2,
            w: 0.35,
            h: 0.45,
        };
        let cw4 = orig.rotate_cw().rotate_cw().rotate_cw().rotate_cw();
        assert!((cw4.x - orig.x).abs() < 1e-5);
        assert!((cw4.y - orig.y).abs() < 1e-5);
        assert!((cw4.w - orig.w).abs() < 1e-5);
        assert!((cw4.h - orig.h).abs() < 1e-5);

        let round = orig.rotate_cw().rotate_ccw();
        assert!((round.x - orig.x).abs() < 1e-5);
        assert!((round.y - orig.y).abs() < 1e-5);

        let fh2 = orig.flip_h().flip_h();
        let fv2 = orig.flip_v().flip_v();
        assert!((fh2.x - orig.x).abs() < 1e-5);
        assert!((fv2.y - orig.y).abs() < 1e-5);
    }

    #[test]
    fn crop_to_pixel_rect_maps_full_and_subrect() {
        assert_eq!(
            CropRect::FULL.to_pixel_rect(4000, 3000),
            PixelRect {
                x: 0,
                y: 0,
                w: 4000,
                h: 3000
            }
        );
        let sub = CropRect {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        };
        assert_eq!(
            sub.to_pixel_rect(4000, 2000),
            PixelRect {
                x: 1000,
                y: 1000,
                w: 2000,
                h: 500
            }
        );
    }

    #[test]
    fn crop_aspect_fit_and_drag_lock() {
        // 4000x3000 (4:3) cropped to 16:9 should keep full width and shrink height to 2250.
        let c = CropRect::FULL.fit_aspect(16.0 / 9.0, 4000, 3000);
        let px = c.to_pixel_rect(4000, 3000);
        assert_eq!(px.w, 4000);
        assert_eq!(px.h, 2250);

        // Dragging SE inward with 1:1 ratio locked must stay square in source pixels.
        let sq = CropRect::FULL.fit_aspect(1.0, 4000, 3000);
        let dragged = sq.apply_drag(CropHandle::Se, -0.2, -0.1, Some(1.0), 4000, 3000);
        let dpx = dragged.to_pixel_rect(4000, 3000);
        assert!((dpx.w as i32 - dpx.h as i32).abs() <= 2);
        assert!(dpx.w >= MIN_CROP_SOURCE_PX);
    }

    #[test]
    fn quarter_turns_inverse_pixel_is_bijection() {
        let (sw, sh) = (5u32, 3u32);
        for turns in [
            QuarterTurns::R0,
            QuarterTurns::R90,
            QuarterTurns::R180,
            QuarterTurns::R270,
        ] {
            for flip_h in [false, true] {
                for flip_v in [false, true] {
                    let (ow, oh) = turns.oriented_dims(sw, sh);
                    let mut seen = vec![false; (sw * sh) as usize];
                    for oy in 0..oh {
                        for ox in 0..ow {
                            let (sx, sy) = turns.inverse_pixel(ox, oy, sw, sh, flip_h, flip_v);
                            assert!(sx < sw && sy < sh);
                            let idx = (sy * sw + sx) as usize;
                            assert!(
                                !seen[idx],
                                "duplicate mapping for {turns:?} {flip_h} {flip_v}"
                            );
                            seen[idx] = true;
                        }
                    }
                    assert!(seen.iter().all(|&b| b));
                }
            }
        }
    }

    #[test]
    fn tone_lut_identity_monochrome_and_alpha_roundtrip() {
        let id = ToneLut::from_adjustments(&ToneAdjustments::default());
        assert!(id.is_identity());
        assert_eq!(id.apply_rgb(12, 134, 240), [12, 134, 240]);
        assert_eq!(id.apply_bgra_premul([20, 40, 60, 128]), [20, 40, 60, 128]);

        let mono = ToneLut::from_adjustments(&ToneAdjustments {
            monochrome: true,
            ..Default::default()
        });
        assert!(!mono.is_identity());
        let [r, g, b] = mono.apply_rgb(200, 100, 50);
        assert_eq!(r, g);
        assert_eq!(g, b);
        // Rec.709: 0.2126*200 + 0.7152*100 + 0.0722*50 = 117.65
        assert!((i32::from(r) - 118).abs() <= 1);

        // Semi-transparent pixel with slight exposure lift preserves alpha and stays valid premul.
        let bright = ToneLut::from_adjustments(&ToneAdjustments {
            exposure: 0.5,
            ..Default::default()
        });
        let out = bright.apply_bgra_premul([32, 32, 32, 128]);
        assert_eq!(out[3], 128);
        assert!(out[0] > 32 && out[0] <= 128);
    }

    #[test]
    fn history_undo_redo_reset_and_cap() {
        let mut hist = EditHistory::new();
        assert!(!hist.can_undo());
        assert!(!hist.can_redo());

        for i in 1..=(MAX_UNDO_STEPS + 10) {
            let mut r = EditRecipe::default();
            r.tone.exposure = (i as f32) / 100.0;
            hist.push(r);
        }
        assert_eq!(hist.len(), MAX_UNDO_STEPS);
        assert!(hist.can_undo());

        let prev = hist.undo().unwrap();
        assert!(hist.can_redo());
        let _ = hist.redo().unwrap();
        assert!(!hist.can_redo());
        assert_ne!(hist.current(), prev);

        // Reset is undoable.
        let before_reset = hist.current();
        hist.reset();
        assert!(hist.current().is_identity());
        assert_eq!(hist.undo(), Some(before_reset));
    }

    #[test]
    fn dock_layout_and_hit_testing_work() {
        let session = EditSession::new(EditTab::Crop, false, 0, 1);
        let layout = DockLayout::compute(1280, 760, 1.0, &session);
        let save_copy = layout.rect_of(DockButton::SaveCopy).unwrap();
        let cx = save_copy.x + save_copy.w * 0.5;
        let cy = save_copy.y + save_copy.h * 0.5;
        assert_eq!(layout.hit_test((cx, cy)), Some(DockButton::SaveCopy));
        assert!(layout.contains_point((cx, cy)));

        let session_adj = EditSession::new(EditTab::Adjust, false, 0, 1);
        let layout_adj = DockLayout::compute(1280, 760, 1.0, &session_adj);
        let exp_rect = layout_adj
            .rect_of(DockButton::Slider(ToneSlider::Exposure))
            .unwrap();
        let (x0, x1) = DockLayout::slider_track(exp_rect, 1.0);
        assert!((DockLayout::slider_value_at(exp_rect, 1.0, (x0 + x1) * 0.5)).abs() < 1e-4);
        assert!((DockLayout::slider_value_at(exp_rect, 1.0, x1) - 1.0).abs() < 1e-4);
        assert!((DockLayout::slider_value_at(exp_rect, 1.0, x0) + 1.0).abs() < 1e-4);
    }
}
