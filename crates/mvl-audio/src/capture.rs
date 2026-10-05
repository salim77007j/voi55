//! Microphone capture via `cpal` — requests 192 kHz / 32-bit float, reports
//! honestly what the device granted (architecture plan D2 and risk item
//! "device caps below 192 kHz").
//!
//! Negotiation policy (pure, unit-tested in [`plan_capture`]):
//! 1. Prefer an `f32` configuration whose rate range contains the preferred
//!    rate (192 000 by default).
//! 2. Otherwise take the `f32` configuration with the highest maximum rate.
//! 3. Otherwise fall back to any convertible integer format (i32/i16/i8/u8).
//!
//! The [`CaptureInfo`] returned with every recording states the requested
//! *and* the negotiated rate so the UI never has to guess.

use crate::buffer::AudioBuffer;
use crate::devices::convertible;
use crate::error::{AudioError, Result};
use crate::meter::MeterTap;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, Stream, StreamConfig};
use std::sync::{Arc, Mutex};

/// Preferred capture rate for the product: 192 kHz.
pub const PREFERRED_SAMPLE_RATE: u32 = 192_000;

/// Safety valve: at 192 kHz/stereo, 10 minutes of f32 ≈ 920 MB. Captures are
/// hard-capped at this many *seconds*; further samples are dropped and the
/// fact is surfaced via [`Recorder::dropped_overflow`].
pub const MAX_CAPTURE_SECONDS: u64 = 600;

/// Absolute sample ceiling corresponding to [`MAX_CAPTURE_SECONDS`] at
/// 192 kHz stereo (used as the in-callback bound).
const MAX_HARD_SAMPLES: usize = (MAX_CAPTURE_SECONDS as usize) * 192_000 * 2;

/// One convertible configuration range offered by a device.
///
/// A plain struct (rather than cpal's `SupportedStreamConfigRange`, whose
/// fields are private) so the selection logic is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigCandidate {
    pub channels: u16,
    pub min_rate: u32,
    pub max_rate: u32,
    pub format: SampleFormat,
}

/// The capture configuration the negotiation picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturePlan {
    pub channels: u16,
    pub sample_rate: u32,
    pub format: SampleFormat,
    /// True when the preferred rate was granted exactly (otherwise the
    /// device's own maximum was used and Phase 3's resampler corrects).
    pub matched_preferred_rate: bool,
}

/// Ranking key for candidates. Lower is better.
/// Format quality first (f32 > i32 > i16 > 8-bit), then exact-rate match,
/// then "small channel counts" (mono/stereo suit a voice tool), then the
/// highest rate.
fn rank(candidate: &ConfigCandidate, rate: u32, matched: bool) -> (u8, u8, u8, u32) {
    let format_rank = match candidate.format {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        SampleFormat::I8 | SampleFormat::U8 => 3,
        _ => 9,
    };
    let channel_rank = match candidate.channels {
        1 | 2 => 0,
        c if c < 8 => 1,
        _ => 2,
    };
    (
        format_rank,
        u8::from(!matched),
        channel_rank,
        u32::MAX - rate,
    )
}

/// Pure negotiation logic: pick the best capture plan from a device's
/// convertible configurations. Deterministic and side-effect free.
pub fn plan_capture(candidates: &[ConfigCandidate], preferred_rate: u32) -> Option<CapturePlan> {
    let mut best: Option<(&ConfigCandidate, u32, bool)> = None;
    let mut best_rank: Option<(u8, u8, u8, u32)> = None;

    for c in candidates.iter().filter(|c| convertible(c.format)) {
        let (rate, matched) = if c.min_rate <= preferred_rate && preferred_rate <= c.max_rate {
            (preferred_rate, true)
        } else {
            (c.max_rate, false)
        };
        let r = rank(c, rate, matched);
        if best_rank.is_none_or(|current| r < current) {
            best = Some((c, rate, matched));
            best_rank = Some(r);
        }
    }

    best.map(|(c, rate, matched)| CapturePlan {
        channels: c.channels,
        sample_rate: rate,
        format: c.format,
        matched_preferred_rate: matched,
    })
}

/// Metadata about a completed (or running) capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInfo {
    pub device: String,
    /// What the app asked for (192 000).
    pub requested_sample_rate: u32,
    /// What the device actually granted.
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: SampleFormat,
    /// `true` when requested == granted.
    pub matched_preferred_rate: bool,
}

