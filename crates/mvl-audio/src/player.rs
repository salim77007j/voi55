//! Playback via `cpal` — play/pause/stop (Phase 2 scope, architecture plan
//! D2/D6).
//!
//! The player owns an `f32` output stream on the default output device and
//! consumes [`AudioBuffer`]s. Buffers are transparently resampled to the
//! output device's rate (via [`crate::resample`]) and channel-mapped in the
//! callback. Transport transitions are safe to call from any thread; the
//! real-time callback only ever locks the state mutex briefly and never
//! allocates.

use crate::buffer::AudioBuffer;
use crate::error::{AudioError, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream};
use std::sync::{Arc, Mutex};

/// Transport state of the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Default)]
struct PlayState {
    buffer: Option<Arc<AudioBuffer>>,
    position: usize,
    transport: Transport,
    stream_error: Option<String>,
}

/// A connected output player.
pub struct Player {
    shared: Arc<Mutex<PlayState>>,
    /// Kept alive for the lifetime of the player; dropped on `Drop`.
    _stream: Option<Stream>,
    output_rate: u32,
    output_channels: u16,
}

impl Player {
    /// Connects to the platform's default output device.
    ///
    /// The output stream is `f32` (negotiated for mono/stereo). Playback
    /// rate is whatever the device grants; buffers are resampled on play.
    ///
    /// # Errors
    /// [`AudioError::NoDevice`] without an output device;
    /// [`AudioError::UnsupportedConfig`] when the device offers no `f32`
    /// mono/stereo configuration; [`AudioError::Device`] / [`AudioError::Stream`]
    /// for cpal failures.
    pub fn connect() -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoDevice)?;
        Self::connect_on_device(&device)
    }

    /// Connects to an explicit output device.
    ///
    /// # Errors
    /// See [`Player::connect`].
    pub fn connect_on_device(device: &Device) -> Result<Self> {
        // Prefer an f32 mono/stereo configuration closest to the device's
        // default rate.
        let mut best: Option<(cpal::SupportedStreamConfig, u32)> = None;
        for range in device.supported_output_configs()? {
            if range.sample_format() != SampleFormat::F32 || range.channels() > 2 {
                continue;
            }
            // Aim for 48 kHz (or the nearest rate the range allows).
            let rate = default_rate_hint().clamp(range.min_sample_rate(), range.max_sample_rate());
            let cost = rate.abs_diff(default_rate_hint());
            if best.as_ref().is_none_or(|(_, best_cost)| cost < *best_cost) {
                best = Some((range.with_sample_rate(rate), cost));
            }
        }
        let (supported, _) = best.ok_or_else(|| {
            AudioError::UnsupportedConfig(
                "output device offers no f32 mono/stereo configuration".into(),
            )
        })?;
        let output_rate = supported.sample_rate();
        let output_channels = supported.channels();

        let shared = Arc::new(Mutex::new(PlayState {
            transport: Transport::Stopped,
            ..PlayState::default()
        }));

        let stream = {
            let shared = Arc::clone(&shared);
            let error_shared = Arc::clone(&shared);
            let out_channels = output_channels;
            device
                .build_output_stream(
                    supported.config(),
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        let Ok(mut state) = shared.lock() else {
                            data.fill(0.0);
                            return;
                        };
                        // Zero-fill first: stopped/paused output is silence.
                        data.fill(0.0);
                        if state.transport != Transport::Playing {
                            return;
                        }
                        let Some(buffer) = state.buffer.clone() else {
                            return;
                        };
                        let written = copy_into_output(
                            &buffer,
                            state.position,
                            data,
                            u32::from(out_channels),
                        );
                        state.position += written;
                        if written == 0 || state.position >= buffer.frames() {
                            // End of material: stop, reset cursor.
                            state.transport = Transport::Stopped;
                            state.position = 0;
                        }
                    },
                    move |err| {
                        log::error!("playback stream error: {err}");
                        if let Ok(mut state) = error_shared.lock() {
                            state.stream_error = Some(err.to_string());
                        }
                    },
                    None,
                )
                .map_err(|e| AudioError::Stream(e.to_string()))?
        };
        stream
            .play()
            .map_err(|e| AudioError::Stream(e.to_string()))?;

        Ok(Self {
            shared,
            _stream: Some(stream),
            output_rate,
            output_channels,
        })
    }

    /// Output configuration actually granted by the device.
    pub fn output_format(&self) -> (u32, u16) {
        (self.output_rate, self.output_channels)
    }

    /// Starts (or restarts) playback of `buffer` from the beginning.
    ///
    /// The buffer is resampled to the output device rate when needed.
    ///
    /// # Errors
    /// [`AudioError::Stream`] when the transport is unavailable.
    pub fn play(&mut self, buffer: Arc<AudioBuffer>) -> Result<()> {
        let ready: Arc<AudioBuffer> = if buffer.sample_rate() == self.output_rate {
            buffer
        } else {
            Arc::new(AudioBuffer::from_interleaved(
                self.output_rate,
                buffer.channels(),
                crate::resample::resample_interleaved(
                    buffer.samples(),
                    usize::from(buffer.channels()),
                    buffer.sample_rate(),
                    self.output_rate,
                ),
            )?)
        };
        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("player state poisoned".into()))?;
        state.buffer = Some(ready);
        state.position = 0;
        state.transport = Transport::Playing;
        Ok(())
    }

    /// Pauses playback (position retained).
    ///
    /// # Errors
    /// [`AudioError::Stream`] when the transport is unavailable.
    pub fn pause(&self) -> Result<()> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("player state poisoned".into()))?;
        if state.transport == Transport::Playing {
            state.transport = Transport::Paused;
        }
        Ok(())
    }

    /// Resumes from the paused position.
    ///
    /// # Errors
    /// [`AudioError::Stream`] when the transport is unavailable.
    pub fn resume(&self) -> Result<()> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("player state poisoned".into()))?;
        if state.transport == Transport::Paused {
            state.transport = Transport::Playing;
        }
        Ok(())
    }

    /// Stops playback and rewinds to the beginning.
    ///
    /// # Errors
    /// [`AudioError::Stream`] when the transport is unavailable.
    pub fn stop(&self) -> Result<()> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("player state poisoned".into()))?;
        state.transport = Transport::Stopped;
        state.position = 0;
        Ok(())
    }

    /// Current transport state.
    pub fn transport(&self) -> Transport {
        self.shared
            .lock()
            .map(|s| s.transport)
            .unwrap_or(Transport::Stopped)
    }

    /// Playback position in seconds.
    pub fn position_secs(&self) -> f64 {
        self.shared
            .lock()
            .map(|s| {
                let rate = f64::from(self.output_rate);
                s.position as f64 / rate
            })
            .unwrap_or(0.0)
    }
}

