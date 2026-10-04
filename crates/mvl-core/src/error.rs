//! Error type for the DSP engine (`mvl-core`).
//!
//! The engine never panics on user/audio data: every fallible step
//! (FFT planning/processing, degenerate analysis inputs) returns a
//! [`CoreError`] that the caller surfaces in the UI.

use thiserror::Error;

/// Errors produced by the analysis and render paths.
#[derive(Debug, Error)]
pub enum CoreError {
    /// An FFT plan or transform failed (realfft/rustfft propagated).
    #[error("FFT processing failed: {0}")]
    Fft(String),

    /// Analysis could not proceed (empty input, degenerate configuration).
    #[error("analysis failed: {0}")]
    Analysis(String),
}

/// Convenience alias used across `mvl-core`.
pub type Result<T> = std::result::Result<T, CoreError>;