#[derive(Debug, Default)]
struct CaptureState {
    recording: bool,
    samples: Vec<f32>,
    overflow_dropped: bool,
    stream_error: Option<String>,
}

/// A running microphone capture. Drop it (or call [`Recorder::stop`]) to end.
pub struct Recorder {
    shared: Arc<Mutex<CaptureState>>,
    stream: Option<Stream>,
    info: CaptureInfo,
    /// Real-time input level tap (the record meters' data source).
    meter: Arc<MeterTap>,
}

impl Recorder {
    /// Starts capturing from the resolved default input device (fallback
    /// chain: platform default → first working input), requesting
    /// [`PREFERRED_SAMPLE_RATE`] in `f32`.
    ///
    /// # Errors
    /// [`AudioError::NoDevice`] without an input device;
    /// [`AudioError::UnsupportedConfig`] when the device offers nothing
    /// convertible; [`AudioError::Stream`] when the stream cannot start.
    pub fn start() -> Result<Self> {
        Self::start_named(None)
    }

    /// Starts capturing from a device chosen by name, falling back through
    /// the [`crate::devices::resolve_input_device`] chain (explicit name →
    /// default → first working input) when the exact device is gone.
    ///
    /// # Errors
    /// See [`Recorder::start`].
    pub fn start_named(device_name: Option<&str>) -> Result<Self> {
        let (device, _fell_back) = crate::devices::resolve_input_device(device_name)?;
        Self::start_on_device(&device, PREFERRED_SAMPLE_RATE)
    }

    /// Starts capturing from an explicit device.
    ///
    /// # Errors
    /// See [`Recorder::start`].
    pub fn start_on_device(device: &cpal::Device, preferred_rate: u32) -> Result<Self> {
        let candidates: Vec<ConfigCandidate> = device
            .supported_input_configs()?
            .filter(|r| convertible(r.sample_format()))
            .map(|r| ConfigCandidate {
                channels: r.channels(),
                min_rate: r.min_sample_rate(),
                max_rate: r.max_sample_rate(),
                format: r.sample_format(),
            })
            .collect();

        let plan = plan_capture(&candidates, preferred_rate).ok_or_else(|| {
            AudioError::UnsupportedConfig(
                "device offers no configuration in a format mvl-audio can convert".into(),
            )
        })?;

        let stream_config = StreamConfig {
            channels: plan.channels,
            sample_rate: plan.sample_rate,
            buffer_size: BufferSize::Default,
        };

        let shared = Arc::new(Mutex::new(CaptureState {
            recording: true,
            ..CaptureState::default()
        }));
        let meter = MeterTap::new();

        // Per-stream sample ceiling for the negotiated rate (the hard
        // MAX_HARD_SAMPLES remains the ultimate bound for fast devices).
        let negotiated_cap =
            (MAX_CAPTURE_SECONDS as usize) * (plan.sample_rate as usize) * (plan.channels as usize);
        let cap = negotiated_cap.min(MAX_HARD_SAMPLES);

        let stream: Stream = match plan.format {
            SampleFormat::F32 => build_stream::<f32>(
                device,
                &stream_config,
                Arc::clone(&shared),
                cap,
                Arc::clone(&meter),
            )?,
            SampleFormat::I16 => build_stream::<i16>(
                device,
                &stream_config,
                Arc::clone(&shared),
                cap,
                Arc::clone(&meter),
            )?,
            SampleFormat::I32 => build_stream::<i32>(
                device,
                &stream_config,
                Arc::clone(&shared),
                cap,
                Arc::clone(&meter),
            )?,
            SampleFormat::I8 => build_stream::<i8>(
                device,
                &stream_config,
                Arc::clone(&shared),
                cap,
                Arc::clone(&meter),
            )?,
            SampleFormat::U8 => build_stream::<u8>(
                device,
                &stream_config,
                Arc::clone(&shared),
                cap,
                Arc::clone(&meter),
            )?,
            other => {
                return Err(AudioError::UnsupportedConfig(format!(
                    "no f32 conversion for device format {other:?}"
                )));
            }
        };
        stream
            .play()
            .map_err(|e| AudioError::Stream(e.to_string()))?;

        let info = CaptureInfo {
            device: device.to_string(),
            requested_sample_rate: preferred_rate,
            sample_rate: plan.sample_rate,
            channels: plan.channels,
            sample_format: plan.format,
            matched_preferred_rate: plan.matched_preferred_rate,
        };
        Ok(Self {
            shared,
            stream: Some(stream),
            info,
            meter,
        })
    }

