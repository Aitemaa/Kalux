//! Startup benchmark instrumentation and console output helper (PRD §10).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, GetFileType,
};
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    SetStdHandle,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// Attach to the parent console when stdout or stderr is not already redirected to a pipe/file.
///
/// Crucial for a `#![windows_subsystem = "windows"]` binary in CI: if PowerShell pipes stdout
/// (`kalux --bench-startup ... | Tee-Object`), `STD_OUTPUT_HANDLE` is already a valid pipe handle.
/// We preserve any already-redirected handle so `AttachConsole` never hijacks it.
pub fn attach_console_if_needed() {
    fn is_valid_handle(h: HANDLE) -> bool {
        if h.is_invalid() || h == INVALID_HANDLE_VALUE || h.0.is_null() {
            return false;
        }
        // SAFETY: `h` is a non-null, non-invalid handle returned by `GetStdHandle`.
        let ft = unsafe { GetFileType(h) };
        ft == FILE_TYPE_DISK || ft == FILE_TYPE_PIPE || ft == FILE_TYPE_CHAR
    }

    // SAFETY: `GetStdHandle` is always safe to call with standard handle IDs.
    let orig_out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.unwrap_or(HANDLE::default());
    // SAFETY: `GetStdHandle` is always safe to call with standard handle IDs.
    let orig_err = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.unwrap_or(HANDLE::default());

    let out_ok = is_valid_handle(orig_out);
    let err_ok = is_valid_handle(orig_err);
    if out_ok && err_ok {
        return;
    }

    // SAFETY: Attaching to the parent process's console is safe; failure (no parent console) is ignored.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
        if out_ok {
            // SAFETY: Restores the previously valid redirected stdout handle.
            let _ = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, orig_out) };
        }
        if err_ok {
            // SAFETY: Restores the previously valid redirected stderr handle.
            let _ = unsafe { SetStdHandle(STD_ERROR_HANDLE, orig_err) };
        }
    }
}

/// Captures timestamps across the startup critical path.
pub struct BenchState {
    pub file: PathBuf,
    pub out_path: Option<PathBuf>,
    main_start: Instant,
    process_create_to_main_ms: f64,
    main_to_window_ms: Option<f64>,
    decode_ms: Option<f64>,
    first_present_ms: Option<f64>,
}

impl BenchState {
    pub fn new(file: PathBuf, out_path: Option<PathBuf>, main_start: Instant) -> Self {
        Self {
            file,
            out_path,
            main_start,
            process_create_to_main_ms: measure_process_create_to_now_ms(),
            main_to_window_ms: None,
            decode_ms: None,
            first_present_ms: None,
        }
    }

    pub fn record_window_ready(&mut self) {
        if self.main_to_window_ms.is_none() {
            self.main_to_window_ms = Some(self.main_start.elapsed().as_secs_f64() * 1000.0);
        }
    }

    pub fn record_decode(&mut self, elapsed: Duration) {
        if self.decode_ms.is_none() {
            self.decode_ms = Some(elapsed.as_secs_f64() * 1000.0);
        }
    }

    /// Record the first real-image present. Returns `true` if this was the first present.
    pub fn record_first_present(&mut self) -> bool {
        if self.first_present_ms.is_some() {
            return false;
        }
        let main_to_present = self.main_start.elapsed().as_secs_f64() * 1000.0;
        self.first_present_ms = Some(self.process_create_to_main_ms + main_to_present);
        true
    }

    pub fn has_presented(&self) -> bool {
        self.first_present_ms.is_some()
    }

    /// Sample memory, emit one JSON line to stdout (and `--bench-out` if set).
    pub fn finish_and_emit(&self) -> io::Result<()> {
        let (private_mb, working_set_mb) = sample_memory_mb();
        let exe_bytes = current_exe_bytes();
        let json = format_bench_json(
            &self.file,
            self.process_create_to_main_ms,
            self.main_to_window_ms.unwrap_or(0.0),
            self.decode_ms.unwrap_or(0.0),
            self.first_present_ms.unwrap_or(0.0),
            private_mb,
            working_set_mb,
            exe_bytes,
        );

        if let Some(ref out_path) = self.out_path {
            if let Some(parent) = out_path.parent()
                && !parent.as_os_str().is_empty()
            {
                let _ = std::fs::create_dir_all(parent);
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(out_path)?;
            writeln!(file, "{json}")?;
            file.flush()?;
        }

        attach_console_if_needed();
        let stdout_res = {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "{json}").and_then(|()| stdout.flush())
        };
        if self.out_path.is_none() {
            stdout_res?;
        }

        Ok(())
    }
}

