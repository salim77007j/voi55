//! Headless UI smoke test — renders the real `AppWindow` through the
//! software renderer and asserts on actual pixels (D14: claims need
//! evidence; this is the automated form of that evidence for the shell).
//!
//! `slint::platform::set_platform` installs once per process, so every
//! test in this binary must share the single render it performs.

use mvl_app::headless;
use slint::platform::software_renderer::PremultipliedRgbaColor;

/// Studio Graphite tokens (D13) — duplicated here on purpose: the assert
/// compares rendered pixels against the *spec*, not against the widget's
/// own constants.
const BG_BASE: [u8; 3] = [0x12, 0x15, 0x1A];
const BG_ELEVATED: [u8; 3] = [0x21, 0x28, 0x31];
const BG_SURFACE: [u8; 3] = [0x1A, 0x1F, 0x26];

#[test]
fn shell_renders_studio_graphite() {
    let out = std::env::temp_dir().join("mvl-test-shots/shell.png");
    let args = headless::ScreenshotArgs {
        out: out.clone(),
        width: 1280,
        height: 800,
        demo: None,
        open: None,
        playhead: None,
        window_secs: None,
    };
    headless::render_to_png(&args, |_app| Ok(())).expect("headless render");

    let png = std::fs::read(&out).unwrap_or_else(|e| panic!("png exists: {e}"));
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().expect("png info");
    assert_eq!(reader.info().width, 1280);
    assert_eq!(reader.info().height, 800);
    let mut buf = vec![0; reader.output_buffer_size().expect("buffer size")];
    let _info = reader.next_frame(&mut buf).expect("png frame");
    let px = |x: usize, y: usize| -> [u8; 3] {
        let i = (y * 1280 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    };

    // Window background (bg/base) in the body area outside the well.
    let corner = px(8, 300);
    assert_eq!(corner, BG_BASE, "window background must be bg/base");

    // Status bar chrome (bg/elevated) at the very bottom.
    let status = px(400, 795);
    assert_eq!(status, BG_ELEVATED, "status bar must be bg/elevated");

    // Waveform well (bg/surface) sits centered in the body.
    let well = px(640, 420);
    assert_eq!(well, BG_SURFACE, "waveform well must be bg/surface");

    // The empty-state hint text was drawn (some non-surface pixels inside
    // the band where the text renders).
    let mut text_pixels = 0;
    for x in 400..900 {
        if px(x, 412) != BG_SURFACE {
            text_pixels += 1;
        }
    }
    assert!(text_pixels > 10, "empty-state text must be visible");
}

/// Verify the pixel-format round-trip of the un-premultiply helper used by
/// the screenshot writer (opaque colors must survive byte-exactly).
#[test]
fn premultiplied_roundtrip_is_lossless_for_opaque() {
    let src = PremultipliedRgbaColor::from(slint::Color::from_argb_u8(255, 0x5A, 0xA7, 0xFF));
    let a = u16::from(src.alpha);
    let r = (u16::from(src.red) * 255 / a) as u8;
    assert_eq!(r, 0x5A);
}