    /// The real-time input level tap. Drained by the UI while a capture
    /// is armed — the meters then show the true incoming signal.
    pub fn meter(&self) -> Arc<MeterTap> {
        Arc::clone(&self.meter)
    }

    /// Format details of this capture (requested vs negotiated).
    pub fn info(&self) -> &CaptureInfo {
        &self.info
    }

    /// Frames captured so far.
    pub fn frames_captured(&self) -> usize {
        self.shared
            .lock()
            .map(|s| s.samples.len() / usize::from(self.info.channels))
            .unwrap_or(0)
    }

    /// Whether the capture hit the [`MAX_CAPTURE_SECONDS`] safety valve.
    pub fn dropped_overflow(&self) -> bool {
        self.shared
            .lock()
            .map(|s| s.overflow_dropped)
            .unwrap_or(false)
    }

    /// Last stream error reported by the device, if any.
    pub fn last_stream_error(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|s| s.stream_error.clone())
    }

    /// Stops the stream and returns the captured audio.
    ///
    /// # Errors
    /// [`AudioError::Stream`] if the device reported a fatal error mid-run;
    /// [`AudioError::EmptyCapture`] when no complete frame was captured
    /// (unplugged/muted device, denied OS permission — a clear error, not
    /// a silent empty buffer).
    pub fn stop(mut self) -> Result<AudioBuffer> {
        if let Ok(mut state) = self.shared.lock() {
            state.recording = false;
        }
        // Dropping the stream joins the callback thread.
        drop(self.stream.take());

        let mut state = self
            .shared
            .lock()
            .map_err(|_| AudioError::Stream("capture state poisoned".into()))?;
        if let Some(err) = state.stream_error.take() {
            return Err(AudioError::Stream(err));
        }
        finalize_capture(
            std::mem::take(&mut state.samples),
            self.info.sample_rate,
            self.info.channels,
        )
    }
}

/// Turns the raw interleaved staging buffer into an [`AudioBuffer`] (pure,
/// unit-tested). Rejects a zero-frame capture with
/// [`AudioError::EmptyCapture`] instead of silently handing the UI an
/// empty project.
///
/// # Errors
/// [`AudioError::EmptyCapture`] on an empty or torn (incomplete frame)
/// capture; [`AudioError::UnsupportedConfig`] on an invalid channel/rate
/// pairing.
pub(crate) fn finalize_capture(
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
) -> Result<AudioBuffer> {
    let ch = usize::from(channels.max(1));
    let complete = samples.len() - samples.len() % ch;
    if complete == 0 {
        return Err(AudioError::EmptyCapture);
    }
    let mut buffer = AudioBuffer::from_interleaved(sample_rate, channels, samples)?;
    buffer.truncate_frames(complete / ch);
    Ok(buffer)
}

/// Builds an input stream for one concrete sample type `T`, converting into
/// the shared `f32` staging buffer inside the callback.
fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    shared: Arc<Mutex<CaptureState>>,
    capacity_samples: usize,
    meter: Arc<MeterTap>,
) -> Result<Stream>
where
    T: cpal::SizedSample + ToF32 + Send + 'static,
{
    let err_shared = Arc::clone(&shared);
    let in_channels = usize::from(config.channels.max(1));
    device
        .build_input_stream(
            *config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                // Publish the true incoming level first (even when the
                // ring overflows below, the meter shows what arrived).
                meter.push_converted(data, in_channels, ToF32::to_f32);
                let Ok(mut state) = shared.lock() else {
                    return; // poisoned: nothing safe to do in the RT thread
                };
                if !state.recording {
                    return;
                }
                if state.samples.len() >= capacity_samples {
                    state.overflow_dropped = true;
                    return;
                }
                // Never exceed the cap by more than one callback.
                let room = capacity_samples - state.samples.len();
                let take = data.len().min(room);
                state.samples.reserve(take);
                for sample in &data[..take] {
                    state.samples.push(sample.to_f32());
                }
            },
            move |err: cpal::Error| {
                log::error!("capture stream error: {err}");
                if let Ok(mut state) = err_shared.lock() {
                    state.stream_error = Some(err.to_string());
                }
            },
            None,
        )
        .map_err(|e| AudioError::Stream(e.to_string()))
}

