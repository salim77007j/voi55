//! Micro-Vocal Lab DSP engine.
//!
//! `mvl-core` hosts the three precision voice engines described in
//! `docs/ARCHITECTURE_PLAN.md` (D7–D10):
//!
//! - **Pitch** — pYIN tracking + TD-PSOLA shifting (±12 st, 1-cent steps,
//!   duration- and formant-preserving).
//! - **Formant** — LPC source–filter decomposition with allpass Bark-scale
//!   envelope warping, controlled in millimetres of vocal-tract length.
//! - **Air / Breath** — STFT harmonic/residual decomposition with a 0.1 dB
//!   gain on the residual path.
//!
//! The crate is deliberately free of audio-I/O dependencies: it consumes and
//! produces plain `f32` sample buffers so it stays unit-testable and portable.
//!
//! Phase 2 note: this crate currently defines the *parameter contract* the UI
//! and the audio layer bind to. The DSP itself lands in Phase 3 with the
//! accuracy gates from D7.

pub mod engine;

/// Crate version, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
