//! Direct2D renderer: single-threaded factory, HwndRenderTarget, GPU bitmap management,
//! adaptive interpolation via `ID2D1DeviceContext`, and `D2DERR_RECREATE_TARGET` recovery.

use std::sync::Arc;

use kalux_core::{DecodedImage, Rect};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_BITMAP_PROPERTIES, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_INTERPOLATION_MODE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1CreateFactory, ID2D1Bitmap,
    ID2D1DeviceContext, ID2D1Factory, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Interface;

use crate::overlay::{OverlayRenderer, OverlayView};

/// Dark neutral canvas background (`#141416`).
const BG_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.078,
    g: 0.078,
    b: 0.086,
    a: 1.0,
};

/// Animations whose decoded frames total <= 32 MiB cache one D2D bitmap per frame (lazily uploaded).
/// Larger animations reuse a single D2D bitmap via `CopyFromMemory` to keep VRAM bounded.
const ANIM_PER_FRAME_BUDGET_BYTES: usize = 32 * 1024 * 1024;

enum FrameStorage {
    /// Lazily uploaded per-frame GPU bitmaps (still images have len == 1).
    PerFrame(Vec<Option<ID2D1Bitmap>>),
    /// Single GPU bitmap updated in-place via `CopyFromMemory` for large animations.
    Streaming {
        bitmap: Option<ID2D1Bitmap>,
        uploaded_frame: Option<usize>,
    },
}

struct GpuImage {
    image: Arc<DecodedImage>,
    storage: FrameStorage,
}

impl GpuImage {
    fn new(image: Arc<DecodedImage>) -> Self {
        let storage = if image.frames.len() <= 1 || image.byte_size() <= ANIM_PER_FRAME_BUDGET_BYTES
        {
            FrameStorage::PerFrame(vec![None; image.frames.len()])
        } else {
            FrameStorage::Streaming {
                bitmap: None,
                uploaded_frame: None,
            }
        };
        Self { image, storage }
    }

    fn discard_gpu_bitmaps(&mut self) {
        match &mut self.storage {
            FrameStorage::PerFrame(slots) => {
                for slot in slots.iter_mut() {
                    *slot = None;
                }
            }
            FrameStorage::Streaming {
                bitmap,
                uploaded_frame,
            } => {
                *bitmap = None;
                *uploaded_frame = None;
            }
        }
    }

    fn bitmap_for_frame(
        &mut self,
        target: &ID2D1HwndRenderTarget,
        frame_idx: usize,
    ) -> windows::core::Result<Option<&ID2D1Bitmap>> {
        let Some(frame) = self.image.frames.get(frame_idx) else {
            return Ok(None);
        };
        let w = self.image.width;
        let h = self.image.height;
        if w == 0 || h == 0 {
            return Ok(None);
        }
        let pitch = w.saturating_mul(4);
        let expected_bytes = (pitch as usize).saturating_mul(h as usize);
        if frame.pixels.len() < expected_bytes {
            return Ok(None);
        }

        match &mut self.storage {
            FrameStorage::PerFrame(slots) => {
                if slots[frame_idx].is_none() {
                    let bmp = create_d2d_bitmap(target, w, h, pitch, &frame.pixels)?;
                    slots[frame_idx] = Some(bmp);
                }
                Ok(slots[frame_idx].as_ref())
            }
            FrameStorage::Streaming {
                bitmap,
                uploaded_frame,
            } => {
                if bitmap.is_none() {
                    let bmp = create_d2d_bitmap(target, w, h, pitch, &frame.pixels)?;
                    *bitmap = Some(bmp);
                    *uploaded_frame = Some(frame_idx);
                } else if *uploaded_frame != Some(frame_idx) {
                    if let Some(bmp) = bitmap.as_ref() {
                        // SAFETY: `frame.pixels` has at least `h * pitch` bytes of BGRA8 premultiplied data.
                        unsafe {
                            bmp.CopyFromMemory(None, frame.pixels.as_ptr().cast(), pitch)?;
                        }
                    }
                    *uploaded_frame = Some(frame_idx);
                }
                Ok(bitmap.as_ref())
            }
        }
    }
}

pub struct Renderer {
    factory: ID2D1Factory,
    hwnd: HWND,
    client_size: (u32, u32),
    target: Option<ID2D1HwndRenderTarget>,
    device_context: Option<ID2D1DeviceContext>,
    current: Option<GpuImage>,
    overlays: OverlayRenderer,
}

