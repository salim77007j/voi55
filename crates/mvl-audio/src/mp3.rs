//! MP3 import via `symphonia` and export via LAME (`mp3lame-encoder`)
//! (architecture plan D4/D5).
//!
//! Import decodes MPEG Layer III to interleaved `f32` at the file's native
//! rate. Export targets LAME's supported rate set — sources above 48 kHz
//! (e.g. our 96/192 kHz sessions) are transparently resampled with
//! [`crate::resample`] first, because the MP3 format physically cannot carry
//! higher rates.
//!
//! Decoder line: symphonia **0.5.x** (the mature, production line) — the
//! 0.6 series is a self-declared preview (Phase 7 decision). Corrupt or
//! torn packets are skipped with a warning instead of failing the whole
//! file; only a file with *zero* decodable audio is an error.

use crate::buffer::AudioBuffer;
use crate::error::{AudioError, Result};
use std::path::Path;

/// Sample rates LAME/MP3 can encode (MPEG 1 + 2 frequency tables).
pub const MP3_SAMPLE_RATES: [u32; 9] = [
    8_000, 11_025, 12_000, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
];

/// Bitrates `mvl-audio` exposes for MP3 export (kbps).
pub const MP3_BITRATES: [u32; 6] = [128, 160, 192, 224, 256, 320];

/// Default MP3 export bitrate.
pub const DEFAULT_MP3_BITRATE: u32 = 320;

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Decodes an MP3 file to an [`AudioBuffer`].
///
/// # Errors
/// [`AudioError::File`] for unrecognizable containers, [`AudioError::Codec`]
/// for decode failures, [`AudioError::UnsupportedConfig`] for tracks with no
/// decodable audio codec.
pub fn import_mp3(path: impl AsRef<Path>) -> Result<AudioBuffer> {
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let src = std::fs::File::open(path.as_ref())?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("mp3");

    // The probe transparently handles ID3v2 headers and raw MPEG streams.
    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| AudioError::File("no audio track found in file".into()))?;
    let track_id = track.id;

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut samples: Vec<f32> = Vec::new();
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut corrupt_packets: u64 = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // Normal end of stream (symphonia signals EOF as an IoError).
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                // Track boundary reached; nothing more decodable here.
                break;
            }
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                if channels == 0 {
                    channels = u16::try_from(decoded.spec().channels.count())
                        .map_err(|_| AudioError::Codec("absurd channel count".into()))?;
                    sample_rate = decoded.spec().rate;
                }
                append_decoded(&decoded, &mut samples)?;
            }
            // Skip torn/corrupt packets; only a fully dead file errors out.
            Err(SymphoniaError::DecodeError(err)) => {
                corrupt_packets += 1;
                log::warn!("skipping corrupt MP3 packet: {err}");
            }
            Err(e) => return Err(e.into()),
        }
    }
    if corrupt_packets > 0 {
        log::warn!("MP3 import skipped {corrupt_packets} corrupt packet(s)");
    }

    if channels == 0 || sample_rate == 0 || samples.is_empty() {
        return Err(AudioError::Codec("file decoded to zero audio".into()));
    }
    let mut buffer = AudioBuffer::from_interleaved(sample_rate, channels, samples)?;
    // Defensive: drop any torn trailing frame.
    let ch = usize::from(channels);
    let complete = buffer.samples().len() - buffer.samples().len() % ch;
    buffer.truncate_frames(complete / ch);
    Ok(buffer)
}

