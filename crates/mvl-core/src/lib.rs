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
//! Phase 3 note: the DSP itself lives here now — `pyin` (D7), `psola` (D8),
//! `formant` (D9), `air` (D10), composed by `pipeline` (D11 order:
//! pitch → formant → air). `synth` provides the deterministic fixtures the
//! accuracy gates and the evidence corpus run on.

#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod lpc;
pub mod psola;
pub mod pyin;
pub mod synth;

/// Crate version, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
