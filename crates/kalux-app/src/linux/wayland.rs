//! Pure-Rust native Wayland client for Linux (designed for Niri / CachyOS and all Wayland compositors).
//!
//! Uses `wayrs-client` (zero C libraries, zero libwayland FFI) over `$WAYLAND_DISPLAY` with:
//! - `xdg_shell` (`set_app_id("kalux")`, tiled/floating configure, fullscreen toggle)
//! - `xdg_decoration_unstable_v1` (requests `ServerSide` so Niri `prefer-no-csd` tiles cleanly)
//! - `wp_fractional_scale_v1` + `wp_viewporter` (crisp 1:1 physical pixels on HiDPI displays)
//! - `wp_cursor_shape_v1` (native `Default` / `Grab` / `Grabbing` cursors)
//! - `wl_data_device` (drag-and-drop `text/uri-list` from file managers)
//! - `wl_shm` double-buffered `memfd_create` + `mmap` (`Format::Argb8888` == BGRA8, zero swizzle)
//! - `poll(2)` event loop sleeping with infinite timeout when idle (0% idle CPU)

use std::ffi::CString;
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kalux_core::action::{Modifiers, action_for_linux_evdev};
use kalux_core::view::ZoomMode;
use kalux_core::{Action, DecodedImage, Palette, Playlist, View};
use kalux_decode::{DecodeError, ImageStore, StoreConfig, StoreEvent};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{read, write};
use rustix::pipe::{PipeFlags, pipe_with};
use wayrs_client::object::Proxy;
use wayrs_client::protocol::{
    WlCallback, WlCompositor, WlDataDevice, WlDataDeviceManager, WlDataOffer, WlKeyboard,
    WlPointer, WlSeat, WlShm, WlSurface, wl_callback, wl_data_device, wl_data_device_manager,
    wl_data_offer, wl_keyboard, wl_pointer, wl_seat, wl_surface,
};
use wayrs_client::{Connection, EventCtx, IoMode};
use wayrs_protocols::cursor_shape_v1::{
    WpCursorShapeDeviceV1, WpCursorShapeManagerV1, wp_cursor_shape_device_v1,
};
use wayrs_protocols::fractional_scale_v1::{
    WpFractionalScaleManagerV1, WpFractionalScaleV1, wp_fractional_scale_v1,
};
use wayrs_protocols::viewporter::{WpViewport, WpViewporter};
use wayrs_protocols::xdg_decoration_unstable_v1::{
    ZxdgDecorationManagerV1, ZxdgToplevelDecorationV1, zxdg_toplevel_decoration_v1,
};
use wayrs_protocols::xdg_shell::{
    XdgSurface, XdgToplevel, XdgWmBase, xdg_surface, xdg_toplevel, xdg_wm_base,
};

use crate::linux::bench::BenchState;
use crate::linux::overlay::{ErrorCard, LinuxOverlay, OverlayView};
use crate::linux::render::Canvas;
use crate::linux::shm::ShmSwapchain;
use crate::linux::theme::system_palette;

const DEFAULT_LOGICAL_W: u32 = 1180;
const DEFAULT_LOGICAL_H: u32 = 760;
const DEFAULT_DECODE_TARGET: (u32, u32) = (2560, 1440);
const TOAST_DURATION: Duration = Duration::from_millis(1200);
const DOUBLE_CLICK_MAX_GAP: Duration = Duration::from_millis(400);
const DOUBLE_CLICK_MAX_DIST_SQ: f64 = 36.0;
const BENCH_SAMPLE_DELAY: Duration = Duration::from_millis(500);
const BENCH_TIMEOUT: Duration = Duration::from_secs(30);
const KEY_ZOOM_FACTOR: f32 = 1.25;

// Linux input-event-codes.h pointer buttons
const BTN_LEFT: u32 = 0x110;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

// Linux evdev modifier keycodes (fallback alongside wl_keyboard::Modifiers)
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;

pub struct LaunchConfig {
    pub initial_file: Option<PathBuf>,
    pub initial_error: Option<ErrorCard>,
    pub bench: Option<BenchState>,
}

type SharedScanResult = Arc<Mutex<Option<(u64, Playlist)>>>;
type SharedDropResult = Arc<Mutex<Option<PathBuf>>>;

struct WakePipe {
    read_fd: OwnedFd,
    write_fd: Arc<OwnedFd>,
}

impl WakePipe {
    fn new() -> Result<Self, String> {
        let (r, w) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)
            .map_err(|e| format!("create wakeup pipe: {e}"))?;
        Ok(Self {
            read_fd: r,
            write_fd: Arc::new(w),
        })
    }

    fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        let w = Arc::clone(&self.write_fd);
        Arc::new(move || {
            let _ = write(&*w, &[1u8]);
        })
    }

    fn drain(&self) {
        let mut buf = [0u8; 64];
        while let Ok(n) = read(&self.read_fd, &mut buf) {
            if n == 0 {
                break;
            }
        }
    }
}

struct RepeatState {
    keycode: u32,
    next_at: Instant,
    interval: Duration,
}

struct DndOfferState {
    offer: WlDataOffer,
    has_uri_list: bool,
}

struct WaylandApp {
    // Wayland protocol objects
    shm: WlShm,
    surface: WlSurface,
    _xdg_surface: XdgSurface,
    toplevel: XdgToplevel,
    _decoration: Option<ZxdgToplevelDecorationV1>,
    viewport: Option<WpViewport>,
    _fractional_scale: Option<WpFractionalScaleV1>,
    cursor_shape_mgr: Option<WpCursorShapeManagerV1>,
    cursor_shape_dev: Option<WpCursorShapeDeviceV1>,
    _data_device: Option<WlDataDevice>,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    frame_cb: Option<WlCallback>,
    swapchain: ShmSwapchain,

    // Window geometry & scaling
    configured: bool,
    logical_size: (u32, u32),
    pending_logical_size: (u32, u32),
    scale_120: u32,
    integer_scale: u32,
    fullscreen: bool,
    pending_fullscreen: bool,
    needs_redraw: bool,
    should_exit: bool,
    exit_code: i32,

