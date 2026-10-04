//! WAV import/export via `hound` (architecture plan D3).
//!
//! Supports the formats relevant to this product: 32-bit float, 24-bit,
//! 16-bit, and 8-bit PCM at any sample rate up to 192 kHz and beyond.
//! Float32 export is bit-exact; integer exports quantize with rounding and
//! are covered by round-trip tolerance tests.

use crate::buffer::AudioBuffer;
use crate::error::{AudioError, Result};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::io::BufWriter;
use std::path::Path;

/// Bit depth / encoding used when writing WAV files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavBitDepth {
    /// 32-bit IEEE float — the professional, bit-exact choice.
    Float32,
    /// 16-bit PCM.
    Int16,
    /// 24-bit PCM.
    Int24,
}

/// Reads any supported WAV file into an [`AudioBuffer`].
///
/// # Errors
/// [`AudioError::Wav`] for malformed files; [`AudioError::UnsupportedConfig`]
/// for encodings this crate does not convert (e.g. non-PCM containers).
pub fn import_wav(path: impl AsRef<Path>) -> Result<AudioBuffer> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();

    let channels = spec.channels;
    let sample_rate = spec.sample_rate;
    let mut samples: Vec<f32> =
        Vec::with_capacity(reader.duration() as usize * usize::from(channels));

    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => {
            for s in reader.samples::<f32>() {
                samples.push(s?);
            }
        }
        (SampleFormat::Int, 16) => {
            for s in reader.samples::<i16>() {
                samples.push(crate::capture::ToF32::to_f32(s?));
            }
        }
        (SampleFormat::Int, 24) => {
            for s in reader.samples::<i32>() {
                samples.push(s? as f32 / 8_388_608.0);
            }
        }
        (SampleFormat::Int, 32) => {
            for s in reader.samples::<i32>() {
                samples.push(crate::capture::ToF32::to_f32(s?));
            }
        }
        (SampleFormat::Int, 8) => {
            for s in reader.samples::<i8>() {
                samples.push(crate::capture::ToF32::to_f32(s?));
            }
        }
        (format, bits) => {
            return Err(AudioError::UnsupportedConfig(format!(
                "unsupported WAV encoding: {format:?} @ {bits} bit"
            )));
        }
    }

    let mut buffer = AudioBuffer::from_interleaved(sample_rate, channels, samples)?;
    if buffer.samples().len() % usize::from(channels) != 0 {
        // Defensive: hound itself rejects torn frames, but never panic.
        let ch = usize::from(channels);
        let complete = buffer.samples().len() - buffer.samples().len() % ch;
        buffer.truncate_frames(complete / ch);
    }
    Ok(buffer)
}

/// Writes an [`AudioBuffer`] to `path` at the requested bit depth.
///
/// # Errors
/// [`AudioError::Io`] / [`AudioError::Wav`] on filesystem failures,
/// [`AudioError::UnsupportedConfig`] for formats hound cannot represent.
pub fn export_wav(path: impl AsRef<Path>, buffer: &AudioBuffer, depth: WavBitDepth) -> Result<()> {
    let spec = match depth {
        WavBitDepth::Float32 => WavSpec {
            channels: buffer.channels(),
            sample_rate: buffer.sample_rate(),
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
        WavBitDepth::Int16 => WavSpec {
            channels: buffer.channels(),
            sample_rate: buffer.sample_rate(),
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        },
        WavBitDepth::Int24 => WavSpec {
            channels: buffer.channels(),
            sample_rate: buffer.sample_rate(),
            bits_per_sample: 24,
            sample_format: SampleFormat::Int,
        },
    };

    let mut writer = WavWriter::create(path, spec)?;
    let result: Result<()> = match depth {
        WavBitDepth::Float32 => write_samples(&mut writer, buffer.samples(), |s| Ok(*s)),
        WavBitDepth::Int16 => write_samples(&mut writer, buffer.samples(), |s| {
            Ok((s * 32_767.0).round().clamp(-32_768.0, 32_767.0) as i16)
        }),
        WavBitDepth::Int24 => write_samples(&mut writer, buffer.samples(), |s| {
            Ok((s * 8_388_607.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32)
        }),
    };
    // `finalize` flushes the data-chunk sizes; without it the file is broken.
    match result {
        Ok(()) => writer.finalize().map_err(AudioError::Wav),
        Err(err) => {
            // Best-effort finalize so the file is not left header-less.
            let _ = writer.finalize();
            Err(err)
        }
    }
}

fn write_samples<S: hound::Sample>(
    writer: &mut WavWriter<BufWriter<std::fs::File>>,
    samples: &[f32],
    convert: impl Fn(&f32) -> Result<S>,
) -> Result<()> {
    for sample in samples {
        writer.write_sample(convert(sample)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::AudioBuffer;

    #[test]
    fn float32_roundtrip_is_bit_exact() {
        let original = AudioBuffer::sine(0.05, 48_000, 2, 220.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_f32_{}.wav", std::process::id()));
        export_wav(&path, &original, WavBitDepth::Float32).unwrap();
        let imported = import_wav(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(imported.sample_rate(), 48_000);
        assert_eq!(imported.channels(), 2);
        assert_eq!(imported.frames(), original.frames());
        assert_eq!(imported.samples(), original.samples());
    }

    #[test]
    fn float32_roundtrip_at_192k_mono() {
        let original = AudioBuffer::sine(0.02, 192_000, 1, 1_000.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_f32_192k_{}.wav", std::process::id()));
        export_wav(&path, &original, WavBitDepth::Float32).unwrap();
        let imported = import_wav(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(imported.sample_rate(), 192_000);
        assert_eq!(imported.samples(), original.samples());
    }

    #[test]
    fn int16_roundtrip_stays_within_one_lsb() {
        let original = AudioBuffer::sine(0.05, 44_100, 1, 440.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_i16_{}.wav", std::process::id()));
        export_wav(&path, &original, WavBitDepth::Int16).unwrap();
        let imported = import_wav(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let max_err = original
            .samples()
            .iter()
            .zip(imported.samples())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err <= 1.5 / 32_767.0, "max error {max_err}");
    }

    #[test]
    fn int24_roundtrip_stays_within_one_lsb() {
        let original = AudioBuffer::sine(0.05, 96_000, 2, 660.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_i24_{}.wav", std::process::id()));
        export_wav(&path, &original, WavBitDepth::Int24).unwrap();
        let imported = import_wav(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(imported.sample_rate(), 96_000);
        let max_err = original
            .samples()
            .iter()
            .zip(imported.samples())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err <= 1.5 / 8_388_607.0, "max error {max_err}");
    }

    #[test]
    fn garbage_file_returns_error_not_panic() {
        let path = std::env::temp_dir().join(format!("mvl_garbage_{}.wav", std::process::id()));
        std::fs::write(&path, [0u8; 64]).unwrap();
        let result = import_wav(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn missing_file_returns_error_not_panic() {
        let path = std::env::temp_dir().join("mvl_definitely_missing_42.wav");
        assert!(import_wav(&path).is_err());
    }
}
