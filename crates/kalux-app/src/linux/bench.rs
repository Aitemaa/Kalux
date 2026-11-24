//! Linux startup benchmark instrumentation (PRD §10), emitting the same JSON schema as Windows.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct BenchState {
    pub file: PathBuf,
    pub out_path: Option<PathBuf>,
    main_start: Instant,
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

    pub fn record_first_present(&mut self) -> bool {
        if self.first_present_ms.is_some() {
            return false;
        }
        self.first_present_ms = Some(self.main_start.elapsed().as_secs_f64() * 1000.0);
        true
    }

    pub fn has_presented(&self) -> bool {
        self.first_present_ms.is_some()
    }

    pub fn finish_and_emit(&self) -> io::Result<()> {
        let (private_mb, working_set_mb) = sample_memory_mb();
        let exe_bytes = current_exe_bytes();
        let json = format_bench_json(
            &self.file,
            0.0,
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
                let _ = fs::create_dir_all(parent);
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(out_path)?;
            writeln!(file, "{json}")?;
            file.flush()?;
        }

        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{json}")?;
        stdout.flush()
    }
}

/// Read `(private_anon_mb, vm_rss_mb)` from `/proc/self/status`.
fn sample_memory_mb() -> (f64, f64) {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return (0.0, 0.0);
    };
    let mut rss_anon_kb = 0u64;
    let mut vm_rss_kb = 0u64;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("RssAnon:") {
            rss_anon_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            vm_rss_kb = parse_kb(rest);
        }
    }
    const KIB_PER_MIB: f64 = 1024.0;
    (
        rss_anon_kb as f64 / KIB_PER_MIB,
        vm_rss_kb as f64 / KIB_PER_MIB,
    )
}

fn parse_kb(s: &str) -> u64 {
    s.split_whitespace()
        .next()
        .and_then(|tok| tok.parse::<u64>().ok())
        .unwrap_or(0)
}

fn current_exe_bytes() -> u64 {
    std::env::current_exe()
        .ok()
        .and_then(|p| fs::metadata(p).ok())
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
