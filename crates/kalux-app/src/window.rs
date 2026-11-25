//! Raw Win32 window, message loop, critical startup ordering, zoom/pan, navigation,
//! fullscreen toggle, DPI handling, animation timers, and folder-scan handoff.

use std::ffi::c_void;
use std::io::Write as _;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kalux_core::action::VK_COMPARE;
use kalux_core::edit::{EditDrag, edit_image_dest_rect, hit_test_crop};
use kalux_core::{
    Action, CropHandle, CropRect, DecodedImage, DockButton, DockLayout, EditAction, EditPrompt,
    EditSession, EditTab, ImageFormat, Palette, Playlist, Rect, ToneSlider, View,
};
use kalux_decode::{
    DecodeError, ExportError, ExportOptions, ExportResult, ImageStore, StoreConfig, StoreEvent,
    export_image, render_edited_image, supports_in_place_overwrite,
};
use windows::Win32::Foundation::{
    COLORREF, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFOHEADER, BeginPaint, CreateSolidBrush, DeleteObject, EndPaint,
    GetMonitorInfoW, HBRUSH, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, PAINTSTRUCT, UpdateWindow,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::DragAcceptFiles;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GWL_STYLE, GWLP_USERDATA, GetClientRect, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GetWindowLongW, GetWindowPlacement, HTCLIENT, HWND_TOP, IDC_ARROW, IDC_CROSS, IDC_HAND,
    IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, KillTimer, LoadCursorW,
    LoadIconW, MINMAXINFO, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SIZE_MINIMIZED,
    SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    SWP_NOZORDER, SetCursor, SetTimer, SetWindowLongPtrW, SetWindowLongW, SetWindowPlacement,
    SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, UnregisterClassW, WINDOWPLACEMENT,
    WM_APP, WM_CAPTURECHANGED, WM_DESTROY, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETCURSOR,
    WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WM_XBUTTONDOWN, WNDCLASSEXW, WS_EX_ACCEPTFILES,
    WS_OVERLAPPEDWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::bench::{BenchState, attach_console_if_needed};
use crate::input;
use crate::overlay::{ErrorCard, OverlayView};
use crate::render::{EditRenderView, Renderer};

const WINDOW_CLASS: PCWSTR = w!("KaluxMainWindow");

const WM_APP_DECODED: u32 = WM_APP + 1;
const WM_APP_PLAYLIST: u32 = WM_APP + 2;
const WM_APP_EXPORT_DONE: u32 = WM_APP + 3;
const WM_APP_CLIPBOARD_DONE: u32 = WM_APP + 4;

const CF_DIB: u32 = 8;

const TIMER_ANIM: usize = 1;
const TIMER_TOAST: usize = 2;
const TIMER_BENCH_SAMPLE: usize = 3;
const TIMER_BENCH_TIMEOUT: usize = 4;

const TOAST_DURATION_MS: u32 = 1600;
const DOUBLE_CLICK_MAX_GAP: Duration = Duration::from_millis(400);
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

struct ExportOutcome {
    save_copy: bool,
    result: Result<ExportResult, ExportError>,
}

type SharedScanResult = Arc<Mutex<Option<(u64, Playlist)>>>;
type SharedExportResult = Arc<Mutex<Option<ExportOutcome>>>;
type SharedClipboardResult = Arc<Mutex<Option<Result<DecodedImage, String>>>>;

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

    // Edit Mode
    edit: Option<EditSession>,
    compare_via_dock: bool,
    last_slider_click: Option<(Instant, ToneSlider)>,
    export_result: SharedExportResult,
    clipboard_result: SharedClipboardResult,

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
    // SAFETY: Loading standard system arrow cursor and the embedded app icon (resource ID 1).
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
    let icon = unsafe { LoadIconW(Some(hinstance), PCWSTR(std::ptr::without_provenance(1))) }
        .unwrap_or_default();

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinstance,
        hIcon: icon,
        hIconSm: icon,
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
        edit: None,
        compare_via_dock: false,
        last_slider_click: None,
        export_result: Arc::new(Mutex::new(None)),
        clipboard_result: Arc::new(Mutex::new(None)),
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

        // Preload N±radius neighbors only when not in Edit Mode so RAM stays strictly bounded.
        if self.edit.is_none() {
            let _ = self
                .store
                .focus(&focus_target, &neighbors, self.monitor_size);
        }
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
        if self.edit.is_some() {
            self.exit_edit_mode(hwnd);
        }
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

    fn dpi_scale(&self) -> f32 {
        (self.dpi.max(96) as f32) / 96.0
    }

    fn enter_edit_mode(&mut self, hwnd: HWND, tab: EditTab) {
        let (Some(img), Some(path)) = (self.current_image.as_ref(), self.focused_path.clone())
        else {
            return;
        };
        if let Some(ref mut s) = self.edit {
            s.tab = tab;
            invalidate(hwnd);
            return;
        }
        // Evict neighbor cache entries immediately so Edit Mode stays strictly within the RAM budget.
        self.store.enter_edit_mode(&path);
        self.show_help = false;
        self.dragging = false;
        self.compare_via_dock = false;
        self.kill_anim_timer(hwnd);

        let is_anim = img.is_animated();
        let total_frames = img.frames.len().max(1);
        let frame_idx = self.frame_index.min(total_frames - 1);
        self.edit = Some(EditSession::new(tab, is_anim, frame_idx, total_frames));
        self.update_title(hwnd);
        invalidate(hwnd);
    }

    fn exit_edit_mode(&mut self, hwnd: HWND) {
        self.edit = None;
        self.compare_via_dock = false;
        self.last_slider_click = None;
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.clear_edit_resources();
        }
        self.sync_animation_timer(hwnd);
        self.update_title(hwnd);
        invalidate(hwnd);
    }

    fn edit_geometry(&self, session: &EditSession) -> Option<(DockLayout, Rect, f32, (u32, u32))> {
        let img = self.current_image.as_ref()?;
        let scale = self.dpi_scale();
        let layout = DockLayout::compute(self.client_size.0, self.client_size.1, scale, session);
        let src = (img.source_width.max(1), img.source_height.max(1));
        let oriented = if session.comparing_original {
            src
        } else {
            session.recipe().oriented_dims(src.0, src.1)
        };
        let (image_dest, image_scale) =
            edit_image_dest_rect(oriented, self.client_size, layout.reserved_bottom_px, scale);
        Some((layout, image_dest, image_scale, src))
    }

    fn request_smart_save(&mut self, hwnd: HWND) {
        let Some(ref path) = self.focused_path else {
            return;
        };
        let (is_dirty, is_anim, orient_only, exporting) = match self.edit.as_ref() {
            Some(s) => (
                s.is_dirty(),
                s.is_animated_source,
                s.recipe().is_orientation_only(),
                s.exporting,
            ),
            None => return,
        };
        if exporting {
            return;
        }
        if !is_dirty {
            self.show_toast(hwnd, "No changes to save".to_string());
            return;
        }
        if is_anim || !supports_in_place_overwrite(path, is_anim) {
            self.start_export(hwnd, true);
            return;
        }
        if orient_only && ImageFormat::from_path(path) == Some(ImageFormat::Jpeg) {
            self.start_export(hwnd, false);
            return;
        }
        if let Some(ref mut s) = self.edit {
            s.prompt = EditPrompt::SaveChoice;
            invalidate(hwnd);
        }
    }

    fn start_export(&mut self, hwnd: HWND, save_copy: bool) {
        let Some(path) = self.focused_path.clone() else {
            return;
        };
        let (recipe, quality, frame_index) = match self.edit.as_mut() {
            Some(s) => {
                if s.exporting {
                    return;
                }
                if !s.is_dirty() {
                    self.show_toast(hwnd, "No changes to save".to_string());
                    return;
                }
                s.prompt = EditPrompt::None;
                s.exporting = true;
                (s.recipe(), s.quality.quality(), s.frame_index)
            }
            None => return,
        };

        self.store.trim_for_export();
        self.show_toast(hwnd, "Saving…".to_string());

        let out_slot = Arc::clone(&self.export_result);
        let hwnd_slot = Arc::clone(&self.hwnd_slot);
        let _ = std::thread::Builder::new()
            .name("kalux-export".to_string())
            .spawn(move || {
                let res = export_image(
                    &path,
                    &recipe,
                    ExportOptions {
                        jpeg_quality: quality,
                        save_copy,
                        format_override: None,
                    },
                    frame_index,
                );
                if let Ok(mut guard) = out_slot.lock() {
                    *guard = Some(ExportOutcome {
                        save_copy,
                        result: res,
                    });
                }
                let raw = hwnd_slot.load(Ordering::Acquire);
                if raw != 0 {
                    let target = HWND(raw as *mut c_void);
                    // SAFETY: `raw` is cleared in `WM_DESTROY`; `PostMessageW` is thread-safe.
                    let _ = unsafe {
                        PostMessageW(Some(target), WM_APP_EXPORT_DONE, WPARAM(0), LPARAM(0))
                    };
                }
            });
    }

    fn handle_export_done(&mut self, hwnd: HWND) {
        let outcome = self
            .export_result
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some(outcome) = outcome else {
            return;
        };

        match outcome.result {
            Ok(res) => {
                let saved_path = res.saved_path.clone();
                let fname = display_file_name(&saved_path);
                let msg = if res.lossless {
                    "Saved lossless".to_string()
                } else if res.redirected_to_copy {
                    format!("Saved copy: {fname}")
                } else if outcome.save_copy {
                    format!("Saved {fname}")
                } else {
                    "Saved".to_string()
                };

                self.exit_edit_mode(hwnd);
                let neighbors = if outcome.save_copy || res.redirected_to_copy {
                    if let Some(ref mut pl) = self.playlist {
                        pl.insert_and_select(saved_path.clone());
                        pl.neighbors(self.preload_radius)
                    } else {
                        self.playlist = Some(Playlist::single(saved_path.clone()));
                        Vec::new()
                    }
                } else {
                    self.store.trim_for_export();
                    self.playlist
                        .as_ref()
                        .map(|pl| pl.neighbors(self.preload_radius))
                        .unwrap_or_default()
                };
                self.focus_path(hwnd, saved_path, &neighbors);
                self.show_toast(hwnd, msg);
            }
            Err(err) => {
                if let Some(ref mut s) = self.edit {
                    s.exporting = false;
                }
                self.show_toast(hwnd, format!("Save failed: {err}"));
            }
        }
    }

    fn start_clipboard_copy(&mut self, hwnd: HWND) {
        let Some(path) = self.focused_path.clone() else {
            return;
        };
        let (recipe, frame_index) = match self.edit.as_ref() {
            Some(s) => (s.recipe(), s.frame_index),
            None => (Default::default(), self.frame_index),
        };
        self.show_toast(hwnd, "Copying…".to_string());

        let out_slot = Arc::clone(&self.clipboard_result);
        let hwnd_slot = Arc::clone(&self.hwnd_slot);
        let _ = std::thread::Builder::new()
            .name("kalux-clip".to_string())
            .spawn(move || {
                let res =
                    render_edited_image(&path, &recipe, frame_index).map_err(|e| e.to_string());
                if let Ok(mut guard) = out_slot.lock() {
                    *guard = Some(res);
                }
                let raw = hwnd_slot.load(Ordering::Acquire);
                if raw != 0 {
                    let target = HWND(raw as *mut c_void);
                    // SAFETY: `raw` is cleared in `WM_DESTROY`; `PostMessageW` is thread-safe.
                    let _ = unsafe {
                        PostMessageW(Some(target), WM_APP_CLIPBOARD_DONE, WPARAM(0), LPARAM(0))
                    };
                }
            });
    }

    fn handle_clipboard_done(&mut self, hwnd: HWND) {
        let item = self
            .clipboard_result
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some(res) = item else {
            return;
        };
        match res {
            Ok(img) => match copy_image_to_win32_clipboard(hwnd, &img) {
                Ok(()) => self.show_toast(hwnd, "Copied image".to_string()),
                Err(e) => self.show_toast(hwnd, format!("Copy failed: {e}")),
            },
            Err(e) => self.show_toast(hwnd, format!("Copy failed: {e}")),
        }
    }

    fn launch_kapix(&mut self, hwnd: HWND) {
        let Some(ref path) = self.focused_path else {
            return;
        };
        match std::process::Command::new("kapix").arg(path).spawn() {
            Ok(_) => self.show_toast(hwnd, "Opened in Kapix".to_string()),
            Err(_) => self.show_toast(hwnd, "Kapix not found in PATH".to_string()),
        }
    }

    fn handle_edit_action(&mut self, hwnd: HWND, action: EditAction) {
        let Some(src) = self.current_source_size() else {
            return;
        };
        let prompt = match self.edit.as_ref() {
            Some(s) => s.prompt,
            None => return,
        };

        if prompt != EditPrompt::None {
            match action {
                EditAction::ConfirmPrimary => {
                    self.start_export(hwnd, true);
                    return;
                }
                EditAction::ConfirmOverwrite if prompt == EditPrompt::SaveChoice => {
                    self.start_export(hwnd, false);
                    return;
                }
                EditAction::ConfirmDiscard if prompt == EditPrompt::UnsavedExit => {
                    self.exit_edit_mode(hwnd);
                    return;
                }
                EditAction::ExitOrCancel => {
                    if let Some(ref mut s) = self.edit {
                        s.prompt = EditPrompt::None;
                        invalidate(hwnd);
                    }
                    return;
                }
                EditAction::CloseWindow => {
                    // SAFETY: `hwnd` is valid.
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return;
                }
                _ => return,
            }
        }

        match action {
            EditAction::TabCrop => {
                if let Some(ref mut s) = self.edit {
                    s.tab = EditTab::Crop;
                    invalidate(hwnd);
                }
            }
            EditAction::TabAdjust => {
                if let Some(ref mut s) = self.edit {
                    s.tab = EditTab::Adjust;
                    invalidate(hwnd);
                }
            }
            EditAction::RotateCw => {
                if let Some(ref mut s) = self.edit {
                    s.rotate_cw();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::RotateCcw => {
                if let Some(ref mut s) = self.edit {
                    s.rotate_ccw();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::FlipH => {
                if let Some(ref mut s) = self.edit {
                    s.flip_h();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::FlipV => {
                if let Some(ref mut s) = self.edit {
                    s.flip_v();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::SwapAspect => {
                if let Some(ref mut s) = self.edit {
                    s.swap_aspect(src.0, src.1);
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::ToggleMonochrome => {
                if let Some(ref mut s) = self.edit {
                    s.toggle_monochrome();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::NudgeCrop(dx, dy, resize) => {
                if let Some(ref mut s) = self.edit {
                    s.nudge_crop(dx, dy, resize, src.0, src.1);
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::Undo => {
                if let Some(ref mut s) = self.edit
                    && s.history.undo().is_some()
                {
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::Redo => {
                if let Some(ref mut s) = self.edit
                    && s.history.redo().is_some()
                {
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            EditAction::ResetAll => {
                if let Some(ref mut s) = self.edit {
                    s.reset_all();
                    self.update_title(hwnd);
                    self.show_toast(hwnd, "Reset edits".to_string());
                }
            }
            EditAction::CopyEdited => {
                self.start_clipboard_copy(hwnd);
            }
            EditAction::SmartSave | EditAction::ConfirmPrimary => {
                self.request_smart_save(hwnd);
            }
            EditAction::SaveCopy => {
                self.start_export(hwnd, true);
            }
            EditAction::ConfirmOverwrite | EditAction::ConfirmDiscard => {}
            EditAction::EditInKapix => {
                self.launch_kapix(hwnd);
            }
            EditAction::ExitOrCancel => {
                let is_dirty = self.edit.as_ref().is_some_and(EditSession::is_dirty);
                if is_dirty {
                    if let Some(ref mut s) = self.edit {
                        s.prompt = EditPrompt::UnsavedExit;
                        invalidate(hwnd);
                    }
                } else {
                    self.exit_edit_mode(hwnd);
                }
            }
            EditAction::CloseWindow => {
                // SAFETY: `hwnd` is valid.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
        }
    }

    fn handle_dock_button(
        &mut self,
        hwnd: HWND,
        button: DockButton,
        phys: (f32, f32),
        layout: &DockLayout,
        src: (u32, u32),
    ) {
        let scale = self.dpi_scale();
        match button {
            DockButton::TabCrop => {
                if let Some(ref mut s) = self.edit {
                    s.tab = EditTab::Crop;
                    invalidate(hwnd);
                }
            }
            DockButton::TabAdjust => {
                if let Some(ref mut s) = self.edit {
                    s.tab = EditTab::Adjust;
                    invalidate(hwnd);
                }
            }
            DockButton::Aspect(preset) => {
                if let Some(ref mut s) = self.edit {
                    s.set_aspect(preset, src.0, src.1);
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::SwapAspect => {
                if let Some(ref mut s) = self.edit {
                    s.swap_aspect(src.0, src.1);
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::RotateCcw => {
                if let Some(ref mut s) = self.edit {
                    s.rotate_ccw();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::RotateCw => {
                if let Some(ref mut s) = self.edit {
                    s.rotate_cw();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::FlipH => {
                if let Some(ref mut s) = self.edit {
                    s.flip_h();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::FlipV => {
                if let Some(ref mut s) = self.edit {
                    s.flip_v();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::Slider(slider) => {
                let now = Instant::now();
                let is_double = self.last_slider_click.is_some_and(|(t, prev)| {
                    prev == slider && now.duration_since(t) <= DOUBLE_CLICK_MAX_GAP
                });
                if is_double {
                    self.last_slider_click = None;
                    if let Some(ref mut s) = self.edit {
                        let mut r = s.recipe();
                        r.tone.set(slider, 0.0);
                        s.history.push(r);
                        self.update_title(hwnd);
                        invalidate(hwnd);
                    }
                    return;
                }
                self.last_slider_click = Some((now, slider));
                if let (Some(ref mut s), Some(rect)) = (
                    self.edit.as_mut(),
                    layout.rect_of(DockButton::Slider(slider)),
                ) {
                    let start_recipe = s.recipe();
                    let val = DockLayout::slider_value_at(rect, scale, phys.0);
                    s.drag = Some(EditDrag::Slider {
                        slider,
                        start_recipe,
                    });
                    s.history.current_mut().tone.set(slider, val);
                    // SAFETY: `hwnd` is valid; paired with `ReleaseCapture` on button up.
                    unsafe { SetCapture(hwnd) };
                    invalidate(hwnd);
                }
            }
            DockButton::Monochrome => {
                if let Some(ref mut s) = self.edit {
                    s.toggle_monochrome();
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::Reset => {
                if let Some(ref mut s) = self.edit {
                    s.reset_all();
                    self.update_title(hwnd);
                    self.show_toast(hwnd, "Reset edits".to_string());
                }
            }
            DockButton::Compare => {
                if let Some(ref mut s) = self.edit {
                    s.comparing_original = true;
                    self.compare_via_dock = true;
                    // SAFETY: `hwnd` is valid; paired with `ReleaseCapture` on button up.
                    unsafe { SetCapture(hwnd) };
                    invalidate(hwnd);
                }
            }
            DockButton::Undo => {
                if let Some(ref mut s) = self.edit
                    && s.history.undo().is_some()
                {
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::Redo => {
                if let Some(ref mut s) = self.edit
                    && s.history.redo().is_some()
                {
                    self.update_title(hwnd);
                    invalidate(hwnd);
                }
            }
            DockButton::QualityCycle => {
                if let Some(ref mut s) = self.edit {
                    s.quality = s.quality.cycle();
                    let label = format!("JPEG quality {}", s.quality.quality());
                    self.show_toast(hwnd, label);
                }
            }
            DockButton::CopyEdited => {
                self.start_clipboard_copy(hwnd);
            }
            DockButton::SaveOverwrite => {
                self.request_smart_save(hwnd);
            }
            DockButton::SaveCopy => {
                self.start_export(hwnd, true);
            }
            DockButton::Done => {
                self.handle_edit_action(hwnd, EditAction::ExitOrCancel);
            }
            DockButton::PromptSaveCopy => {
                self.start_export(hwnd, true);
            }
            DockButton::PromptOverwrite => {
                self.start_export(hwnd, false);
            }
            DockButton::PromptDiscard => {
                self.exit_edit_mode(hwnd);
            }
            DockButton::PromptCancel => {
                if let Some(ref mut s) = self.edit {
                    s.prompt = EditPrompt::None;
                    invalidate(hwnd);
                }
            }
        }
    }

    fn handle_edit_mouse_down(&mut self, hwnd: HWND, lp: LPARAM) {
        let phys = (
            input::get_x_lparam(lp) as f32,
            input::get_y_lparam(lp) as f32,
        );
        let scale = self.dpi_scale();

        let Some(session_snap) = self.edit.as_ref() else {
            return;
        };
        let Some((layout, image_dest, _, src)) = self.edit_geometry(session_snap) else {
            return;
        };

        if let Some(btn) = layout.hit_test(phys) {
            self.handle_dock_button(hwnd, btn, phys, &layout, src);
            return;
        }
        if layout.contains_point(phys) {
            return;
        }

        if let Some(ref mut s) = self.edit {
            if s.prompt != EditPrompt::None {
                s.prompt = EditPrompt::None;
                invalidate(hwnd);
                return;
            }

            let crop_screen = s.recipe().crop.to_screen_rect(image_dest);
            if let Some(handle) = hit_test_crop(phys, crop_screen, 10.0 * scale) {
                let start_norm = (
                    (phys.0 - image_dest.x) / image_dest.w.max(1.0),
                    (phys.1 - image_dest.y) / image_dest.h.max(1.0),
                );
                let start_recipe = s.recipe();
                s.drag = Some(EditDrag::CropHandle {
                    handle,
                    start_recipe,
                    start_norm,
                });
                // SAFETY: `hwnd` is valid; paired with `ReleaseCapture` on button up.
                unsafe { SetCapture(hwnd) };
                invalidate(hwnd);
            } else if phys.0 >= image_dest.x
                && phys.0 <= image_dest.x + image_dest.w
                && phys.1 >= image_dest.y
                && phys.1 <= image_dest.y + image_dest.h
            {
                let anchor_norm = (
                    ((phys.0 - image_dest.x) / image_dest.w.max(1.0)).clamp(0.0, 1.0),
                    ((phys.1 - image_dest.y) / image_dest.h.max(1.0)).clamp(0.0, 1.0),
                );
                let start_recipe = s.recipe();
                s.drag = Some(EditDrag::NewCrop {
                    start_recipe,
                    anchor_norm,
                });
                // SAFETY: `hwnd` is valid; paired with `ReleaseCapture` on button up.
                unsafe { SetCapture(hwnd) };
                invalidate(hwnd);
            }
        }
    }

    fn handle_edit_mouse_move(&mut self, hwnd: HWND, lp: LPARAM) {
        let phys = (
            input::get_x_lparam(lp) as f32,
            input::get_y_lparam(lp) as f32,
        );
        let scale = self.dpi_scale();
        let shift_held = input::current_modifiers().shift;

        let Some(session_snap) = self.edit.as_ref() else {
            return;
        };
        let Some((layout, image_dest, _, src)) = self.edit_geometry(session_snap) else {
            return;
        };

        let Some(ref mut session) = self.edit else {
            return;
        };

        if let Some(drag) = session.drag {
            match drag {
                EditDrag::Slider { slider, .. } => {
                    if let Some(rect) = layout.rect_of(DockButton::Slider(slider)) {
                        let val = DockLayout::slider_value_at(rect, scale, phys.0);
                        session.history.current_mut().tone.set(slider, val);
                        invalidate(hwnd);
                    }
                }
                EditDrag::CropHandle {
                    handle,
                    start_recipe,
                    start_norm,
                } => {
                    if image_dest.w > 1.0 && image_dest.h > 1.0 {
                        let cur_u = (phys.0 - image_dest.x) / image_dest.w;
                        let cur_v = (phys.1 - image_dest.y) / image_dest.h;
                        let dx = cur_u - start_norm.0;
                        let dy = cur_v - start_norm.1;
                        let (ow, oh) = start_recipe.oriented_dims(src.0, src.1);
                        let locked = session.effective_aspect_ratio(src.0, src.1, shift_held);
                        let next_crop =
                            start_recipe.crop.apply_drag(handle, dx, dy, locked, ow, oh);
                        session.history.current_mut().crop = next_crop;
                        invalidate(hwnd);
                    }
                }
                EditDrag::NewCrop {
                    start_recipe,
                    anchor_norm,
                } => {
                    if image_dest.w > 1.0 && image_dest.h > 1.0 {
                        let cur_u = ((phys.0 - image_dest.x) / image_dest.w).clamp(0.0, 1.0);
                        let cur_v = ((phys.1 - image_dest.y) / image_dest.h).clamp(0.0, 1.0);
                        let x0 = anchor_norm.0.min(cur_u);
                        let y0 = anchor_norm.1.min(cur_v);
                        let w = (cur_u - anchor_norm.0).abs();
                        let h = (cur_v - anchor_norm.1).abs();
                        let (ow, oh) = start_recipe.oriented_dims(src.0, src.1);
                        let (min_w, min_h) = CropRect::min_norm_for(ow, oh);
                        let mut rect = CropRect { x: x0, y: y0, w, h }.clamped(min_w, min_h);
                        if let Some(ratio) =
                            session.effective_aspect_ratio(src.0, src.1, shift_held)
                        {
                            rect = rect.fit_aspect(ratio, ow, oh);
                        }
                        session.history.current_mut().crop = rect;
                        invalidate(hwnd);
                    }
                }
            }
            return;
        }

        let hovered_btn = layout.hit_test(phys);
        let hovered_handle = if hovered_btn.is_none()
            && !layout.contains_point(phys)
            && session.prompt == EditPrompt::None
        {
            let crop_screen = session.recipe().crop.to_screen_rect(image_dest);
            hit_test_crop(phys, crop_screen, 10.0 * scale)
        } else {
            None
        };

        if hovered_btn != session.hovered_button || hovered_handle != session.hovered_handle {
            session.hovered_button = hovered_btn;
            session.hovered_handle = hovered_handle;
            invalidate(hwnd);
        }
    }

    fn handle_edit_mouse_up(&mut self, hwnd: HWND) {
        let mut had_capture = false;
        if self.compare_via_dock {
            self.compare_via_dock = false;
            had_capture = true;
            if let Some(ref mut s) = self.edit {
                s.comparing_original = false;
            }
        }
        if let Some(ref mut s) = self.edit
            && let Some(drag) = s.drag.take()
        {
            had_capture = true;
            let start_recipe = match drag {
                EditDrag::CropHandle { start_recipe, .. }
                | EditDrag::NewCrop { start_recipe, .. }
                | EditDrag::Slider { start_recipe, .. } => start_recipe,
            };
            let after = s.recipe();
            s.history.commit_from(start_recipe, after);
        }
        if had_capture {
            // SAFETY: Releases capture set during drag/compare press.
            let _ = unsafe { ReleaseCapture() };
            self.update_title(hwnd);
            invalidate(hwnd);
        }
    }

    fn apply_edit_cursor(&self) -> bool {
        let Some(ref s) = self.edit else {
            return false;
        };
        let cursor_id = if let Some(drag) = s.drag {
            match drag {
                EditDrag::Slider { .. } => IDC_HAND,
                EditDrag::NewCrop { .. } => IDC_CROSS,
                EditDrag::CropHandle { handle, .. } => cursor_for_handle(handle),
            }
        } else if s.hovered_button.is_some() {
            IDC_HAND
        } else if let Some(handle) = s.hovered_handle {
            cursor_for_handle(handle)
        } else {
            IDC_ARROW
        };
        // SAFETY: Loading standard system cursor and applying it inside `WM_SETCURSOR`.
        if let Ok(hcur) = unsafe { LoadCursorW(None, cursor_id) } {
            unsafe {
                let _ = SetCursor(Some(hcur));
            }
            return true;
        }
        false
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
            Action::EditMode | Action::CropMode => {
                self.enter_edit_mode(hwnd, EditTab::Crop);
            }
            Action::RotateCw => {
                self.enter_edit_mode(hwnd, EditTab::Crop);
                self.handle_edit_action(hwnd, EditAction::RotateCw);
            }
            Action::RotateCcw => {
                self.enter_edit_mode(hwnd, EditTab::Crop);
                self.handle_edit_action(hwnd, EditAction::RotateCcw);
            }
            Action::CopyImage => {
                self.start_clipboard_copy(hwnd);
            }
            Action::EditInKapix => {
                self.launch_kapix(hwnd);
            }
            Action::ToggleInfo | Action::Delete | Action::CopyPath => {
                self.show_toast(hwnd, "Coming in v0.4".to_string());
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
        if self.edit.is_some() {
            return;
        }
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
        if self.edit.is_some() {
            self.handle_edit_mouse_down(hwnd, lp);
            return;
        }
        if self.current_image.is_none() {
            return;
        }
        self.dragging = true;
        self.last_drag_pos = (input::get_x_lparam(lp), input::get_y_lparam(lp));
        // SAFETY: `hwnd` is valid; paired with `ReleaseCapture` on button up / capture changed.
        unsafe { SetCapture(hwnd) };
    }

    fn handle_mouse_move(&mut self, hwnd: HWND, lp: LPARAM) {
        if self.edit.is_some() {
            self.handle_edit_mouse_move(hwnd, lp);
            return;
        }
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

    fn handle_lbutton_up(&mut self, hwnd: HWND) {
        if self.edit.is_some() {
            self.handle_edit_mouse_up(hwnd);
            return;
        }
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
            show_help: self.show_help && self.edit.is_none(),
        };

        let edit_geo = self.edit.as_ref().and_then(|s| self.edit_geometry(s));

        let presented_image = if let Some(renderer) = self.renderer.as_mut() {
            let edit_view = match (self.edit.as_ref(), edit_geo.as_ref()) {
                (Some(session), Some((layout, image_dest, image_scale, source_dims))) => {
                    Some(EditRenderView {
                        session,
                        layout,
                        image_dest: *image_dest,
                        image_scale: *image_scale,
                        source_dims: *source_dims,
                    })
                }
                _ => None,
            };
            let frame_idx = self
                .edit
                .as_ref()
                .map_or(self.frame_index, |s| s.frame_index);
            renderer.render(
                dest,
                self.view.scale,
                frame_idx,
                self.dpi,
                &overlay_view,
                edit_view.as_ref(),
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
        let edit_suffix = match self.edit.as_ref() {
            Some(s) if s.is_dirty() => " \u{2014} Editing*",
            Some(_) => " \u{2014} Editing",
            None => "",
        };
        let title = match (&self.playlist, &self.current_image) {
            (Some(pl), Some(img)) if self.loading_label.is_none() => {
                let name = display_file_name(pl.current());
                let idx = pl.index() + 1;
                let total = pl.len();
                format!(
                    "{name} \u{2014} {idx} / {total} \u{2014} {}\u{00D7}{}{edit_suffix} \u{2014} Kalux",
                    img.source_width, img.source_height
                )
            }
            (Some(pl), _) => {
                let name = display_file_name(pl.current());
                let idx = pl.index() + 1;
                let total = pl.len();
                format!("{name} \u{2014} {idx} / {total}{edit_suffix} \u{2014} Kalux")
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

        WM_APP_EXPORT_DONE => {
            state.handle_export_done(hwnd);
            LRESULT(0)
        }

        WM_APP_CLIPBOARD_DONE => {
            state.handle_clipboard_done(hwnd);
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
            if state.edit.is_some() {
                if input::vk_from_wparam(wparam) == VK_COMPARE {
                    if let Some(ref mut s) = state.edit
                        && !s.comparing_original
                    {
                        s.comparing_original = true;
                        invalidate(hwnd);
                    }
                    return LRESULT(0);
                }
                if let Some(action) = input::edit_action_from_keydown(wparam) {
                    state.handle_edit_action(hwnd, action);
                    return LRESULT(0);
                }
                return LRESULT(0);
            }
            if let Some(action) = input::action_from_keydown(wparam) {
                state.handle_action(hwnd, action);
                return LRESULT(0);
            }
            // SAFETY: Unhandled keys go to `DefWindowProcW`.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_KEYUP => {
            if state.edit.is_some() && input::vk_from_wparam(wparam) == VK_COMPARE {
                if let Some(ref mut s) = state.edit
                    && s.comparing_original
                    && !state.compare_via_dock
                {
                    s.comparing_original = false;
                    invalidate(hwnd);
                }
                return LRESULT(0);
            }
            // SAFETY: Unhandled key-up goes to `DefWindowProcW`.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        WM_SETCURSOR => {
            let hit_area = (lparam.0 & 0xFFFF) as u32;
            if hit_area == HTCLIENT && state.apply_edit_cursor() {
                return LRESULT(1);
            }
            // SAFETY: Let Windows set default non-client/viewer cursor.
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
            state.handle_lbutton_up(hwnd);
            LRESULT(0)
        }

        WM_CAPTURECHANGED => {
            state.dragging = false;
            if state.edit.is_some() {
                state.handle_edit_mouse_up(hwnd);
            }
            LRESULT(0)
        }

        WM_LBUTTONDBLCLK => {
            if state.edit.is_some() {
                // Forward double-clicks inside Edit Mode so double-clicking sliders resets them.
                state.handle_lbutton_down(hwnd, lparam);
            } else {
                state.toggle_fullscreen(hwnd);
            }
            LRESULT(0)
        }

        WM_XBUTTONDOWN => {
            if state.edit.is_some() {
                return LRESULT(1);
            }
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

fn cursor_for_handle(handle: CropHandle) -> PCWSTR {
    match handle {
        CropHandle::Nw | CropHandle::Se => IDC_SIZENWSE,
        CropHandle::Ne | CropHandle::Sw => IDC_SIZENESW,
        CropHandle::N | CropHandle::S => IDC_SIZENS,
        CropHandle::E | CropHandle::W => IDC_SIZEWE,
        CropHandle::Move => IDC_SIZEALL,
    }
}

fn copy_image_to_win32_clipboard(hwnd: HWND, img: &DecodedImage) -> Result<(), String> {
    let frame = img
        .frames
        .first()
        .ok_or_else(|| "image has no frames".to_string())?;
    let w = img.width as usize;
    let h = img.height as usize;
    let row_bytes = w
        .checked_mul(4)
        .ok_or_else(|| "image dimensions overflow".to_string())?;
    let pixel_bytes = row_bytes
        .checked_mul(h)
        .ok_or_else(|| "image byte size overflow".to_string())?;
    if frame.pixels.len() < pixel_bytes || w == 0 || h == 0 {
        return Err("invalid image buffer".to_string());
    }

    let hdr_size = size_of::<BITMAPINFOHEADER>();
    let total_bytes = hdr_size
        .checked_add(pixel_bytes)
        .ok_or_else(|| "DIB size overflow".to_string())?;

    let header = BITMAPINFOHEADER {
        biSize: hdr_size as u32,
        biWidth: i32::try_from(img.width).map_err(|_| "width exceeds i32")?,
        biHeight: i32::try_from(img.height).map_err(|_| "height exceeds i32")?,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: u32::try_from(pixel_bytes).unwrap_or(0),
        ..Default::default()
    };

    // SAFETY: Allocating `total_bytes` of moveable global memory for `SetClipboardData(CF_DIB)`.
    let hmem: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, total_bytes) }
        .map_err(|e| format!("GlobalAlloc failed: {e}"))?;

    // SAFETY: `hmem` is a valid non-null handle returned by `GlobalAlloc`.
    let dst_ptr = unsafe { GlobalLock(hmem) } as *mut u8;
    if dst_ptr.is_null() {
        // SAFETY: Free unowned `hmem` on lock failure.
        let _ = unsafe { GlobalFree(Some(hmem)) };
        return Err("GlobalLock failed".to_string());
    }

    // SAFETY: `dst_ptr` points to `total_bytes` writable bytes; write header + bottom-up BGRX rows.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&header as *const BITMAPINFOHEADER).cast::<u8>(),
            dst_ptr,
            hdr_size,
        );
        let pixel_dst = std::slice::from_raw_parts_mut(dst_ptr.add(hdr_size), pixel_bytes);
        for y in 0..h {
            let src_row = &frame.pixels[(h - 1 - y) * row_bytes..(h - y) * row_bytes];
            let dst_row = &mut pixel_dst[y * row_bytes..(y + 1) * row_bytes];
            for (spx, dpx) in src_row
                .as_chunks::<4>()
                .0
                .iter()
                .zip(dst_row.as_chunks_mut::<4>().0.iter_mut())
            {
                let a = spx[3];
                if a == 255 {
                    dpx[0] = spx[0];
                    dpx[1] = spx[1];
                    dpx[2] = spx[2];
                    dpx[3] = 255;
                } else {
                    // Composite premultiplied BGRA onto white for opaque 32-bit CF_DIB targets.
                    let inv = 255u8.saturating_sub(a);
                    dpx[0] = spx[0].saturating_add(inv);
                    dpx[1] = spx[1].saturating_add(inv);
                    dpx[2] = spx[2].saturating_add(inv);
                    dpx[3] = 255;
                }
            }
        }
        let _ = GlobalUnlock(hmem);
    }

    // SAFETY: Opening clipboard for `hwnd`; paired with `CloseClipboard` on every path.
    if let Err(e) = unsafe { OpenClipboard(Some(hwnd)) } {
        // SAFETY: Clipboard was not opened; free `hmem`.
        let _ = unsafe { GlobalFree(Some(hmem)) };
        return Err(format!("OpenClipboard failed: {e}"));
    }

    // SAFETY: Clipboard is open on the calling thread.
    let set_res = unsafe {
        let _ = EmptyClipboard();
        SetClipboardData(CF_DIB, Some(HANDLE(hmem.0)))
    };
    // SAFETY: Always close the clipboard after `OpenClipboard`.
    let _ = unsafe { CloseClipboard() };

    if let Err(e) = set_res {
        // SAFETY: `SetClipboardData` failed, so ownership remains ours and we must free `hmem`.
        let _ = unsafe { GlobalFree(Some(hmem)) };
        return Err(format!("SetClipboardData failed: {e}"));
    }
    Ok(())
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
