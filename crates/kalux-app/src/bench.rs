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
        return (0.0, 0.0);
    }

    const MIB: f64 = 1024.0 * 1024.0;
    (
        pmc.PrivateUsage as f64 / MIB,
        pmc.WorkingSetSize as f64 / MIB,
    )
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