    // Palette & overlays
    palette: Palette,
    overlay: LinuxOverlay,
    error: Option<ErrorCard>,
    loading_label: Option<String>,
    toast: Option<String>,
    toast_expires_at: Option<Instant>,
    show_help: bool,

    // Image & viewing state
    store: ImageStore,
    preload_radius: usize,
    decode_target: (u32, u32),
    playlist: Option<Playlist>,
    focused_path: Option<PathBuf>,
    current_image: Option<Arc<DecodedImage>>,
    view: View,
    full_res_requested: bool,

    // Animation
    frame_index: usize,
    loops_completed: u32,
    next_anim_at: Option<Instant>,

    // Input state
    mods: Modifiers,
    repeat_delay_ms: i32,
    repeat_rate_hz: i32,
    active_repeat: Option<RepeatState>,
    pointer_pos_logical: (f64, f64),
    pointer_enter_serial: u32,
    dragging: bool,
    last_drag_phys: (f32, f32),
    last_left_click: Option<(Instant, (f64, f64))>,
    scroll_v120_accum: i32,
    scroll_px_accum: f64,
    last_cursor_grab: u8, // 0=default, 1=grab, 2=grabbing

    // Drag-and-drop state
    dnd_offers: Vec<DndOfferState>,
    active_dnd_offer: Option<WlDataOffer>,
    drop_result: SharedDropResult,

    // Deferred folder scan
    wake_notify: Arc<dyn Fn() + Send + Sync>,
    first_real_present_done: bool,
    pending_folder_scan: Option<PathBuf>,
    scan_generation: u64,
    scan_result: SharedScanResult,

    // Benchmark
    bench: Option<BenchState>,
    bench_sample_at: Option<Instant>,
    bench_timeout_at: Option<Instant>,
}

impl WaylandApp {
    fn physical_size(&self) -> (u32, u32) {
        let (lw, lh) = self.logical_size;
        if self.viewport.is_some() {
            let pw = ((u64::from(lw) * u64::from(self.scale_120) + 60) / 120).max(1) as u32;
            let ph = ((u64::from(lh) * u64::from(self.scale_120) + 60) / 120).max(1) as u32;
            (pw, ph)
        } else {
            let s = self.integer_scale.max(1);
            (lw.saturating_mul(s).max(1), lh.saturating_mul(s).max(1))
        }
    }

    fn source_size(&self) -> Option<(u32, u32)> {
        self.current_image
            .as_ref()
            .map(|img| (img.source_width, img.source_height))
    }

    fn scale_f32(&self) -> f32 {
        if self.viewport.is_some() {
            self.scale_120 as f32 / 120.0
        } else {
            self.integer_scale.max(1) as f32
        }
    }

    fn pointer_pos_physical(&self) -> (f32, f32) {
        let s = self.scale_f32();
        (
            self.pointer_pos_logical.0 as f32 * s,
            self.pointer_pos_logical.1 as f32 * s,
        )
    }

    fn relayout_view(&mut self) {
        if let Some(src) = self.source_size() {
            let client = self.physical_size();
            self.view.relayout(src, client);
        }
    }

