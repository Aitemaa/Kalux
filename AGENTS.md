# AGENTS.md: rules for anyone (human or AI) writing code in Kalux

Read [docs/PRD.md](docs/PRD.md) first. The performance budgets in §5 are release blockers.

## Crates and ownership

| Crate | Purpose | Builds on Linux? |
|---|---|---|
| `kalux-core` | Image types, playlist and natural sort, view math, byte LRU, keymap. **No deps.** | yes (tests run on Linux) |
| `kalux-decode` | `decode_file` (zune-jpeg on all platforms, WIC on Windows) + `ImageStore` worker pool / cache / preload | yes (WIC parts behind `cfg(windows)`) |
| `kalux-app` | `kalux.exe`: raw Win32 window, Direct2D renderer, input, `--bench-startup` | `cargo check --target x86_64-pc-windows-msvc` only |

The public APIs of `kalux-core` and `kalux-decode` are the **contract** between crates. Do not change a public signature without updating every consumer in the same change.

## Hard rules
- **Pure Rust.** No `cc`, `cmake`, C libraries, WebView, `winit` or `wgpu` (CI enforces this). Windows APIs only through the `windows` crate (0.62).
- **New third-party dependencies need a reason.** Add them to `[workspace.dependencies]` in the root `Cargo.toml` and explain why in the PR. Binary size is a budget (≤ 4 MiB target).
- **No async runtime.** Use `std::thread` + `crossbeam-channel`.
- **Never block the UI thread** on decode, directory listing or EXIF.
- **Pixel format everywhere:** BGRA8 premultiplied, `stride = width * 4`.
- No `unwrap()` / `expect()` on paths reachable from user input (files, args). Show the error in the window instead.
- `unsafe` blocks get a one-line `// SAFETY:` comment.
- Edition 2024; must pass `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`.

## Local verification (Linux dev box)
```bash
cargo test -p kalux-core -p kalux-decode
cargo clippy --workspace --all-targets --target x86_64-pc-windows-msvc -- -D warnings   # type-checks Win32 code
cargo fmt --all --check
```
Real Windows build, tests and the startup benchmark run in GitHub Actions (`.github/workflows/ci.yml`) on every PR.

## Git
- Work on a feature branch; open a PR against `main`; CI must be green.
- Conventional-ish commit subjects: `core: …`, `decode: …`, `app: …`, `ci: …`, `docs: …`.
