//! Audio device discovery.
//!
//! Thin, honest wrapper over `cpal` device enumeration (D2). Everything
//! here is fallible on purpose — headless machines, missing drivers, and
//! busy devices are normal situations, not panics.
//!
//! Phase 7: full enumeration for inputs *and* outputs, plus a resolution
//! fallback chain for device selection (explicit name → platform default
//! → first working device) so a missing "default" never blinds the app
//! on machines that only expose concrete devices.
//!
//! Platform notes (real-machine behaviour, honestly disclosed):
//! - **Windows**: the cpal default host is WASAPI; every render/capture
//!   endpoint shows up as a separate device. No extra initialization is
//!   needed; exclusive-mode is not requested (shared mode only).
//! - **macOS**: CoreAudio asks for microphone permission when the first
//!   input stream is *built* (`Recorder::start*`), not at enumeration —
//!   so a fresh install can list devices yet fail the first capture; the
//!   denial surfaces as `AudioError::Stream`/`EmptyCapture`, which the UI
//!   reports verbatim. The app bundle must carry the NSMicrophoneUsage-
//!   -Description key for the prompt to appear (release-bundle task).
//! - **Linux**: cpal talks ALSA; under PipeWire/PulseAudio desktops the
//!   `pulse`/`pipewire` ALSA plugins expose every application-visible
//!   device, so "no device" usually means the user session (or the
//!   XDG portal environment) is missing, not the hardware. Loopback
//!   capture of system audio is *not* enumerated here (it is a
//!   PulseAudio/PipeWire module feature, not an ALSA capture endpoint).

use crate::capture::ConfigCandidate;
use crate::error::{AudioError, Result};
use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, HostTrait};

/// Everything the UI needs to know about one input device.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Human-readable device name (via `Device`'s `Display` impl).
    pub name: String,
    /// Configurations the device supports for capture.
    pub input_configs: Vec<ConfigCandidate>,
}

/// Everything the UI needs to know about one output device.
#[derive(Debug, Clone)]
pub struct OutputDeviceInfo {
    /// Human-readable device name.
    pub name: String,
    /// Number of distinct output configuration ranges offered.
    pub output_configs: usize,
    /// Best (highest) output sample rate offered.
    pub max_sample_rate: u32,
    /// Sample formats the device grants for playback.
    pub formats: Vec<SampleFormat>,
}

impl DeviceInfo {
    /// The best sample rate this device offers for capture.
    pub fn max_sample_rate(&self) -> u32 {
        self.input_configs
            .iter()
            .map(|c| c.max_rate)
            .max()
            .unwrap_or(0)
    }
}

/// The platform's default input device.
///
/// # Errors
/// [`AudioError::NoDevice`] when no input device exists.
pub fn default_input_device() -> Result<cpal::Device> {
    let host = cpal::default_host();
    host.default_input_device().ok_or(AudioError::NoDevice)
}

/// Resolves an input device by name with an honest fallback chain:
/// 1. an exact-name match from the full enumeration (when `Some`),
/// 2. the platform default input,
/// 3. the first enumerable input device that offers anything convertible.
///
/// Returns the device and whether it fell back (a `Some(name)` that could
/// not be honored, or a missing default) so the UI can disclose it.
///
/// # Errors
/// [`AudioError::NoDevice`] when every step of the chain fails.
pub fn resolve_input_device(name: Option<&str>) -> Result<(cpal::Device, bool)> {
    let host = cpal::default_host();
    let all: Vec<cpal::Device> = host.input_devices()?.collect();
    let fallback_used;

    if let Some(wanted) = name {
        if let Some(found) = all.iter().find(|d| d.to_string() == wanted) {
            return Ok((found.clone(), false));
        }
        fallback_used = true; // requested device vanished: fall back
    } else {
        fallback_used = false;
    }

    if let Some(default) = host.default_input_device() {
        return Ok((default, fallback_used));
    }
    all.into_iter()
        .next()
        .map(|d| (d, true))
        .ok_or(AudioError::NoDevice)
}

