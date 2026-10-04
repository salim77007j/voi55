//! Playback via `cpal` — play/pause/stop (Phase 2 scope, architecture plan
//! D2/D6).
//!
//! The player owns an output stream on the default (or user-selected)
//! output device and consumes [`AudioBuffer`]s — or, since Phase 6, a
//! streaming preview [`StreamFifo`] (see [`crate::preview`]): the callback
//! pulls mono frames from the FIFO and maps them to the output channels, so
//! the transport works identically for both sources. The FIFO source
//! requires the session rate to match the device rate (the caller checks;
//! there is no resampler on the real-time path — disclosed).
//!
//! Phase 7: the output stream is negotiated in **whatever sample format the
//! device grants** (f32/i32/i16/i8/u8 — the internal state stays `f32`, the
//! callback converts), **any channel count** (mono→N mapping, generic N→M
//! fold), and by explicit device name with a fallback chain (see
//! [`crate::devices::resolve_output_device`]).
//!
//! Buffers are transparently resampled to the output device's rate (via
//! [`crate::resample`]) and channel-mapped in the callback. Transport
//! transitions are safe to call from any thread; the real-time callback
//! only ever locks the state mutex briefly and never allocates.

use crate::buffer::AudioBuffer;
use crate::error::{AudioError, Result};
use crate::preview::StreamFifo;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
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
    /// Streaming preview source (Phase 6). Takes precedence over `buffer`
    /// while set; the two are mutually exclusive.
    fifo: Option<Arc<StreamFifo>>,
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
        Self::connect_named(None)
    }

    /// Connects to an output device by name, falling back through the
    /// [`crate::devices::resolve_output_device`] chain (explicit name →
    /// default → first working device) when the exact device is gone.
    ///
    /// # Errors
    /// See [`Player::connect`].
    pub fn connect_named(device_name: Option<&str>) -> Result<Self> {
        let (device, _fell_back) = crate::devices::resolve_output_device(device_name)?;
        Self::connect_on_device(&device)
    }

    /// Connects to an explicit output device.
    ///
    /// # Errors
    /// See [`Player::connect`].
    pub fn connect_on_device(device: &Device) -> Result<Self> {
        // Pick the best convertible output configuration: any granted
        // sample format (f32 > i32 > i16 > 8-bit), any channel count,
        // aiming for 48 kHz or the nearest rate the range allows.
        let mut best: Option<(cpal::SupportedStreamConfig, (u8, u32, u32))> = None;
        for range in device.supported_output_configs()? {
            if !crate::devices::output_convertible(range.sample_format()) {
                continue;
            }
            let rate = default_rate_hint().clamp(range.min_sample_rate(), range.max_sample_rate());
            let cost = rate.abs_diff(default_rate_hint());
            // Rank: format quality first, then rate proximity, then fewer
            // channels (mono/stereo suits a voice tool and the FIFO map).
            let rank = (
                format_rank(range.sample_format()),
                cost,
                u32::from(range.channels()),
            );
            if best.as_ref().is_none_or(|(_, best_rank)| rank < *best_rank) {
                best = Some((range.with_sample_rate(rate), rank));
            }
        }
        let (supported, _) = best.ok_or_else(|| {
            AudioError::UnsupportedConfig(
                "output device offers no configuration mvl-audio can drive".into(),
            )
        })?;
        let output_rate = supported.sample_rate();
        let output_channels = supported.channels();

        let shared = Arc::new(Mutex::new(PlayState {
            transport: Transport::Stopped,
            ..PlayState::default()
        }));

        let stream = {
            let stream_config = supported.config();
            match supported.sample_format() {
                SampleFormat::F32 => build_output_stream::<f32>(
                    device,
                    &stream_config,
                    Arc::clone(&shared),
                    Arc::clone(&shared),
                    output_channels,
                )?,
                SampleFormat::I32 => build_output_stream::<i32>(
                    device,
                    &stream_config,
                    Arc::clone(&shared),
                    Arc::clone(&shared),
                    output_channels,
                )?,
                SampleFormat::I16 => build_output_stream::<i16>(
                    device,
                    &stream_config,
                    Arc::clone(&shared),
                    Arc::clone(&shared),
                    output_channels,
                )?,
                SampleFormat::I8 => build_output_stream::<i8>(
                    device,
                    &stream_config,
                    Arc::clone(&shared),
                    Arc::clone(&shared),
                    output_channels,
                )?,
                SampleFormat::U8 => build_output_stream::<u8>(
                    device,
                    &stream_config,
                    Arc::clone(&shared),
                    Arc::clone(&shared),
                    output_channels,
                )?,
                other => {
                    return Err(AudioError::UnsupportedConfig(format!(
                        "no output conversion for device format {other:?}"
                    )));
                }
            }
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
    /// Replaces any streaming preview source (the sources are mutually
    /// exclusive).
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
        state.fifo = None;
        state.position = 0;
        state.transport = Transport::Playing;
        Ok(())
    }

    /// Starts (or resumes) playback from the streaming preview FIFO
    /// (Phase 6). The playback cursor is the FIFO's own read position, so
    /// this doubles as resume-after-pause for the streaming source.
    ///
    /// The session rate must equal the output device rate — the streaming
    /// path carries no resampler (disclosed in [`crate::preview`]). The
    /// caller enforces this and falls back to the buffered path otherwise.
    ///
    /// # Errors
    /// [`AudioError::UnsupportedConfig`] when the rates mismatch;
    /// [`AudioError::Stream`] when the transport is unavailable.
    pub fn play_stream(&mut self, fifo: Arc<StreamFifo>, session_rate: u32) -> Result<()> {
        if session_rate != self.output_rate {
            return Err(AudioError::UnsupportedConfig(format!(
                "streaming preview needs session rate {session_rate} == output rate {}",
                self.output_rate
            )));
        }
        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("player state poisoned".into()))?;
        state.buffer = None;
        state.fifo = Some(fifo);
        state.position = 0;
        state.transport = Transport::Playing;
        Ok(())
    }

    /// Detaches the streaming preview source (the cursor freezes; a later
    /// [`Player::play_stream`] resumes from it).
    pub fn detach_stream(&self) -> Result<()> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("player state poisoned".into()))?;
        state.fifo = None;
        if state.transport == Transport::Playing {
            state.transport = Transport::Paused;
        }
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

    /// Playback position in seconds (the FIFO read cursor for the
    /// streaming source, the buffer cursor otherwise).
    pub fn position_secs(&self) -> f64 {
        self.shared
            .lock()
            .map(|s| {
                let rate = f64::from(self.output_rate);
                if let Some(fifo) = s.fifo.as_ref() {
                    fifo.read_frame() as f64 / rate
                } else {
                    s.position as f64 / rate
                }
            })
            .unwrap_or(0.0)
    }
}

