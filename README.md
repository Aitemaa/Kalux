<p align="center">
  <img src="assets/banner.svg" alt="Kalux: instant native image viewer for Windows and Linux" width="100%">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows%2010%2F11%20%7C%20Linux%20Wayland-262626?style=flat-square&labelColor=0a0a0a" alt="Platform: Windows 10/11 | Linux Wayland">
  <img src="https://img.shields.io/badge/built%20with-Rust%20%C2%B7%20Direct2D%20%C2%B7%20Wayland-262626?style=flat-square&labelColor=0a0a0a" alt="Built with Rust · Direct2D · Wayland">
  <img src="https://img.shields.io/badge/status-early%20stage-e8262d?style=flat-square&labelColor=0a0a0a" alt="Status: early stage">
  <img src="https://img.shields.io/badge/size-647%20KB%20Win%20%C2%B7%201.8%20MB%20Linux-262626?style=flat-square&labelColor=0a0a0a" alt="Size: 647 KB Win · 1.8 MB Linux">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-262626?style=flat-square&labelColor=0a0a0a" alt="License: MIT"></a>
</p>

<p align="center">
  <a href="#screenshots">Screenshots</a> ·
  <a href="#footprint">Footprint</a> ·
  <a href="#shortcuts">Shortcuts</a> ·
  <a href="#cli">CLI</a> ·
  <a href="#building">Building</a> ·
  <a href="docs/PRD.md">PRD</a> ·
  <a href="#license">License</a>
</p>

---

Instant native image viewer for **Windows** and **Linux (Wayland)** — 100% pure Rust, one portable binary, zero bloat. Built for the thing you actually do: open an image and flip to the next one.

