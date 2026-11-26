//! kalux — instant native image viewer for Windows (Win32 + Direct2D) and Linux (Wayland / Niri).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod args;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(windows)]
mod bench;
#[cfg(windows)]
mod edit_ui;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod render;
#[cfg(windows)]
mod screenshot;
#[cfg(windows)]
mod testimg;
#[cfg(windows)]
mod window;

#[cfg(target_os = "linux")]
fn main() {
    let main_start = std::time::Instant::now();
    let exit_code = linux::run_linux(main_start);
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() {
    match args::parse_args(std::env::args_os().skip(1)) {
        Ok(args::Command::Version) => {
            println!("kalux {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("kalux supports Windows (Win32 + Direct2D) and Linux (Wayland).");
        }
    }
}

#[cfg(windows)]
fn main() {
    let main_start = std::time::Instant::now();
    let exit_code = run_windows(main_start);
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

#[cfg(windows)]
fn run_windows(main_start: std::time::Instant) -> i32 {
    use std::io::Write;

    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };

    use crate::args::{Command, parse_args};
    use crate::bench::{BenchState, attach_console_if_needed};
    use crate::overlay::ErrorCard;
    use crate::window::LaunchConfig;

    /// RAII guard for COM initialization on the UI thread (STA).
    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: Paired with a successful `CoInitializeEx` on this thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    let parsed = parse_args(std::env::args_os().skip(1));

    match parsed {
        Ok(Command::Version) => {
            attach_console_if_needed();
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "kalux {}", env!("CARGO_PKG_VERSION"));
            let _ = out.flush();
            0
        }

        Ok(Command::GenTestImage { out, width, height }) => {
            // SAFETY: Initialize COM STA on the main thread for WIC encoding.
            let com_ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let _com = ComGuard(com_ok);

            if let Err(err) = testimg::generate_test_jpeg(&out, width, height) {
                attach_console_if_needed();
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "kalux: {err}");
                let _ = stderr.flush();
                return 1;
            }
            0
        }

        Ok(Command::Screenshot {
            image,
            out,
            light,
            show_help,
            show_viewer_bar,
            edit_mode,
            toast,
            title,
            width,
            height,
        }) => {
            // SAFETY: Initialize COM STA on the main thread for WIC + Direct2D.
            let com_ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let _com = ComGuard(com_ok);

            let opts = screenshot::ScreenshotOptions {
                image,
                out,
                light,
                show_help,
                show_viewer_bar,
                edit_mode,
                toast,
                title,
                width,
                height,
            };
            if let Err(err) = screenshot::render_screenshot(&opts) {
                attach_console_if_needed();
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "kalux: {err}");
                let _ = stderr.flush();
                return 1;
            }
            0
        }

        Ok(Command::BenchStartup { file, out }) => {
            // SAFETY: Initialize COM STA on the UI thread.
            let com_ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let _com = ComGuard(com_ok);

            let bench = BenchState::new(file.clone(), out, main_start);
            window::run(LaunchConfig {
                initial_file: Some(file),
                initial_error: None,
                bench: Some(bench),
            })
        }

        Ok(Command::BenchEdit { file, out }) => {
            // SAFETY: Initialize COM STA on the main thread for WIC decoding.
            let com_ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let _com = ComGuard(com_ok);

            if let Err(err) = bench::run_edit_bench(&file, out.as_deref()) {
                attach_console_if_needed();
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "kalux: {err}");
                let _ = stderr.flush();
                return 1;
            }
            0
        }

        Ok(Command::Open { file }) => {
            // SAFETY: Initialize COM STA on the UI thread.
            let com_ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let _com = ComGuard(com_ok);

            window::run(LaunchConfig {
                initial_file: file,
                initial_error: None,
                bench: None,
            })
        }

        Err(arg_err) => {
            if arg_err.is_cli_mode {
                attach_console_if_needed();
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(stderr, "kalux: {}", arg_err.message);
                let _ = stderr.flush();
                return 1;
            }

            // SAFETY: Initialize COM STA on the UI thread to show argument error in-window.
            let com_ok =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
                    .is_ok();
            let _com = ComGuard(com_ok);

            window::run(LaunchConfig {
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
