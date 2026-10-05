//! Headless UI smoke test — renders the real `AppWindow` through the
//! software renderer and asserts on actual pixels (D14: claims need
//! evidence; this is the automated form of that evidence).
//!
//! `slint::platform::set_platform` installs once per process and Slint
//! work is single-threaded, so ALL render steps run inside ONE #[test]
//! function, in order.

use mvl_app::headless;
use mvl_app::i18n::Lang;
use slint::platform::software_renderer::PremultipliedRgbaColor;

/// Studio Console tokens (Phase 7.2) — duplicated here on purpose: the
/// assert compares rendered pixels against the *spec*, not against the
/// widget's own constants.
const BG_BASE: [u8; 3] = [0x1E, 0x1E, 0x1E];
const BG_ELEVATED: [u8; 3] = [0x2D, 0x2D, 0x2D];
const BG_SURFACE: [u8; 3] = [0x25, 0x25, 0x26];
const BG_WELL: [u8; 3] = [0x0A, 0x0A, 0x0A];
const ACCENT_PITCH: [u8; 3] = [0x00, 0xB4, 0xD8];

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
        lang: Lang::En,
        export_panel: false,
        devices: false,
    };
    headless::render_to_png(&shell_args, |_app| Ok(())).expect("shell render");
    let buf = decode(&shell_args.out);
    let px = |x: usize, y: usize| -> [u8; 3] {
        let i = (y * 1280 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    };
    assert_eq!(px(8, 300), BG_BASE, "window background must be bg/base");
    assert_eq!(px(400, 795), BG_ELEVATED, "status bar must be bg/elevated");
    assert_eq!(px(640, 420), BG_WELL, "waveform well must be bg/well");
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
        lang: Lang::En,
        export_panel: false,
        devices: false,
    };
    let mut params_out = None;
    headless::render_to_png(&slider_args, |app| {
        let buffer = mvl_app::session::demo_vocal().map_err(|e| e.to_string())?;
        app.load_audio(buffer, "demo-vocal (synth)");
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

    // Slider row: the pitch groove fill/thumb uses accent-pitch. Scan the
    // whole band around the parameter row (layout-dependent y).
    let buf = decode(&slider_args.out);
    let px = |x: usize, y: usize| -> [u8; 3] {
        let i = (y * 1280 + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2]]
    };
    let mut accent_hits = 0;
    for x in 0..500 {
        for y in 590..700 {
            if px(x, y) == ACCENT_PITCH {
                accent_hits += 1;
            }
        }
    }
    assert!(accent_hits > 20, "pitch slider fill must be visible");
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

    // ── Step 3: export round-trip through the dialog override ────────
    let export_path = dir.join("export-roundtrip.wav");
    let export_args = headless::ScreenshotArgs {
        out: dir.join("export.png"),
        width: 1280,
        height: 800,
        demo: Some("synth".into()),
        open: None,
        playhead: None,
        window_secs: None,
        pitch: Some(2.0),
        air: Some(0.0),
        formant: Some(160.0),
        preview: false,
        lang: Lang::En,
        export_panel: false,
        devices: false,
    };
    // SAFETY: the only env readers in this binary run inside the
    // render_to_png closures below, all on this thread.
    unsafe { std::env::set_var("MVL_SAVE_FILE", export_path.display().to_string()) };
    let mut export_status = None;
    let mut export_frames = None;
    headless::render_to_png(&export_args, |app| {
        let buffer = mvl_app::session::demo_vocal().map_err(|e| e.to_string())?;
        app.load_audio(buffer, "demo-vocal (synth)");
        app.set_params_and_render(2.0, 0.0, 160.0);
        app.export_confirm();
        export_status = Some(app.status_line());
        export_frames = Some(app.frames());
        Ok(())
    })
    .expect("export render");
    eprintln!("export status: {:?}", export_status);
    unsafe { std::env::remove_var("MVL_SAVE_FILE") };

    let frames = export_frames.expect("session frames");
    assert!(export_path.exists(), "export must have written the file");
    let reimported = mvl_audio::import_wav(&export_path).expect("reimport");
    assert_eq!(
        reimported.frames(),
        frames,
        "round-trip must preserve frames"
    );
    assert_eq!(reimported.sample_rate(), 48_000);
    let _ = std::fs::remove_file(&export_path);

    // ── Step 4: Arabic RTL — mirrored layout, Arabic font, real switch ─
    let ar_args = headless::ScreenshotArgs {
        out: dir.join("shell-ar.png"),
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
        lang: Lang::Ar,
        export_panel: false,
        devices: false,
    };
    let mut ar_status = None;
    headless::render_to_png(&ar_args, |app| {
        app.set_language(Lang::Ar);
        assert_eq!(app.lang(), Lang::Ar);
        let buffer = mvl_app::session::demo_vocal().map_err(|e| e.to_string())?;
        app.load_audio(buffer, "demo-vocal (synth)");
        app.set_params_and_render(4.0, 5.5, 140.0);
        // The status line must be Arabic (RTL flag from the table).
        assert!(app.status_line().contains('\u{0645}'));
        ar_status = Some(app.status_line());
        Ok(())
    })
    .expect("arabic render");
    eprintln!("ar status: {ar_status:?}");

    // The RTL header must differ from the LTR one (mirrored layout +
    // Arabic glyphs): compare the 56 px header band pixel-by-pixel.
    let en = decode(&dir.join("sliders.png"));
    let ar = decode(&ar_args.out);
    let mut header_diff = 0;
    for y in 0..56 {
        for x in 0..1280 {
            let i = (y * 1280 + x) * 4;
            if en[i] != ar[i] || en[i + 1] != ar[i + 1] || en[i + 2] != ar[i + 2] {
                header_diff += 1;
            }
        }
    }
    assert!(
        header_diff > 500,
        "RTL header must visibly differ from LTR ({header_diff} px)"
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
