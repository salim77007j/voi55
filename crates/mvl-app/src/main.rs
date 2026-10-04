//! Micro-Vocal Lab — desktop application entry point.
//!
//! Modes:
//! - **GUI** (default): the Slint window (Studio Graphite) on the platform
//!   backend (winit/femtovg on desktop). `--open FILE` loads audio first.
//! - `--screenshot --out P [flags]`: renders the real UI headlessly into a
//!   PNG (evidence pipeline, D14; also drives `tests/screenshot.rs`).
//! - `--selftest-audio`: legacy Phase 2 playback self-test, kept for CI.
//!
//! All logic lives in the `mvl-app` library; this binary only parses the
//! command line.

use mvl_app::{app, headless};
use mvl_audio::{AudioBuffer, Player};
use slint::ComponentHandle;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--screenshot") {
        if let Err(err) = screenshot_mode(&args) {
            eprintln!("Screenshot failed: {err}");
            std::process::exit(1);
        }
        return;
    }

    println!(
        "Micro-Vocal Lab v{} — Slint UI (Phase 4)",
        mvl_core::VERSION
    );
    println!("Engine contract: pitch ±12 st / 1 cent · air ±dB / 0.1 dB · formant 130–190 mm.");
    println!(
        "Capture requests {} Hz / f32 (device maximum used when unavailable).",
        mvl_audio::PREFERRED_SAMPLE_RATE
    );
    report_inputs();

    if args.iter().any(|a| a == "--selftest-audio")
        && let Err(err) = playback_selftest()
    {
        eprintln!("Audio self-test failed: {err}");
        std::process::exit(1);
    }

    if let Err(err) = gui_mode(&args) {
        eprintln!("GUI failed to start: {err}");
        std::process::exit(1);
    }
}

/// Loads audio per the CLI flags (`--demo synth` / `--open PATH`).
fn load_flagged(
    app: &app::App,
    demo: Option<&str>,
    open: Option<&std::path::Path>,
) -> Result<(), String> {
    match (demo, open) {
        (Some("synth"), _) => {
            let buf = mvl_app::session::demo_vocal().map_err(|e| e.to_string())?;
            app.load_audio(buf, "demo-vocal (synth)");
            Ok(())
        }
        (Some(other), _) => Err(format!("unknown --demo kind: {other} (only 'synth')")),
        (_, Some(path)) => {
            let buffer = mvl_app::dialogs::load_any(path)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            app.load_audio(buffer, &name);
            Ok(())
        }
        (None, None) => Ok(()),
    }
}

fn screenshot_mode(args: &[String]) -> Result<(), String> {
    let parsed = headless::parse_args(args)?;
    headless::render_to_png(&parsed, |app| {
        load_flagged(app, parsed.demo.as_deref(), parsed.open.as_deref())?;
        if let Some(secs) = parsed.window_secs {
            app.set_view_window_secs(secs);
        }
        if let Some(secs) = parsed.playhead {
            app.set_playhead(secs);
        }
        if parsed.pitch.is_some() || parsed.air.is_some() || parsed.formant.is_some() {
            app.set_params_and_render(
                parsed.pitch.unwrap_or(0.0),
                parsed.air.unwrap_or(0.0),
                parsed.formant.unwrap_or(175.0),
            );
        }
        if parsed.preview {
            app.toggle_preview();
        }
        Ok(())
    })?;
    println!("Screenshot written: {}", parsed.out.display());
    Ok(())
}

fn gui_mode(args: &[String]) -> Result<(), String> {
    let app = app::App::new().map_err(|e| e.to_string())?;
    // --open PATH (GUI): load before showing so the window appears ready.
    let open = args
        .iter()
        .position(|a| a == "--open")
        .and_then(|i| args.get(i + 1));
    if let Some(path) = open {
        load_flagged(&app, None, Some(std::path::Path::new(path)))?;
    }
    app.window()
        .show()
        .map_err(|e| format!("cannot show window: {e}"))?;
    slint::run_event_loop().map_err(|e| format!("event loop: {e}"))
}

fn report_inputs() {
    match mvl_audio::list_input_devices() {
        Ok(devices) if devices.is_empty() => println!("No input devices found on this system."),
        Ok(devices) => {
            for d in &devices {
                println!("  input: {} (max {} Hz)", d.name, d.max_sample_rate());
            }
        }
        Err(err) => println!("Audio device enumeration failed: {err}"),
    }
}

/// Real playback exercise: connect, play a 1 s 440 Hz sine, observe the
/// transport and position advance, then stop. Exits non-zero on failure.
fn playback_selftest() -> Result<(), mvl_audio::AudioError> {
    let mut player = Player::connect()?;
    let (rate, channels) = player.output_format();
    println!("  output: {rate} Hz / {channels} ch (f32)");

    let sine = AudioBuffer::sine(1.0, 48_000, channels.clamp(1, 2), 440.0)?;
    player.play(std::sync::Arc::new(sine))?;
    println!("  transport after play(): {:?}", player.transport());

    let start = Instant::now();
    let mut saw_position_advance = false;
    let mut last = player.position_secs();
    while start.elapsed() < Duration::from_millis(700) {
        std::thread::sleep(Duration::from_millis(50));
        let now = player.position_secs();
        if now > last {
            saw_position_advance = true;
        }
        last = now;
    }
    if player.transport() == mvl_audio::Transport::Stopped
        && start.elapsed() < Duration::from_secs(2)
    {
        // The 1 s buffer may already have finished on fast null devices.
        println!("  playback completed before poll window ended (OK)");
    } else if !saw_position_advance {
        return Err(mvl_audio::AudioError::Stream(
            "playback position did not advance".into(),
        ));
    }
    println!("  position advanced to {:.3} s (OK)", last);

    player.pause()?;
    println!("  transport after pause(): {:?}", player.transport());
    player.stop()?;
    println!("  transport after stop():  {:?}", player.transport());
    println!("Audio playback self-test passed.");
    Ok(())
}