fn filetime_to_u64(ft: FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

fn measure_process_create_to_now_ms() -> f64 {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();

    // SAFETY: `GetCurrentProcess()` returns a pseudo-handle; pointers are valid stack locals.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    if ok.is_err() {
        return 0.0;
    }

    // SAFETY: `GetSystemTimePreciseAsFileTime` has no preconditions.
    let now = unsafe { GetSystemTimePreciseAsFileTime() };
    let delta_100ns = filetime_to_u64(now).saturating_sub(filetime_to_u64(creation));
    delta_100ns as f64 / 10_000.0
}

fn sample_memory_mb() -> (f64, f64) {
    let (priv_mb, ws_mb, _) = sample_memory_mb_all();
    (priv_mb, ws_mb)
}

fn sample_memory_mb_all() -> (f64, f64, f64) {
    let mut pmc = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };

    // SAFETY: `PROCESS_MEMORY_COUNTERS_EX` is ABI-compatible with `PROCESS_MEMORY_COUNTERS`
    // when `cb` is set to `size_of::<PROCESS_MEMORY_COUNTERS_EX>()`.
    let res = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut pmc as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            pmc.cb,
        )
    };
    if res.is_err() {
        return (0.0, 0.0, 0.0);
    }

    const MIB: f64 = 1024.0 * 1024.0;
    (
        pmc.PrivateUsage as f64 / MIB,
        pmc.WorkingSetSize as f64 / MIB,
        pmc.PeakWorkingSetSize as f64 / MIB,
    )
}

