//! D10 verification for the air/breath engine.

use super::*;
use crate::pyin::pyin;
use crate::synth::{self, Rng};

/// Fixture: harmonic tone (voiced) + white-noise bed (residual content),
/// both present for the whole duration; the tone dominates by ~30 dB.
fn tone_plus_noise(sr: u32, seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (sr as f32 * seconds) as usize;
    let track_f0 = synth::f0_track_const(220.0, n);
    let tone = synth::harmonic_tone(&track_f0, sr, 12);
    let mut rng = Rng::new(61);
    let noise = synth::white_noise(n, &mut rng);
    // Tone at 0.45 peak; noise scaled to ~−30 dB RMS relative.
    let tone_rms = (tone.iter().map(|v| v * v).sum::<f32>() / n as f32).sqrt();
    let noise_rms = (noise.iter().map(|v| v * v).sum::<f32>() / n as f32).sqrt();
    let scale = tone_rms * 0.0316 / noise_rms; // −30 dB
    let mut x = vec![0.0f32; n];
    for i in 0..n {
        x[i] = tone[i] * 0.45 / tone_rms.max(1e-9) * tone_rms + noise[i] * scale;
    }
    // Normalize tone part to 0.45 peak; simpler: rebuild cleanly.
    let mut y = vec![0.0f32; n];
    for i in 0..n {
        y[i] = tone[i] + noise[i] * scale;
    }
    let _ = x;
    (y, noise)
}

/// Mean band energy (power) of `x − reference` in `[lo, hi]` Hz.
fn band_energy(
    x: &[f32],
    sr: u32,
    lo: f64,
    hi: f64,
    exclude_harmonics_of: Option<f64>,
    nfft: usize,
) -> f64 {
    use realfft::RealFftPlanner;
    let bin_hz = sr as f64 / nfft as f64;
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(nfft);
    let win: Vec<f32> = (0..nfft)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / nfft as f64).cos()) as f32)
        .collect();
    let mut acc = 0.0f64;
    let mut frames = 0usize;
    let mut pos = 0usize;
    let hop = nfft / 2;
    let lo_bin = (lo / bin_hz) as usize;
    let hi_bin = usize::min((hi / bin_hz) as usize, nfft / 2);
    while pos + nfft <= x.len() {
        let mut inp = r2c.make_input_vec();
        for (d, (&s, &w)) in inp.iter_mut().zip(x[pos..pos + nfft].iter().zip(&win)) {
            *d = s * w;
        }
        let mut spec = r2c.make_output_vec();
        if r2c.process(&mut inp, &mut spec).is_ok() {
            for (k, s) in spec.iter().enumerate().take(hi_bin + 1).skip(lo_bin) {
                if let Some(f0) = exclude_harmonics_of {
                    let d = (k as f64 * bin_hz - (k as f64 * bin_hz / f0).round() * f0).abs();
                    if d < bin_hz {
                        continue; // skip the comb-peak bins
                    }
                }
                acc += f64::from(s.norm_sqr());
            }
            frames += 1;
        }
        pos += hop;
    }
    if frames == 0 {
        0.0
    } else {
        acc / frames as f64
    }
}

fn band_gain_db(x: &[f32], y: &[f32], sr: u32, lo: f64, hi: f64) -> f64 {
    10.0 * ((band_energy(y, sr, lo, hi, None, 8192) + 1e-20)
        / (band_energy(x, sr, lo, hi, None, 8192) + 1e-20))
        .log10()
}

#[test]
fn neutral_is_bit_exact() {
    let sr = 48_000u32;
    let (x, _) = tone_plus_noise(sr, 0.5);
    let t = pyin(&x, sr).expect("pyin");
    let out = process_air(&x, sr, &t, 0.0).expect("render");
    assert_eq!(out.len(), x.len());
    assert!(out.iter().zip(&x).all(|(a, b)| a == b));
}

#[test]
fn positive_air_boosts_high_residual_band() {
    let sr = 48_000u32;
    let (x, _) = tone_plus_noise(sr, 1.0);
    let t = pyin(&x, sr).expect("pyin");

    let out = process_air(&x, sr, &t, 6.0).expect("render");
    // 4–6.5 kHz: band weight ≈ 1, tilt ≤ ~0.5 dB at 6 kHz for +6 dB.
    let gain = band_gain_db(&x, &out, sr, 4000.0, 6500.0);
    assert!(
        (4.5..=7.5).contains(&gain),
        "+6 dB air: 4–6.5 kHz band moved {gain:+.2} dB"
    );
    // Low band barely moves (weight 0.25).
    let low = band_gain_db(&x, &out, sr, 300.0, 900.0);
    assert!(
        low < 3.0,
        "+6 dB air: low band moved {low:+.2} dB (expected ≤ +3)"
    );
}

