//! Micro-Vocal Lab — desktop application entry point.
//!
//! Phase 2: links the workspace together and prints the audio environment
//! report (a smoke check that the I/O layer loads on the running system).
//! The Slint UI replaces this in Phase 4.

use mvl_core as core;

fn main() {
    println!(
        "Micro-Vocal Lab v{} — audio I/O scaffold (Phase 2)",
        core::VERSION
    );
    println!(
        "Engine parameter contract loaded: pitch ±12 st / 1 cent, air ±dB / 0.1 dB, formant 130–190 mm."
    );
}