/// Run the headless Edit Mode RAM & latency benchmark (`--bench-edit`) on Windows.
pub fn run_edit_bench(file: &Path, out_path: Option<&Path>) -> Result<(), String> {
    use kalux_core::edit::edit_image_dest_rect;
    use kalux_core::{
        CropRect, DockLayout, EditRecipe, EditSession, EditTab, QuarterTurns, ToneAdjustments,
    };
    use kalux_decode::{ExportOptions, decode_file, export_image};

    let (vp_w, vp_h) = (1920u32, 1080u32);

    // 1. Viewer baseline: display-scaled decode at 1920x1080 target.
    let decoded = decode_file(file, Some((vp_w, vp_h))).map_err(|e| e.to_string())?;
    let src_sz = (decoded.source_width, decoded.source_height);
    let (viewer_private_mb, viewer_rss_mb, _) = sample_memory_mb_all();

    // 2. Edit Mode active: 30 live-preview scrub frames over the proxy buffer.
    let mut session = EditSession::new(
        EditTab::Adjust,
        decoded.is_animated(),
        0,
        decoded.frames.len(),
    );
    let base_pixels = decoded
        .frames
        .first()
        .map(|f| f.pixels.as_slice())
        .ok_or_else(|| "decoded image has no frames".to_string())?;
    let mut staging = vec![0u8; base_pixels.len()];
    let mut frame_times_ms = Vec::with_capacity(30);

    for i in 0..30usize {
        let t = (i as f32 / 29.0) * 2.0 - 1.0;
        session.history.push(EditRecipe {
            crop: CropRect {
                x: 0.1,
                y: 0.1,
                w: 0.8,
                h: 0.8,
            },
            turns: if i % 2 == 0 {
                QuarterTurns::R0
            } else {
                QuarterTurns::R90
            },
            flip_h: false,
            flip_v: false,
            tone: ToneAdjustments {
                exposure: t * 0.4,
                contrast: t * 0.3,
                saturation: t * 0.25,
                warmth: -t * 0.2,
                monochrome: false,
            },
        });
        let t0 = Instant::now();
        let layout = DockLayout::compute(vp_w, vp_h, 1.0, &session);
        let recipe = session.recipe();
        let oriented = recipe.oriented_dims(src_sz.0, src_sz.1);
        let _ = edit_image_dest_rect(oriented, (vp_w, vp_h), layout.reserved_bottom_px, 1.0);
        let lut = session.preview_lut();
        staging.copy_from_slice(base_pixels);
        lut.apply_bgra_premul_slice(&mut staging);
        frame_times_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    frame_times_ms.sort_by(|a, b| a.total_cmp(b));
    let preview_median_ms = frame_times_ms[frame_times_ms.len() / 2];
    let preview_p95_ms = frame_times_ms[(frame_times_ms.len() * 95 / 100).min(29)];
    let (edit_active_private_mb, edit_active_rss_mb, _) = sample_memory_mb_all();

    // 3. Full-resolution export of a cropped + toned recipe to a temporary copy.
    drop(staging);
    let export_recipe = EditRecipe {
        crop: CropRect {
            x: 0.12,
            y: 0.10,
            w: 0.76,
            h: 0.80,
        },
        turns: QuarterTurns::R90,
        flip_h: false,
        flip_v: false,
        tone: ToneAdjustments {
            exposure: 0.15,
            contrast: 0.2,
            saturation: 0.1,
            warmth: 0.15,
            monochrome: false,
        },
    };
    let t_exp = Instant::now();
    let res = export_image(
        file,
        &export_recipe,
        ExportOptions {
            jpeg_quality: 92,
            save_copy: true,
            format_override: None,
        },
        0,
    )
    .map_err(|e| e.to_string())?;
    let export_ms = t_exp.elapsed().as_secs_f64() * 1000.0;
    let _ = std::fs::remove_file(&res.saved_path);

    // 4. Exit Edit Mode & drop session; sample post-export settled RAM + peak working set.
    drop(session);
    let (post_export_private_mb, post_export_rss_mb, export_peak_rss_mb) = sample_memory_mb_all();

    let mut out = String::with_capacity(384);
    out.push_str("{\"file\":\"");
    push_json_escaped(&mut out, &file.to_string_lossy());
    out.push_str("\",\"source_width\":");
    out.push_str(&decoded.source_width.to_string());
    out.push_str(",\"source_height\":");
    out.push_str(&decoded.source_height.to_string());
    out.push_str(",\"viewer_private_mb\":");
    push_f64_2(&mut out, viewer_private_mb);
    out.push_str(",\"viewer_rss_mb\":");
    push_f64_2(&mut out, viewer_rss_mb);
    out.push_str(",\"edit_active_private_mb\":");
    push_f64_2(&mut out, edit_active_private_mb);
    out.push_str(",\"edit_active_rss_mb\":");
    push_f64_2(&mut out, edit_active_rss_mb);
    out.push_str(",\"preview_frame_median_ms\":");
    push_f64_2(&mut out, preview_median_ms);
    out.push_str(",\"preview_frame_p95_ms\":");
    push_f64_2(&mut out, preview_p95_ms);
    out.push_str(",\"export_ms\":");
    push_f64_2(&mut out, export_ms);
    out.push_str(",\"export_peak_rss_mb\":");
    push_f64_2(&mut out, export_peak_rss_mb);
    out.push_str(",\"post_export_private_mb\":");
    push_f64_2(&mut out, post_export_private_mb);
    out.push_str(",\"post_export_rss_mb\":");
    push_f64_2(&mut out, post_export_rss_mb);
    out.push_str(",\"export_width\":");
    out.push_str(&res.width.to_string());
    out.push_str(",\"export_height\":");
    out.push_str(&res.height.to_string());
    out.push('}');

    if let Some(p) = out_path {
        if let Some(parent) = p.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .map_err(|e| format!("open {}: {e}", p.display()))?;
        writeln!(f, "{out}").map_err(|e| format!("write {}: {e}", p.display()))?;
        let _ = f.flush();
    }

    attach_console_if_needed();
    let stdout_res = {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{out}").and_then(|()| stdout.flush())
    };
    if out_path.is_none() {
        stdout_res.map_err(|e| format!("stdout: {e}"))?;
    }
    Ok(())
}

fn current_exe_bytes() -> u64 {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn format_bench_json(
    file: &Path,
    process_create_to_main_ms: f64,
    main_to_window_ms: f64,
    decode_ms: f64,
    first_present_ms: f64,
    private_mb: f64,
    working_set_mb: f64,
    exe_bytes: u64,
) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("{\"file\":\"");
    push_json_escaped(&mut out, &file.to_string_lossy());
    out.push_str("\",\"process_create_to_main_ms\":");
    push_f64_2(&mut out, process_create_to_main_ms);
    out.push_str(",\"main_to_window_ms\":");
    push_f64_2(&mut out, main_to_window_ms);
    out.push_str(",\"decode_ms\":");
    push_f64_2(&mut out, decode_ms);
    out.push_str(",\"first_present_ms\":");
    push_f64_2(&mut out, first_present_ms);
    out.push_str(",\"private_mb\":");
    push_f64_2(&mut out, private_mb);
    out.push_str(",\"working_set_mb\":");
    push_f64_2(&mut out, working_set_mb);
    out.push_str(",\"exe_bytes\":");
    out.push_str(&exe_bytes.to_string());
    out.push('}');
    out
}

fn push_f64_2(dst: &mut String, val: f64) {
    use std::fmt::Write as _;
    let clamped = if val.is_finite() && val >= 0.0 {
        val
    } else {
        0.0
    };
    let _ = write!(dst, "{clamped:.2}");
}

fn push_json_escaped(dst: &mut String, s: &str) {
    use std::fmt::Write as _;
    for ch in s.chars() {
        match ch {
            '"' => dst.push_str("\\\""),
            '\\' => dst.push_str("\\\\"),
            '\n' => dst.push_str("\\n"),
            '\r' => dst.push_str("\\r"),
            '\t' => dst.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(dst, "\\u{:04x}", c as u32);
            }
            c => dst.push(c),
        }
    }
}
