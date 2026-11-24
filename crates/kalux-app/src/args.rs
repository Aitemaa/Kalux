//! Command-line argument parsing (platform-independent).

use std::ffi::OsString;
use std::path::PathBuf;

/// Maximum test image dimension accepted by `--gen-test-image` to avoid absurd allocations.
const MAX_TEST_DIM: u32 = 32_768;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open an optional image file in the GUI window.
    Open { file: Option<PathBuf> },
    /// Print `kalux <version>` and exit 0.
    Version,
    /// Run the normal GUI startup path, emit one JSON timing/memory line, and exit 0.
    BenchStartup { file: PathBuf, out: Option<PathBuf> },
    /// Generate a deterministic non-trivial test JPEG via WIC and exit 0.
    GenTestImage {
        out: PathBuf,
        width: u32,
        height: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgError {
    pub message: String,
    /// True when the error happened while parsing a headless/CLI mode flag.
    pub is_cli_mode: bool,
}

impl ArgError {
    fn gui(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            is_cli_mode: false,
        }
    }

    fn cli(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            is_cli_mode: true,
        }
    }
}

/// Parse argv (excluding argv[0]).
pub fn parse_args<I>(args: I) -> Result<Command, ArgError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut iter = args.into_iter();
    let Some(first) = iter.next() else {
        return Ok(Command::Open { file: None });
    };

    if first.is_empty() {
        return Ok(Command::Open { file: None });
    }

    if let Some(flag) = first.to_str() {
        match flag {
            "--version" | "-V" => return Ok(Command::Version),
            "--bench-startup" => return parse_bench_startup(iter),
            "--gen-test-image" => return parse_gen_test_image(iter),
            "--" => {
                let file = iter.next().filter(|s| !s.is_empty()).map(PathBuf::from);
                return Ok(Command::Open { file });
            }
            other if other.starts_with("--") => {
                return Err(ArgError::gui(format!("Unknown option: {other}")));
            }
            _ => {}
        }
    }

    Ok(Command::Open {
        file: Some(PathBuf::from(first)),
    })
}

fn parse_bench_startup(mut iter: impl Iterator<Item = OsString>) -> Result<Command, ArgError> {
    let mut file: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;

    while let Some(arg) = iter.next() {
        if let Some(s) = arg.to_str() {
            if s == "--bench-out" {
                let p = iter
                    .next()
                    .ok_or_else(|| ArgError::cli("--bench-out requires a file path"))?;
                if p.is_empty() {
                    return Err(ArgError::cli("--bench-out path cannot be empty"));
                }
                out = Some(PathBuf::from(p));
                continue;
            }
            if let Some(rest) = s.strip_prefix("--bench-out=") {
                if rest.is_empty() {
                    return Err(ArgError::cli("--bench-out path cannot be empty"));
                }
                out = Some(PathBuf::from(rest));
                continue;
            }
            if s.starts_with("--") {
                return Err(ArgError::cli(format!(
                    "Unknown option for --bench-startup: {s}"
                )));
            }
        }

        if file.is_none() {
            file = Some(PathBuf::from(arg));
        } else {
            return Err(ArgError::cli(
                "Unexpected extra argument to --bench-startup",
            ));
        }
    }

    let file = file
        .ok_or_else(|| ArgError::cli("Usage: kalux --bench-startup <file> [--bench-out <path>]"))?;

    Ok(Command::BenchStartup { file, out })
}

fn parse_gen_test_image(mut iter: impl Iterator<Item = OsString>) -> Result<Command, ArgError> {
    let usage = "Usage: kalux --gen-test-image <out.jpg> <width> <height>";
    let out = iter.next().ok_or_else(|| ArgError::cli(usage))?;
    let w_os = iter.next().ok_or_else(|| ArgError::cli(usage))?;
    let h_os = iter.next().ok_or_else(|| ArgError::cli(usage))?;
    if iter.next().is_some() {
        return Err(ArgError::cli(usage));
    }

    let width = parse_dim(&w_os, "width")?;
    let height = parse_dim(&h_os, "height")?;

    Ok(Command::GenTestImage {
        out: PathBuf::from(out),
        width,
        height,
    })
}

fn parse_dim(val: &OsString, name: &str) -> Result<u32, ArgError> {
    let s = val
        .to_str()
        .ok_or_else(|| ArgError::cli(format!("Invalid {name}: not valid UTF-8")))?;
    let n: u32 = s
        .parse()
        .map_err(|_| ArgError::cli(format!("Invalid {name}: '{s}'")))?;
    if n == 0 || n > MAX_TEST_DIM {
        return Err(ArgError::cli(format!(
            "Invalid {name}: must be between 1 and {MAX_TEST_DIM}"
        )));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn no_args_opens_empty_window() {
        assert_eq!(parse_args(os(&[])), Ok(Command::Open { file: None }));
    }

    #[test]
    fn single_file_arg() {
        assert_eq!(
            parse_args(os(&["photo.jpg"])),
            Ok(Command::Open {
                file: Some(PathBuf::from("photo.jpg"))
            })
        );
    }

    #[test]
    fn double_dash_allows_flag_like_filename() {
        assert_eq!(
            parse_args(os(&["--", "--weird.jpg"])),
            Ok(Command::Open {
                file: Some(PathBuf::from("--weird.jpg"))
            })
        );
    }

    #[test]
    fn version_flag() {
        assert_eq!(parse_args(os(&["--version"])), Ok(Command::Version));
        assert_eq!(parse_args(os(&["-V"])), Ok(Command::Version));
    }

    #[test]
    fn bench_startup_variants() {
        assert_eq!(
            parse_args(os(&["--bench-startup", "a.jpg"])),
            Ok(Command::BenchStartup {
                file: PathBuf::from("a.jpg"),
                out: None,
            })
        );
        assert_eq!(
            parse_args(os(&["--bench-startup", "a.jpg", "--bench-out", "out.json"])),
            Ok(Command::BenchStartup {
                file: PathBuf::from("a.jpg"),
                out: Some(PathBuf::from("out.json")),
            })
        );
        assert_eq!(
            parse_args(os(&["--bench-startup", "--bench-out=out.json", "a.jpg"])),
            Ok(Command::BenchStartup {
                file: PathBuf::from("a.jpg"),
                out: Some(PathBuf::from("out.json")),
            })
        );
        assert!(
            parse_args(os(&["--bench-startup"]))
                .unwrap_err()
                .is_cli_mode
        );
    }

    #[test]
    fn gen_test_image_valid_and_invalid() {
        assert_eq!(
            parse_args(os(&["--gen-test-image", "out.jpg", "4000", "3000"])),
            Ok(Command::GenTestImage {
                out: PathBuf::from("out.jpg"),
                width: 4000,
                height: 3000,
            })
        );
        assert!(parse_args(os(&["--gen-test-image", "out.jpg", "0", "100"])).is_err());
        assert!(parse_args(os(&["--gen-test-image", "out.jpg", "100"])).is_err());
    }
}
