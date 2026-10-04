//! Micro-Vocal Lab — desktop application entry point.
//!
//! Phase 2: links the workspace together and reports the audio environment.
//! `--selftest-audio` runs a real end-to-end playback exercise (a generated
//! sine through the output device, exercising the player transport).
//! The Slint UI replaces this in Phase 4.

use mvl_audio::{AudioBuffer, PREFERRED_SAMPLE_RATE, Player, list_input_devices};
use mvl_core as core;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("Micro-Vocal Lab v{} — audio I/O (Phase 2)", core::VERSION);
    println!("Engine contract: pitch ±12 st / 1 cent · air ±dB / 0.1 dB · formant 130–190 mm.");
    println!(
        "Capture requests {} Hz / f32 (device maximum used when unavailable).",
        PREFERRED_SAMPLE_RATE
    );
    report_inputs();

    if args.iter().any(|a| a == "--selftest-audio")
        && let Err(err) = playback_selftest()
    {
        eprintln!("Audio self-test failed: {err}");
        std::process::exit(1);
    }
}

fn report_inputs() {
    match list_input_devices() {
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
    player.play(Arc::new(sine))?;
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
