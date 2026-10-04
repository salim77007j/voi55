//! Formant (vocal-tract length) engine — D9.
//!
//! **Implementation note (Phase 3, amends D9).** D9 sketched LPC with an
//! allpass Bark warp. Two amendments, both verified against the fixtures:
//!
//! 1. **Exact envelope resampling instead of the allpass approximation.**
//!    The allpass map's effective ratio varies per formant (up to ~30 % at
//!    F4), violating the contractual mapping `F_new ≈ F_old · 175/L` that
//!    `EngineParams::formant_scale` exposes. The warp is the exact
//!    frequency-axis resampling of the envelope.
//! 2. **Cepstral envelope estimation instead of LPC.** The per-frame
//!    spectral envelope is measured by quefrency liftering (comb removed,
//!    features ≥ 300 Hz kept). LPC's least-squares fit proved fragile on
//!    strongly periodic synthetic excitations — its poles follow the
//!    spectral tilt rather than the narrow formant bumps, which starves
//!    the warp of formant evidence. Cepstral lifting is the robust
//!    standard for envelope estimation and needs no excitation-model
//!    assumptions. `lpc.rs` remains for diagnostics.
//!
//! Pipeline per Hann-STFT frame: log-spectrum → cepstrum → lifter →
//! smooth envelope `env(f)` (log domain) → warped envelope
//! `env(f/r)` with `r = 175/target_mm` → corrective ratio
//! `R(f) = exp(env(f/r) − env(f))` → smooth, ±18 dB limit, band-edge
//! taper → `Y = X·R` (loudness-compensated) → weighted overlap-add.
//! Voiced-only processing: unvoiced frames pass through bit-exactly, so
//! sibilance never lisps (D9 guardrail). Pitch is untouched: the ratio is
//! a fixed linear filter per frame and the excitation periodicity is
//! preserved (verified by the pYIN gate below).

use crate::error::{CoreError, Result};
use crate::ola::{CarryOla, EnvStream};
use crate::pyin::PyinResult;
use rustfft::num_complex::Complex;

/// Reference adult vocal-tract length (mm) — mirrors `engine::REFERENCE_VTL_MM`.
const REFERENCE_VTL_MM: f64 = 175.0;
/// Hard guardrails for the raw mm argument (the UI clamp is ±[130, 190]).
const MM_HARD_LIMITS: (f64, f64) = (100.0, 250.0);
/// Envelope-ratio limiter (±18 dB).
const RATIO_LIMIT: f64 = 7.943; // 10^(18/20)
/// Analysis band edge (Hz); envelope is estimated and warped below it.
const BAND_LIMIT_HZ: f64 = 8000.0;
/// Envelope feature floor (Hz) — sets the cepstral lifter length.
const ENV_FEATURE_HZ: f64 = 300.0;
/// Overlap-add window-sum floor.
const WSUM_FLOOR: f32 = 1e-6;

