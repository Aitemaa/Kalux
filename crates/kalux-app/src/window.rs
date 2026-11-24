//! Raw Win32 window, message loop, critical startup ordering, zoom/pan, navigation,
//! fullscreen toggle, DPI handling, animation timers, and folder-scan handoff.

use std::ffi::c_void;
use std::io::Write as _;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kalux_core::{Action, DecodedImage, Palette, Playlist, View};
use kalux_decode::{DecodeError, ImageStore, StoreConfig, StoreEvent};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, GetMonitorInfoW, HBRUSH, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    MonitorFromWindow, PAINTSTRUCT, UpdateWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::DragAcceptFiles;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWL_STYLE, GWLP_USERDATA, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowLongW, GetWindowPlacement, HWND_TOP, IDC_ARROW, KillTimer, LoadCursorW, MINMAXINFO,
    MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SIZE_MINIMIZED, SW_SHOW,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SetTimer, SetWindowLongPtrW, SetWindowLongW, SetWindowPlacement, SetWindowPos, SetWindowTextW,
    ShowWindow, TranslateMessage, UnregisterClassW, WINDOWPLACEMENT, WM_APP, WM_CAPTURECHANGED,
    WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE,
    WM_NCDESTROY, WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WM_XBUTTONDOWN, WNDCLASSEXW,
    WS_EX_ACCEPTFILES, WS_OVERLAPPEDWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::bench::{BenchState, attach_console_if_needed};
use crate::input;
use crate::overlay::{ErrorCard, OverlayView};
use crate::render::Renderer;

const WINDOW_CLASS: PCWSTR = w!("KaluxMainWindow");

const WM_APP_DECODED: u32 = WM_APP + 1;
const WM_APP_PLAYLIST: u32 = WM_APP + 2;

const TIMER_ANIM: usize = 1;
const TIMER_TOAST: usize = 2;
const TIMER_BENCH_SAMPLE: usize = 3;
const TIMER_BENCH_TIMEOUT: usize = 4;

const TOAST_DURATION_MS: u32 = 1200;
const BENCH_SAMPLE_DELAY_MS: u32 = 1000;
const BENCH_TIMEOUT_MS: u32 = 30_000;

/// Keyboard zoom multiplier (`+` / `-`).
const KEY_ZOOM_FACTOR: f32 = 1.25;

pub struct LaunchConfig {
    pub initial_file: Option<PathBuf>,
    pub initial_error: Option<ErrorCard>,
    pub bench: Option<BenchState>,
}

struct FullscreenSaved {
    style: i32,
    placement: WINDOWPLACEMENT,
}

type SharedScanResult = Arc<Mutex<Option<(u64, Playlist)>>>;

struct AppState {
    hwnd_slot: Arc<AtomicIsize>,
    store: ImageStore,
    preload_radius: usize,
    monitor_size: (u32, u32),
    client_size: (u32, u32),
    dpi: u32,
    palette: Palette,

    renderer: Option<Renderer>,

    playlist: Option<Playlist>,
    focused_path: Option<PathBuf>,
    current_image: Option<Arc<DecodedImage>>,
    view: View,
    full_res_requested: bool,

    // Animation playback
    frame_index: usize,
    loops_completed: u32,

    // Overlays
    error: Option<ErrorCard>,
    loading_label: Option<String>,
    toast: Option<String>,
    show_help: bool,

    // Mouse drag panning
    dragging: bool,
    last_drag_pos: (i32, i32),

    // Fullscreen
    fullscreen: Option<FullscreenSaved>,

    // Deferred post-first-present folder scan
    first_real_present_done: bool,
    pending_folder_scan: Option<PathBuf>,
    scan_generation: u64,
    scan_result: SharedScanResult,

    // Benchmark mode
    bench: Option<BenchState>,
    exit_code: i32,
}