    fn update_title(&self, conn: &mut Connection<Self>) {
        let title = match (&self.focused_path, &self.current_image) {
            (Some(path), Some(img)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                let pos = self
                    .playlist
                    .as_ref()
                    .map(|pl| format!("{} / {}", pl.index() + 1, pl.len()))
                    .unwrap_or_else(|| "1 / 1".to_string());
                format!(
                    "{name} — {pos} — {}×{} — Kalux",
                    img.source_width, img.source_height
                )
            }
            (Some(path), None) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                format!("{name} — Kalux")
            }
            (None, _) => "Kalux".to_string(),
        };
        if let Ok(c_title) = CString::new(title) {
            self.toplevel.set_title(conn, c_title);
        }
    }

    fn update_cursor(&mut self, conn: &mut Connection<Self>) {
        let desired: u8 = if self.dragging {
            2
        } else if self.is_pannable() {
            1
        } else {
            0
        };
        if desired == self.last_cursor_grab {
            return;
        }
        self.last_cursor_grab = desired;
        if let Some(dev) = self.cursor_shape_dev {
            let shape = match desired {
                2 => wp_cursor_shape_device_v1::Shape::Grabbing,
                1 => wp_cursor_shape_device_v1::Shape::Grab,
                _ => wp_cursor_shape_device_v1::Shape::Default,
            };
            dev.set_shape(conn, self.pointer_enter_serial, shape);
        }
    }

    fn is_pannable(&self) -> bool {
        let Some(ref img) = self.current_image else {
            return false;
        };
        let (pw, ph) = self.physical_size();
        let draw_w = img.source_width as f32 * self.view.scale;
        let draw_h = img.source_height as f32 * self.view.scale;
        draw_w > pw as f32 + 1.0 || draw_h > ph as f32 + 1.0
    }

    fn show_toast(&mut self, text: impl Into<String>) {
        self.toast = Some(text.into());
        self.toast_expires_at = Some(Instant::now() + TOAST_DURATION);
        self.needs_redraw = true;
    }

    fn zoom_percent_label(&self) -> String {
        let pct = (self.view.scale * 100.0).round() as u32;
        if self.view.mode == ZoomMode::Fit {
            format!("{pct}% · Fit")
        } else {
            format!("{pct}%")
        }
    }

    fn open_file(&mut self, conn: &mut Connection<Self>, path: PathBuf, rescan: bool) {
        self.focused_path = Some(path.clone());
        self.error = None;
        self.full_res_requested = false;
        self.frame_index = 0;
        self.loops_completed = 0;
        self.next_anim_at = None;
        self.dragging = false;

        if rescan {
            self.playlist = Some(Playlist::single(path.clone()));
            self.pending_folder_scan = Some(path.clone());
            if self.first_real_present_done {
                self.kick_folder_scan();
            }
        }

        let neighbors = self
            .playlist
            .as_ref()
            .map(|pl| pl.neighbors(self.preload_radius))
            .unwrap_or_default();

        let target = self.decode_target;
        if let Some(hit) = self.store.focus(&path, &neighbors, target) {
            self.loading_label = None;
            self.install_image(conn, hit, false);
        } else {
            self.current_image = None;
            self.loading_label = Some("Loading…".to_string());
            self.update_title(conn);
            self.needs_redraw = true;
        }
    }

    fn install_image(
        &mut self,
        conn: &mut Connection<Self>,
        image: Arc<DecodedImage>,
        keep_view: bool,
    ) {
        let client = self.physical_size();
        if !keep_view {
            self.view = View::fit((image.source_width, image.source_height), client);
            self.frame_index = 0;
            self.loops_completed = 0;
            self.full_res_requested = false;
        }

        let is_anim = image.is_animated();
        let first_delay = image.frames.first().map(|f| f.delay_ms).unwrap_or(0);
        self.current_image = Some(image);
        self.error = None;
        self.loading_label = None;

        self.next_anim_at = if is_anim && first_delay > 0 {
            Some(Instant::now() + Duration::from_millis(u64::from(first_delay)))
        } else {
            None
        };

        self.update_title(conn);
        self.update_cursor(conn);
        self.needs_redraw = true;
    }

    fn maybe_request_full_res(&mut self) {
        if self.full_res_requested {
            return;
        }
        let (Some(img), Some(path)) = (&self.current_image, &self.focused_path) else {
            return;
        };
        if self.view.needs_full_resolution(
            (img.source_width, img.source_height),
            (img.width, img.height),
        ) {
            self.full_res_requested = true;
            self.store.request_full_resolution(path);
        }
    }

    fn kick_folder_scan(&mut self) {
        let Some(path) = self.pending_folder_scan.take() else {
            return;
        };
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let gen_id = self.scan_generation;
        let out = Arc::clone(&self.scan_result);
        let wake = Arc::clone(&self.wake_notify);

        std::thread::spawn(move || {
            let pl = Playlist::scan_folder_of(&path).unwrap_or_else(|_| Playlist::single(path));
            if let Ok(mut guard) = out.lock() {
                *guard = Some((gen_id, pl));
            }
            wake();
        });
    }

    fn drain_async_events(&mut self, conn: &mut Connection<Self>) {
        // 1. Drain dropped file from drag-and-drop
        let dropped = self.drop_result.lock().ok().and_then(|mut g| g.take());
        if let Some(dropped) = dropped {
            self.open_file(conn, dropped, true);
        }

        // 2. Drain folder scan result
        let scanned = self.scan_result.lock().ok().and_then(|mut g| g.take());
        if let Some((gen_id, pl)) = scanned
            && gen_id == self.scan_generation
        {
            let neighbors = pl.neighbors(self.preload_radius);
            let focused = pl.current().to_path_buf();
            self.playlist = Some(pl);
            self.update_title(conn);
            if self.focused_path.as_deref() == Some(focused.as_path()) {
                let _ = self.store.focus(&focused, &neighbors, self.decode_target);
            }
        }

        // 3. Drain decoded image events from ImageStore
        for ev in self.store.poll() {
            match ev {
                StoreEvent::Ready {
                    path,
                    image,
                    full_resolution,
                    elapsed,
                } => {
                    if self.focused_path.as_deref() == Some(path.as_path()) {
                        if let Some(ref mut bench) = self.bench {
                            bench.record_decode(elapsed);
                        }
                        let is_upgrade = full_resolution
                            && self
                                .current_image
                                .as_ref()
                                .is_some_and(|cur| cur.is_downscaled());
                        self.install_image(conn, image, is_upgrade);
                    }
                }
                StoreEvent::Failed { path, error } => {
                    if self.focused_path.as_deref() == Some(path.as_path()) {
                        self.current_image = None;
                        self.loading_label = None;
                        self.next_anim_at = None;
                        self.error = Some(error_card_for_decode(&path, &error));
                        self.update_title(conn);
                        self.needs_redraw = true;
                        if self.bench.is_some() {
                            eprintln!("kalux: failed to decode {}: {error}", path.display());
                            self.exit_code = 1;
                            self.should_exit = true;
                        }
                    }
                }
            }
        }
    }

    fn handle_action(&mut self, conn: &mut Connection<Self>, action: Action) {
        match action {
            Action::Next => {
                if let Some(ref mut pl) = self.playlist
                    && pl.len() > 1
                {
                    let next = pl.step(1).to_path_buf();
                    self.open_file(conn, next, false);
                }
            }
            Action::Prev => {
                if let Some(ref mut pl) = self.playlist
                    && pl.len() > 1
                {
                    let prev = pl.step(-1).to_path_buf();
                    self.open_file(conn, prev, false);
                }
            }
            Action::First => {
                if let Some(ref mut pl) = self.playlist
                    && pl.len() > 1
                {
                    let first = pl.go_first().to_path_buf();
                    self.open_file(conn, first, false);
                }
            }
            Action::Last => {
                if let Some(ref mut pl) = self.playlist
                    && pl.len() > 1
                {
                    let last = pl.go_last().to_path_buf();
                    self.open_file(conn, last, false);
                }
            }
            Action::ZoomIn => {
                if let Some(src) = self.source_size() {
                    let client = self.physical_size();
                    let center = (client.0 as f32 * 0.5, client.1 as f32 * 0.5);
                    self.view.zoom_at(KEY_ZOOM_FACTOR, center, src, client);
                    self.maybe_request_full_res();
                    self.update_cursor(conn);
                    let label = self.zoom_percent_label();
                    self.show_toast(label);
                }
            }
            Action::ZoomOut => {
                if let Some(src) = self.source_size() {
                    let client = self.physical_size();
                    let center = (client.0 as f32 * 0.5, client.1 as f32 * 0.5);
                    self.view
                        .zoom_at(1.0 / KEY_ZOOM_FACTOR, center, src, client);
                    self.update_cursor(conn);
                    let label = self.zoom_percent_label();
                    self.show_toast(label);
                }
            }
            Action::ZoomFit => {
                if let Some(src) = self.source_size() {
                    let client = self.physical_size();
                    self.view = View::fit(src, client);
                    self.update_cursor(conn);
                    let label = self.zoom_percent_label();
                    self.show_toast(label);
                }
            }
            Action::ZoomActual => {
                if let Some(src) = self.source_size() {
                    let client = self.physical_size();
                    self.view.actual_size(None, src, client);
                    self.maybe_request_full_res();
                    self.update_cursor(conn);
                    let label = self.zoom_percent_label();
                    self.show_toast(label);
                }
            }
            Action::ToggleFullscreen => {
                if self.fullscreen {
                    self.toplevel.unset_fullscreen(conn);
                } else {
                    self.toplevel.set_fullscreen(conn, None);
                }
            }
            Action::ShowHelp => {
                self.show_help = !self.show_help;
                self.needs_redraw = true;
            }
            Action::Escape => {
                if self.show_help {
                    self.show_help = false;
                    self.needs_redraw = true;
                } else if self.fullscreen {
                    self.toplevel.unset_fullscreen(conn);
                } else {
                    self.should_exit = true;
                }
            }
            Action::Close => {
                self.should_exit = true;
            }
            Action::RotateCw
            | Action::RotateCcw
            | Action::ToggleInfo
            | Action::Delete
            | Action::CopyImage
            | Action::CopyPath
            | Action::EditInKapix => {
                self.show_toast("Coming in v0.2");
            }
        }
    }

    fn apply_scroll_zoom(&mut self, conn: &mut Connection<Self>, steps: f32) {
        let Some(src) = self.source_size() else {
            return;
        };
        if steps.abs() < 1e-4 {
            return;
        }
        let client = self.physical_size();
        let anchor = self.pointer_pos_physical();
        let factor = 1.15f32.powf(steps);
        self.view.zoom_at(factor, anchor, src, client);
        self.maybe_request_full_res();
        self.update_cursor(conn);
        let label = self.zoom_percent_label();
        self.show_toast(label);
    }

    fn tick_timers(&mut self, conn: &mut Connection<Self>, now: Instant) {
        if let Some(exp) = self.toast_expires_at
            && now >= exp
        {
            self.toast = None;
            self.toast_expires_at = None;
            self.needs_redraw = true;
        }

        if let Some(anim_at) = self.next_anim_at
            && now >= anim_at
        {
            if let Some(ref img) = self.current_image
                && img.is_animated()
            {
                let next_idx = self.frame_index + 1;
                if next_idx >= img.frames.len() {
                    self.loops_completed = self.loops_completed.saturating_add(1);
                    if img.loop_count != 0 && self.loops_completed >= img.loop_count {
                        self.next_anim_at = None;
                    } else {
                        self.frame_index = 0;
                        let delay = img.frames[0].delay_ms.max(10);
                        self.next_anim_at = Some(now + Duration::from_millis(u64::from(delay)));
                        self.needs_redraw = true;
                    }
                } else {
                    self.frame_index = next_idx;
                    let delay = img.frames[next_idx].delay_ms.max(10);
                    self.next_anim_at = Some(now + Duration::from_millis(u64::from(delay)));
                    self.needs_redraw = true;
                }
            } else {
                self.next_anim_at = None;
            }
        }

        if let Some(ref mut rep) = self.active_repeat
            && now >= rep.next_at
        {
            let keycode = rep.keycode;
            rep.next_at = now + rep.interval;
            if let Some(action) = action_for_linux_evdev(keycode, self.mods)
                && matches!(
                    action,
                    Action::Next | Action::Prev | Action::ZoomIn | Action::ZoomOut
                )
            {
                self.handle_action(conn, action);
            }
        }

        if let Some(sample_at) = self.bench_sample_at
            && now >= sample_at
        {
            self.bench_sample_at = None;
            if let Some(ref bench) = self.bench
                && let Err(e) = bench.finish_and_emit()
            {
                eprintln!("kalux: bench output failed: {e}");
                self.exit_code = 1;
            }
            self.should_exit = true;
        }

        if let Some(timeout_at) = self.bench_timeout_at
            && now >= timeout_at
        {
            eprintln!("kalux: --bench-startup timed out waiting for first frame");
            self.exit_code = 1;
            self.should_exit = true;
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.toast_expires_at,
            self.next_anim_at,
            self.active_repeat.as_ref().map(|r| r.next_at),
            self.bench_sample_at,
            self.bench_timeout_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn render_if_needed(&mut self, conn: &mut Connection<Self>) {
        if !self.configured || !self.needs_redraw || self.frame_cb.is_some() {
            return;
        }
        let (pw, ph) = self.physical_size();
        if pw == 0 || ph == 0 {
            return;
        }

        let shm = self.shm;
        let scale = self.scale_f32();
        let Ok((_slot_idx, buffer, pixels)) =
            self.swapchain.acquire(conn, shm, pw, ph, |state, idx| {
                state.swapchain.mark_released(idx);
            })
        else {
            return;
        };

        let mut canvas = Canvas::new(pw, ph, pixels);
        canvas.clear(self.palette.bg);

        if let Some(ref img) = self.current_image {
            let src = (img.source_width, img.source_height);
            let dest = self.view.dest_rect(src, (pw, ph));
            canvas.draw_image(img, self.frame_index, dest, self.view.scale);
        }

        let ov = OverlayView {
            empty_hint: self.focused_path.is_none() && self.current_image.is_none(),
            error: self.error.as_ref(),
            loading_label: self.loading_label.as_deref(),
            toast: self.toast.as_deref(),
            show_help: self.show_help,
        };
        self.overlay.draw(&mut canvas, scale, self.palette, &ov);

        // Set logical viewport destination or integer buffer scale.
        if let Some(vp) = self.viewport {
            vp.set_destination(conn, self.logical_size.0 as i32, self.logical_size.1 as i32);
        } else {
            self.surface
                .set_buffer_scale(conn, self.integer_scale.max(1) as i32);
        }

        self.surface.attach(conn, Some(buffer), 0, 0);
        self.surface.damage_buffer(conn, 0, 0, pw as i32, ph as i32);

        let cb = self.surface.frame_with_cb(conn, |ctx| {
            let wl_callback::Event::Done(_) = ctx.event;
            ctx.state.frame_cb = None;
        });
        self.frame_cb = Some(cb);
        self.surface.commit(conn);
        self.needs_redraw = false;

        // First real image present hook: trigger deferred folder scan + bench sample timer!
        if self.current_image.is_some() && !self.first_real_present_done {
            self.first_real_present_done = true;
            if let Some(ref mut bench) = self.bench
                && !bench.has_presented()
                && bench.record_first_present()
            {
                self.bench_sample_at = Some(Instant::now() + BENCH_SAMPLE_DELAY);
            }
            if self.bench.is_none() {
                self.kick_folder_scan();
            }
        }
    }
}

pub fn run(cfg: LaunchConfig) -> i32 {
    let wake = match WakePipe::new() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("kalux: {e}");
            return 1;
        }
    };
    let wake_notify = wake.notifier();

    // 1. Start decoding the initial file BEFORE connecting to Wayland so decode overlaps handshake!
    let store_cfg = StoreConfig::default();
    let preload_radius = store_cfg.preload_radius;
    let mut store = ImageStore::new(store_cfg, Arc::clone(&wake_notify));

    let decode_target = DEFAULT_DECODE_TARGET;
    let mut immediate_image: Option<Arc<DecodedImage>> = None;
    let mut playlist = None;
    let mut focused_path = None;
    let mut pending_folder_scan = None;
    let mut loading_label = None;

    if let Some(ref file) = cfg.initial_file {
        immediate_image = store.focus(file, &[], decode_target);
        playlist = Some(Playlist::single(file.clone()));
        focused_path = Some(file.clone());
        pending_folder_scan = Some(file.clone());
        if immediate_image.is_none() {
            loading_label = Some("Loading…".to_string());
        }
    }

    // 2. Connect to Wayland compositor (Niri / Sway / Hyprland / GNOME / KDE)
    let mut conn = match Connection::<WaylandApp>::connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kalux: could not connect to Wayland display ($WAYLAND_DISPLAY): {e}");
            return 1;
        }
    };
    if let Err(e) = conn.blocking_roundtrip() {
        eprintln!("kalux: Wayland initial roundtrip failed: {e}");
        return 1;
    }

    let Ok(compositor) = conn.bind_singleton::<WlCompositor>(4..=6) else {
        eprintln!("kalux: compositor does not advertise wl_compositor v4+");
        return 1;
    };
    let Ok(shm) = conn.bind_singleton::<WlShm>(1..=2) else {
        eprintln!("kalux: compositor does not advertise wl_shm");
        return 1;
    };
    let Ok(xdg_wm_base) = conn.bind_singleton_with_cb::<XdgWmBase, _>(1..=6, xdg_wm_base_cb) else {
        eprintln!("kalux: compositor does not advertise xdg_wm_base");
        return 1;
    };

    let decoration_mgr = conn.bind_singleton::<ZxdgDecorationManagerV1>(1..=1).ok();
    let frac_scale_mgr = conn
        .bind_singleton::<WpFractionalScaleManagerV1>(1..=1)
        .ok();
    let viewporter = conn.bind_singleton::<WpViewporter>(1..=1).ok();
    let cursor_shape_mgr = conn.bind_singleton::<WpCursorShapeManagerV1>(1..=2).ok();
    let data_device_mgr = conn.bind_singleton::<WlDataDeviceManager>(1..=3).ok();
    let seat = conn
        .bind_singleton_with_cb::<WlSeat, _>(5..=9, wl_seat_cb)
        .ok();

    let surface = compositor.create_surface_with_cb(&mut conn, wl_surface_cb);
    let xdg_surface = xdg_wm_base.get_xdg_surface_with_cb(&mut conn, surface, xdg_surface_cb);
    let toplevel = xdg_surface.get_toplevel_with_cb(&mut conn, xdg_toplevel_cb);
    toplevel.set_app_id(&mut conn, c"kalux".into());
    toplevel.set_title(&mut conn, c"Kalux".into());

    // Ask Niri / compositor for server-side / borderless tiling (no wasted CSD bar).
    let decoration = decoration_mgr.map(|mgr| {
        let dec = mgr.get_toplevel_decoration_with_cb(&mut conn, toplevel, |ctx| {
            let _ = ctx.event;
        });
        dec.set_mode(&mut conn, zxdg_toplevel_decoration_v1::Mode::ServerSide);
        dec
    });

    // Crisp fractional HiDPI scaling via wp_fractional_scale_v1 + wp_viewporter.
    let (viewport, fractional_scale) = match (viewporter, frac_scale_mgr) {
        (Some(vp_mgr), Some(fs_mgr)) => {
            let vp = vp_mgr.get_viewport(&mut conn, surface);
            let fs = fs_mgr.get_fractional_scale_with_cb(&mut conn, surface, |ctx| {
                if let wp_fractional_scale_v1::Event::PreferredScale(scale) = ctx.event
                    && scale > 0
                    && scale != ctx.state.scale_120
                {
                    ctx.state.scale_120 = scale;
                    ctx.state.relayout_view();
                    ctx.state.needs_redraw = true;
                }
            });
            (Some(vp), Some(fs))
        }
        _ => (None, None),
    };

    // Drag-and-drop data device on the primary seat.
    let data_device = match (data_device_mgr, seat) {
        (Some(mgr), Some(s)) => Some(mgr.get_data_device_with_cb(&mut conn, s, wl_data_device_cb)),
        _ => None,
    };

    // Commit initial empty surface so Niri sends the initial xdg_toplevel + xdg_surface configure.
    surface.commit(&mut conn);
    let _ = conn.flush(IoMode::Blocking);

    let mut bench = cfg.bench;
    if let Some(ref mut b) = bench {
        b.record_window_ready();
    }
    let bench_timeout_at = bench.as_ref().map(|_| Instant::now() + BENCH_TIMEOUT);

    let initial_view = immediate_image
        .as_ref()
        .map(|img| {
            View::fit(
                (img.source_width, img.source_height),
                (DEFAULT_LOGICAL_W, DEFAULT_LOGICAL_H),
            )
        })
        .unwrap_or_else(|| View::fit((1, 1), (DEFAULT_LOGICAL_W, DEFAULT_LOGICAL_H)));

    let mut state = WaylandApp {
        shm,
        surface,
        _xdg_surface: xdg_surface,
        toplevel,
        _decoration: decoration,
        viewport,
        _fractional_scale: fractional_scale,
        cursor_shape_mgr,
        cursor_shape_dev: None,
        _data_device: data_device,
        keyboard: None,
        pointer: None,
        frame_cb: None,
        swapchain: ShmSwapchain::new(),

        configured: false,
        logical_size: (DEFAULT_LOGICAL_W, DEFAULT_LOGICAL_H),
        pending_logical_size: (0, 0),
        scale_120: 120,
        integer_scale: 1,
        fullscreen: false,
        pending_fullscreen: false,
        needs_redraw: true,
        should_exit: false,
        exit_code: 0,

        palette: system_palette(),
        overlay: LinuxOverlay::new(),
        error: cfg.initial_error,
        loading_label,
        toast: None,
        toast_expires_at: None,
        show_help: false,

        store,
        preload_radius,
        decode_target,
        playlist,
        focused_path,
        current_image: immediate_image,
        view: initial_view,
        full_res_requested: false,

        frame_index: 0,
        loops_completed: 0,
        next_anim_at: None,

        mods: Modifiers::default(),
        repeat_delay_ms: 400,
        repeat_rate_hz: 25,
        active_repeat: None,
        pointer_pos_logical: (0.0, 0.0),
        pointer_enter_serial: 0,
        dragging: false,
        last_drag_phys: (0.0, 0.0),
        last_left_click: None,
        scroll_v120_accum: 0,
        scroll_px_accum: 0.0,
        last_cursor_grab: 0,

        dnd_offers: Vec::new(),
        active_dnd_offer: None,
        drop_result: Arc::new(Mutex::new(None)),

        wake_notify,
        first_real_present_done: false,
        pending_folder_scan,
        scan_generation: 0,
        scan_result: Arc::new(Mutex::new(None)),

        bench,
        bench_sample_at: None,
        bench_timeout_at,
    };

    state.update_title(&mut conn);

    // 3. Event-driven poll(2) loop (0% idle CPU!)
    while !state.should_exit {
        state.drain_async_events(&mut conn);
        state.tick_timers(&mut conn, Instant::now());
        if state.should_exit {
            break;
        }
        state.render_if_needed(&mut conn);

        if let Err(e) = conn.flush(IoMode::Blocking)
            && e.kind() != std::io::ErrorKind::WouldBlock
        {
            break;
        }

        let timeout = state.next_deadline().map(|dl| {
            let dur = dl.saturating_duration_since(Instant::now());
            Timespec {
                tv_sec: dur.as_secs() as i64,
                tv_nsec: dur.subsec_nanos() as i64,
            }
        });

        // SAFETY: `conn` remains open and outlives `wl_fd` and `poll_fds` within this loop iteration.
        let wl_fd = unsafe { BorrowedFd::borrow_raw(conn.as_raw_fd()) };
        let wake_fd = wake.read_fd.as_fd();
        let mut poll_fds = [
            PollFd::new(&wl_fd, PollFlags::IN),
            PollFd::new(&wake_fd, PollFlags::IN),
        ];

        match poll(&mut poll_fds, timeout.as_ref()) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => break,
        }

        let wl_revents = poll_fds[0].revents();
        let wake_revents = poll_fds[1].revents();

        if wake_revents.contains(PollFlags::IN) {
            wake.drain();
        }

        if wl_revents.contains(PollFlags::IN) {
            match conn.recv_events(IoMode::NonBlocking) {
                Ok(()) => conn.dispatch_events(&mut state),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
        if wl_revents.intersects(PollFlags::HUP | PollFlags::ERR) {
            break;
        }
    }

    state.swapchain.destroy_all(&mut conn);
    let _ = conn.flush(IoMode::Blocking);
    state.exit_code
}