/// Converts one decoded packet (planar, any supported format) into
/// interleaved `f32`.
fn append_decoded(
    decoded: &symphonia::core::audio::AudioBufferRef<'_>,
    out: &mut Vec<f32>,
) -> Result<()> {
    use symphonia::core::audio::AudioBufferRef;
    use symphonia::core::conv::FromSample as _;

    fn push_interleaved<S: symphonia::core::sample::Sample>(
        buf: &symphonia::core::audio::AudioBuffer<S>,
        frames: usize,
        channels: usize,
        out: &mut Vec<f32>,
        convert: impl Fn(S) -> f32,
    ) -> Result<()> {
        use symphonia::core::audio::Signal;
        let mut planes: Vec<&[S]> = Vec::with_capacity(channels);
        for ch in 0..channels {
            let plane = buf.chan(ch);
            planes.push(&plane[..frames.min(plane.len())]);
        }
        out.reserve(frames * channels);
        for frame in 0..frames {
            for plane in &planes {
                out.push(convert(plane[frame]));
            }
        }
        Ok(())
    }

    let frames = decoded.frames();
    let channels = decoded.spec().channels.count();
    match decoded {
        AudioBufferRef::F32(buf) => push_interleaved(buf, frames, channels, out, |s| s)?,
        AudioBufferRef::F64(buf) => push_interleaved(buf, frames, channels, out, |s| s as f32)?,
        AudioBufferRef::S16(buf) => {
            push_interleaved(buf, frames, channels, out, |s| f32::from(s) / 32_768.0)?;
        }
        AudioBufferRef::S24(buf) => push_interleaved(buf, frames, channels, out, f32::from_sample)?,
        AudioBufferRef::S32(buf) => {
            push_interleaved(buf, frames, channels, out, |s| s as f32 / 2_147_483_648.0)?;
        }
        AudioBufferRef::S8(buf) => {
            push_interleaved(buf, frames, channels, out, |s| f32::from(s) / 128.0)?;
        }
        AudioBufferRef::U8(buf) => {
            push_interleaved(buf, frames, channels, out, |s| {
                (f32::from(s) - 128.0) / 128.0
            })?;
        }
        AudioBufferRef::U16(buf) => {
            push_interleaved(buf, frames, channels, out, |s| {
                (f32::from(s) - 32_768.0) / 32_768.0
            })?;
        }
        AudioBufferRef::U24(buf) => push_interleaved(buf, frames, channels, out, f32::from_sample)?,
        AudioBufferRef::U32(buf) => {
            push_interleaved(buf, frames, channels, out, |s| {
                s as f32 / 2_147_483_648.0 - 1.0
            })?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Encodes an [`AudioBuffer`] to an MP3 file at `bitrate_kbps` (see
/// [`MP3_BITRATES`]).
///
/// Rates MP3 cannot carry are transparently resampled (192 kHz → 48 kHz and
/// friends); channel counts beyond stereo are rejected.
///
/// # Errors
/// [`AudioError::UnsupportedConfig`] for unsupported rates/bitrates/channel
/// counts, [`AudioError::Codec`] for encoder failures, [`AudioError::Io`] for
/// filesystem failures.
pub fn export_mp3(path: impl AsRef<Path>, buffer: &AudioBuffer, bitrate_kbps: u32) -> Result<()> {
    use mp3lame_encoder::{Builder, DualPcm, Mode, MonoPcm, Quality};

    if !MP3_BITRATES.contains(&bitrate_kbps) {
        return Err(AudioError::UnsupportedConfig(format!(
            "bitrate {bitrate_kbps} kbps unsupported; choose from {MP3_BITRATES:?}"
        )));
    }
    match buffer.channels() {
        1 | 2 => {}
        other => {
            return Err(AudioError::UnsupportedConfig(format!(
                "MP3 export supports mono/stereo, got {other} channels"
            )));
        }
    }
    let target_rate = mp3_target_rate(buffer.sample_rate());
    let resampled;
    let samples: &[f32] = if target_rate == buffer.sample_rate() {
        buffer.samples()
    } else {
        resampled = crate::resample::resample_interleaved(
            buffer.samples(),
            usize::from(buffer.channels()),
            buffer.sample_rate(),
            target_rate,
        );
        &resampled
    };

    let mut builder = Builder::new()
        .ok_or_else(|| AudioError::Codec("failed to create LAME encoder builder".into()))?;
    builder
        .set_sample_rate(target_rate)
        .map_err(|e| AudioError::Codec(format!("LAME rejected rate {target_rate}: {e}")))?;
    builder
        .set_num_channels(buffer.channels() as u8)
        .map_err(|e| AudioError::Codec(format!("LAME rejected channels: {e}")))?;
    builder
        .set_mode(if buffer.channels() == 1 {
            Mode::Mono
        } else {
            Mode::JointStereo
        })
        .map_err(|e| AudioError::Codec(format!("LAME rejected mode: {e}")))?;
    builder
        .set_brate(lame_bitrate(bitrate_kbps))
        .map_err(|e| AudioError::Codec(format!("LAME rejected bitrate: {e}")))?;
    builder
        .set_quality(Quality::Best)
        .map_err(|e| AudioError::Codec(format!("LAME rejected quality: {e}")))?;

    let mut encoder = builder
        .build()
        .map_err(|e| AudioError::Codec(format!("LAME build failed: {e}")))?;

    let mut mp3: Vec<u8> = Vec::with_capacity(samples.len() / 4 + 4096);
    // `encode_to_vec` writes into the Vec's *spare capacity* only, so the
    // budget must be reserved up front: worst-case CBR bytes for the whole
    // input plus the flush tail (>= 7200 bytes).
    let frames = samples.len() / usize::from(buffer.channels());
    let byte_budget =
        (frames as u64 * u64::from(bitrate_kbps) * 125 / u64::from(target_rate)) as usize + 8192;
    mp3.reserve(byte_budget);
    match buffer.channels() {
        1 => {
            encoder
                .encode_to_vec(MonoPcm(samples), &mut mp3)
                .map_err(|e| AudioError::Codec(format!("LAME encode failed: {e}")))?;
        }
        2 => {
            // De-interleave into planar L/R for LAME.
            let frames = samples.len() / 2;
            let mut left = Vec::with_capacity(frames);
            let mut right = Vec::with_capacity(frames);
            let (stereo, _) = samples.as_chunks::<2>();
            for frame in stereo {
                left.push(frame[0]);
                right.push(frame[1]);
            }
            encoder
                .encode_to_vec(
                    DualPcm {
                        left: &left,
                        right: &right,
                    },
                    &mut mp3,
                )
                .map_err(|e| AudioError::Codec(format!("LAME encode failed: {e}")))?;
        }
        _ => unreachable!("channel count validated above"),
    }
    // Flush writes the final frame(s); LAME needs >= 7200 bytes of headroom.
    mp3.reserve(8192);
    encoder
        .flush_to_vec::<mp3lame_encoder::FlushNoGap>(&mut mp3)
        .map_err(|e| AudioError::Codec(format!("LAME flush failed: {e}")))?;

    std::fs::write(path.as_ref(), mp3)?;
    Ok(())
}

/// Maps an arbitrary source rate to the closest rate MP3 can carry:
/// rates inside [`MP3_SAMPLE_RATES`] pass through; higher rates collapse to
/// 48 kHz; others snap to the nearest supported rate.
pub fn mp3_target_rate(source_rate: u32) -> u32 {
    if MP3_SAMPLE_RATES.contains(&source_rate) {
        return source_rate;
    }
    if source_rate > 48_000 {
        return 48_000;
    }
    MP3_SAMPLE_RATES
        .iter()
        .copied()
        .min_by_key(|r| r.abs_diff(source_rate))
        .unwrap_or(48_000)
}

fn lame_bitrate(kbps: u32) -> mp3lame_encoder::Bitrate {
    match kbps {
        128 => mp3lame_encoder::Bitrate::Kbps128,
        160 => mp3lame_encoder::Bitrate::Kbps160,
        192 => mp3lame_encoder::Bitrate::Kbps192,
        224 => mp3lame_encoder::Bitrate::Kbps224,
        256 => mp3lame_encoder::Bitrate::Kbps256,
        _ => mp3lame_encoder::Bitrate::Kbps320,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::AudioBuffer;

    /// Best-offset SNR search to absorb codec delay/padding between the
    /// original signal and the decoded MP3.
    fn aligned_snr(original: &[f32], decoded: &[f32], max_offset: usize) -> f64 {
        let mut best = f64::NEG_INFINITY;
        for offset in 0..=max_offset {
            let (o, d) = if original.len() > decoded.len() + offset {
                (&original[offset..], decoded)
            } else {
                (original, &decoded[offset.min(decoded.len())..])
            };
            let n = o.len().min(d.len());
            let (mut signal, mut noise) = (0.0f64, 0.0f64);
            for i in 0..n {
                let s = f64::from(o[i]);
                let e = f64::from(o[i]) - f64::from(d[i]);
                signal += s * s;
                noise += e * e;
            }
            if noise > 0.0 {
                best = best.max(10.0 * (signal / noise).log10());
            }
        }
        best
    }

    #[test]
    fn mp3_roundtrip_mono_44k1() {
        let original = AudioBuffer::sine(0.5, 44_100, 1, 440.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_rt_mono_{}.mp3", std::process::id()));
        export_mp3(&path, &original, 192).expect("export must succeed");
        let meta = std::fs::metadata(&path).unwrap();
        let _ = meta;
        let decoded = import_mp3(&path).expect("decode must succeed");
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.sample_rate(), 44_100);
        assert_eq!(decoded.channels(), 1);
        // Duration within codec delay+padding slack (±60 ms).
        assert!(
            (decoded.duration_secs() - original.duration_secs()).abs() < 0.06,
            "duration {} vs {}",
            decoded.duration_secs(),
            original.duration_secs()
        );
        let snr = aligned_snr(original.samples(), decoded.samples(), 4_000);
        assert!(snr > 20.0, "round-trip SNR {snr:.1} dB below 20 dB floor");
    }

    #[test]
    fn mp3_roundtrip_stereo_192k_resamples_to_48k() {
        let original = AudioBuffer::sine(0.4, 192_000, 2, 880.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_rt_192k_{}.mp3", std::process::id()));
        export_mp3(&path, &original, 256).expect("export must succeed");
        let decoded = import_mp3(&path).expect("decode must succeed");
        let _ = std::fs::remove_file(&path);

        assert_eq!(decoded.sample_rate(), 48_000, "192 kHz must land at 48 kHz");
        assert_eq!(decoded.channels(), 2);
        // ~2 s of decoded audio after rate conversion (0.4 s @ 48 kHz).
        assert!(
            (decoded.duration_secs() - 0.4).abs() < 0.06,
            "duration {}",
            decoded.duration_secs()
        );
    }

    #[test]
    fn unsupported_bitrate_is_rejected() {
        let buffer = AudioBuffer::sine(0.01, 44_100, 1, 440.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_badbr_{}.mp3", std::process::id()));
        let result = export_mp3(&path, &buffer, 111);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn target_rate_mapping() {
        assert_eq!(mp3_target_rate(192_000), 48_000);
        assert_eq!(mp3_target_rate(96_000), 48_000);
        assert_eq!(mp3_target_rate(88_200), 48_000);
        assert_eq!(mp3_target_rate(44_100), 44_100);
        assert_eq!(mp3_target_rate(48_000), 48_000);
        assert_eq!(mp3_target_rate(22_050), 22_050);
        assert_eq!(mp3_target_rate(4_000), 8_000);
        assert_eq!(mp3_target_rate(18_000), 16_000);
    }

    #[test]
    fn garbage_mp3_is_an_error_not_panic() {
        let path = std::env::temp_dir().join(format!("mvl_garbage_{}.mp3", std::process::id()));
        std::fs::write(&path, [7u8; 512]).unwrap();
        let result = import_mp3(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn mp3_decode_is_deterministic() {
        // Same file decoded twice must produce bit-identical samples (the
        // decoder is a pure function of its input).
        let original = AudioBuffer::sine(0.5, 44_100, 1, 440.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_det_{}.mp3", std::process::id()));
        export_mp3(&path, &original, 192).expect("export must succeed");
        let first = import_mp3(&path).expect("decode 1 must succeed");
        let second = import_mp3(&path).expect("decode 2 must succeed");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            first.samples(),
            second.samples(),
            "decode must be deterministic"
        );
    }

    #[test]
    fn truncated_mp3_does_not_panic() {
        // A file cut mid-stream must either decode (skipping torn packets)
        // or fail cleanly — never panic, never loop forever.
        let original = AudioBuffer::sine(0.5, 44_100, 1, 440.0).unwrap();
        let path = std::env::temp_dir().join(format!("mvl_trunc_{}.mp3", std::process::id()));
        export_mp3(&path, &original, 128).expect("export must succeed");
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // Cut at 60 % (mid-frame for sure) and at 5 % (header only).
        for cut in [0.60, 0.05] {
            let torn = std::env::temp_dir().join(format!(
                "mvl_trunc_{}_{}k.mp3",
                std::process::id(),
                (cut * 100.0) as u32
            ));
            let end = (bytes.len() as f64 * cut) as usize;
            std::fs::write(&torn, &bytes[..end.max(1)]).unwrap();
            let result = import_mp3(&torn);
            let _ = std::fs::remove_file(&torn);
            // Either outcome is fine as long as it is a clean Result.
            if let Ok(buffer) = result {
                assert!(buffer.frames() > 0);
                assert!(buffer.frames() < original.frames());
            }
        }
    }

    /// Quality gate 1 (scaled down for unit CI): random blobs must never
    /// panic the decoder. Full 1000-file fuzzing runs on real machines;
    /// 64 seeded blobs run everywhere.
    #[test]
    fn random_blobs_never_panic() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15; // seed
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let path = std::env::temp_dir().join(format!("mvl_fuzz_{}.mp3", std::process::id()));
        for i in 0..64 {
            let len = (next() % 4096) as usize + 64;
            let mut blob: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
            // Sprinkle valid-looking MPEG frame sync words so the probe
            // sometimes accepts the container.
            if i % 4 == 0 && len > 8 {
                blob[0] = 0xFF;
                blob[1] = 0xFB;
            }
            std::fs::write(&path, &blob).unwrap();
            let _ = import_mp3(&path); // must return, not panic
        }
        let _ = std::fs::remove_file(&path);
    }
}