/// Execute the complete GUI lifecycle, following PRD §9.2 critical startup ordering.
pub fn run(cfg: LaunchConfig) -> i32 {
    // 1. Query target monitor under cursor BEFORE creating the window so decoding uses the
    //    monitor size directly (resize/fullscreen won't need re-decode).
    let (monitor_size, work_area) = monitor_under_cursor();

    // 2. Create ImageStore + notify hook BEFORE window creation.
    let hwnd_slot = Arc::new(AtomicIsize::new(0));
    let notify_slot = Arc::clone(&hwnd_slot);
    let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let raw = notify_slot.load(Ordering::Acquire);
        if raw != 0 {
            let hwnd = HWND(raw as *mut c_void);
            // SAFETY: `raw` is cleared to 0 in `WM_DESTROY`; `PostMessageW` safely rejects
            // stale HWNDs if a race occurs during teardown.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_DECODED, WPARAM(0), LPARAM(0)) };
        }
    });

    let store_cfg = StoreConfig::default();
    let preload_radius = store_cfg.preload_radius;
    let mut store = ImageStore::new(store_cfg, notify);

    // 3. Kick off decode of the initial file IMMEDIATELY in parallel with window creation!
    let mut immediate_image: Option<Arc<DecodedImage>> = None;
    let mut playlist = None;
    let mut focused_path = None;
    let mut pending_folder_scan = None;

    if let Some(ref file) = cfg.initial_file {
        immediate_image = store.focus(file, &[], monitor_size);
        playlist = Some(Playlist::single(file.clone()));
        focused_path = Some(file.clone());
        pending_folder_scan = Some(file.clone());
    }

    // 4. Register Win32 window class and create the centered 80% window.
    // SAFETY: `GetModuleHandleW(None)` retrieves the current exe module handle.
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map(HINSTANCE::from)
        .unwrap_or_default();

    let palette = system_palette();

    // SAFETY: Creating a solid GDI brush with a valid COLORREF matching the ka-suite canvas bg.
    let bg_brush: HBRUSH = unsafe { CreateSolidBrush(COLORREF(palette.bg.to_colorref())) };
    // SAFETY: Loading standard system arrow cursor.
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        hCursor: cursor,
        hbrBackground: bg_brush,
        lpszClassName: WINDOW_CLASS,
        ..Default::default()
    };

    // SAFETY: `wc` is properly initialized with valid static class name and WndProc.
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        // SAFETY: Free brush on failure.
        let _ = unsafe { DeleteObject(bg_brush.into()) };
        return 1;
    }

    let (win_x, win_y, win_w, win_h) = centered_80_percent_rect(work_area);

    let mut state = Box::new(AppState {
        hwnd_slot: Arc::clone(&hwnd_slot),
        store,
        preload_radius,
        monitor_size,
        client_size: (win_w.max(1) as u32, win_h.max(1) as u32),
        dpi: 96,
        palette,
        renderer: None,
        playlist,
        focused_path,
        current_image: None,
        view: View::fit((1, 1), (win_w.max(1) as u32, win_h.max(1) as u32)),
        full_res_requested: false,
        frame_index: 0,
        loops_completed: 0,
        error: cfg.initial_error,
        loading_label: None,
        toast: None,
        show_help: false,
        dragging: false,
        last_drag_pos: (0, 0),
        fullscreen: None,
        first_real_present_done: false,
        pending_folder_scan,
        scan_generation: 0,
        scan_result: Arc::new(Mutex::new(None)),
        bench: cfg.bench,
        exit_code: 0,
    });

    let state_ptr: *mut AppState = &mut *state;

    // SAFETY: Class is registered; `state_ptr` lives until after the message loop exits.
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_ACCEPTFILES,
            WINDOW_CLASS,
            w!("Kalux"),
            WS_OVERLAPPEDWINDOW,
            win_x,
            win_y,
            win_w,
            win_h,
            None,
            None,
            Some(hinstance),
            Some(state_ptr.cast::<c_void>()),
        )
    } {
        Ok(h) => h,
        Err(_) => {
            // SAFETY: Clean up class and brush on window creation failure.
            let _ = unsafe { UnregisterClassW(WINDOW_CLASS, Some(hinstance)) };
            let _ = unsafe { DeleteObject(bg_brush.into()) };
            return 1;
        }
    };

    // Publish HWND to background workers now that the window exists.
    hwnd_slot.store(hwnd.0 as isize, Ordering::Release);

    // Apply native Windows 11 title bar theme + Mica backdrop (gracefully ignored on Windows 10).
    apply_chrome(hwnd, state.palette.is_dark);

    // SAFETY: `hwnd` is valid.
    unsafe { DragAcceptFiles(hwnd, true) };

    // SAFETY: `hwnd` is valid.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    state.dpi = if dpi == 0 { 96 } else { dpi };

    let mut cr = RECT::default();
    // SAFETY: `hwnd` is valid.
    if unsafe { GetClientRect(hwnd, &mut cr) }.is_ok() {
        state.client_size = (
            (cr.right - cr.left).max(1) as u32,
            (cr.bottom - cr.top).max(1) as u32,
        );
    }

    // Create Direct2D factory + HwndRenderTarget.
    match Renderer::new(hwnd, state.client_size, state.palette) {
        Ok(renderer) => state.renderer = Some(renderer),
        Err(e) => {
            if state.bench.is_some() {
                write_stderr(&format!("kalux: failed to create Direct2D renderer: {e}"));
                state.exit_code = 1;
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
        }
    }

    if let Some(bench) = state.bench.as_mut() {
        bench.record_window_ready();
        // Safety timeout so CI never hangs if decoding stalls unexpectedly.
        // SAFETY: `hwnd` is valid.
        unsafe {
            SetTimer(Some(hwnd), TIMER_BENCH_TIMEOUT, BENCH_TIMEOUT_MS, None);
        }
    }

    if let Some(img) = immediate_image {
        if let Some(bench) = state.bench.as_mut() {
            bench.record_decode(Duration::ZERO);
        }
        state.set_decoded_image(hwnd, img, /*reset_view=*/ true);
    } else {
        state.update_title(hwnd);
    }

    // SAFETY: `hwnd` is valid.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    // Drain any decode results that finished while the window & D2D target were being created.
    state.drain_store_events(hwnd);

    // Trigger initial paint synchronously if invalidated.
    // SAFETY: `hwnd` is valid.
    unsafe {
        let _ = UpdateWindow(hwnd);
    }

    // Event-driven Win32 message loop: 0% idle CPU.
    let mut msg = MSG::default();
    loop {
        // SAFETY: Standard Win32 message pump.
        let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        // SAFETY: Standard Win32 message translation and dispatch.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let exit_code = state.exit_code;

    // Ensure `AppState` (including `ImageStore` workers and D2D resources) drops before cleanup.
    drop(state);

    // SAFETY: Window is destroyed; unregister class and free background brush.
    let _ = unsafe { UnregisterClassW(WINDOW_CLASS, Some(hinstance)) };
    // SAFETY: Brush is no longer referenced by any registered class.
    let _ = unsafe { DeleteObject(bg_brush.into()) };

    exit_code
}

impl AppState {
    fn drain_store_events(&mut self, hwnd: HWND) {
        let events = self.store.poll();
        if events.is_empty() {
            return;
        }

        let mut need_repaint = false;
        for event in events {
            match event {
                StoreEvent::Ready {
                    path,
                    image,
                    full_resolution,
                    elapsed,
                } => {
                    if self.focused_path.as_ref() != Some(&path) {
                        continue;
                    }
                    if let Some(bench) = self.bench.as_mut() {
                        bench.record_decode(elapsed);
                    }
                    let reset_view = !full_resolution || self.current_image.is_none();
                    self.set_decoded_image(hwnd, image, reset_view);
                    need_repaint = true;
                }
                StoreEvent::Failed { path, error } => {
                    if self.focused_path.as_ref() != Some(&path) {
                        continue;
                    }
                    if matches!(error, DecodeError::Cancelled) {
                        continue;
                    }
                    if let Some(bench) = self.bench.as_ref()
                        && !bench.has_presented()
                    {
                        write_stderr(&format!(
                            "kalux: failed to decode '{}': {error}",
                            path.display()
                        ));
                        self.exit_code = 1;
                        // SAFETY: `hwnd` is valid.
                        let _ = unsafe { DestroyWindow(hwnd) };
                        return;
                    }
                    self.show_decode_error(hwnd, &path, &error);
                    need_repaint = true;
                }
            }
        }

        if need_repaint {
            invalidate_and_paint_if_ready(hwnd);
        }
    }

    fn handle_playlist_ready(&mut self, hwnd: HWND) {
        let maybe_scanned = self
            .scan_result
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some((scan_gen, scanned)) = maybe_scanned else {
            return;
        };
        if scan_gen != self.scan_generation {
            return;
        }

        let neighbors = scanned.neighbors(self.preload_radius);
        let focus_target = self
            .focused_path
            .clone()
            .unwrap_or_else(|| scanned.current().to_path_buf());
        self.playlist = Some(scanned);
        self.update_title(hwnd);

        // Preload N±radius neighbors without disturbing the already-displayed first frame.
        let _ = self
            .store
            .focus(&focus_target, &neighbors, self.monitor_size);
    }

    fn set_decoded_image(&mut self, hwnd: HWND, image: Arc<DecodedImage>, reset_view: bool) {
        let source = (image.source_width.max(1), image.source_height.max(1));
        if reset_view {
            self.view = View::fit(source, self.client_size);
            self.full_res_requested = !image.is_downscaled();
        } else {
            self.view.relayout(source, self.client_size);
            if !image.is_downscaled() {
                self.full_res_requested = true;
            }
        }

        self.loading_label = None;
        self.error = None;
        self.frame_index = 0;
        self.loops_completed = 0;

        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_image(Arc::clone(&image));
        }

        self.current_image = Some(image);
        self.sync_animation_timer(hwnd);
        self.update_title(hwnd);
        self.maybe_request_full_res();
    }

    fn show_decode_error(&mut self, hwnd: HWND, path: &Path, error: &DecodeError) {
        self.kill_anim_timer(hwnd);
        self.loading_label = None;
        self.current_image = None;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.clear_image();
        }

        let (message, hint) = match error {
            DecodeError::Unsupported { hint } => {
                let msg = format_decode_error_without_duplicate_hint(error, hint.as_deref());
                (msg, hint.clone())
            }
            _ => (error.to_string(), None),
        };

        self.error = Some(ErrorCard {
            title: "Couldn't open image".to_string(),
            subject: display_file_name(path),
            message,
            hint,
        });
        self.update_title(hwnd);
    }

    fn focus_path(&mut self, hwnd: HWND, path: PathBuf, neighbors: &[PathBuf]) {
        self.focused_path = Some(path.clone());
        self.full_res_requested = false;
        self.error = None;
        self.kill_anim_timer(hwnd);

        if let Some(cached) = self.store.focus(&path, neighbors, self.monitor_size) {
            self.set_decoded_image(hwnd, cached, /*reset_view=*/ true);
            // Present cache hit synchronously in the same message (<= 16 ms target).
            invalidate_and_paint_if_ready(hwnd);
        } else {
            // Keep previous image visible (dimmed) with a loading pill — never a blank flash.
            // If there is no previous image (initial cold launch), stay dark without triggering
            // DirectWrite initialization on the startup critical path.
            if self.current_image.is_some() {
                self.loading_label = Some(format!("Loading {}…", display_file_name(&path)));
            }
            self.update_title(hwnd);
            invalidate(hwnd);
        }
    }

    fn open_dropped_file(&mut self, hwnd: HWND, path: PathBuf) {
        self.playlist = Some(Playlist::single(path.clone()));
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.focus_path(hwnd, path.clone(), &[]);

        if self.first_real_present_done {
            self.spawn_folder_scan(path);
        } else {
            self.pending_folder_scan = Some(path);
        }
    }

    fn spawn_folder_scan(&mut self, path: PathBuf) {
        let scan_gen = self.scan_generation;
        let hwnd_slot = Arc::clone(&self.hwnd_slot);
        let result_slot = Arc::clone(&self.scan_result);

        let _ = std::thread::Builder::new()
            .name("kalux-scan".to_string())
            .spawn(move || {
                let Ok(playlist) = Playlist::scan_folder_of(&path) else {
                    return;
                };
                if let Ok(mut guard) = result_slot.lock() {
                    *guard = Some((scan_gen, playlist));
                }
                let raw = hwnd_slot.load(Ordering::Acquire);
                if raw != 0 {
                    let hwnd = HWND(raw as *mut c_void);
                    // SAFETY: `raw` is cleared in `WM_DESTROY`; `PostMessageW` is thread-safe.
                    let _ =
                        unsafe { PostMessageW(Some(hwnd), WM_APP_PLAYLIST, WPARAM(0), LPARAM(0)) };
                }
            });
    }

    fn navigate(&mut self, hwnd: HWND, step: NavTarget) {
        let Some(playlist) = self.playlist.as_mut() else {
            return;
        };
        if playlist.len() <= 1 {
            return;
        }
        let next_path = match step {
            NavTarget::Delta(d) => playlist.step(d).to_path_buf(),
            NavTarget::First => playlist.go_first().to_path_buf(),
            NavTarget::Last => playlist.go_last().to_path_buf(),
        };
        let neighbors = playlist.neighbors(self.preload_radius);
        self.focus_path(hwnd, next_path, &neighbors);
    }

    fn handle_action(&mut self, hwnd: HWND, action: Action) {
        match action {
            Action::Next => self.navigate(hwnd, NavTarget::Delta(1)),
            Action::Prev => self.navigate(hwnd, NavTarget::Delta(-1)),
            Action::First => self.navigate(hwnd, NavTarget::First),
            Action::Last => self.navigate(hwnd, NavTarget::Last),
            Action::ZoomIn => self.zoom_centered(hwnd, KEY_ZOOM_FACTOR),
            Action::ZoomOut => self.zoom_centered(hwnd, 1.0 / KEY_ZOOM_FACTOR),
            Action::ZoomFit => {
                if let Some(source) = self.current_source_size() {
                    self.view = View::fit(source, self.client_size);
                    let pct = (self.view.scale * 100.0).round() as u32;
                    self.show_toast(hwnd, format!("Fit ({pct}%)"));
                    invalidate(hwnd);
                }
            }
            Action::ZoomActual => {
                if let Some(source) = self.current_source_size() {
                    self.view.actual_size(None, source, self.client_size);
                    self.maybe_request_full_res();
                    self.show_toast(hwnd, "100%".to_string());
                    invalidate(hwnd);
                }
            }
            Action::ToggleFullscreen => self.toggle_fullscreen(hwnd),
            Action::Escape => {
                if self.show_help {
                    self.show_help = false;
                    invalidate(hwnd);
                } else if self.fullscreen.is_some() {
                    self.toggle_fullscreen(hwnd);
                } else {
                    // SAFETY: `hwnd` is valid.
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
            }
            Action::Close => {
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            Action::ShowHelp => {
                self.show_help = !self.show_help;
                invalidate(hwnd);
            }
            Action::RotateCw | Action::RotateCcw => {
                self.show_toast(hwnd, "Rotate — coming in v0.2".to_string());
            }
            Action::ToggleInfo => {
                self.show_toast(hwnd, "Info panel — coming in v0.2".to_string());
            }
            Action::Delete => {
                self.show_toast(hwnd, "Recycle Bin delete — coming in v0.2".to_string());
            }
            Action::CopyImage => {
                self.show_toast(hwnd, "Copy image — coming in v0.2".to_string());
            }
            Action::CopyPath => {
                self.show_toast(hwnd, "Copy path — coming in v0.2".to_string());
            }
            Action::EditInKapix => {
                self.show_toast(hwnd, "Edit in kapix — coming in v0.2".to_string());
            }
        }
    }

    fn zoom_centered(&mut self, hwnd: HWND, factor: f32) {
        let Some(source) = self.current_source_size() else {
            return;
        };
        let center = (
            self.client_size.0 as f32 * 0.5,
            self.client_size.1 as f32 * 0.5,
        );
        self.view.zoom_at(factor, center, source, self.client_size);
        self.maybe_request_full_res();
        let pct = (self.view.scale * 100.0).round() as u32;
        self.show_toast(hwnd, format!("{pct}%"));
        invalidate(hwnd);
    }

    fn handle_mouse_wheel(&mut self, hwnd: HWND, wp: WPARAM, lp: LPARAM) {
        let Some(source) = self.current_source_size() else {
            return;
        };
        let delta = input::get_wheel_delta(wp);
        if delta == 0 {
            return;
        }
        let cursor = input::wheel_cursor_client(hwnd, lp);
        let factor = input::wheel_zoom_factor(delta);
        self.view.zoom_at(factor, cursor, source, self.client_size);
        self.maybe_request_full_res();
        let pct = (self.view.scale * 100.0).round() as u32;
        self.show_toast(hwnd, format!("{pct}%"));
        invalidate(hwnd);
    }

    fn handle_lbutton_down(&mut self, hwnd: HWND, lp: LPARAM) {
        if self.current_image.is_none() {
            return;
        }
        self.dragging = true;
        self.last_drag_pos = (input::get_x_lparam(lp), input::get_y_lparam(lp));
        // SAFETY: `hwnd` is valid; paired with `ReleaseCapture` on button up / capture changed.
        unsafe { SetCapture(hwnd) };
    }

    fn handle_mouse_move(&mut self, hwnd: HWND, lp: LPARAM) {
        if !self.dragging {
            return;
        }
        let Some(source) = self.current_source_size() else {
            return;
        };
        let x = input::get_x_lparam(lp);
        let y = input::get_y_lparam(lp);
        let dx = (x - self.last_drag_pos.0) as f32;
        let dy = (y - self.last_drag_pos.1) as f32;
        self.last_drag_pos = (x, y);

        if dx != 0.0 || dy != 0.0 {
            self.view.pan_by(dx, dy, source, self.client_size);
            invalidate(hwnd);
        }
    }

    fn handle_lbutton_up(&mut self) {
        if self.dragging {
            self.dragging = false;
            // SAFETY: Releases capture previously set by `SetCapture`.
            let _ = unsafe { ReleaseCapture() };
        }
    }

    fn toggle_fullscreen(&mut self, hwnd: HWND) {
        if let Some(saved) = self.fullscreen.take() {
            // SAFETY: Restoring previously captured window style and placement.
            unsafe {
                SetWindowLongW(hwnd, GWL_STYLE, saved.style);
                let _ = SetWindowPlacement(hwnd, &saved.placement);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                );
            }
        } else {
            // SAFETY: Querying current style and placement of a valid window.
            let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
            let mut placement = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            // SAFETY: `placement.length` is properly initialized.
            let _ = unsafe { GetWindowPlacement(hwnd, &mut placement) };

            let mon_rect = monitor_rect_for_window(hwnd);
            let borderless = (style & !(WS_OVERLAPPEDWINDOW.0 as i32)) | (WS_POPUP.0 as i32);

            self.fullscreen = Some(FullscreenSaved { style, placement });

            // SAFETY: Switching window to borderless popup covering its monitor.
            unsafe {
                SetWindowLongW(hwnd, GWL_STYLE, borderless);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOP),
                    mon_rect.left,
                    mon_rect.top,
                    mon_rect.right - mon_rect.left,
                    mon_rect.bottom - mon_rect.top,
                    SWP_NOOWNERZORDER | SWP_FRAMECHANGED,
                );
            }

            let new_mon = (
                (mon_rect.right - mon_rect.left).max(1) as u32,
                (mon_rect.bottom - mon_rect.top).max(1) as u32,
            );
            self.update_monitor_target(new_mon);
        }
    }

    fn handle_size(&mut self, hwnd: HWND, wp: WPARAM, lp: LPARAM) {
        if (wp.0 as u32) == SIZE_MINIMIZED {
            return;
        }
        let w = (lp.0 & 0xFFFF) as u32;
        let h = ((lp.0 >> 16) & 0xFFFF) as u32;
        if w == 0 || h == 0 {
            return;
        }
        self.client_size = (w, h);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(w, h);
        }
        if let Some(source) = self.current_source_size() {
            self.view.relayout(source, self.client_size);
            self.maybe_request_full_res();
        }
        invalidate(hwnd);
    }

    fn handle_dpi_changed(&mut self, hwnd: HWND, wp: WPARAM, lp: LPARAM) {
        let new_dpi = (wp.0 & 0xFFFF) as u32;
        if new_dpi > 0 {
            self.dpi = new_dpi;
        }

        if lp.0 != 0 {
            // SAFETY: `WM_DPICHANGED` passes a pointer to the suggested `RECT` in `lParam`.
            let suggested = unsafe { *(lp.0 as *const RECT) };
            // SAFETY: Applying Windows-suggested DPI rect.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
        }

        let mon_rect = monitor_rect_for_window(hwnd);
        let new_mon = (
            (mon_rect.right - mon_rect.left).max(1) as u32,
            (mon_rect.bottom - mon_rect.top).max(1) as u32,
        );
        self.update_monitor_target(new_mon);
        invalidate(hwnd);
    }

    fn update_monitor_target(&mut self, new_mon: (u32, u32)) {
        if new_mon != self.monitor_size {
            self.monitor_size = new_mon;
            self.store.set_target(new_mon);
        }
    }

    fn handle_timer(&mut self, hwnd: HWND, timer_id: usize) {
        match timer_id {
            TIMER_ANIM => self.advance_animation_frame(hwnd),
            TIMER_TOAST => {
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_TOAST) };
                if self.toast.take().is_some() {
                    invalidate(hwnd);
                }
            }
            TIMER_BENCH_SAMPLE => {
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_BENCH_SAMPLE) };
                if let Some(bench) = self.bench.as_ref()
                    && let Err(e) = bench.finish_and_emit()
                {
                    write_stderr(&format!("kalux: failed to emit bench JSON: {e}"));
                    self.exit_code = 1;
                }
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            TIMER_BENCH_TIMEOUT => {
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_BENCH_TIMEOUT) };
                write_stderr("kalux: --bench-startup timed out waiting for first frame");
                self.exit_code = 1;
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            _ => {}
        }
    }

    fn sync_animation_timer(&mut self, hwnd: HWND) {
        self.kill_anim_timer(hwnd);
        let Some(img) = self.current_image.as_ref() else {
            return;
        };
        if !img.is_animated() {
            return;
        }
        let delay = frame_delay_ms(img, self.frame_index);
        // SAFETY: `hwnd` is valid.
        unsafe {
            SetTimer(Some(hwnd), TIMER_ANIM, delay, None);
        }
    }

    fn kill_anim_timer(&self, hwnd: HWND) {
        // SAFETY: `hwnd` is valid; killing a non-existent timer is a harmless no-op.
        let _ = unsafe { KillTimer(Some(hwnd), TIMER_ANIM) };
    }

    fn advance_animation_frame(&mut self, hwnd: HWND) {
        let Some(img) = self.current_image.as_ref() else {
            self.kill_anim_timer(hwnd);
            return;
        };
        if !img.is_animated() {
            self.kill_anim_timer(hwnd);
            return;
        }

        let next = self.frame_index + 1;
        if next >= img.frames.len() {
            self.loops_completed = self.loops_completed.saturating_add(1);
            if img.loop_count != 0 && self.loops_completed >= img.loop_count {
                self.kill_anim_timer(hwnd);
                return;
            }
            self.frame_index = 0;
        } else {
            self.frame_index = next;
        }

        let delay = frame_delay_ms(img, self.frame_index);
        // SAFETY: `hwnd` is valid; re-arms timer with this frame's delay.
        unsafe {
            SetTimer(Some(hwnd), TIMER_ANIM, delay, None);
        }
        invalidate(hwnd);
    }

    fn show_toast(&mut self, hwnd: HWND, message: String) {
        self.toast = Some(message);
        // SAFETY: `hwnd` is valid; resets one-shot toast timer.
        unsafe {
            SetTimer(Some(hwnd), TIMER_TOAST, TOAST_DURATION_MS, None);
        }
        invalidate(hwnd);
    }

    fn maybe_request_full_res(&mut self) {
        if self.full_res_requested {
            return;
        }
        let (Some(img), Some(path)) = (self.current_image.as_ref(), self.focused_path.as_ref())
        else {
            return;
        };
        if !img.is_downscaled() {
            self.full_res_requested = true;
            return;
        }
        let source = (img.source_width.max(1), img.source_height.max(1));
        let decoded = (img.width.max(1), img.height.max(1));
        if self.view.needs_full_resolution(source, decoded) {
            self.full_res_requested = true;
            self.store.request_full_resolution(path);
        }
    }

    fn current_source_size(&self) -> Option<(u32, u32)> {
        self.current_image
            .as_ref()
            .map(|img| (img.source_width.max(1), img.source_height.max(1)))
    }

    fn paint(&mut self, hwnd: HWND) {
        let dest = self
            .current_source_size()
            .map(|src| self.view.dest_rect(src, self.client_size));

        let empty_hint =
            self.current_image.is_none() && self.focused_path.is_none() && self.error.is_none();

        let overlay_view = OverlayView {
            empty_hint,
            error: self.error.as_ref(),
            loading_label: self.loading_label.as_deref(),
            toast: self.toast.as_deref(),
            show_help: self.show_help,
        };

        let presented_image = if let Some(renderer) = self.renderer.as_mut() {
            renderer.render(
                dest,
                self.view.scale,
                self.frame_index,
                self.dpi,
                &overlay_view,
            )
        } else {
            false
        };

        // Trigger post-first-present work strictly AFTER the real image has been presented!
        if presented_image
            && self.current_image.is_some()
            && self.loading_label.is_none()
            && !self.first_real_present_done
        {
            self.first_real_present_done = true;

            if let Some(bench) = self.bench.as_mut()
                && bench.record_first_present()
            {
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { KillTimer(Some(hwnd), TIMER_BENCH_TIMEOUT) };
                // SAFETY: `hwnd` is valid; keep pumping messages ~1 s before sampling memory.
                unsafe {
                    SetTimer(Some(hwnd), TIMER_BENCH_SAMPLE, BENCH_SAMPLE_DELAY_MS, None);
                }
            }

            if let Some(scan_path) = self.pending_folder_scan.take() {
                self.spawn_folder_scan(scan_path);
            }
        }
    }

    fn update_title(&self, hwnd: HWND) {
        let title = match (&self.playlist, &self.current_image) {
            (Some(pl), Some(img)) if self.loading_label.is_none() => {
                let name = display_file_name(pl.current());
                let idx = pl.index() + 1;
                let total = pl.len();
                format!(
                    "{name} \u{2014} {idx} / {total} \u{2014} {}\u{00D7}{} \u{2014} Kalux",
                    img.source_width, img.source_height
                )
            }
            (Some(pl), _) => {
                let name = display_file_name(pl.current());
                let idx = pl.index() + 1;
                let total = pl.len();
                format!("{name} \u{2014} {idx} / {total} \u{2014} Kalux")
            }
            (None, _) => "Kalux".to_string(),
        };

        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `hwnd` is valid and `wide` is NUL-terminated UTF-16.
        let _ = unsafe { SetWindowTextW(hwnd, PCWSTR(wide.as_ptr())) };
    }
}