// --- Wayland protocol callbacks ---

fn xdg_wm_base_cb(ctx: EventCtx<WaylandApp, XdgWmBase>) {
    if let xdg_wm_base::Event::Ping(serial) = ctx.event {
        ctx.proxy.pong(ctx.conn, serial);
    }
}

fn wl_surface_cb(ctx: EventCtx<WaylandApp, WlSurface>) {
    if let wl_surface::Event::PreferredBufferScale(factor) = ctx.event {
        let s = (factor.max(1)) as u32;
        if s != ctx.state.integer_scale {
            ctx.state.integer_scale = s;
            if ctx.state.viewport.is_none() {
                ctx.state.relayout_view();
                ctx.state.needs_redraw = true;
            }
        }
    }
}

fn xdg_toplevel_cb(ctx: EventCtx<WaylandApp, XdgToplevel>) {
    match ctx.event {
        xdg_toplevel::Event::Configure(args) => {
            let w = if args.width > 0 { args.width as u32 } else { 0 };
            let h = if args.height > 0 {
                args.height as u32
            } else {
                0
            };
            ctx.state.pending_logical_size = (w, h);

            let mut fs = false;
            for &chunk in args.states.as_chunks::<4>().0 {
                let val = u32::from_ne_bytes(chunk);
                if val == xdg_toplevel::State::Fullscreen as u32 {
                    fs = true;
                }
            }
            ctx.state.pending_fullscreen = fs;
        }
        xdg_toplevel::Event::Close => {
            ctx.state.should_exit = true;
        }
        _ => {}
    }
}

