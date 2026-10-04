//! Audio device discovery.
//!
//! Thin, honest wrapper over `cpal` device enumeration (D2). Everything
//! here is fallible on purpose — headless machines, missing drivers, and
//! busy devices are normal situations, not panics.

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