enum NavTarget {
    Delta(isize),
    First,
    Last,
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: `lparam` points to `CREATESTRUCTW` during `WM_NCCREATE`.
        let cs = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        // SAFETY: Storing the `AppState` pointer in `GWLP_USERDATA`.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize) };
        // SAFETY: Forward `WM_NCCREATE` to default proc so window creation succeeds.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    // SAFETY: Retrieve `AppState` pointer stored in `WM_NCCREATE`.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;
    if ptr.is_null() {
        // SAFETY: Default window handling before `WM_NCCREATE` or after `WM_NCDESTROY`.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    if msg == WM_NCDESTROY {
        // SAFETY: Clear pointer so no late messages ever dereference `AppState`.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
        // SAFETY: Forward to default proc.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    // SAFETY: `ptr` points to the live `Box<AppState>` owned by `run()` on the UI thread.
    let state = unsafe { &mut *ptr };

    match msg {
        WM_ERASEBKGND => LRESULT(1),

        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: Valid HWND inside `WM_PAINT`.
            let _hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            state.paint(hwnd);
            // SAFETY: Pairs with `BeginPaint` above.
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }

        WM_APP_DECODED => {
            state.drain_store_events(hwnd);
            LRESULT(0)
        }

        WM_APP_PLAYLIST => {
            state.handle_playlist_ready(hwnd);
            LRESULT(0)
        }

        WM_SIZE => {
            state.handle_size(hwnd, wparam, lparam);
            LRESULT(0)
        }

        WM_DPICHANGED => {
            state.handle_dpi_changed(hwnd, wparam, lparam);
            LRESULT(0)
        }

        WM_GETMINMAXINFO => {
            if lparam.0 != 0 {
                // SAFETY: `lparam` points to `MINMAXINFO` for `WM_GETMINMAXINFO`.
                let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                let scale = (state.dpi.max(96) as f32) / 96.0;
                mmi.ptMinTrackSize.x = (320.0 * scale) as i32;
                mmi.ptMinTrackSize.y = (240.0 * scale) as i32;
            }
            LRESULT(0)
        }

        WM_KEYDOWN => {
            if let Some(action) = input::action_from_keydown(wparam) {
                state.handle_action(hwnd, action);
                return LRESULT(0);
            }
            // SAFETY: Unhandled keys go to `DefWindowProcW`.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_MOUSEWHEEL => {
            state.handle_mouse_wheel(hwnd, wparam, lparam);
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            state.handle_lbutton_down(hwnd, lparam);
            LRESULT(0)
        }

        WM_MOUSEMOVE => {
            state.handle_mouse_move(hwnd, lparam);
            LRESULT(0)
        }

        WM_LBUTTONUP => {
            state.handle_lbutton_up();
            LRESULT(0)
        }

        WM_CAPTURECHANGED => {
            state.dragging = false;
            LRESULT(0)
        }

        WM_LBUTTONDBLCLK => {
            state.toggle_fullscreen(hwnd);
            LRESULT(0)
        }

        WM_XBUTTONDOWN => {
            if let Some(action) = input::action_from_xbutton(wparam) {
                state.handle_action(hwnd, action);
                // Win32 requires returning TRUE (1) when handling `WM_XBUTTONDOWN`.
                return LRESULT(1);
            }
            // SAFETY: Unhandled X-buttons go to `DefWindowProcW`.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_DROPFILES => {
            if let Some(path) = input::extract_dropped_file(wparam) {
                state.open_dropped_file(hwnd, path);
            }
            LRESULT(0)
        }

        WM_TIMER => {
            state.handle_timer(hwnd, wparam.0);
            LRESULT(0)
        }

        WM_SETTINGCHANGE => {
            let next = system_palette();
            if next.is_dark != state.palette.is_dark {
                state.palette = next;
                apply_chrome(hwnd, next.is_dark);
                if let Some(renderer) = state.renderer.as_mut() {
                    renderer.set_palette(next);
                }
                invalidate(hwnd);
            }
            // SAFETY: Let Windows finish default setting-change handling.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_DESTROY => {
            state.hwnd_slot.store(0, Ordering::Release);
            // SAFETY: Kill all timers and post quit message with `exit_code`.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_ANIM);
                let _ = KillTimer(Some(hwnd), TIMER_TOAST);
                let _ = KillTimer(Some(hwnd), TIMER_BENCH_SAMPLE);
                let _ = KillTimer(Some(hwnd), TIMER_BENCH_TIMEOUT);
                PostQuitMessage(state.exit_code);
            }
            LRESULT(0)
        }