/// Resolves an output device by name with the same fallback chain as
/// [`resolve_input_device`].
///
/// # Errors
/// [`AudioError::NoDevice`] when every step of the chain fails.
pub fn resolve_output_device(name: Option<&str>) -> Result<(cpal::Device, bool)> {
    let host = cpal::default_host();
    let all: Vec<cpal::Device> = host.output_devices()?.collect();
    let fallback_used;

    if let Some(wanted) = name {
        if let Some(found) = all.iter().find(|d| d.to_string() == wanted) {
            return Ok((found.clone(), false));
        }
        fallback_used = true;
    } else {
        fallback_used = false;
    }

    if let Some(default) = host.default_output_device() {
        return Ok((default, fallback_used));
    }
    all.into_iter()
        .next()
        .map(|d| (d, true))
        .ok_or(AudioError::NoDevice)
}

/// The sample rate the default *output* device would grant an `f32`
/// mono/stereo stream (the same selection the [`crate::Player`] makes).
/// Used by the UI to decide whether the streaming preview can run without
/// a resampler.
///
/// Returns `None` without an output device (headless machines) — the
/// caller then keeps the offline preview path.
pub fn default_output_rate() -> Option<u32> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let mut best: Option<(cpal::SupportedStreamConfig, u32)> = None;
    for range in device.supported_output_configs().ok()? {
        if range.sample_format() != SampleFormat::F32 || range.channels() > 2 {
            continue;
        }
        let rate = 48_000u32.clamp(range.min_sample_rate(), range.max_sample_rate());
        let cost = rate.abs_diff(48_000);
        if best.as_ref().is_none_or(|(_, best_cost)| cost < *best_cost) {
            best = Some((range.with_sample_rate(rate), cost));
        }
    }
    best.map(|(config, _)| config.sample_rate())
}

/// Lists every input device visible to the platform's default host.
///
/// # Errors
/// Returns the underlying `cpal` error if enumeration itself fails;
/// devices that fail individual probing are skipped (logged).
pub fn list_input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    for device in host.input_devices()? {
        let configs = match device.supported_input_configs() {
            Ok(ranges) => ranges
                .filter(|r| convertible(r.sample_format()))
                .map(|r| ConfigCandidate {
                    channels: r.channels(),
                    min_rate: r.min_sample_rate(),
                    max_rate: r.max_sample_rate(),
                    format: r.sample_format(),
                })
                .collect::<Vec<_>>(),
            Err(err) => {
                log::warn!("skipping device that reported no input configs: {err}");
                continue;
            }
        };
        devices.push(DeviceInfo {
            name: device.to_string(),
            input_configs: configs,
        });
    }
    Ok(devices)
}

/// Lists every output device visible to the platform's default host.
///
/// # Errors
/// Returns the underlying `cpal` error if enumeration itself fails;
/// devices that fail individual probing are skipped (logged).
pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    for device in host.output_devices()? {
        let name = device.to_string();
        let ranges = match device.supported_output_configs() {
            Ok(r) => r.collect::<Vec<_>>(),
            Err(err) => {
                log::warn!("skipping output device without configs: {err}");
                continue;
            }
        };
        if ranges.is_empty() {
            continue;
        }
        let mut formats: Vec<SampleFormat> = ranges.iter().map(|r| r.sample_format()).collect();
        formats.sort_by_key(|f| format_rank(*f));
        formats.dedup();
        let max_sample_rate = ranges
            .iter()
            .map(|r| r.max_sample_rate())
            .max()
            .unwrap_or(0);
        devices.push(OutputDeviceInfo {
            name,
            output_configs: ranges.len(),
            max_sample_rate,
            formats,
        });
    }
    Ok(devices)
}

/// Ranking for display: f32 first, then the wide integers, then the rest.
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

/// Formats `mvl-audio` can convert into `f32` buffers (D2/D6).
pub(crate) fn convertible(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::I8
            | SampleFormat::U8
    )
}

/// Formats `mvl-audio` can emit to an output device (any integer format the
/// `FromF32` conversion covers, plus `f32` itself).
pub(crate) fn output_convertible(format: SampleFormat) -> bool {
    matches!(
        format,
        SampleFormat::F32
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::I8
            | SampleFormat::U8
    )
}