impl Renderer {
    pub fn new(hwnd: HWND, client_size: (u32, u32)) -> windows::core::Result<Self> {
        // SAFETY: Creating a single-threaded Direct2D factory on the UI thread is safe.
        let factory: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) }?;

        let mut renderer = Self {
            factory,
            hwnd,
            client_size,
            target: None,
            device_context: None,
            current: None,
            overlays: OverlayRenderer::default(),
        };
        renderer.ensure_target()?;
        Ok(renderer)
    }

    /// Replace the displayed image (drops previous GPU bitmaps immediately).
    pub fn set_image(&mut self, image: Arc<DecodedImage>) {
        self.current = Some(GpuImage::new(image));
    }

    /// Clear the displayed image and release its GPU bitmaps.
    pub fn clear_image(&mut self) {
        self.current = None;
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.client_size = (width, height);
        if width == 0 || height == 0 {
            return;
        }
        if let Some(target) = self.target.as_ref() {
            let size = D2D_SIZE_U { width, height };
            // SAFETY: `target` is a valid HwndRenderTarget.
            if let Err(e) = unsafe { target.Resize(&size) }
                && e.code() == D2DERR_RECREATE_TARGET
            {
                self.discard_target();
            }
        }
    }

    /// Paint the current image (if any) and overlays. Returns `true` if a real image frame was
    /// presented during this call.
    pub fn render(
        &mut self,
        dest: Option<Rect>,
        view_scale: f32,
        frame_idx: usize,
        dpi: u32,
        overlays: &OverlayView<'_>,
    ) -> bool {
        // Allow one automatic retry if the device/target was lost (`D2DERR_RECREATE_TARGET`).
        for attempt in 0..2 {
            match self.render_once(dest, view_scale, frame_idx, dpi, overlays) {
                Ok(presented_image) => return presented_image,
                Err(e) if e.code() == D2DERR_RECREATE_TARGET && attempt == 0 => {
                    self.discard_target();
                }
                Err(_) => return false,
            }
        }
        false
    }

    fn render_once(
        &mut self,
        dest: Option<Rect>,
        view_scale: f32,
        frame_idx: usize,
        dpi: u32,
        overlays: &OverlayView<'_>,
    ) -> windows::core::Result<bool> {
        self.ensure_target()?;
        let Some(target) = self.target.clone() else {
            return Ok(false);
        };

        // Upload bitmap (if needed) before BeginDraw so creation errors don't leave a dangling BeginDraw.
        let mut drew_image = false;
        let bitmap_to_draw: Option<(ID2D1Bitmap, (u32, u32))> =
            if let Some(gpu_img) = self.current.as_mut() {
                let dims = (gpu_img.image.width, gpu_img.image.height);
                gpu_img
                    .bitmap_for_frame(&target, frame_idx)?
                    .map(|b| (b.clone(), dims))
            } else {
                None
            };

        // SAFETY: Valid D2D render target; paired with `EndDraw` below.
        unsafe { target.BeginDraw() };

        // SAFETY: Inside BeginDraw..EndDraw.
        unsafe { target.Clear(Some(&BG_COLOR)) };

        if let (Some((bitmap, bmp_dims)), Some(rect)) = (bitmap_to_draw, dest) {
            let d2d_rect = D2D_RECT_F {
                left: rect.x,
                top: rect.y,
                right: rect.x + rect.w,
                bottom: rect.y + rect.h,
            };

            if let Some(dc) = self.device_context.as_ref() {
                let mode = choose_interpolation(rect, bmp_dims, view_scale);
                // SAFETY: `dc` is the same render target (via QI) inside BeginDraw..EndDraw.
                unsafe {
                    dc.DrawBitmap(&bitmap, Some(&d2d_rect), 1.0, mode, None, None);
                }
            } else {
                let mode = if view_scale > 4.0 {
                    D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR
                } else {
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR
                };
                // SAFETY: `target` is inside BeginDraw..EndDraw with a valid bitmap from this target.
                unsafe {
                    target.DrawBitmap(&bitmap, Some(&d2d_rect), 1.0, mode, None);
                }
            }
            drew_image = true;
        }

        self.overlays.draw(&target, self.client_size, dpi, overlays);

        // SAFETY: Pairs with `BeginDraw` above.
        unsafe { target.EndDraw(None, None) }?;
        Ok(drew_image)
    }

    fn ensure_target(&mut self) -> windows::core::Result<()> {
        if self.target.is_some() {
            return Ok(());
        }

        // Set render target DPI to 96×96 so D2D coordinates map 1:1 to physical pixels, matching
        // `kalux_core::View` math and avoiding blurry 100% zoom on fractional-DPI displays.
        let rt_props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: self.client_size.0.max(1),
                height: self.client_size.1.max(1),
            },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };

        // SAFETY: `self.factory` and `self.hwnd` are valid on the UI thread.
        let target = unsafe { self.factory.CreateHwndRenderTarget(&rt_props, &hwnd_props) }?;
        let device_context = target.cast::<ID2D1DeviceContext>().ok();

        self.target = Some(target);
        self.device_context = device_context;
        Ok(())
    }

    fn discard_target(&mut self) {
        self.overlays.discard_target_resources();
        if let Some(img) = self.current.as_mut() {
            img.discard_gpu_bitmaps();
        }
        self.device_context = None;
        self.target = None;
    }
}

fn create_d2d_bitmap(
    target: &ID2D1HwndRenderTarget,
    width: u32,
    height: u32,
    pitch: u32,
    pixels: &[u8],
) -> windows::core::Result<ID2D1Bitmap> {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    let size = D2D_SIZE_U { width, height };
    // SAFETY: `pixels` holds at least `height * pitch` bytes of BGRA8 premultiplied pixels.
    unsafe { target.CreateBitmap(size, Some(pixels.as_ptr().cast()), pitch, &props) }
}

fn choose_interpolation(
    dest: Rect,
    bmp_dims: (u32, u32),
    view_scale: f32,
) -> D2D1_INTERPOLATION_MODE {
    if view_scale > 4.0 {
        D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR
    } else if dest.w < (bmp_dims.0 as f32) - 0.5 || dest.h < (bmp_dims.1 as f32) - 0.5 {
        D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC
    } else {
        D2D1_INTERPOLATION_MODE_LINEAR
    }
}