        _ => {
            // SAFETY: Standard Win32 default message processing.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn system_palette() -> Palette {
    let mut val: u32 = 0;
    let mut len: u32 = size_of::<u32>() as u32;
    // SAFETY: Reading a single REG_DWORD from HKCU Personalize; stack pointers are valid.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut val as *mut u32).cast::<c_void>()),
            Some(&mut len),
        )
    };
    if status.is_ok() {
        Palette::for_light(val != 0)
    } else {
        Palette::DARK
    }
}

fn apply_chrome(hwnd: HWND, is_dark: bool) {
    let dark: i32 = i32::from(is_dark);
    // SAFETY: Passing a valid 4-byte `BOOL` for `DWMWA_USE_IMMERSIVE_DARK_MODE`.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const i32).cast::<c_void>(),
            size_of::<i32>() as u32,
        )
    };

    let backdrop: DWM_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW;
    // SAFETY: Passing a valid 4-byte `DWM_SYSTEMBACKDROP_TYPE` (ignored on Windows 10).
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&backdrop as *const DWM_SYSTEMBACKDROP_TYPE).cast::<c_void>(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
    };
}

fn monitor_under_cursor() -> ((u32, u32), RECT) {
    let mut pt = POINT::default();
    // SAFETY: `GetCursorPos` writes into a valid stack `POINT`.
    let _ = unsafe { GetCursorPos(&mut pt) };
    // SAFETY: `MonitorFromPoint` always returns a valid monitor handle with `MONITOR_DEFAULTTONEAREST`.
    let hmon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };

    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `mi.cbSize` is initialized.
    if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        let mw = (mi.rcMonitor.right - mi.rcMonitor.left).max(1) as u32;
        let mh = (mi.rcMonitor.bottom - mi.rcMonitor.top).max(1) as u32;
        ((mw, mh), mi.rcWork)
    } else {
        (
            (1920, 1080),
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            },
        )
    }
}

