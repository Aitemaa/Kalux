//! Linux native Wayland backend (`kalux` on CachyOS / Niri and any Wayland compositor).

pub mod bench;
pub mod edit_ui;
pub mod overlay;
pub mod render;
pub mod screenshot;
pub mod shm;
pub mod testimg;
pub mod theme;
pub mod wayland;

use std::io::Write;
use std::time::Instant;

use crate::args::{Command, parse_args};
use crate::linux::bench::BenchState;
use crate::linux::overlay::ErrorCard;
use crate::linux::wayland::LaunchConfig;

pub fn run_linux(main_start: Instant) -> i32 {
    let parsed = parse_args(std::env::args_os().skip(1));

    match parsed {
        Ok(Command::Version) => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "kalux {}", env!("CARGO_PKG_VERSION"));
            let _ = out.flush();
            0
        }

        Ok(Command::GenTestImage { out, width, height }) => {
            if let Err(err) = testimg::generate_test_jpeg(&out, width, height) {
                eprintln!("kalux: {err}");
                return 1;
            }
            0
        }

        Ok(Command::Screenshot {
            image,
            out,
            light,
            show_help,
            edit_mode,
            toast,
            title,
            width,
            height,
        }) => {
            let opts = screenshot::ScreenshotOptions {
                image,
                out,
                light,
                show_help,
                edit_mode,
                toast,
                title,
                width,
                height,
            };
            if let Err(err) = screenshot::render_screenshot(&opts) {
                eprintln!("kalux: {err}");
                return 1;
            }
            0
        }

        Ok(Command::BenchStartup { file, out }) => {
            let bench = BenchState::new(file.clone(), out, main_start);
            wayland::run(LaunchConfig {
                initial_file: Some(file),
                initial_error: None,
                bench: Some(bench),
            })
        }

        Ok(Command::BenchEdit { file, out }) => {
            if let Err(err) = bench::run_edit_bench(&file, out.as_deref()) {
                eprintln!("kalux: --bench-edit failed: {err}");
                return 1;
            }
            0
        }

        Ok(Command::Open { file }) => wayland::run(LaunchConfig {
            initial_file: file,
            initial_error: None,
            bench: None,
        }),

        Err(arg_err) => {
            if arg_err.is_cli_mode {
                eprintln!("kalux: {}", arg_err.message);
                return 1;
            }
            wayland::run(LaunchConfig {
                initial_file: None,
                initial_error: Some(ErrorCard {
                    title: "Invalid command-line argument".to_string(),
                    subject: String::new(),
                    message: arg_err.message,
                    hint: Some("Drop an image file onto this window to view it.".to_string()),
                }),
                bench: None,
            })
        }
    }
}