fn xdg_surface_cb(ctx: EventCtx<WaylandApp, XdgSurface>) {
    let xdg_surface::Event::Configure(serial) = ctx.event else {
        return;
    };
    ctx.proxy.ack_configure(ctx.conn, serial);

    let (pw_cfg, ph_cfg) = ctx.state.pending_logical_size;
    let new_w = if pw_cfg > 0 {
        pw_cfg
    } else {
        ctx.state.logical_size.0
    };
    let new_h = if ph_cfg > 0 {
        ph_cfg
    } else {
        ctx.state.logical_size.1
    };
    let size_changed = (new_w, new_h) != ctx.state.logical_size || !ctx.state.configured;
    ctx.state.logical_size = (new_w, new_h);
    ctx.state.fullscreen = ctx.state.pending_fullscreen;
    ctx.state.configured = true;

    if size_changed {
        let (phys_w, phys_h) = ctx.state.physical_size();
        ctx.state.decode_target = (phys_w.max(1920), phys_h.max(1080));
        ctx.state.relayout_view();
        ctx.state.update_cursor(ctx.conn);
    }
    ctx.state.needs_redraw = true;
}

fn wl_seat_cb(ctx: EventCtx<WaylandApp, WlSeat>) {
    if let wl_seat::Event::Capabilities(caps) = ctx.event {
        if caps.contains(wl_seat::Capability::Keyboard) && ctx.state.keyboard.is_none() {
            let kb = ctx.proxy.get_keyboard_with_cb(ctx.conn, wl_keyboard_cb);
            ctx.state.keyboard = Some(kb);
        }
        if caps.contains(wl_seat::Capability::Pointer) && ctx.state.pointer.is_none() {
            let ptr = ctx.proxy.get_pointer_with_cb(ctx.conn, wl_pointer_cb);
            if let Some(mgr) = ctx.state.cursor_shape_mgr {
                let dev = mgr.get_pointer(ctx.conn, ptr);
                ctx.state.cursor_shape_dev = Some(dev);
            }
            ctx.state.pointer = Some(ptr);
        }
    }
}

