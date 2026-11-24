# Kalux

**A fast, native image viewer for Windows, written entirely in Rust.** Kalux replaces the Windows 11 Photos app for the thing you actually do: open an image and flip to the next one.

Part of the ka-suite: [Kavox](https://github.com/Aitemaa/Kavox) · [Kapix](https://github.com/Aitemaa/Kapix) · [Kadex](https://github.com/Aitemaa/Kadex) · **Kalux**

> *lux*: Latin for light. Photography is drawing with light.

## Targets (PRD §5)

| | Windows Photos (est.) | **Kalux target** |
|---|---|---|
| Cold start → first image | 1.5–4 s | **≤ 150 ms** |
| Next / previous image | 100–400 ms | **≤ 16 ms** (preloaded) |
| RAM, one image open | 150–350 MB | **≤ 50 MB** |
| Binary | 150–400 MB install | **≤ 4 MB** single exe |
| Running after close | sometimes | **never** |

Measured numbers will replace the estimates once M0 (baseline) is done. See [docs/PRD.md](docs/PRD.md).

## Status
🚧 v0.1 (MVP) in progress.

## Build
```powershell
cargo build --release -p kalux-app   # → target\release\kalux.exe
kalux.exe path\to\photo.jpg
kalux.exe --bench-startup path\to\photo.jpg   # prints startup timings as JSON (optional --bench-out <path>)
kalux.exe --gen-test-image test.jpg 4000 3000 # generates a deterministic 12 MP test JPEG via WIC
```

## Keys
| Key | Action |
|---|---|
| ← → / PgUp PgDn / Mouse 4–5 | Previous / next |
| Home / End | First / last in folder |
| Wheel, `+` `-` | Zoom |
| `0` / `1` | Fit / 100% |
| Left-drag | Pan when zoomed |
| F11, double-click | Fullscreen |
| `?` | All shortcuts |
| Esc / Ctrl+W | Exit fullscreen, then close |

## License
MIT
