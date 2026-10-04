//! The shared in-memory audio type: interleaved 32-bit float samples.
//!
//! Every I/O path (capture, WAV, MP3, player) and, from Phase 3 on, the DSP
//! engine exchange [`AudioBuffer`] values. Invariants enforced at
//! construction: positive sample rate, at least one channel, and a sample
//! count that is an exact multiple of the channel count.

use crate::error::{AudioError, Result};

/// Interleaved `f32` audio with its format metadata.
///
/// Samples are stored interleaved (`L R L R ...`), the layout every callback
/// and codec in this crate speaks, so no conversion is needed at boundaries.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
    sample_rate: u32,
    channels: u16,
    samples: Vec<f32>,
}

impl AudioBuffer {
    /// Allocates an empty buffer at the given format.
    ///
    /// # Errors
    /// Fails when `sample_rate == 0` or `channels == 0`.
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        validate_format(sample_rate, channels)?;
        Ok(Self {
            sample_rate,
            channels,
            samples: Vec::new(),
        })
    }

    /// Wraps an existing interleaved sample vector.
    ///
    /// # Errors
    /// Fails on invalid format or when `samples.len()` is not a multiple of
    /// `channels` (which would mean a torn frame).
    pub fn from_interleaved(sample_rate: u32, channels: u16, samples: Vec<f32>) -> Result<Self> {
        let buf = Self::new(sample_rate, channels)?;
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return Err(AudioError::UnsupportedConfig(format!(
                "sample count {} is not a multiple of {} channels",
                samples.len(),
                channels
            )));
        }
        Ok(Self { samples, ..buf })
    }

    /// Sample rate in Hz (up to 192 kHz and beyond).
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count (1 = mono, 2 = stereo).
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Read access to the interleaved samples.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Mutable access to the interleaved samples (DSP staging).
    pub fn samples_mut(&mut self) -> &mut [f32] {
        &mut self.samples
    }

    /// Number of complete frames (one frame = one sample per channel).
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }

    /// Duration in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / f64::from(self.sample_rate)
    }

    /// Borrow of one frame's channel samples, or `None` past the end.
    pub fn frame(&self, index: usize) -> Option<&[f32]> {
        let ch = usize::from(self.channels);
        let start = index.checked_mul(ch)?;
        self.samples.get(start..start + ch)
    }

    /// Appends interleaved samples (used live by the capture callback).
    ///
    /// # Errors
    /// Fails when the slice is not a multiple of the channel count.
    pub fn append_interleaved(&mut self, samples: &[f32]) -> Result<()> {
        if !samples.len().is_multiple_of(usize::from(self.channels)) {
            return Err(AudioError::UnsupportedConfig(format!(
                "appended slice length {} is not a multiple of {} channels",
                samples.len(),
                self.channels
            )));
        }
        self.samples.extend_from_slice(samples);
        Ok(())
    }

    /// Truncates to at most `frames` frames (used to drop a torn final frame
    /// from a live capture).
    pub fn truncate_frames(&mut self, frames: usize) {
        let keep = frames.min(self.frames()) * usize::from(self.channels);
        self.samples.truncate(keep);
    }

    /// Convenience constructor of pure silence (tests, gap filling).
    ///
    /// # Errors
    /// Fails on invalid format.
    pub fn silence(duration_secs: f64, sample_rate: u32, channels: u16) -> Result<Self> {
        let mut buf = Self::new(sample_rate, channels)?;
        let frames = (duration_secs * f64::from(sample_rate)).round() as usize;
        buf.samples = vec![0.0; frames * usize::from(channels)];
        Ok(buf)
    }

    /// Deterministic sine test signal in `[−1, 1)` (tests and demo audio).
    ///
    /// # Errors
    /// Fails on invalid format.
    pub fn sine(duration_secs: f64, sample_rate: u32, channels: u16, freq_hz: f64) -> Result<Self> {
        let mut buf = Self::new(sample_rate, channels)?;
        let frames = (duration_secs * f64::from(sample_rate)).round() as usize;
        let ch = usize::from(channels);
        let mut samples = Vec::with_capacity(frames * ch);
        for n in 0..frames {
            let value = (2.0 * std::f64::consts::PI * freq_hz * n as f64 / f64::from(sample_rate))
                .sin() as f32
                * 0.8;
            samples.extend(std::iter::repeat_n(value, ch));
        }
        buf.samples = samples;
        Ok(buf)
    }
}

fn validate_format(sample_rate: u32, channels: u16) -> Result<()> {
    if sample_rate == 0 {
        return Err(AudioError::UnsupportedConfig(
            "sample rate must be positive".into(),
        ));
    }
    if channels == 0 {
        return Err(AudioError::UnsupportedConfig(
            "channel count must be at least 1".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_and_reports_format() {
        let buf = AudioBuffer::from_interleaved(192_000, 2, vec![0.0; 192_000]).unwrap();
        assert_eq!(buf.sample_rate(), 192_000);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.frames(), 96_000);
        assert_eq!(buf.duration_secs(), 0.5);
    }

    #[test]
    fn rejects_torn_frames() {
        let err = AudioBuffer::from_interleaved(48_000, 2, vec![0.0; 7]);
        assert!(err.is_err());
    }

    #[test]
    fn rejects_zero_format() {
        assert!(AudioBuffer::new(0, 2).is_err());
        assert!(AudioBuffer::new(48_000, 0).is_err());
    }

    #[test]
    fn frame_slicing_is_channel_exact() {
        let buf = AudioBuffer::from_interleaved(48_000, 2, vec![1.0, 2.0, 3.0, 4.0]).unwrap();
        assert_eq!(buf.frame(0), Some(&[1.0, 2.0][..]));
        assert_eq!(buf.frame(1), Some(&[3.0, 4.0][..]));
        assert_eq!(buf.frame(2), None);
    }

    #[test]
    fn append_and_truncate_roundtrip() {
        let mut buf = AudioBuffer::new(48_000, 1).unwrap();
        buf.append_interleaved(&[0.1, 0.2, 0.3]).unwrap();
        assert_eq!(buf.frames(), 3);
        buf.truncate_frames(2);
        assert_eq!(buf.frames(), 2);
        // A stereo buffer rejects slices that would tear a frame.
        let mut stereo = AudioBuffer::new(48_000, 2).unwrap();
        assert!(stereo.append_interleaved(&[0.0, 0.0, 0.0]).is_err());
    }

    #[test]
    fn sine_is_deterministic_and_bounded() {
        let a = AudioBuffer::sine(0.01, 48_000, 2, 440.0).unwrap();
        let b = AudioBuffer::sine(0.01, 48_000, 2, 440.0).unwrap();
        assert_eq!(a, b);
        assert!(a.samples().iter().all(|s| s.is_finite() && s.abs() <= 1.0));
    }
}