fn wl_keyboard_cb(ctx: EventCtx<WaylandApp, WlKeyboard>) {
    match ctx.event {
        wl_keyboard::Event::Keymap(args) => {
            drop(args.fd);
        }
        wl_keyboard::Event::Leave(_) => {
            ctx.state.mods = Modifiers::default();
            ctx.state.active_repeat = None;
        }
        wl_keyboard::Event::Modifiers(args) => {
            // Standard XKB modifier bitmask: bit 0 = Shift, bit 2 = Control, bit 3 = Mod1 (Alt).
            ctx.state.mods.shift = (args.mods_depressed & 1) != 0;
            ctx.state.mods.ctrl = (args.mods_depressed & 4) != 0;
            ctx.state.mods.alt = (args.mods_depressed & 8) != 0;
        }
        wl_keyboard::Event::RepeatInfo(args) => {
            ctx.state.repeat_rate_hz = args.rate;
            ctx.state.repeat_delay_ms = args.delay;
        }
        wl_keyboard::Event::Key(args) => {
            let pressed = args.state == wl_keyboard::KeyState::Pressed;
            match args.key {
                KEY_LEFTSHIFT | KEY_RIGHTSHIFT => ctx.state.mods.shift = pressed,
                KEY_LEFTCTRL | KEY_RIGHTCTRL => ctx.state.mods.ctrl = pressed,
                KEY_LEFTALT | KEY_RIGHTALT => ctx.state.mods.alt = pressed,
                _ => {}
            }

            if pressed {
                if let Some(action) = action_for_linux_evdev(args.key, ctx.state.mods) {
                    ctx.state.handle_action(ctx.conn, action);
                    if matches!(
                        action,
                        Action::Next | Action::Prev | Action::ZoomIn | Action::ZoomOut
                    ) && ctx.state.repeat_rate_hz > 0
                    {
                        let delay =
                            Duration::from_millis(ctx.state.repeat_delay_ms.max(100) as u64);
                        let interval =
                            Duration::from_millis((1000 / ctx.state.repeat_rate_hz.max(1)) as u64);
                        ctx.state.active_repeat = Some(RepeatState {
                            keycode: args.key,
                            next_at: Instant::now() + delay,
                            interval,
                        });
                    } else {
                        ctx.state.active_repeat = None;
                    }
                }
            } else if ctx
                .state
                .active_repeat
                .as_ref()
                .is_some_and(|r| r.keycode == args.key)
            {
                ctx.state.active_repeat = None;
            }
        }
        _ => {}
    }
}

