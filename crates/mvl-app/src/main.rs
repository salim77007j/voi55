//! Micro-Vocal Lab — desktop application entry point.
//!
//! Phase 2: links the workspace together and prints the audio environment
//! report (a smoke check that the I/O layer loads on the running system).
//! The Slint UI replaces this in Phase 4.

use mvl_audio::{PREFERRED_SAMPLE_RATE, list_input_devices};
use mvl_core as core;

fn main() {
    println!(
        "Micro-Vocal Lab v{} — audio I/O scaffold (Phase 2)",
        core::VERSION
    );
    println!(
        "Engine parameter contract loaded: pitch ±12 st / 1 cent, air ±dB / 0.1 dB, formant 130–190 mm."
    );
    println!(
        "Requesting {} Hz / f32 capture (device maximum used when 192 kHz is unavailable).",
        PREFERRED_SAMPLE_RATE
    );
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