/// A sensible default device rate to aim for when the device supports a
/// range (used only as a target; the device's own default governs).
fn default_rate_hint() -> u32 {
    48_000
}

/// Ranking for the output negotiation: f32 first, then the wide integers,
/// then the rest (mirrors `devices::format_rank` for display).
fn format_rank(format: SampleFormat) -> u8 {
    match format {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        SampleFormat::U8 => 3,
        SampleFormat::I8 => 4,
        _ => 9,
    }
}

/// Conversion from the internal `f32` domain to the device's output sample
/// type (full-scale aware, hard-clamped — the DSP must never emit beyond
/// ±1.0 into an integer device).
pub trait FromF32: Copy {
    fn from_f32(value: f32) -> Self;
}

impl FromF32 for f32 {
    fn from_f32(value: f32) -> Self {
        value
    }
}

impl FromF32 for i32 {
    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 2_147_483_647.0) as i32
    }
}

impl FromF32 for i16 {
    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 32_767.0) as i16
    }
}

impl FromF32 for i8 {
    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 127.0) as i8
    }
}

impl FromF32 for u8 {
    fn from_f32(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * 127.0 + 128.0) as u8
    }
}

/// Builds an output stream in the device's own sample format `T`. The
/// transport state machine and the channel mapping live in the `f32`
/// domain (a reused scratch buffer, zero per-callback allocation after
/// warm-up); the final conversion to `T` clamps.
fn build_output_stream<T>(
    device: &Device,
    config: &StreamConfig,
    shared: Arc<Mutex<PlayState>>,
    error_shared: Arc<Mutex<PlayState>>,
    out_channels: u16,
) -> Result<Stream>
where
    T: cpal::SizedSample + FromF32 + Send + 'static,
{
    let mut scratch: Vec<f32> = Vec::new();
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let ch = usize::from(out_channels.max(1));
                let out_frames = data.len() / ch;

                // Snapshot under a brief lock, then render outside it (the
                // RT callback never holds the state lock across the FIFO
                // pull — Phase 6 contract).
                let snapshot = {
                    let Ok(state) = shared.lock() else {
                        data.fill(T::from_f32(0.0));
                        return;
                    };
                    if state.transport != Transport::Playing {
                        data.fill(T::from_f32(0.0));
                        return;
                    }
                    (state.fifo.clone(), state.buffer.clone(), state.position)
                };

                scratch.clear();
                scratch.resize(out_frames * ch, 0.0);

                let mut fifo_done = false;
                let mut buffer_written: Option<usize> = None;
                let fifo_source = snapshot.0.is_some();
                match snapshot.0 {
                    // Streaming preview: the FIFO pull maps mono → N.
                    Some(fifo) => {
                        let frames = fifo.pull_mono_into(&mut scratch, u32::from(out_channels));
                        fifo_done = frames == 0
                            && fifo.total_frames() > 0
                            && fifo.read_frame() >= fifo.total_frames();
                    }
                    None => {
                        if let Some(buffer) = snapshot.1.clone() {
                            buffer_written = Some(copy_into_output(
                                &buffer,
                                snapshot.2,
                                &mut scratch,
                                u32::from(out_channels),
                            ));
                        }
                    }
                }

                // Convert into the device's sample domain (clamped).
                for (dst, src) in data.iter_mut().zip(scratch.iter()) {
                    *dst = T::from_f32(*src);
                }

                // Publish transport transitions under a short lock.
                if let Ok(mut state) = shared.lock() {
                    if fifo_source {
                        if fifo_done {
                            state.transport = Transport::Stopped;
                        }
                    } else if let Some(written) = buffer_written {
                        state.position = snapshot.2 + written;
                        let frames = snapshot.1.as_ref().map_or(0, |b| b.frames());
                        if written == 0 || snapshot.2 + written >= frames {
                            state.transport = Transport::Stopped;
                            state.position = 0;
                        }
                    }
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
        .map_err(|e| AudioError::Stream(e.to_string()))
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
    fn output_conversions_are_full_scale_aware_and_clamped() {
        assert_eq!(0.5f32, f32::from_f32(0.5));
        assert_eq!(32_767i16, i16::from_f32(1.0));
        assert_eq!(-32_767i16, i16::from_f32(-1.0));
        assert_eq!(i16::from_f32(2.0), 32_767, "over-range must clamp");
        assert_eq!(i16::from_f32(-2.0), -32_767);
        assert_eq!(2_147_483_647i32, i32::from_f32(1.0));
        assert_eq!(127i8, i8::from_f32(1.0));
        assert_eq!(-127i8, i8::from_f32(-1.0));
        assert_eq!(255u8, u8::from_f32(1.0));
        assert_eq!(1u8, u8::from_f32(-1.0));
        assert_eq!(128u8, u8::from_f32(0.0), "silence is mid-scale");
        assert_eq!(0i32, i32::from_f32(0.0));
    }

    #[test]
    fn integer_output_formats_survive_a_round_trip_to_f32() {
        // i16 path: convert to f32 and back stays within one LSB.
        for v in [-1.0f32, -0.5, 0.0, 0.25, 1.0] {
            let as_i16 = i16::from_f32(v);
            let back = f32::from(as_i16) / 32_767.0;
            assert!((back - v).abs() < 1.0 / 32_767.0, "v={v} back={back}");
        }
    }

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
