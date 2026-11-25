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

/// Read `(private_anon_mb, vm_rss_mb, vm_hwm_mb)` from `/proc/self/status`.
fn sample_memory_mb_all() -> (f64, f64, f64) {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return (0.0, 0.0, 0.0);
    };
    let mut rss_anon_kb = 0u64;
    let mut vm_rss_kb = 0u64;
    let mut vm_hwm_kb = 0u64;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("RssAnon:") {
            rss_anon_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("VmRSS:") {
            vm_rss_kb = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            vm_hwm_kb = parse_kb(rest);
        }
    }
    const KIB_PER_MIB: f64 = 1024.0;
    (
        rss_anon_kb as f64 / KIB_PER_MIB,
        vm_rss_kb as f64 / KIB_PER_MIB,
        vm_hwm_kb as f64 / KIB_PER_MIB,
    )
}

/// Read `(private_anon_mb, vm_rss_mb)` from `/proc/self/status`.
fn sample_memory_mb() -> (f64, f64) {
    let (anon, rss, _) = sample_memory_mb_all();
    (anon, rss)
}

/// Headless Edit Mode RAM & latency benchmark (`--bench-edit <file> [--out <json>]`).
pub fn run_edit_bench(file: &Path, out_path: Option<&Path>) -> Result<(), String> {
    use kalux_core::edit::edit_image_dest_rect;
    use kalux_core::{
        CropRect, DockLayout, EditRecipe, EditSession, EditTab, Palette, QuarterTurns,
        ToneAdjustments,
    };
    use kalux_decode::{ExportOptions, decode_file, export_image};

    use crate::linux::edit_ui::draw_edit_ui;
    use crate::linux::overlay::LinuxOverlay;
    use crate::linux::render::Canvas;

    let vp_w = 1280u32;
    let vp_h = 800u32;

    // 1. Viewer baseline: display-scaled decode + single canvas frame.
    let decoded = decode_file(file, Some((1920, 1080)))
        .map_err(|e| format!("decode {}: {e}", file.display()))?;
    let mut canvas_buf = vec![0u8; (vp_w as usize) * (vp_h as usize) * 4];
    let mut overlay = LinuxOverlay::new();
    let palette = Palette::DARK;

    let (viewer_private_mb, viewer_rss_mb, _) = sample_memory_mb_all();

    // 2. Edit Mode active: 30 live-preview scrub frames.
    let mut session = EditSession::new(EditTab::Adjust, false, 0, 1);
    let src_sz = (decoded.source_width, decoded.source_height);
    let mut frame_times_ms = Vec::with_capacity(30);

    for i in 0..30u32 {
        let t = (i as f32 / 29.0) * 2.0 - 1.0; // -1.0 .. +1.0
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
        let (dest, img_scale) =
            edit_image_dest_rect(oriented, (vp_w, vp_h), layout.reserved_bottom_px, 1.0);
        let lut = session.preview_lut();
        let mut canvas = Canvas::new(vp_w, vp_h, &mut canvas_buf);
        canvas.clear(palette.bg);
        canvas.draw_image_edited(
            &decoded,
            0,
            dest,
            img_scale,
            recipe.turns,
            recipe.flip_h,
            recipe.flip_v,
            &lut,
        );
        draw_edit_ui(
            &mut canvas,
            &mut overlay,
            1.0,
            palette,
            dest,
            src_sz,
            &session,
            &layout,
        );
        frame_times_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }

    frame_times_ms.sort_by(|a, b| a.total_cmp(b));
    let preview_median_ms = frame_times_ms[frame_times_ms.len() / 2];
    let preview_p95_ms = frame_times_ms[(frame_times_ms.len() * 95 / 100).min(29)];
    let (edit_active_private_mb, edit_active_rss_mb, _) = sample_memory_mb_all();

    // 3. Full-resolution export of a cropped + toned recipe to a temporary copy.
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
    let _ = fs::remove_file(&res.saved_path);

    // 4. Exit Edit Mode & drop session; sample post-export settled RAM + HWM peak.
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
            let _ = fs::create_dir_all(parent);
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .map_err(|e| format!("open {}: {e}", p.display()))?;
        writeln!(f, "{out}").map_err(|e| format!("write {}: {e}", p.display()))?;
        let _ = f.flush();
    }

    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{out}").map_err(|e| format!("stdout: {e}"))?;
    let _ = stdout.flush();
    Ok(())
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
