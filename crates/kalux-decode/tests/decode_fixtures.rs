//! Integration tests against real image fixtures in `tests/fixtures/`.

use std::path::{Path, PathBuf};

use kalux_core::ImageFormat;
use kalux_decode::{DecodeError, decode_file};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn pixel_at(img: &kalux_core::DecodedImage, x: u32, y: u32) -> [u8; 4] {
    let idx = ((y * img.width + x) * 4) as usize;
    img.frames[0].pixels[idx..idx + 4].try_into().unwrap()
}

fn assert_quadrants_oriented(img: &kalux_core::DecodedImage, label: &str) {
    assert_eq!(
        (img.width, img.height),
        (32, 16),
        "{label}: wrong dimensions"
    );
    assert_eq!(
        (img.source_width, img.source_height),
        (32, 16),
        "{label}: wrong source dimensions"
    );
    assert_eq!(img.format, ImageFormat::Jpeg);
    assert!(!img.is_animated());
    assert!(!img.is_downscaled());

    // Sample interior of each quadrant to avoid JPEG block-boundary artifacts.
    // BGRA layout: [B, G, R, A]
    let tl = pixel_at(img, 4, 4); // Red:   R > 200, G < 60, B < 60
    let tr = pixel_at(img, 24, 4); // Green: G > 200, R < 60, B < 60
    let bl = pixel_at(img, 4, 12); // Blue:  B > 200, R < 60, G < 60
    let br = pixel_at(img, 24, 12); // White: B, G, R > 200

    assert!(
        tl[2] > 200 && tl[1] < 60 && tl[0] < 60 && tl[3] == 255,
        "{label}: TL expected red, got BGRA {tl:?}"
    );
    assert!(
        tr[1] > 200 && tr[2] < 60 && tr[0] < 60 && tr[3] == 255,
        "{label}: TR expected green, got BGRA {tr:?}"
    );
    assert!(
        bl[0] > 200 && bl[2] < 60 && bl[1] < 60 && bl[3] == 255,
        "{label}: BL expected blue, got BGRA {bl:?}"
    );
    assert!(
        br[0] > 200 && br[1] > 200 && br[2] > 200 && br[3] == 255,
        "{label}: BR expected white, got BGRA {br:?}"
    );
}

#[test]
fn jpeg_exif_orientations_decode_consistently() {
    for name in [
        "orient_1.jpg",
        "orient_3.jpg",
        "orient_6.jpg",
        "orient_8.jpg",
    ] {
        let img = decode_file(&fixture(name), None).expect("JPEG decode should succeed");
        assert_quadrants_oriented(&img, name);
    }
}

#[test]
fn jpeg_downscale_preserves_aspect_and_orientation() {
    // orient_6.jpg is stored 16x32 and becomes 32x16 after orientation.
    // Requesting max_size (16, 16) must yield 16x8 with source_width=32, source_height=16.
    let img = decode_file(&fixture("orient_6.jpg"), Some((16, 16)))
        .expect("downscaled rotated JPEG should decode");
    assert_eq!((img.width, img.height), (16, 8));
    assert_eq!((img.source_width, img.source_height), (32, 16));
    assert!(img.is_downscaled());

    // Never upscale when max_size exceeds source dimensions.
    let same = decode_file(&fixture("orient_1.jpg"), Some((500, 500)))
        .expect("JPEG with larger max_size should decode");
    assert_eq!((same.width, same.height), (32, 16));
    assert!(!same.is_downscaled());
}

#[test]
fn grayscale_jpeg_decodes_to_bgra() {
    let img = decode_file(&fixture("grayscale.jpg"), None).expect("grayscale JPEG should decode");
    assert_eq!((img.width, img.height), (32, 16));
    let px = pixel_at(&img, 10, 8);
    assert!((170..=190).contains(&px[0]));
    assert_eq!(px[0], px[1]);
    assert_eq!(px[1], px[2]);
    assert_eq!(px[3], 255);
}

#[test]
fn missing_file_and_display_errors() {
    let err = decode_file(&fixture("does_not_exist.jpg"), None).unwrap_err();
    assert!(matches!(err, DecodeError::NotFound));
    assert!(!err.to_string().is_empty());

    let hint_err = DecodeError::Unsupported {
        hint: Some("Install HEIF Image Extensions from the Microsoft Store".into()),
    };
    assert!(hint_err.to_string().contains("HEIF Image Extensions"));
}

#[test]
fn decodes_png_with_premultiplied_alpha() {
    let img = decode_file(&fixture("alpha.png"), None).expect("PNG decode should succeed");
    assert_eq!(img.format, ImageFormat::Png);
    assert_eq!((img.width, img.height), (16, 16));
    assert!(!img.is_animated());

    // Left half: RGBA (255, 0, 0, 128) -> premultiplied BGRA (0, 0, ~128, 128)
    let left = pixel_at(&img, 2, 8);
    assert_eq!(left[0], 0);
    assert_eq!(left[1], 0);
    assert!(
        (126..=130).contains(&left[2]),
        "expected premultiplied R≈128, got {left:?}"
    );
    assert_eq!(left[3], 128);

    // Right half: fully transparent
    let right = pixel_at(&img, 12, 8);
    assert_eq!(right, [0, 0, 0, 0]);
}

#[test]
fn decodes_animated_gif_with_disposal_and_delays() {
    let img = decode_file(&fixture("animated.gif"), None).expect("GIF decode should succeed");
    assert_eq!(img.format, ImageFormat::Gif);
    assert_eq!((img.width, img.height), (16, 16));
    assert!(img.is_animated());
    assert_eq!(img.frames.len(), 3);
    assert_eq!(img.loop_count, 0);

    // Delays: 50 ms, 10 ms -> normalized to 100 ms, 100 ms
    assert_eq!(img.frames[0].delay_ms, 50);
    assert_eq!(img.frames[1].delay_ms, 100);
    assert_eq!(img.frames[2].delay_ms, 100);

    let frame_px = |f: usize, x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * img.width + x) * 4) as usize;
        img.frames[f].pixels[idx..idx + 4].try_into().unwrap()
    };

    // Frame 0: solid red
    assert_eq!(frame_px(0, 1, 1), [0, 0, 255, 255]);
    // Frame 1: red background retained (disposal=1 on frame 0), green center overlaid
    assert_eq!(frame_px(1, 1, 1), [0, 0, 255, 255]);
    assert_eq!(frame_px(1, 6, 6), [0, 255, 0, 255]);
    // Frame 2: frame 1 had disposal=2 (restore background), so top-left is transparent,
    // bottom-right is blue!
    assert_eq!(frame_px(2, 1, 1), [0, 0, 0, 0]);
    assert_eq!(frame_px(2, 12, 12), [255, 0, 0, 255]);
}
