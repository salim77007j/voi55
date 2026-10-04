//! Error type shared by the whole `mvl-audio` layer.

use thiserror::Error;

/// Errors produced by capture, playback, and file I/O.
#[derive(Debug, Error)]
pub enum AudioError {
    /// No usable audio device (input or output) was found.
    #[error("no audio device found")]
    NoDevice,

    /// The device/driver rejected the requested configuration; `0` carries
    /// what was actually available or attempted.
    #[error("unsupported stream configuration: {0}")]
    UnsupportedConfig(String),

    /// The audio stream failed while running (device unplugged, driver
    /// reset, ...).
    #[error("audio stream error: {0}")]
    Stream(String),

    /// A file could not be parsed or written (container/codec level).
    #[error("audio file error: {0}")]
    File(String),

    /// Device-level failure from `cpal` (enumeration, stream build, ...).
    #[error("audio device error: {0}")]
    Device(#[from] cpal::Error),

    /// Underlying WAV (hound) error.
    #[error("WAV error: {0}")]
    Wav(#[from] hound::Error),

    /// Codec-level failure (MP3 decode/encode) with context.
    #[error("codec error: {0}")]
    Codec(String),

    /// Failure surfaced by the symphonia decode stack.
    #[error("decode error: {0}")]
    Symphonia(#[from] symphonia::core::errors::Error),

    /// Filesystem / std I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenient alias used across `mvl-audio`.
pub type Result<T> = std::result::Result<T, AudioError>;