/// A sensible default device rate to aim for when the device supports a
/// range (used only as a target; the device's own default governs).
fn default_rate_hint() -> u32 {
    48_000
}

/// Copies frames from `buffer` starting at `start_frame` into the
/// interleaved output slice, mapping channels. Pure and unit-tested.
///
/// Returns the number of frames written (may be less than available output
/// frames at the end of the buffer).
pub fn copy_into_output(
    buffer: &AudioBuffer,
    start_frame: usize,
    out: &mut [f32],
    out_channels: u32,
) -> usize {
    let in_channels = usize::from(buffer.channels());
    let out_channels = out_channels as usize;
    if out_channels == 0 || in_channels == 0 {
        return 0;
    }
    let out_frames = out.len() / out_channels;
    let available = buffer.frames().saturating_sub(start_frame);
    let frames = out_frames.min(available);

    for f in 0..frames {
        let src = &buffer.samples()[(start_frame + f) * in_channels..][..in_channels];
        let dst = &mut out[f * out_channels..][..out_channels];
        match (in_channels, out_channels) {
            (1, 1) => dst[0] = src[0],
            (1, 2) => {
                dst[0] = src[0];
                dst[1] = src[0];
            }
            (2, 1) => dst[0] = (src[0] + src[1]) * 0.5,
            (2, 2) => {
                dst[0] = src[0];
                dst[1] = src[1];
            }
            (in_ch, out_ch) => {
                // Generic map: copy the overlapping channels, averaging any
                // extra input channels into the last output channel.
                let common = in_ch.min(out_ch);
                for (o, s) in dst.iter_mut().zip(src.iter()).take(common) {
                    *o = *s;
                }
                if in_ch > out_ch {
                    let tail = &src[common - 1..];
                    let mix: f32 = tail.iter().sum::<f32>() / tail.len() as f32;
                    dst[common - 1] = mix;
                }
            }
        }
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_to_stereo_duplicates() {
        let buffer = AudioBuffer::from_interleaved(48_000, 1, vec![0.25, -0.5]).unwrap();
        let mut out = vec![0.0f32; 4];
        let written = copy_into_output(&buffer, 0, &mut out, 2);
        assert_eq!(written, 2);
        assert_eq!(out, vec![0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn stereo_to_mono_downmixes() {
        let buffer = AudioBuffer::from_interleaved(48_000, 2, vec![0.2, 0.4, -0.6, 0.2]).unwrap();
        let mut out = vec![0.0f32; 2];
        let written = copy_into_output(&buffer, 0, &mut out, 1);
        assert_eq!(written, 2);
        assert!((out[0] - 0.3).abs() < 1e-6);
        assert!((out[1] + 0.2).abs() < 1e-6);
    }

    #[test]
    fn respects_start_frame_and_end_of_buffer() {
        let buffer = AudioBuffer::from_interleaved(48_000, 1, vec![1.0, 2.0, 3.0]).unwrap();
        let mut out = vec![0.0f32; 4];
        let written = copy_into_output(&buffer, 2, &mut out, 1);
        assert_eq!(written, 1);
        assert_eq!(out[0], 3.0);
        // Past the end: nothing.
        assert_eq!(copy_into_output(&buffer, 3, &mut out, 1), 0);
    }
}