fn monitor_rect_for_window(hwnd: HWND) -> RECT {
    // SAFETY: `hwnd` is valid.
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `mi.cbSize` is initialized.
    if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        mi.rcMonitor
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

fn centered_80_percent_rect(work: RECT) -> (i32, i32, i32, i32) {
    let work_w = (work.right - work.left).max(640);
    let work_h = (work.bottom - work.top).max(480);
    let w = ((work_w as f32) * 0.80).round() as i32;
    let h = ((work_h as f32) * 0.80).round() as i32;
    let x = work.left + (work_w - w) / 2;
    let y = work.top + (work_h - h) / 2;
    (x, y, w, h)
}

fn invalidate(hwnd: HWND) {
    // SAFETY: `hwnd` is valid.
    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
}

fn invalidate_and_paint_if_ready(hwnd: HWND) {
    invalidate(hwnd);
    // SAFETY: `hwnd` is valid; forces immediate synchronous `WM_PAINT` for low-latency present.
    let _ = unsafe { UpdateWindow(hwnd) };
}

fn frame_delay_ms(img: &DecodedImage, frame_idx: usize) -> u32 {
    let raw = img.frames.get(frame_idx).map_or(100, |f| f.delay_ms);
    if raw == 0 { 100 } else { raw.max(10) }
}

fn display_file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn format_decode_error_without_duplicate_hint(error: &DecodeError, hint: Option<&str>) -> String {
    let full = error.to_string();
    if let Some(h) = hint
        && !h.is_empty()
        && full.contains(h)
    {
        return "Unsupported image format or missing codec.".to_string();
    }
    full
}

fn write_stderr(msg: &str) {
    attach_console_if_needed();
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{msg}");
    let _ = stderr.flush();
}
