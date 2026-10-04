//! Offline sample-rate conversion.
//!
//! A windowed-sinc (Hann) interpolator used to bring recordings to rates
//! MP3 can carry (LAME supports 8–48 kHz) and, from Phase 3, for adapting
//! device rates. Deterministic, allocation-light, and unit-tested for
//! transparency (SNR) and exact length ratio.
//!
//! For the *live preview* path the engine (Phase 3) will use an async
//! resampler; this module targets offline renders where quality trumps
//! latency.

/// Resamples interleaved `f32` samples from `from_rate` to `to_rate`.
///
/// Returns exactly `ceil(frames_out)` frames where
/// `frames_out = in_frames * to / from` (rounded), preserving channel count.
pub fn resample_interleaved(
    input: &[f32],
    channels: usize,
    from_rate: u32,
    to_rate: u32,
) -> Vec<f32> {
    assert!(channels > 0, "channels must be positive");
    assert!(from_rate > 0 && to_rate > 0, "rates must be positive");
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }

    let in_frames = input.len() / channels;
    let out_frames = ((in_frames as u64 * u64::from(to_rate) + u64::from(from_rate) / 2)
        / u64::from(from_rate)) as usize;

    // Normalized cutoff (relative to the input Nyquist): never above 0.5 of
    // the output rate's Nyquist requirement, so downsampling anti-aliases.
    let ratio = f64::from(to_rate) / f64::from(from_rate);
    let cutoff = if ratio < 1.0 { 0.5 * ratio } else { 0.5 };

    // Half-width of the sinc kernel in input samples: fixed zero-crossing
    // count scaled by 1/cutoff keeps the filter quality constant.
    const ZERO_CROSSINGS: f64 = 24.0;
    let half_width = (ZERO_CROSSINGS / (2.0 * cutoff)).ceil() as i64;

    let step = f64::from(from_rate) / f64::from(to_rate);
    let mut out = Vec::with_capacity(out_frames * channels);
    let mut scratch = vec![0.0f64; channels];

    for n in 0..out_frames {
        for v in scratch.iter_mut() {
            *v = 0.0;
        }
        let center = n as f64 * step;
        let base = center.floor() as i64;

        for k in (base - half_width + 1)..=(base + half_width) {
            if k < 0 || k as usize >= in_frames {
                continue;
            }
            // Distance in samples from the interpolation point.
            let d = (k as f64 - center).abs();
            if d >= half_width as f64 {
                continue;
            }
            // Windowed-sinc kernel for an ideal lowpass at `cutoff`:
            // h[d] = 2·fc·sinc(2·fc·d), sinc(x) = sin(x)/x.
            let x = 2.0 * std::f64::consts::PI * cutoff * d;
            let sinc = if d == 0.0 { 1.0 } else { x.sin() / x };
            let window = 0.5 * (1.0 + (std::f64::consts::PI * d / half_width as f64).cos());
            let weight = sinc * window * 2.0 * cutoff;
            let frame = k as usize;
            for ch in 0..channels {
                scratch[ch] += f64::from(input[frame * channels + ch]) * weight;
            }
        }
        for ch in 0..channels {
            out.push(scratch[ch] as f32);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_snr(observed: &[f32], original: &[f32]) -> f64 {
        // SNR in dB between original and aligned observed signals (same length).
        let mut signal = 0.0f64;
        let mut noise = 0.0f64;
        for (o, r) in observed.iter().zip(original) {
            signal += f64::from(*r) * f64::from(*r);
            noise += f64::from(*o - *r) * f64::from(*o - *r);
        }
        10.0 * (signal / noise).log10()
    }

    #[test]
    fn same_rate_is_identity() {
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = resample_interleaved(&input, 2, 48_000, 48_000);
        assert_eq!(out, input);
    }

    #[test]
    fn output_length_matches_rate_ratio() {
        let input = vec![0.5f32; 48_000 * 2]; // 1 s stereo
        let out = resample_interleaved(&input, 2, 48_000, 44_100);
        assert_eq!(out.len(), 44_100 * 2);
        let out = resample_interleaved(&input, 2, 192_000, 48_000);
        assert_eq!(out.len(), 12_000 * 2);
    }

    #[test]
    fn downsampling_is_transparent_in_band() {
        // 440 Hz tone @ 192 kHz -> 48 kHz. In-band tone must survive.
        let frames = 19_200; // 0.1 s
        let mut input = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = (2.0 * std::f64::consts::PI * 440.0 * n as f64 / 192_000.0).sin() as f32;
            input.extend_from_slice(&[s, s]);
        }
        let out = resample_interleaved(&input, 2, 192_000, 48_000);
        assert_eq!(out.len(), 4_800 * 2);

        // Reference: same tone born at 48 kHz.
        let mut reference = Vec::with_capacity(4_800 * 2);
        for n in 0..4_800 {
            let s = (2.0 * std::f64::consts::PI * 440.0 * n as f64 / 48_000.0).sin() as f32;
            reference.extend_from_slice(&[s, s]);
        }
        // Skip 5 % at each edge for filter ramp-up/ramp-down.
        let skip = 240;
        let snr = sine_snr(
            &out[skip * 2..out.len() - skip * 2],
            &reference[skip * 2..reference.len() - skip * 2],
        );
        assert!(snr > 50.0, "SNR {snr:.1} dB below 50 dB floor");
    }

    #[test]
    fn upsampling_is_transparent_in_band() {
        let frames = 4_800; // 0.1 s @ 48 kHz
        let mut input = Vec::with_capacity(frames);
        for n in 0..frames {
            input.push((2.0 * std::f64::consts::PI * 440.0 * n as f64 / 48_000.0).sin() as f32);
        }
        let out = resample_interleaved(&input, 1, 48_000, 96_000);
        assert_eq!(out.len(), 9_600);
        let mut reference = Vec::with_capacity(9_600);
        for n in 0..9_600 {
            reference.push((2.0 * std::f64::consts::PI * 440.0 * n as f64 / 96_000.0).sin() as f32);
        }
        let skip = 480;
        let snr = sine_snr(
            &out[skip..out.len() - skip],
            &reference[skip..reference.len() - skip],
        );
        assert!(snr > 50.0, "SNR {snr:.1} dB below 50 dB floor");
    }
}
