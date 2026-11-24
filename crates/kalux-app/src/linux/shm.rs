//! Double-buffered Wayland `wl_shm` framebuffer backed by anonymous `memfd_create` + `mmap`.
//!
//! 100% pure Rust via `rustix` syscalls — zero C FFI, zero temp files on disk.

use std::ffi::CStr;
use std::os::fd::OwnedFd;
use std::ptr::NonNull;
use std::slice;

use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use wayrs_client::Connection;
use wayrs_client::protocol::{WlBuffer, WlShm, WlShmPool, wl_buffer, wl_shm};

/// One mapped shared-memory slot attached to a `WlBuffer`.
struct ShmSlot {
    width: u32,
    height: u32,
    byte_len: usize,
    ptr: NonNull<u8>,
    _fd: OwnedFd,
    pool: WlShmPool,
    buffer: WlBuffer,
    busy: bool,
}

impl ShmSlot {
    fn new<D: 'static>(
        conn: &mut Connection<D>,
        shm: WlShm,
        width: u32,
        height: u32,
        slot_index: usize,
        on_release: fn(&mut D, usize),
    ) -> Result<Self, String> {
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| "framebuffer stride overflow".to_string())?;
        let byte_len = (stride as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| "framebuffer size overflow".to_string())?;
        let alloc_len = byte_len.max(4);

        let name: &CStr = c"kalux-shm";
        let fd = memfd_create(name, MemfdFlags::CLOEXEC)
            .map_err(|e| format!("memfd_create failed: {e}"))?;
        ftruncate(&fd, alloc_len as u64).map_err(|e| format!("ftruncate failed: {e}"))?;

        // SAFETY: `fd` is a valid anonymous memfd sized to `alloc_len` bytes; mapping it RW/SHARED
        // returns a valid aligned pointer of `alloc_len` bytes owned by this `ShmSlot` until `Drop`.
        let raw_ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                alloc_len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )
        }
        .map_err(|e| format!("mmap failed: {e}"))?;

        let ptr = NonNull::new(raw_ptr.cast::<u8>())
            .ok_or_else(|| "mmap returned null pointer".to_string())?;

        // Duplicate fd for `wl_shm::create_pool` (which takes ownership of the passed fd).
        let pool_fd = fd
            .try_clone()
            .map_err(|e| format!("dup memfd failed: {e}"))?;
        let pool = shm.create_pool(conn, pool_fd, alloc_len as i32);
        let buffer = pool.create_buffer_with_cb(
            conn,
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            move |ctx| {
                let wl_buffer::Event::Release = ctx.event;
                on_release(ctx.state, slot_index);
            },
        );

        Ok(Self {
            width,
            height,
            byte_len: alloc_len,
            ptr,
            _fd: fd,
            pool,
            buffer,
            busy: false,
        })
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: `self.ptr` points to a valid `self.byte_len`-byte RW mmap, and `&mut self`
        // guarantees exclusive access (we only borrow when `!self.busy`).
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.byte_len) }
    }

    fn destroy_wl<D>(&self, conn: &mut Connection<D>) {
        self.buffer.destroy(conn);
        self.pool.destroy(conn);
    }
}

impl Drop for ShmSlot {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` and `self.byte_len` came directly from `mmap` in `ShmSlot::new`.
        let _ = unsafe { munmap(self.ptr.as_ptr().cast(), self.byte_len) };
    }
}

/// Double-buffered `wl_shm` swapchain so we never overwrite a buffer while the compositor reads it.
pub struct ShmSwapchain {
    slots: [Option<ShmSlot>; 2],
}

impl ShmSwapchain {
    pub fn new() -> Self {
        Self {
            slots: [None, None],
        }
    }

    pub fn mark_released(&mut self, index: usize) {
        if let Some(Some(slot)) = self.slots.get_mut(index) {
            slot.busy = false;
        }
    }

    /// Acquire a non-busy slot matching `(width, height)`, recreating it if resized.
    /// Returns `(slot_index, buffer, pixels_mut)`.
    pub fn acquire<D: 'static>(
        &mut self,
        conn: &mut Connection<D>,
        shm: WlShm,
        width: u32,
        height: u32,
        on_release: fn(&mut D, usize),
    ) -> Result<(usize, WlBuffer, &mut [u8]), String> {
        let w = width.max(1);
        let h = height.max(1);

        // Prefer a free slot already matching the desired dimensions.
        let idx = self
            .slots
            .iter()
            .position(
                |s| matches!(s, Some(slot) if !slot.busy && slot.width == w && slot.height == h),
            )
            .or_else(|| {
                self.slots
                    .iter()
                    .position(|s| s.as_ref().is_none_or(|slot| !slot.busy))
            })
            .unwrap_or(0);

        let needs_alloc = match &self.slots[idx] {
            Some(slot) => slot.width != w || slot.height != h || slot.busy,
            None => true,
        };

        if needs_alloc {
            if let Some(old) = self.slots[idx].take() {
                old.destroy_wl(conn);
            }
            let fresh = ShmSlot::new(conn, shm, w, h, idx, on_release)?;
            self.slots[idx] = Some(fresh);
        }

        let slot = self.slots[idx]
            .as_mut()
            .ok_or_else(|| "shm slot missing after allocation".to_string())?;
        slot.busy = true;
        let buffer = slot.buffer;
        let pixels = slot.pixels_mut();
        Ok((idx, buffer, pixels))
    }

    pub fn destroy_all<D>(&mut self, conn: &mut Connection<D>) {
        for slot in &mut self.slots {
            if let Some(s) = slot.take() {
                s.destroy_wl(conn);
            }
        }
    }
}
