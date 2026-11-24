//! Linux system theme detection (dark / light) matching the shared ka-suite `Palette`.
//!
//! Fast, non-blocking file/env inspection (<0.1 ms, zero D-Bus startup latency):
//! 1. `KALUX_THEME=light|dark` explicit override
//! 2. `$XDG_CONFIG_HOME/gtk-3.0/settings.ini` or `~/.config/gtk-3.0/settings.ini`
//! 3. `GTK_THEME` environment variable
//! 4. Defaults to `Palette::DARK` (matches Niri/CachyOS dark default and ka-suite dark default).

use std::env;
use std::fs;
use std::path::PathBuf;

use kalux_core::Palette;

/// Detect whether the user's Linux desktop prefers light or dark appearance.
pub fn system_palette() -> Palette {
    if let Ok(override_val) = env::var("KALUX_THEME") {
        let lower = override_val.trim().to_ascii_lowercase();
        if lower == "light" {
            return Palette::LIGHT;
        }
        if lower == "dark" {
            return Palette::DARK;
        }
    }

    if let Some(is_dark) = read_gtk3_prefer_dark() {
        return if is_dark {
            Palette::DARK
        } else {
            Palette::LIGHT
        };
    }

    if let Ok(gtk_theme) = env::var("GTK_THEME") {
        let lower = gtk_theme.to_ascii_lowercase();
        if lower.contains("dark") {
            return Palette::DARK;
        }
        if lower.contains("light") {
            return Palette::LIGHT;
        }
    }

    Palette::DARK
}

fn gtk3_settings_path() -> Option<PathBuf> {
    if let Some(cfg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(cfg).join("gtk-3.0/settings.ini"));
    }
    let home = env::var_os("HOME").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(home).join(".config/gtk-3.0/settings.ini"))
}

fn read_gtk3_prefer_dark() -> Option<bool> {
    let path = gtk3_settings_path()?;
    let contents = fs::read_to_string(path).ok()?;
    let mut theme_name_dark: Option<bool> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.eq_ignore_ascii_case("gtk-application-prefer-dark-theme") {
            if val == "1" || val.eq_ignore_ascii_case("true") {
                return Some(true);
            }
            if val == "0" || val.eq_ignore_ascii_case("false") {
                return Some(false);
            }
        } else if key.eq_ignore_ascii_case("gtk-theme-name") {
            let lower = val.to_ascii_lowercase();
            if lower.contains("dark") {
                theme_name_dark = Some(true);
            } else if lower.contains("light") {
                theme_name_dark = Some(false);
            }
        }
    }

    theme_name_dark
}