fn wl_pointer_cb(ctx: EventCtx<WaylandApp, WlPointer>) {
    match ctx.event {
        wl_pointer::Event::Enter(args) => {
            ctx.state.pointer_enter_serial = args.serial;
            ctx.state.pointer_pos_logical = (args.surface_x.as_f64(), args.surface_y.as_f64());
            ctx.state.last_cursor_grab = u8::MAX;
            ctx.state.update_cursor(ctx.conn);
        }
        wl_pointer::Event::Leave(_) => {
            ctx.state.dragging = false;
        }
        wl_pointer::Event::Motion(args) => {
            ctx.state.pointer_pos_logical = (args.surface_x.as_f64(), args.surface_y.as_f64());
            if ctx.state.dragging {
                let cur = ctx.state.pointer_pos_physical();
                let dx = cur.0 - ctx.state.last_drag_phys.0;
                let dy = cur.1 - ctx.state.last_drag_phys.1;
                ctx.state.last_drag_phys = cur;
                if (dx.abs() > 0.01 || dy.abs() > 0.01)
                    && let Some(src) = ctx.state.source_size()
                {
                    let client = ctx.state.physical_size();
                    ctx.state.view.pan_by(dx, dy, src, client);
                    ctx.state.needs_redraw = true;
                }
            }
        }
        wl_pointer::Event::Button(args) => {
            let pressed = args.state == wl_pointer::ButtonState::Pressed;
            match args.button {
                BTN_LEFT => {
                    if pressed {
                        let now = Instant::now();
                        let pos = ctx.state.pointer_pos_logical;
                        let is_double =
                            ctx.state.last_left_click.is_some_and(|(prev_t, prev_p)| {
                                let dx = pos.0 - prev_p.0;
                                let dy = pos.1 - prev_p.1;
                                now.duration_since(prev_t) <= DOUBLE_CLICK_MAX_GAP
                                    && (dx * dx + dy * dy) <= DOUBLE_CLICK_MAX_DIST_SQ
                            });
                        if is_double {
                            ctx.state.last_left_click = None;
                            ctx.state.dragging = false;
                            ctx.state.handle_action(ctx.conn, Action::ToggleFullscreen);
                        } else {
                            ctx.state.last_left_click = Some((now, pos));
                            if ctx.state.is_pannable() {
                                ctx.state.dragging = true;
                                ctx.state.last_drag_phys = ctx.state.pointer_pos_physical();
                            }
                        }
                    } else {
                        ctx.state.dragging = false;
                    }
                    ctx.state.update_cursor(ctx.conn);
                }
                BTN_SIDE if pressed => {
                    ctx.state.handle_action(ctx.conn, Action::Prev);
                }
                BTN_EXTRA if pressed => {
                    ctx.state.handle_action(ctx.conn, Action::Next);
                }
                _ => {}
            }
        }
        wl_pointer::Event::AxisValue120(args) => {
            if args.axis == wl_pointer::Axis::VerticalScroll {
                ctx.state.scroll_v120_accum += args.value120;
            }
        }
        wl_pointer::Event::Axis(args) => {
            if args.axis == wl_pointer::Axis::VerticalScroll {
                ctx.state.scroll_px_accum += args.value.as_f64();
            }
        }
        wl_pointer::Event::Frame => {
            if ctx.state.scroll_v120_accum != 0 {
                let steps = -(ctx.state.scroll_v120_accum as f32) / 120.0;
                ctx.state.scroll_v120_accum = 0;
                ctx.state.scroll_px_accum = 0.0;
                ctx.state.apply_scroll_zoom(ctx.conn, steps);
            } else if ctx.state.scroll_px_accum.abs() > 0.01 {
                let steps = -(ctx.state.scroll_px_accum as f32) / 15.0;
                ctx.state.scroll_px_accum = 0.0;
                ctx.state.apply_scroll_zoom(ctx.conn, steps);
            }
        }
        _ => {}
    }
}