/// Conversion from device sample types into the internal `f32` domain.
pub trait ToF32: Copy {
    /// Converts one sample to `f32` in `[-1.0, 1.0]` (full-scale aware).
    fn to_f32(self) -> f32;
}

impl ToF32 for f32 {
    fn to_f32(self) -> f32 {
        self
    }
}

impl ToF32 for i16 {
    fn to_f32(self) -> f32 {
        f32::from(self) / 32_768.0
    }
}

impl ToF32 for i32 {
    fn to_f32(self) -> f32 {
        self as f32 / 2_147_483_648.0
    }
}

impl ToF32 for i8 {
    fn to_f32(self) -> f32 {
        f32::from(self) / 128.0
    }
}

impl ToF32 for u8 {
    fn to_f32(self) -> f32 {
        (f32::from(self) - 128.0) / 128.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::SampleFormat;

    fn cand(channels: u16, min: u32, max: u32, format: SampleFormat) -> ConfigCandidate {
        ConfigCandidate {
            channels,
            min_rate: min,
            max_rate: max,
            format,
        }
    }

    #[test]
    fn prefers_f32_containing_preferred_rate() {
        let cands = vec![
            cand(1, 44_100, 48_000, SampleFormat::F32),
            cand(1, 8_000, 192_000, SampleFormat::F32),
        ];
        let plan = plan_capture(&cands, 192_000).unwrap();
        assert_eq!(plan.sample_rate, 192_000);
        assert!(plan.matched_preferred_rate);
        assert_eq!(plan.format, SampleFormat::F32);
    }

    #[test]
    fn falls_back_to_highest_rate_when_preferred_unavailable() {
        let plan = plan_capture(&[cand(1, 44_100, 48_000, SampleFormat::F32)], 192_000).unwrap();
        assert_eq!(plan.sample_rate, 48_000);
        assert!(!plan.matched_preferred_rate);
    }

    #[test]
    fn prefers_f32_over_integer_formats() {
        let cands = vec![
            cand(1, 44_100, 192_000, SampleFormat::I16),
            cand(1, 44_100, 48_000, SampleFormat::F32),
        ];
        let plan = plan_capture(&cands, 96_000).unwrap();
        assert_eq!(plan.format, SampleFormat::F32);
        assert_eq!(plan.sample_rate, 48_000);
    }

    #[test]
    fn prefers_mono_stereo_over_multichannel() {
        let cands = vec![
            cand(8, 8_000, 192_000, SampleFormat::F32),
            cand(2, 8_000, 192_000, SampleFormat::F32),
        ];
        let plan = plan_capture(&cands, 192_000).unwrap();
        assert_eq!(plan.channels, 2);
    }

    #[test]
    fn rejects_only_unconvertible_formats() {
        let cands = vec![cand(2, 44_100, 48_000, SampleFormat::F64)];
        assert!(plan_capture(&cands, 192_000).is_none());
    }

    #[test]
    fn zero_frame_capture_is_a_clear_error_not_an_empty_buffer() {
        // Regression (Phase 7.1 ISSUE 3): stopping a capture whose device
        // never delivered a callback must yield a named error, not an
        // empty project the UI would silently accept.
        let err = finalize_capture(Vec::new(), 48_000, 1).unwrap_err();
        assert!(matches!(err, AudioError::EmptyCapture), "got {err:?}");
        // Torn capture (half a stereo frame) is equally empty.
        let err = finalize_capture(vec![0.5], 48_000, 2).unwrap_err();
        assert!(matches!(err, AudioError::EmptyCapture), "got {err:?}");
        // A complete single frame passes.
        let buf = finalize_capture(vec![0.5, -0.5], 48_000, 2).expect("one stereo frame");
        assert_eq!(buf.frames(), 1);
    }

    #[test]
    fn integer_conversions_are_full_scale_aware() {
        // i16::MAX maps to exactly one LSB below 1.0.
        assert_eq!(i16::MAX.to_f32(), 32_767.0 / 32_768.0);
        assert_eq!(i16::MIN.to_f32(), -1.0);
        assert_eq!(0u8.to_f32(), -1.0);
        assert_eq!(255u8.to_f32(), 127.0 / 128.0);
        assert_eq!(i32::MAX.to_f32(), i32::MAX as f32 / 2_147_483_648.0);
        assert_eq!(0i8.to_f32(), 0.0);
    }
}
