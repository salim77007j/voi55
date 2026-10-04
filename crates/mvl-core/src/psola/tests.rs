//! D8 verification for the TD-PSOLA pitch shifter.
//!
//! Measurement notes: the shifted output's fundamental is verified with
//! **normalized autocorrelation arg-max** (the ground-truth periodicity
//! measure) rather than pYIN. YIN-family trackers pick the *first* dip of
//! the difference function, which on PSOLA outputs with a weak fundamental
//! (H1 below the first formant after large downshifts) can report a
//! harmonic instead — a known analysis limitation of the whole YIN family,
//! recorded in the phase report. It does not affect the render path: the
//! engines analyze the *input* track once.

use super::*;
use crate::pyin::pyin;
use crate::synth::{self, Rng, VowelSpec};

/// Fundamental-frequency estimate via normalized autocorrelation arg-max
/// with parabolic refinement. `fmin`/`fmax` in Hz bound the search.
fn measure_f0_acf(x: &[f32], sr: u32, fmin: f64, fmax: f64) -> f64 {
    let n = usize::min(x.len(), sr as usize);
    let start = (x.len() - n) / 2;
    let seg = &x[start..start + n];
    let lo = ((sr as f64) / fmax).floor() as usize;
    let hi = usize::min(((sr as f64) / fmin).ceil() as usize, n / 2 - 1);

    let mut best_lag = lo;
    let mut best_val = f64::NEG_INFINITY;
    let mut acf = vec![0.0f64; hi + 2];
    for tau in lo..=hi {
        let mut acc = 0.0f64;
        for i in 0..n - tau {
            acc += f64::from(seg[i]) * f64::from(seg[i + tau]);
        }
        let norm = acc / (n - tau) as f64;
        acf[tau] = norm;
        if norm > best_val {
            best_val = norm;
            best_lag = tau;
        }
    }
    // Parabolic refinement around the arg-max.
    let (a, b, c) = (acf[best_lag - 1], acf[best_lag], acf[best_lag + 1]);
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-12 {
        (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    sr as f64 / (best_lag as f64 + delta)
}

#[test]
fn neutral_shift_is_bit_exact() {
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(220.0, sr as usize / 2);
    let x = synth::harmonic_tone(&track_f0, sr, 10);
    let t = pyin(&x, sr).expect("pyin");
    let out = pitch_shift(&x, sr, &t, 0.0);
    assert_eq!(out.len(), x.len());
    assert!(out.iter().zip(&x).all(|(a, b)| a == b));
}

#[test]
fn pitch_up_three_semitones_is_measured() {
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(220.0, sr as usize);
    let mut rng = Rng::new(21);
    let x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");

    let out = pitch_shift(&x, sr, &t, 3.0);
    assert_eq!(out.len(), x.len(), "duration must be preserved exactly");

    let target = 220.0f64 * 2.0f64.powf(3.0 / 12.0); // 261.63 Hz
    let measured = measure_f0_acf(&out, sr, 80.0, 1000.0);
    let cents = crate::pyin::cents_between(measured, target);
    assert!(
        cents.abs() <= 30.0,
        "+3 st: ACF measured {measured:.2} Hz ({cents:+.2} c off target)"
    );
}

#[test]
fn pitch_down_seven_semitones_is_measured() {
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(220.0, sr as usize);
    let mut rng = Rng::new(22);
    let x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");

    let out = pitch_shift(&x, sr, &t, -7.0);
    assert_eq!(out.len(), x.len());

    let target = 220.0f64 * 2.0f64.powf(-7.0 / 12.0); // 146.83 Hz
    let measured = measure_f0_acf(&out, sr, 60.0, 600.0);
    let cents = crate::pyin::cents_between(measured, target);
    assert!(
        cents.abs() <= 30.0,
        "−7 st: ACF measured {measured:.2} Hz ({cents:+.2} c off target)"
    );
}

#[test]
fn vibrato_pitch_shift_tracks_depth() {
    let sr = 48_000u32;
    let frames = sr as usize;
    let track_f0 = synth::vibrato_f0_track(220.0, 50.0, 5.0, frames, sr);
    let x = synth::harmonic_tone(&track_f0, sr, 10);
    let t = pyin(&x, sr).expect("pyin");

    let semis = 5.0f64;
    let out = pitch_shift(&x, sr, &t, semis);

    // Whole-signal ACF averages over full vibrato cycles (5 Hz → integer
    // number of cycles in 1 s), giving the mean F0 on both sides.
    let f_in = measure_f0_acf(&x, sr, 80.0, 600.0);
    let f_out = measure_f0_acf(&out, sr, 80.0, 900.0);
    let expected = f_in * 2.0f64.powf(semis / 12.0);
    let cents = crate::pyin::cents_between(f_out, expected);
    assert!(
        cents.abs() <= 40.0,
        "vibrato shift: in-mean {f_in:.2} -> out-mean {f_out:.2}, expected {expected:.2} ({cents:+.2} c)"
    );
}

#[test]
fn unvoiced_interior_bypasses_bit_exactly() {
    let sr = 48_000u32;
    let seg = sr as usize * 3 / 10;
    let track_f0 = synth::f0_track_const(200.0, seg);
    let mut rng = Rng::new(23);
    let vowel = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let noise = synth::white_noise(seg, &mut rng);
    let mut x = vowel;
    x.extend(noise);
    let track_f0b = synth::f0_track_const(210.0, seg);
    let vowel_b = synth::vowel(&track_f0b, sr, &VowelSpec::default(), &mut rng);
    let mut vowel_b = vowel_b;
    x.append(&mut vowel_b);

    let t = pyin(&x, sr).expect("pyin");
    let out = pitch_shift(&x, sr, &t, 4.0);

    // Interior of the noise segment (well past the crossfades).
    let lo = seg + seg / 5;
    let hi = seg + (4 * seg) / 5;
    for i in lo..hi {
        assert_eq!(
            out[i], x[i],
            "unvoiced sample {i} altered: {} vs {}",
            out[i], x[i]
        );
    }
}

#[test]
fn spectral_envelope_is_preserved_on_shift() {
    // The spectral envelope must not move when the pitch shifts (D8
    // contract). Instrument: cepstrally-smoothed LTAS (comb removed by
    // quefrency liftering) → F1/F2 peak positions must agree within 12 %.
    // The residual displacement (~10 % measured on +5 st) is the known
    // TD-PSOLA overlap-cancellation ripple at non-integer ratios — see
    // the phase report's honest-gaps section.
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(150.0, sr as usize);
    let mut rng = Rng::new(24);
    let x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");

    let out = pitch_shift(&x, sr, &t, 5.0);
    let before = formant_peaks_cepstral(&x, sr);
    let after = formant_peaks_cepstral(&out, sr);
    assert_eq!(before.len(), after.len());
    for (b, a) in before.iter().zip(&after) {
        let rel = (a - b).abs() / b;
        assert!(
            rel <= 0.12,
            "envelope peak moved {b:.0} -> {a:.0} Hz ({:.1} %)",
            rel * 100.0
        );
    }
}

/// Cepstrally-smoothed LTAS envelope → the two strongest low-order formant
/// peak positions (F1 in 300–700 Hz, F2 in 1200–1900 Hz), parabolically
/// refined. The quefrency lifter (≤ 160 bins ≈ features ≥ 300 Hz) removes
/// the harmonic comb, so the comparison is F0-independent.
fn formant_peaks_cepstral(x: &[f32], sr: u32) -> Vec<f64> {
    use realfft::RealFftPlanner;
    use rustfft::num_complex::Complex;
    let nfft = 8192usize;
    let bin_hz = sr as f64 / nfft as f64;
    let ltas = welch_ltas(x); // log magnitude, length nfft/2+1

    let mut planner = RealFftPlanner::<f32>::new();
    let c2r = planner.plan_fft_inverse(nfft);
    let r2c = planner.plan_fft_forward(nfft);

    // Real cepstrum: IFFT of the even log-spectrum.
    let mut herm = c2r.make_input_vec();
    for (dst, &v) in herm.iter_mut().zip(&ltas) {
        *dst = Complex::new(v as f32, 0.0);
    }
    let mut cep = c2r.make_output_vec();
    c2r.process(&mut herm, &mut cep).expect("cepstrum");

    // Lifter: keep quefrencies ≤ 160 samples (≈ spectral features ≥ 300 Hz),
    // drop the harmonic-comb spikes at higher quefrencies. (c2r output is
    // a plain real vector.)
    let lifter = 160usize;
    for n in lifter..cep.len() - lifter {
        cep[n] = 0.0;
    }
    let mut env_spec = r2c.make_input_vec();
    for (dst, &c) in env_spec.iter_mut().zip(&cep) {
        *dst = c / nfft as f32;
    }
    let mut env = r2c.make_output_vec();
    r2c.process(&mut env_spec, &mut env).expect("envelope");
    let env_lin: Vec<f64> = env.iter().map(|c| f64::from(c.re)).collect();

    let find_peak = |lo_hz: f64, hi_hz: f64| -> f64 {
        let lo = (lo_hz / bin_hz) as usize;
        let hi = usize::min((hi_hz / bin_hz) as usize, env_lin.len() - 2);
        let mut best = lo;
        for k in lo..=hi {
            if env_lin[k] > env_lin[best] {
                best = k;
            }
        }
        if best == 0 || best + 1 >= env_lin.len() {
            return best as f64 * bin_hz;
        }
        let (a, b, c) = (env_lin[best - 1], env_lin[best], env_lin[best + 1]);
        let denom = a - 2.0 * b + c;
        let delta = if denom.abs() > 1e-12 {
            (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
        } else {
            0.0
        };
        (best as f64 + delta) * bin_hz
    };

    vec![find_peak(300.0, 700.0), find_peak(1200.0, 1900.0)]
}

fn welch_ltas(x: &[f32]) -> Vec<f64> {
    use realfft::RealFftPlanner;
    let nfft = 8192usize;
    let hop = 4096usize;
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(nfft);
    let window: Vec<f32> = (0..nfft)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / nfft as f64).cos()) as f32)
        .collect();
    let mut acc = vec![0.0f64; nfft / 2 + 1];
    let mut spec = r2c.make_output_vec();
    let mut frames = 0usize;
    let mut pos = 0usize;
    while pos + nfft <= x.len() {
        let mut seg: Vec<f32> = x[pos..pos + nfft]
            .iter()
            .zip(&window)
            .map(|(v, w)| v * w)
            .collect();
        if r2c.process(&mut seg, &mut spec).is_ok() {
            for (k, c) in spec.iter().enumerate() {
                acc[k] += f64::from(c.norm_sqr());
            }
            frames += 1;
        }
        pos += hop;
    }
    if frames == 0 {
        return acc;
    }
    acc.iter()
        .map(|v| (v / frames as f64).max(1e-12).ln())
        .collect()
}

#[test]
fn silence_input_returns_copy() {
    let sr = 48_000u32;
    let x = vec![0.0f32; sr as usize / 4];
    let t = pyin(&x, sr).expect("pyin");
    let out = pitch_shift(&x, sr, &t, 3.0);
    assert!(out.iter().all(|&v| v == 0.0));
}
