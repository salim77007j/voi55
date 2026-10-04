//! Measurement utilities for verification gates and evidence fixtures.
//!
//! These are analysis instruments, not part of the render path: Welch
//! long-term average spectra, cepstrally-smoothed spectral envelopes
//! (harmonic comb removed by quefrency liftering) and an autocorrelation
//! F0 estimator. They exist so every accuracy claim in the reports can be
//! reproduced from committed code with the same instrument.

use realfft::RealFftPlanner;
use rustfft::num_complex::Complex;

/// Welch-averaged log-magnitude spectrum (Hann windows, 50 % hop).
/// Returns `nfft/2 + 1` bins; bin width is `sr / nfft`.
pub fn welch_ltas(x: &[f32], nfft: usize) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(nfft);
    let window: Vec<f32> = (0..nfft)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / nfft as f64).cos()) as f32)
        .collect();
    let mut acc = vec![0.0f64; nfft / 2 + 1];
    let mut spec = r2c.make_output_vec();
    let mut frames = 0usize;
    let mut pos = 0usize;
    let hop = nfft / 2;
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
    if frames > 0 {
        for v in &mut acc {
            *v /= frames as f64;
        }
    }
    acc.iter().map(|v| v.max(1e-12).ln()).collect()
}

/// Cepstrally-smoothed log-spectral envelope: the harmonic comb is removed
/// by zeroing quefrencies above `lifter` (a comb spaced `Δf` Hz lives at
/// quefrency `sr/Δf`; `lifter = sr/300` keeps features ≥ 300 Hz wide).
/// Returned bins match `welch_ltas` geometry (`nfft/2 + 1`).
pub fn cepstral_envelope(x: &[f32], nfft: usize, lifter: usize) -> Vec<f64> {
    let ltas = welch_ltas(x, nfft);
    let mut planner = RealFftPlanner::<f32>::new();
    let c2r = planner.plan_fft_inverse(nfft);
    let r2c = planner.plan_fft_forward(nfft);

    // Real cepstrum: inverse transform of the even log-spectrum. The c2r
    // output is a plain real vector scaled by nfft.
    let mut herm = c2r.make_input_vec();
    for (dst, &v) in herm.iter_mut().zip(&ltas) {
        *dst = Complex::new(v as f32, 0.0);
    }
    let mut cep = c2r.make_output_vec();
    if c2r.process(&mut herm, &mut cep).is_err() {
        return ltas;
    }
    for v in cep.iter_mut().take(nfft).skip(lifter) {
        *v = 0.0;
    }
    for v in cep.iter_mut().rev().take(lifter.saturating_sub(1)) {
        *v = 0.0;
    }

    let mut env_spec = r2c.make_input_vec();
    for (dst, &c) in env_spec.iter_mut().zip(&cep) {
        *dst = c / nfft as f32;
    }
    let mut env = r2c.make_output_vec();
    if r2c.process(&mut env_spec, &mut env).is_err() {
        return ltas;
    }
    env.iter().map(|c| f64::from(c.re)).collect()
}