Kalux = **Ka**rel + **lux** (Latin for light). Same family as [Kavox](https://github.com/Aitemaa/Kavox), [Kadex](https://github.com/Aitemaa/Kadex) and [Kapix](https://github.com/Aitemaa/Kapix): the same tokens, neutral surfaces and one red accent, with light/dark following your OS theme.

- **Decode overlaps window creation:** the opened file starts decoding before the window finishes initializing; folder listing and neighbor preloading (`N±2`) run only after the first frame is on screen.
- **Native Windows stack:** raw Win32, Direct2D (`HIGH_QUALITY_CUBIC` / `LINEAR` / `NEAREST_NEIGHBOR`), lazy DirectWrite overlays, WIC + `zune-jpeg` decoding, per-monitor DPI v2, and Mica chrome.
- **Native Linux Wayland stack (tested on CachyOS + Niri):** pure-Rust `wayrs-client` over `$WAYLAND_DISPLAY` (`xdg-decoration` server-side mode for Niri `prefer-no-csd`, `wp_fractional_scale_v1` + `wp_viewporter`, `wp_cursor_shape_v1`, `wl_data_device` drag-and-drop, `wl_shm` `memfd` double buffering, `poll(2)` 0% idle loop, and pure-Rust JPEG/PNG/GIF/WebP/BMP/ICO/TIFF decoding). Zero `libwayland`, GTK, Qt, X11, or `fontconfig` shared library dependencies (`ldd` links only `libc`, `libm`, `libgcc_s`).
- **Single portable binary:** no WebView2, no Electron, no .NET, no background processes after close, zero idle CPU, no telemetry.

Full requirements: [docs/PRD.md](docs/PRD.md).

## Screenshots

<p align="center">
  <img src="docs/screenshots/viewer-dark.png" alt="Kalux in dark theme viewing a Dolomites landscape photograph with the floating zoom pill" width="100%">
</p>

<p align="center">
  <img src="docs/screenshots/shortcuts-light.png" alt="Kalux in light theme following system theme with the keyboard shortcuts overlay open" width="100%">
</p>

<sub>Rendered offscreen by Kalux's own renderer (<code>kalux --screenshot assets/demo-photo.jpg ...</code>).</sub>

## Footprint

Measured on GitHub Actions (`windows-latest`, software WARP, 12 MP 4000×3000 JPEG) and live on **CachyOS + Niri** (Wayland, 12 MP 4000×3000 JPEG & 1376×768 photo):

| Metric | Windows Photos (est.) | **Kalux Win (CI VM)** | **Kalux Linux (Niri)** | PRD target |
| :--- | :--- | :--- | :--- | :--- |
| **Release binary** | 150–400 MB install | **0.62 MiB** (647 KB) | **1.84 MiB** (1.9 MB) | ≤ 4.0 MiB |
| **Private RAM (12 MP open)** | 150–350 MB | **23.6 MB** *(41.5 MB WS)* | **13.4 MB** *(24.3 MB RSS)* | ≤ 50 MB |
| **Start → first present** | 1.5–4.0 s | **~204 ms** *(WARP VM)* | **~37 ms** *(1 MP)* / **~223 ms** *(12 MP)* | ≤ 150 ms |
| **Next / previous (preloaded)** | 100–400 ms | **1 frame** *(cache hit)* | **1 frame** *(cache hit)* | ≤ 16 ms |
| **Idle CPU / after close** | background process | **0% / 0 processes** | **0% / 0 processes** | 0% / 0 |

## Shortcuts

| Key | Action |
| :--- | :--- |
| `←` `→` / `PgUp` `PgDn` / Mouse 4–5 | Previous / next image in folder (natural sort) |
| `Home` / `End` | First / last image |
| Wheel, `+` `-` | Zoom (anchored at cursor; high-res wheel supported) |
| `0` / `1` | Fit to window / 100% actual pixels |
| Left-drag | Pan when zoomed in |
| `F11`, double-click | Fullscreen |
| `?` | Keyboard shortcuts overlay |
| `Esc` / `Ctrl+W` | Exit fullscreen, then close |

## CLI

```bash
kalux path/to/photo.jpg                                  # open an image
kalux --version                                          # print version
kalux --bench-startup photo.jpg [--bench-out out.jsonl]  # JSON startup + RAM timings
kalux --gen-test-image test.jpg 4000 3000                # deterministic 12 MP test JPEG
kalux --screenshot photo.jpg out.png [--light] [--help]  # offscreen PNG render
```

On Linux, you can override the auto-detected GTK dark/light theme with `KALUX_THEME=dark` or `KALUX_THEME=light`.

## Building

Requires Rust 1.88+ (edition 2024).

### Linux (Wayland / CachyOS + Niri)

```bash
cargo build --release -p kalux-app   # → target/release/kalux (~1.8 MiB, zero GUI .so deps)
cargo test --workspace

# Install ~/.local/bin/kalux, desktop entry, and icon:
./packaging/linux/install.sh
```

To make Kalux your default image viewer:

```bash
xdg-mime default kalux.desktop image/jpeg image/png image/gif image/webp image/bmp image/tiff
```

Kalux sets `app_id = "kalux"` and requests server-side decorations so Niri's `prefer-no-csd` tiles it cleanly with zero titlebar bloat. If you prefer opening Kalux as a centered floating window in Niri, add to `~/.config/niri/config.kdl`:

```kdl
window-rule {
    match app-id=r#"^kalux$"#
    open-floating true
}
```

### Windows

Download `kalux-<version>-windows-x64-setup.exe` from the [Releases](https://github.com/Aitemaa/Kalux/releases) page and run it. No admin rights are needed: it installs for your user into `%LOCALAPPDATA%\Programs\Kalux` (choose "for all users" on the first page to install into Program Files instead). It adds:

- a Start menu entry, so **Kalux** shows up in Windows search and `Win+R` (`kalux`)
- optional: a desktop shortcut and an **Open with** entry for JPEG, PNG, GIF, WebP, BMP, ICO, TIFF, HEIC, AVIF, and JXL image files
- an uninstaller in **Settings → Apps**

A portable zip (`kalux-<version>-windows-x64.zip`) and standalone `kalux.exe` are attached to each release as well. To build from source:

```powershell
cargo build --release -p kalux-app   # → target\release\kalux.exe (~647 KB)
cargo test --workspace
```

To build the Inno Setup 6 installer locally on Windows:

```powershell
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DAppVersion=0.1.0 /DSourceDir="$PWD\target\release" /DOutputDir="$PWD\target\release" packaging\windows\kalux.iss
```

## License

MIT — see [LICENSE](LICENSE).
