//! Simple decode latency benchmark.
//!
//! Usage:
//!   cargo run --release -p kalux-decode --example decode_bench -- <path> [iterations]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kalux_decode::decode_file;

fn median_ms(mut samples: Vec<Duration>) -> f64 {
    samples.sort();
    let mid = samples.len() / 2;
    if samples.len().is_multiple_of(2) {
        (samples[mid - 1].as_secs_f64() + samples[mid].as_secs_f64()) * 500.0
    } else {
        samples[mid].as_secs_f64() * 1000.0
    }
}

fn bench(label: &str, path: &Path, max_size: Option<(u32, u32)>, n: usize) {
    // Warm up once before measuring.
    let first = match decode_file(path, max_size) {
        Ok(img) => img,
        Err(e) => {
            eprintln!("error ({label}): {e}");
            std::process::exit(1);
        }
    };

    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        let img = decode_file(path, max_size).expect("decode failed during benchmark");
        samples.push(start.elapsed());
        std::hint::black_box(img.byte_size());
    }

    let med = median_ms(samples);
    println!(
        "{label:<16} -> {}x{} (source {}x{}, {} frames): median {:.2} ms over {n} runs",
        first.width,
        first.height,
        first.source_width,
        first.source_height,
        first.frames.len(),
        med
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path_str) = args.next() else {
        eprintln!("Usage: decode_bench <image-path> [iterations=10]");
        std::process::exit(2);
    };
    let n: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(10);

    let path = PathBuf::from(path_str);
    println!("Benchmarking {} ({n} iterations):", path.display());
    bench("full", &path, None, n);
    bench("fit 1920x1080", &path, Some((1920, 1080)), n);
}