/// Parabolically-refined arg-max of `env` (bin grid, `bin_hz` per bin)
/// inside a frequency band. Returns the peak frequency in Hz.
pub fn envelope_peak(env: &[f64], bin_hz: f64, lo_hz: f64, hi_hz: f64) -> f64 {
    let lo = (lo_hz / bin_hz).clamp(1.0, (env.len() - 2) as f64) as usize;
    let hi = usize::min((hi_hz / bin_hz) as usize, env.len() - 2);
    if hi <= lo {
        return lo as f64 * bin_hz;
    }
    let mut best = lo;
    for k in lo..=hi {
        if env[k] > env[best] {
            best = k;
        }
    }
    let (a, b, c) = (env[best - 1], env[best], env[best + 1]);
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-12 {
        (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (best as f64 + delta) * bin_hz
}

/// Fundamental-frequency estimate via normalized autocorrelation arg-max
/// with parabolic refinement (the ground-truth periodicity measure used to
/// verify the PSOLA stage; see `psola::tests` module docs).
pub fn f0_acf(x: &[f32], sample_rate: u32, fmin: f64, fmax: f64) -> f64 {
    if x.len() < 4 {
        return 0.0;
    }
    let n = usize::min(x.len(), sample_rate as usize);
    let start = (x.len() - n) / 2;
    let seg = &x[start..start + n];
    let lo = ((sample_rate as f64) / fmax).floor() as usize;
    let hi = usize::min(((sample_rate as f64) / fmin).ceil() as usize, n / 2 - 1);
    if hi <= lo + 1 {
        return 0.0;
    }

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
    let (a, b, c) = (acf[best_lag - 1], acf[best_lag], acf[best_lag + 1]);
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-12 {
        (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    sample_rate as f64 / (best_lag as f64 + delta)
}

/// Fundamental-frequency estimate via the **cepstrum**: the peak of the
/// real cepstrum in the quefrency range `[sr/fmax, sr/fmin]` is the
/// harmonic-comb period. This is the instrument for PSOLA outputs, where
/// two periodicities coexist — the grain-placement comb (the audible
/// pitch) and the grain-internal content periodicity (the input pitch,
/// which can dominate a plain autocorrelation). The cepstrum locks onto
/// the comb spacing, i.e. the placement rate.
pub fn f0_ceps(x: &[f32], sample_rate: u32, fmin: f64, fmax: f64) -> f64 {
    let nfft = 8192usize;
    if x.len() < nfft / 2 {
        return 0.0;
    }
    let ltas = welch_ltas(x, nfft); // log magnitude, nfft/2+1 bins
    let mut planner = RealFftPlanner::<f32>::new();
    let c2r = planner.plan_fft_inverse(nfft);
    let mut herm = c2r.make_input_vec();
    for (dst, &v) in herm.iter_mut().zip(&ltas) {
        *dst = Complex::new(v as f32, 0.0);
    }
    let mut cep = c2r.make_output_vec();
    if c2r.process(&mut herm, &mut cep).is_err() {
        return 0.0;
    }

    let lo = ((sample_rate as f64) / fmax).ceil() as usize;
    let hi = usize::min(((sample_rate as f64) / fmin).floor() as usize, nfft / 2 - 2);
    if hi <= lo + 1 {
        return 0.0;
    }
    let mut best_q = lo;
    let mut best_v = f64::NEG_INFINITY;
    for (q, &v) in cep.iter().enumerate().take(hi + 1).skip(lo) {
        if f64::from(v) > best_v {
            best_v = f64::from(v);
            best_q = q;
        }
    }
    if best_q == 0 || best_q + 1 >= cep.len() {
        return 0.0;
    }
    let (a, b, c) = (
        f64::from(cep[best_q - 1]),
        f64::from(cep[best_q]),
        f64::from(cep[best_q + 1]),
    );
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-12 {
        (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    sample_rate as f64 / (best_q as f64 + delta)
}

/// Strength of the harmonic comb at frequency `f` in `x`: the real
/// cepstral coefficient at quefrency `sr/f` (parabolically refined).
/// Used to compare *competing* combs in a PSOLA output — the placement
/// comb (target pitch) versus the grain-content comb (input pitch).
pub fn ceps_strength(x: &[f32], sample_rate: u32, f: f64) -> f64 {
    let nfft = 8192usize;
    if x.len() < nfft / 2 || f <= 0.0 {
        return f64::NEG_INFINITY;
    }
    let ltas = welch_ltas(x, nfft);
    let mut planner = RealFftPlanner::<f32>::new();
    let c2r = planner.plan_fft_inverse(nfft);
    let mut herm = c2r.make_input_vec();
    for (dst, &v) in herm.iter_mut().zip(&ltas) {
        *dst = Complex::new(v as f32, 0.0);
    }
    let mut cep = c2r.make_output_vec();
    if c2r.process(&mut herm, &mut cep).is_err() {
        return f64::NEG_INFINITY;
    }
    let q0 = ((sample_rate as f64) / f).round() as usize;
    if q0 == 0 || q0 + 1 >= cep.len() {
        return f64::NEG_INFINITY;
    }
    let (a, b, c) = (
        f64::from(cep[q0 - 1]),
        f64::from(cep[q0]),
        f64::from(cep[q0 + 1]),
    );
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-12 {
        (0.5 * (a - c) / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    b - 0.25 * (a - c) * delta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{self, Rng, VowelSpec};

    #[test]
    fn cepstral_envelope_finds_the_synth_formants() {
        let sr = 48_000u32;
        let track = synth::f0_track_const(140.0, sr as usize);
        let mut rng = Rng::new(31);
        let x = synth::vowel(&track, sr, &VowelSpec::default(), &mut rng);
        let nfft = 8192;
        let lifter = sr as usize / 300; // features ≥ 300 Hz
        let env = cepstral_envelope(&x, nfft, lifter);
        let bin_hz = sr as f64 / nfft as f64;
        let f1 = envelope_peak(&env, bin_hz, 300.0, 700.0);
        let f2 = envelope_peak(&env, bin_hz, 1200.0, 1900.0);
        assert!((f1 - 500.0).abs() < 40.0, "F1 = {f1:.1}");
        assert!((f2 - 1500.0).abs() < 60.0, "F2 = {f2:.1}");
    }

    #[test]
    fn f0_acf_matches_constant_tone() {
        let sr = 48_000u32;
        let track = synth::f0_track_const(196.0, sr as usize / 2);
        let x = synth::harmonic_tone(&track, sr, 10);
        let f = f0_acf(&x, sr, 70.0, 600.0);
        assert!((f - 196.0).abs() / 196.0 < 0.02, "f0_acf = {f:.2}");
    }
}