fn wl_data_device_cb(ctx: EventCtx<WaylandApp, WlDataDevice>) {
    match ctx.event {
        wl_data_device::Event::DataOffer(offer) => {
            ctx.state.dnd_offers.push(DndOfferState {
                offer,
                has_uri_list: false,
            });
            ctx.conn.set_callback_for(offer, |offer_ctx| {
                if let wl_data_offer::Event::Offer(mime) = offer_ctx.event
                    && mime.as_bytes() == b"text/uri-list"
                    && let Some(entry) = offer_ctx
                        .state
                        .dnd_offers
                        .iter_mut()
                        .find(|o| o.offer == offer_ctx.proxy)
                {
                    entry.has_uri_list = true;
                }
            });
        }
        wl_data_device::Event::Enter(args) => {
            let found = args.id.and_then(|obj_id| {
                ctx.state
                    .dnd_offers
                    .iter()
                    .find(|o| o.offer.id() == obj_id)
                    .map(|o| (o.offer, o.has_uri_list))
            });
            ctx.state.active_dnd_offer = found.map(|(offer, _)| offer);
            if let Some((offer, has_uri_list)) = found {
                if has_uri_list {
                    offer.accept(ctx.conn, args.serial, Some(c"text/uri-list".into()));
                    offer.set_actions(
                        ctx.conn,
                        wl_data_device_manager::DndAction::Copy,
                        wl_data_device_manager::DndAction::Copy,
                    );
                } else {
                    offer.accept(ctx.conn, args.serial, None);
                }
            }
        }
        wl_data_device::Event::Leave => {
            ctx.state.active_dnd_offer = None;
        }
        wl_data_device::Event::Drop => {
            if let Some(offer) = ctx.state.active_dnd_offer.take() {
                let accepts = ctx
                    .state
                    .dnd_offers
                    .iter()
                    .any(|o| o.offer == offer && o.has_uri_list);
                if accepts && let Ok((r_fd, w_fd)) = pipe_with(PipeFlags::CLOEXEC) {
                    offer.receive(ctx.conn, c"text/uri-list".into(), w_fd);
                    offer.finish(ctx.conn);
                    let _ = ctx.conn.flush(IoMode::Blocking);

                    let drop_slot = Arc::clone(&ctx.state.drop_result);
                    let wake = Arc::clone(&ctx.state.wake_notify);
                    std::thread::spawn(move || {
                        let mut file = std::fs::File::from(r_fd);
                        let mut text = String::new();
                        let _ = file.read_to_string(&mut text);
                        if let Some(path) = parse_first_file_uri(&text) {
                            if let Ok(mut guard) = drop_slot.lock() {
                                *guard = Some(path);
                            }
                            wake();
                        }
                    });
                }
                ctx.state.dnd_offers.retain(|o| o.offer != offer);
                offer.destroy(ctx.conn);
            }
        }
        wl_data_device::Event::Selection(Some(obj_id)) => {
            if let Some(pos) = ctx
                .state
                .dnd_offers
                .iter()
                .position(|o| o.offer.id() == obj_id)
            {
                let offer = ctx.state.dnd_offers.remove(pos).offer;
                offer.destroy(ctx.conn);
            }
        }
        _ => {}
    }
}

/// Parse the first `file://...` URI from a `text/uri-list` payload, percent-decoding UTF-8 bytes.
fn parse_first_file_uri(uri_list: &str) -> Option<PathBuf> {
    for raw in uri_list.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("file://") else {
            continue;
        };
        let path_part = rest.strip_prefix("localhost").unwrap_or(rest);
        if !path_part.starts_with('/') {
            continue;
        }
        let decoded = percent_decode(path_part);
        let s = String::from_utf8(decoded).ok()?;
        return Some(PathBuf::from(s));
    }
    None
}

fn percent_decode(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn error_card_for_decode(path: &Path, err: &DecodeError) -> ErrorCard {
    let subject = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    match err {
        DecodeError::NotFound => ErrorCard {
            title: "File not found".to_string(),
            subject,
            message: "This file may have been moved or deleted.".to_string(),
            hint: None,
        },
        DecodeError::Unsupported { hint } => ErrorCard {
            title: "Unsupported image format".to_string(),
            subject,
            message: "Kalux cannot decode this file format yet.".to_string(),
            hint: hint.clone(),
        },
        _ => ErrorCard {
            title: "Could not open image".to_string(),
            subject,
            message: err.to_string(),
            hint: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_percent_encoded_file_uri() {
        let uri = "# comment\r\nfile:///home/karel/My%20Photos/dolomites%20%231.jpg\r\nfile:///other.png\r\n";
        assert_eq!(
            parse_first_file_uri(uri),
            Some(PathBuf::from("/home/karel/My Photos/dolomites #1.jpg"))
        );
        assert_eq!(
            parse_first_file_uri("file://localhost/tmp/a.png"),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(parse_first_file_uri("https://example.com/a.jpg"), None);
    }
}