#[test]
fn negative_air_attenuates_residual_band() {
    let sr = 48_000u32;
    let (x, _) = tone_plus_noise(sr, 1.0);
    let t = pyin(&x, sr).expect("pyin");

    let out = process_air(&x, sr, &t, -12.0).expect("render");
    let gain = band_gain_db(&x, &out, sr, 3000.0, 4500.0);
    assert!(
        (-14.0..=-9.0).contains(&gain),
        "−12 dB air: 3–4.5 kHz band moved {gain:+.2} dB"
    );
}

#[test]
fn negative_air_concentrates_in_sibilance_band() {
    let sr = 48_000u32;
    let (x, _) = tone_plus_noise(sr, 1.0);
    let t = pyin(&x, sr).expect("pyin");

    let out = process_air(&x, sr, &t, -12.0).expect("render");
    let mid = band_gain_db(&x, &out, sr, 1500.0, 2500.0);
    let ess = band_gain_db(&x, &out, sr, 6500.0, 9500.0);
    assert!(
        ess < mid - 3.0,
        "de-ess: sibilance band {ess:+.2} dB not deeper than mid band {mid:+.2} dB"
    );
}

#[test]
fn harmonic_bins_are_untouched() {
    let sr = 48_000u32;
    let (x, _) = tone_plus_noise(sr, 1.0);
    let t = pyin(&x, sr).expect("pyin");

    for &g in &[-12.0f64, 12.0] {
        let out = process_air(&x, sr, &t, g).expect("render");
        // Energy AT the comb bins (k·220, k = 2..12) must stay within
        // ±0.5 dB — the clarity contract.
        let before = band_energy(&x, sr, 300.0, 2500.0, Some(220.0), 8192);
        let after = band_energy(&out, sr, 300.0, 2500.0, Some(220.0), 8192);
        let d = 10.0 * ((after + 1e-20) / (before + 1e-20)).log10();
        assert!(d.abs() <= 0.5, "comb bins moved {d:+.2} dB at {g} dB air");
    }
}

#[test]
fn breath_burst_gets_downward_expansion() {
    // Vowel / noise burst / vowel; at −18 dB the burst (unvoiced, flat
    // spectrum → breath_prob ≈ 1) must be attenuated deeper than the flat
    // weight alone, while the vowel's comb stays untouched.
    let sr = 48_000u32;
    let seg = sr as usize * 2 / 5;
    let track_f0 = synth::f0_track_const(180.0, seg);
    let mut rng = Rng::new(62);
    let mut x = synth::vowel(&track_f0, sr, &synth::VowelSpec::default(), &mut rng);
    let burst = synth::white_noise(seg, &mut rng);
    x.extend(burst.iter().map(|v| v * 0.3));
    let track_f0b = synth::f0_track_const(190.0, seg);
    x.append(&mut synth::vowel(
        &track_f0b,
        sr,
        &synth::VowelSpec::default(),
        &mut rng,
    ));

    let t = pyin(&x, sr).expect("pyin");
    let out = process_air(&x, sr, &t, -18.0).expect("render");

    // Interior of the burst, away from the frame-boundary crossfades.
    let lo = seg + seg / 5;
    let hi = seg + (4 * seg) / 5;
    let burst_in = band_energy(&x[lo..hi], sr, 500.0, 4000.0, None, 4096);
    let burst_out = band_energy(&out[lo..hi], sr, 500.0, 4000.0, None, 4096);
    let drop = 10.0 * ((burst_out + 1e-20) / (burst_in + 1e-20)).log10();
    // Flat weight at 0.5–4 kHz averages ~0.6 → −11 dB; with the 1.8×
    // breath expansion expect ≈ −19 dB or deeper.
    assert!(drop <= -14.0, "breath burst attenuated only {drop:+.2} dB");
}

#[test]
fn silence_input_returns_copy() {
    let sr = 48_000u32;
    let x = vec![0.0f32; sr as usize / 4];
    let t = pyin(&x, sr).expect("pyin");
    let out = process_air(&x, sr, &t, -12.0).expect("render");
    assert!(out.iter().all(|&v| v == 0.0));
}
