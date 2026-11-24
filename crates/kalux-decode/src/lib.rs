//! Decoding and preloading for Kalux.
//!
//! * [`decode_file`]: synchronous decode of one file (used by workers and tests).
//! * [`ImageStore`]: owns a worker pool + byte-budget cache; the app tells it what is
//!   *focused* and which neighbors to preload, and drains results with [`ImageStore::poll`].
//!
//! Backends:
//! * JPEG: `zune-jpeg` (pure Rust) — on all platforms. EXIF orientation via `kamadak-exif`.
//! * Everything else (and JPEG fallback): WIC on Windows (`cfg(windows)`), including codec
//!   extensions (HEIF/AVIF/RAW/JXL) when installed. On non-Windows, non-JPEG returns
//!   `DecodeError::Unsupported` (Linux is only a test host).
//!
//! Output is always [`kalux_core::DecodedImage`] in BGRA8 premultiplied, orientation applied,
//! optionally downscaled to fit `max_size` (never upscaled; aspect preserved).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kalux_core::DecodedImage;

#[derive(Debug, Clone)]
pub enum DecodeError {
    NotFound,
    /// Format recognised but no decoder available (e.g. HEIF extension not installed).
    /// `hint` is a user-facing next step, e.g. "Install HEIF Image Extensions from the Microsoft Store".
    Unsupported {
        hint: Option<String>,
    },
    Corrupt(String),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f;
        todo!("kalux-decode: user-facing message")
    }
}

impl std::error::Error for DecodeError {}

/// Decode `path`. When `max_size` is `Some((w, h))`, the result fits inside `w×h`
/// (use decoder-native scaling where possible: zune/WIC DCT scaling, `IWICBitmapScaler`).
/// Must be callable from any thread (initialise COM per thread on Windows, MTA).
pub fn decode_file(path: &Path, max_size: Option<(u32, u32)>) -> Result<DecodedImage, DecodeError> {
    let _ = (path, max_size);
    todo!("kalux-decode")
}

#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// Worker threads. Default: min(available_parallelism - 1, 4), at least 1.
    pub threads: usize,
    /// Cache budget in bytes. Default 128 MiB.
    pub cache_budget: usize,
    /// How many neighbors on each side to preload. Default 2.
    pub preload_radius: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        todo!("kalux-decode")
    }
}

#[derive(Debug)]
pub enum StoreEvent {
    /// A decode finished for `path`. `full_resolution` is true when it was decoded without `max_size`.
    Ready {
        path: PathBuf,
        image: Arc<DecodedImage>,
        full_resolution: bool,
        elapsed: Duration,
    },
    Failed {
        path: PathBuf,
        error: DecodeError,
    },
}

/// Background decoder + cache. Owned by the UI thread; not `Sync`.
pub struct ImageStore {
    _private: (),
}

impl ImageStore {
    /// Start workers. `notify` is called from a worker thread whenever a result is queued;
    /// the app uses it to `PostMessageW` its window so the UI thread calls [`poll`](Self::poll).
    pub fn new(config: StoreConfig, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        let _ = (config, notify);
        todo!("kalux-decode")
    }

    /// Make `current` the focused image and preload `neighbors` (nearest first).
    /// * Returns the cached image immediately if present (no event will be sent for it).
    /// * Otherwise queues `current` at top priority; neighbors after it.
    /// * Queued-but-not-started jobs for paths no longer in {current} ∪ neighbors are dropped.
    /// * Pins current + neighbors in the cache.
    /// `target` is the client size in physical px; images are decoded to fit it.
    pub fn focus(
        &mut self,
        current: &Path,
        neighbors: &[PathBuf],
        target: (u32, u32),
    ) -> Option<Arc<DecodedImage>> {
        let _ = (current, neighbors, target);
        todo!("kalux-decode")
    }

    /// Queue a full-resolution decode of `path` (when the user zooms past the downscaled detail).
    /// Result arrives as `Ready { full_resolution: true }` and replaces the cached entry.
    pub fn request_full_resolution(&mut self, path: &Path) {
        let _ = path;
        todo!("kalux-decode")
    }

    /// Drain finished jobs (non-blocking). Results are inserted into the cache before being returned.
    pub fn poll(&mut self) -> Vec<StoreEvent> {
        todo!("kalux-decode")
    }

    /// Forget a path (file rotated/deleted/changed on disk).
    pub fn invalidate(&mut self, path: &Path) {
        let _ = path;
        todo!("kalux-decode")
    }

    /// Target size changed significantly (e.g. moved to a 4K monitor / fullscreen). Cached images
    /// smaller than needed are re-requested on next focus.
    pub fn set_target(&mut self, target: (u32, u32)) {
        let _ = target;
        todo!("kalux-decode")
    }

    /// Current cache weight in bytes (for `--bench` reporting).
    pub fn cache_bytes(&self) -> usize {
        todo!("kalux-decode")
    }
}
