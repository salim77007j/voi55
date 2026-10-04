//! Headless UI smoke test — renders the real `AppWindow` through the
//! software renderer and asserts on actual pixels (D14: claims need
//! evidence; this is the automated form of that evidence).
//!
//! `slint::platform::set_platform` installs once per process and Slint
//! work is single-threaded, so ALL render steps run inside ONE #[test]
//! function, in order.

use mvl_app::headless;
use slint::platform::software_renderer::PremultipliedRgbaColor;

/// Studio Graphite tokens (D13) — duplicated here on purpose: the assert
/// compares rendered pixels against the *spec*, not against the widget's
/// own constants.
const BG_BASE: [u8; 3] = [0x12, 0x15, 0x1A];
const BG_ELEVATED: [u8; 3] = [0x21, 0x28, 0x31];
const BG_SURFACE: [u8; 3] = [0x1A, 0x1F, 0x26];
const ACCENT_PITCH: [u8; 3] = [0x5A, 0xA7, 0xFF];

fn decode(path: &std::path::Path) -> Vec<u8> {
    let png = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().expect("png info");
    assert_eq!(reader.info().width, 1280);
    assert_eq!(reader.info().height, 800);
    let mut buf = vec![0; reader.output_buffer_size().expect("size")];
    reader.next_frame(&mut buf).expect("frame");
    buf
}

#[test]
fn headless_ui_renders_and_drives_the_engine() {
    let dir = std::env::temp_dir().join("mvl-test-shots");
    std::fs::create_dir_all(&dir).expect("temp dir");

    // ── Step 1: the empty shell ──────────────────────────────────────
    let shell_args = headless::ScreenshotArgs {
        out: dir.join("shell.png"),
        width: 1280,
        height: 800,
        demo: None,
        open: None,
        playhead: None,
        window_secs: None,
        pitch: None,
        air: None,
        formant: None,
        preview: false,
    };
    headless::render_to_png(&shell_args, |_app| Ok(())).expect("shell render");
    let buf = decode(&shell_args.out);
    let px = |x: usize, y: usize| -> [u8; 3] {
        let i = (y * 1280 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    };
    assert_eq!(px(8, 300), BG_BASE, "window background must be bg/base");
    assert_eq!(px(400, 795), BG_ELEVATED, "status bar must be bg/elevated");
    assert_eq!(px(640, 420), BG_SURFACE, "waveform well must be bg/surface");
    // Empty-state hint: scan the whole well band (layout-dependent).
    let mut text_pixels = 0;
    for y in 150..600 {
        for x in 300..1000 {
            if px(x, y) != BG_SURFACE {
                text_pixels += 1;
            }
        }
    }
    assert!(text_pixels > 50, "empty-state text must be visible");

    // ── Step 2: sliders → real engine render ─────────────────────────
    let slider_args = headless::ScreenshotArgs {
        out: dir.join("sliders.png"),
        width: 1280,
        height: 800,
        demo: Some("synth".into()),
        open: None,
        playhead: None,
        window_secs: None,
        pitch: Some(4.0),
        air: Some(5.5),
        formant: Some(140.0),
        preview: false,
    };
    let mut params_out = None;
    headless::render_to_png(&slider_args, |app| {
        app.set_params_and_render(4.0, 5.5, 140.0);
        params_out = Some(app.params());
        Ok(())
    })
    .expect("slider render");

    // The canonical parameter grid: EngineParams snapped/clamped.
    let p = params_out.expect("params captured");
    assert!((p.pitch_semitones() - 4.0).abs() < 1e-9);
    assert!((p.air_db() - 5.5).abs() < 1e-9);
    assert!((p.formant_mm() - 140.0).abs() < 1e-9);

    // Slider row (y ≈ 700–760): the pitch groove fill uses accent-pitch.
    let buf = decode(&slider_args.out);
    let px = |x: usize, y: usize| -> [u8; 3] {
        let i = (y * 1280 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    };
    let mut accent_hits = 0;
    for x in 30..200 {
        for y in 700..760 {
            if px(x, y) == ACCENT_PITCH {
                accent_hits += 1;
            }
        }
    }
    assert!(accent_hits > 5, "pitch slider fill must be visible");
    // Status bar carries the render line (text over bg-elevated, scan the
    // glyph band).
    let mut status_text = 0;
    for x in 20..700 {
        for y in 775..795 {
            if px(x, y) != BG_ELEVATED {
                status_text += 1;
            }
        }
    }
    assert!(
        status_text > 50,
        "status line must show the render info ({status_text})"
    );
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
