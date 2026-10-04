//! Micro-Vocal Lab audio I/O layer.
//!
//! Responsibilities (architecture plan D2–D6):
//!
//! - [`buffer`] — the shared interleaved `f32` [`AudioBuffer`] type.
//! - [`capture`] — microphone recording via `cpal`, requesting 192 kHz /
//!   32-bit float and honestly reporting what the device actually granted.
//! - [`player`] — play/pause/stop playback via `cpal`.
//! - [`wav`] — lossless WAV import/export via `hound` (f32/i16/i24, up to
//!   192 kHz).
//! - [`mp3`] — MP3 import via `symphonia`, MP3 export via LAME
//!   (`mp3lame-encoder`), with automatic resampling for rates MP3 cannot
//!   carry (e.g. 96/192 kHz → 48 kHz).
//!
//! Error handling policy: no `unwrap()`/`panic!` on user or audio data
//! anywhere in this crate — every fallible path returns
//! [`AudioResultResult`](`Result`) with [`AudioError`].

pub mod buffer;
pub mod capture;
pub mod devices;
pub mod error;
pub mod mp3;
pub mod player;
pub mod resample;
pub mod wav;

pub use buffer::AudioBuffer;
pub use capture::{CaptureInfo, CapturePlan, ConfigCandidate, PREFERRED_SAMPLE_RATE, Recorder};
pub use devices::{DeviceInfo, list_input_devices};
pub use error::{AudioError, Result};
pub use mp3::{DEFAULT_MP3_BITRATE, MP3_BITRATES, MP3_SAMPLE_RATES, export_mp3, import_mp3};
pub use player::{Player, Transport};
pub use wav::{WavBitDepth, export_wav, import_wav};