/// Rescales the vocal tract to `target_mm` (formants scale by
/// `175/target_mm`), driven by the pYIN voicing track.
///
/// # Errors
/// Fails when `target_mm` is outside the hard limits or on FFT errors.
pub fn shift_formants(
    x: &[f32],
    sample_rate: u32,
    track: &PyinResult,
    target_mm: f64,
) -> Result<Vec<f32>> {
    if !(MM_HARD_LIMITS.0..=MM_HARD_LIMITS.1).contains(&target_mm) {
        return Err(CoreError::Analysis(format!(
            "vocal-tract length {target_mm} mm outside {}..{} mm",
            MM_HARD_LIMITS.0, MM_HARD_LIMITS.1
        )));
    }
    let ratio = REFERENCE_VTL_MM / target_mm;
    if (ratio - 1.0).abs() < 1e-9 || x.is_empty() {
        return Ok(x.to_vec());
    }

    let n =
        usize::clamp((0.02 * sample_rate as f64).round() as usize, 1024, 8192).next_power_of_two();
    let hop = n / 2;
    let bin_hz = sample_rate as f64 / n as f64;
    let band_bins = usize::min(n / 2, (BAND_LIMIT_HZ / bin_hz) as usize);
    let lifter = usize::max(8, (sample_rate as f64 / ENV_FEATURE_HZ) as usize);

    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(n);
    let c2r = planner.plan_fft_inverse(n);

    let window: Vec<f32> = (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect();

    let len = x.len();
    // Carry-based weighted OLA: frame t writes [t·hop, t·hop + n), so a
    // fixed ring of span n replaces the two full-length buffers; finished
    // samples flush to the output before each frame (bit-identical adds —
    // see `ola`).
    let mut ola = CarryOla::new(len, n);
    let mut env = EnvStream::new(track, sample_rate);
    let mut out: Vec<f32> = Vec::with_capacity(len);
    let frames = (len - 1) / hop + 1;

    let mut spectrum = r2c.make_output_vec();
    let mut in_buf = r2c.make_input_vec();
    let mut y_out = c2r.make_output_vec();
    let mut cep_in = c2r.make_input_vec();
    let mut cep = c2r.make_output_vec();
    let mut env_spec = r2c.make_input_vec();
    let mut env_bins = r2c.make_output_vec();
    // Log-envelope on the band grid (frame scratch).
    let mut env_ln = vec![0.0f64; band_bins + 2];
    let mut env_w = vec![0.0f64; band_bins + 2];

    for frame in 0..frames {
        // Everything before this frame's write window is complete: the
        // previous frame wrote up to (frame−1)·hop + n, which is covered.
        ola.flush_to(frame * hop, x, Some(&mut env), WSUM_FLOOR, &mut out);
        let start = frame * hop;
        let take = usize::min(n, len.saturating_sub(start));
        in_buf[..take].fill(0.0);
        for (dst, (&src, &w)) in in_buf[..take]
            .iter_mut()
            .zip(x[start..start + take].iter().zip(&window))
        {
            *dst = src * w;
        }
        r2c.process(&mut in_buf, &mut spectrum)
            .map_err(|e| CoreError::Fft(e.to_string()))?;

        // --- cepstral log-envelope of this frame ------------------------
        // IFFT of the even log-spectrum → real cepstrum → lifter → FFT
        // back. Quefrencies above `sr/ENV_FEATURE_HZ` (the harmonic comb
        // and finer ripple) are removed; what remains is the spectral
        // envelope in log magnitude.
        cep_in[..=band_bins].copy_from_slice(&spectrum[..=band_bins]);
        for c in cep_in[..=band_bins].iter_mut() {
            *c = Complex::new((c.norm() + 1e-12).ln(), 0.0);
        }
        for c in cep_in[band_bins + 1..].iter_mut() {
            *c = Complex::new(0.0, 0.0);
        }
        c2r.process(&mut cep_in, &mut cep)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        let cep_len = cep.len();
        for (q, v) in cep.iter_mut().enumerate() {
            if q > lifter && q + 1 < cep_len - lifter {
                *v = 0.0;
            }
        }
        for (dst, &v) in env_spec.iter_mut().zip(&cep) {
            *dst = v / n as f32;
        }
        r2c.process(&mut env_spec, &mut env_bins)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        for (k, slot) in env_ln.iter_mut().enumerate().take(band_bins + 2) {
            let k_c = usize::min(k, band_bins);
            *slot = f64::from(env_bins[k_c].re);
        }

        // --- warped envelope and corrective ratio (log domain) ----------
        for (k, slot) in env_w.iter_mut().enumerate().take(band_bins + 2) {
            let src = k as f64 / ratio;
            let i0 = usize::min(src.floor() as usize, band_bins);
            let frac = (src - i0 as f64).clamp(0.0, 1.0);
            *slot = env_ln[i0] * (1.0 - frac) + env_ln[i0 + 1] * frac;
        }
        let taper_start = (band_bins as f64 * 0.85) as usize;
        let mut applied = vec![1.0f64; band_bins + 2];
        for (k, slot) in applied.iter_mut().enumerate().take(band_bins + 2) {
            *slot = env_w[k] - env_ln[k]; // log-domain ratio
        }
        smooth5(&mut applied, band_bins + 1);
        let limit = RATIO_LIMIT.ln();
        for (k, slot) in applied.iter_mut().enumerate().take(band_bins + 2) {
            let mut rk = slot.clamp(-limit, limit);
            if k > taper_start {
                let t = (k - taper_start) as f64 / (band_bins + 1 - taper_start) as f64;
                let fade = 1.0 - t * t * (3.0 - 2.0 * t);
                rk *= fade;
            }
            *slot = rk.exp();
        }

        // --- loudness compensation, apply, overlap-add ------------------
        let mut e_in = 0.0f64;
        let mut e_out = 0.0f64;
        for (k, spec) in spectrum.iter().enumerate() {
            let rk = if k <= band_bins + 1 { applied[k] } else { 1.0 };
            e_in += f64::from(spec.norm_sqr());
            e_out += f64::from(spec.norm_sqr()) * rk * rk;
        }
        let gain = if e_out > 1e-12 {
            (e_in / e_out).sqrt().clamp(0.5, 2.0)
        } else {
            1.0
        };

        for (k, spec) in spectrum.iter_mut().enumerate() {
            let rk = if k <= band_bins + 1 { applied[k] } else { 1.0 };
            *spec *= (rk * gain) as f32;
        }
        c2r.process(&mut spectrum, &mut y_out)
            .map_err(|e| CoreError::Fft(e.to_string()))?;

        let inv_n = 1.0 / n as f32;
        for i in 0..take {
            let w = window[i];
            ola.add(start + i, y_out[i] * inv_n * w, w * w);
        }
    }

    // Weighted-OLA normalize, then voicing crossfade (exact bypass where
    // the envelope is zero — sibilance and breath never warp).
    ola.flush_all(x, Some(&mut env), WSUM_FLOOR, &mut out);
    Ok(out)
}

/// In-place 5-tap moving average over `0..=limit` of `r`.
fn smooth5(r: &mut [f64], limit: usize) {
    if limit < 2 {
        return;
    }
    let mut scratch = vec![0.0f64; limit + 1];
    for (k, slot) in scratch.iter_mut().enumerate().take(limit + 1) {
        let lo = k.saturating_sub(2);
        let hi = usize::min(k + 2, limit);
        let n = (hi - lo + 1) as f64;
        *slot = r[lo..=hi].iter().sum::<f64>() / n;
    }
    r[..=limit].copy_from_slice(&scratch);
}

#[cfg(test)]
mod tests;
