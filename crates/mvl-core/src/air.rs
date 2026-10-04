//! Air & Breath engine — D10 (the signature feature).
//!
//! Two-path STFT processor: a **harmonic path** (passed through untouched —
//! the clarity contract) and a **residual path** (breath, aspiration,
//! fricatives, sibilance, room air) whose gain the slider controls at
//! 0.1 dB resolution.
//!
//! **Separation (implementation note, amends D10's median detail).** The
//! discriminator between "harmonic" and "residual" is the *pYIN comb*: on
//! voiced frames the harmonic mask is a Gaussian around the harmonics of
//! the tracked F0 (width = analysis main lobe + a small frequency-
//! proportional smear term); unvoiced frames are 100 % residual, so breath
//! removal acts on them at full strength. A time-median of |X| was
//! evaluated first (Fitzgerald HPSS-style) and rejected: stationary noise
//! is as time-stable as the harmonics, so the median classifies it as
//! harmonic — the pitch comb is the actual discriminator for voice. The
//! mask is smoothed over ±2 frames to avoid flicker.
//!
//! Slider semantics (0.1 dB steps, [−24, +12] dB via the parameter
//! contract):
//! - **positive (add air):** residual gain with a band weight rising from
//!   0.25 at 200 Hz to 1.0 at 4 kHz, plus a gentle tilt (up to +6 dB,
//!   scaled by the slider) above ~6 kHz — air reads as warm breathiness.
//! - **negative (remove):** the same band weight, concentrated further in
//!   the 5–13 kHz sibilance band (de-esser) and deepened by **downward
//!   expansion during detected breath** — frames with high spectral
//!   flatness (1–8 kHz geometric/arithmetic ratio) and no voicing get up
//!   to 80 % more attenuation.
//!
//! Neutral (`air_db == 0`) returns the input bit-exactly; the harmonic
//! path gain is exactly 1.0 for any slider position — only the residual
//! is ever touched.

use std::collections::VecDeque;

use crate::error::{CoreError, Result};
use crate::ola::CarryOla;
use crate::pyin::PyinResult;

/// Mask smoothing half-width in STFT frames (±2 frames ≈ ±21 ms @48 kHz).
const MASK_SMOOTH_HALF: usize = 2;
/// Comb width floor: the Hann main-lobe half-width (~2.5 bins).
const COMB_MAIN_LOBE_BINS: f64 = 2.5;
/// Comb width proportional term (covers small F0 drift; vibrato beyond
/// this leaks harmonics into the residual — accepted v1 trade-off).
const COMB_SMEAR: f64 = 0.01;
/// Residual band weight: 25 % of the slider at 200 Hz, 100 % at 4 kHz.
const BAND_LOW_HZ: f64 = 200.0;
const BAND_HIGH_HZ: f64 = 4000.0;
/// De-ess band edges (Hz) for the negative path.
const ESS_RISE: (f64, f64) = (4500.0, 5500.0);
const ESS_FALL: (f64, f64) = (11000.0, 13000.0);
/// Breath downward-expansion depth (extra attenuation fraction at full
/// breath probability).
const BREATH_EXPANSION: f64 = 0.8;
/// Breath-detector flatness gate (geometric/arithmetic spectral ratio in
/// 1–8 kHz).
const BREATH_FLAT_LO: f64 = 0.2;
const BREATH_FLAT_HI: f64 = 0.5;
/// Peak-above-floor gate: a bin is harmonic when its magnitude exceeds
/// 2.5× (full weight at 5×) the local spectral floor. Noise magnitudes
/// (Rayleigh-distributed around the floor) then sit at zero prominence,
/// which also keeps pYIN's rare voiced-misclassified noise frames from
/// leaking full-band energy.
const PEAK_FLOOR_LO: f64 = 2.5;
const PEAK_FLOOR_HI: f64 = 5.0;
/// Local-floor window for the peak test (±bins either side).
const PEAK_FLOOR_BINS: usize = 10;
/// Gain clamp (dB).
const GAIN_MIN_DB: f64 = -40.0;
const GAIN_MAX_DB: f64 = 30.0;

/// Processes the air/breath content of `x` by `air_db` (negative removes
/// breath/sibilance, positive adds air).
///
/// # Errors
/// Fails on FFT errors.
pub fn process_air(
    x: &[f32],
    sample_rate: u32,
    track: &PyinResult,
    air_db: f64,
) -> Result<Vec<f32>> {
    if air_db.abs() < 1e-9 || x.is_empty() {
        return Ok(x.to_vec());
    }
    let slider = air_db.clamp(-24.0, 12.0);

    // Frame geometry: ~2048 samples at 48 kHz equivalent, 75 % overlap.
    let n = usize::max(
        1024,
        (2048.0 * sample_rate as f64 / 48_000.0).round() as usize,
    )
    .next_power_of_two();
    let hop = n / 4;
    let bin_hz = sample_rate as f64 / n as f64;
    let nyq_bins = n / 2;
    let breath_lo = (1000.0 / bin_hz) as usize;
    let breath_hi = usize::min((8000.0 / bin_hz) as usize, nyq_bins);

    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(n);
    let c2r = planner.plan_fft_inverse(n);

    let window: Vec<f32> = (0..n)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
        .collect();

    let len = x.len();
    let frames = (len - 1) / hop + 1;
    // Carry-based OLA (75 % overlap → span 2n; see `ola`). No voicing
    // crossfade in this stage: the residual path has no bypass blend.
    let mut ola = CarryOla::new(len, 2 * n);
    let mut out: Vec<f32> = Vec::with_capacity(len);

    let mut in_buf = r2c.make_input_vec();
    let mut spectrum = r2c.make_output_vec();
    let mut y_out = c2r.make_output_vec();

    // Comb-mask ring for temporal smoothing (±2 frames).
    let mut mask_ring: VecDeque<Vec<f64>> = VecDeque::with_capacity(2 * MASK_SMOOTH_HALF + 1);
    let mut breath_hist: VecDeque<f64> = VecDeque::with_capacity(2 * MASK_SMOOTH_HALF + 1);

    for t in 0..frames {
        // Frame t writes [t·hop, t·hop + n); the previous frame's writes
        // end at (t−1)·hop + n ≤ t·hop + n − hop, so everything below
        // t·hop is final (75 % overlap keeps the ring bounded).
        ola.flush_to(t * hop, x, None, 1e-6, &mut out);
        let start = t * hop;
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

        // --- pYIN parameters for this frame -----------------------------
        let t_sec = (t * hop + n / 2) as f64 / sample_rate as f64;
        let (f0, v_prob) = track_at(track, t_sec);
        let voiced = f0 > 0.0;

        // --- comb mask for this frame ------------------------------------
        // A bin counts as harmonic only if it sits near k·f0 *and* its
        // magnitude sticks out above the local spectral floor (the median
        // over a ±PEAK_FLOOR_BINS frequency window). The second condition
        // keeps "ghost harmonics" — frequencies that are multiples of F0
        // but carry no voiced energy — fully in the residual path.
        let mut comb = vec![0.0f64; nyq_bins + 1];
        let mut breath_obs = 0.0f64;
        if voiced {
            let f0d = f64::from(f0);
            // Local spectral floor: median magnitude over a ±10-bin window
            // excluding the centre ±2 bins.
            let floor = local_floor(&spectrum, nyq_bins + 1);
            for (k, slot) in comb.iter_mut().enumerate() {
                let f = k as f64 * bin_hz;
                let nearest = (f / f0d).round().max(1.0) * f0d;
                let d = f - nearest;
                let width = (COMB_MAIN_LOBE_BINS * bin_hz).max(COMB_SMEAR * f);
                let geometry = (-(d * d) / (width * width)).exp();
                let prominence = smoothstep(
                    PEAK_FLOOR_LO * floor[k],
                    PEAK_FLOOR_HI * floor[k],
                    f64::from(spectrum[k].norm()),
                );
                *slot = geometry * prominence;
            }
        } else {
            // Unvoiced: spectral flatness in 1–8 kHz feeds the breath
            // detector; the whole frame is residual.
            let (mut ln_sum, mut sum, mut count) = (0.0f64, 0.0f64, 0.0f64);
            for s in spectrum.iter().take(breath_hi + 1).skip(breath_lo) {
                let p = f64::from(s.norm_sqr()) + 1e-20;
                ln_sum += p.ln();
                sum += p;
                count += 1.0;
            }
            if count > 0.0 {
                let flatness = ((ln_sum / count).exp() / (sum / count)).clamp(0.0, 1.0);
                breath_obs = smoothstep(BREATH_FLAT_LO, BREATH_FLAT_HI, flatness)
                    * (1.0 - f64::from(v_prob));
            }
        }
        breath_hist.push_back(breath_obs);
        if breath_hist.len() > 2 * MASK_SMOOTH_HALF + 1 {
            breath_hist.pop_front();
        }
        mask_ring.push_back(comb);
        if mask_ring.len() > 2 * MASK_SMOOTH_HALF + 1 {
            mask_ring.pop_front();
        }
        let ring_len = mask_ring.len();
        let breath_prob = breath_hist.iter().sum::<f64>() / breath_hist.len() as f64;

        // --- per-bin total gain: harmonic path at 1.0, residual scaled ---
        let mut applied = vec![0.0f64; nyq_bins + 1];
        for (k, slot) in applied.iter_mut().enumerate() {
            // Temporally smoothed mask.
            let mut m = 0.0f64;
            for masks in &mask_ring {
                m += masks[k];
            }
            m /= ring_len as f64;
            let f = k as f64 * bin_hz;
            let w = 0.25 + 0.75 * smoothstep(BAND_LOW_HZ, BAND_HIGH_HZ, f);
            let g = if slider > 0.0 {
                let tilt = 6.0 * (f / 6000.0).max(1.0).log2();
                slider * w + (slider / 12.0) * tilt.min(6.0)
            } else {
                let ess = smoothstep(ESS_RISE.0, ESS_RISE.1, f)
                    * (1.0 - smoothstep(ESS_FALL.0, ESS_FALL.1, f));
                slider * w * (1.0 + BREATH_EXPANSION * breath_prob + 0.6 * ess)
            };
            let gain = 10.0f64.powf(g.clamp(GAIN_MIN_DB, GAIN_MAX_DB) / 20.0);
            *slot = m + (1.0 - m) * gain;
        }

        // --- apply, overlap-add ------------------------------------------
        for (spec, m) in spectrum.iter_mut().zip(&applied) {
            *spec *= *m as f32;
        }
        c2r.process(&mut spectrum, &mut y_out)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        let inv_n = 1.0 / n as f32;
        for i in 0..take {
            let w = window[i];
            ola.add(start + i, y_out[i] * inv_n * w, w * w);
        }
    }

    // Normalize the overlap-add.
    ola.flush_all(x, None, 1e-6, &mut out);
    Ok(out)
}

/// Nearest pYIN track frame values (f0, voiced probability) for time `t`.
fn track_at(track: &PyinResult, t: f64) -> (f32, f32) {
    if track.is_empty() {
        return (0.0, 0.0);
    }
    let mut lo = 0usize;
    let mut hi = track.len() - 1;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if track.frame_time(mid) < t {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let i = if lo > 0 && (track.frame_time(lo - 1) - t).abs() < (track.frame_time(lo) - t).abs() {
        lo - 1
    } else {
        lo
    };
    (track.f0[i], track.voiced_prob[i])
}

/// Local spectral floor: per-bin median magnitude over a ±`PEAK_FLOOR_BINS`
/// window excluding the centre ±2 bins (so a line does not raise its own
/// floor). `scratch` must hold `2·PEAK_FLOOR_BINS − 3` values.
fn local_floor(spectrum: &[rustfft::num_complex::Complex<f32>], count: usize) -> Vec<f64> {
    let mut floor = vec![0.0f64; count];
    let mut scratch = [0.0f32; 2 * PEAK_FLOOR_BINS - 3];
    for (k, slot) in floor.iter_mut().enumerate() {
        let lo = k.saturating_sub(PEAK_FLOOR_BINS);
        let hi = usize::min(k + PEAK_FLOOR_BINS, count);
        let mut n = 0usize;
        for (j, s) in spectrum.iter().enumerate().take(hi).skip(lo) {
            if (j as i64 - k as i64).abs() <= 2 {
                continue;
            }
            scratch[n] = s.norm();
            n += 1;
        }
        scratch[..n].sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        *slot = f64::from(scratch[n / 2]);
    }
    floor
}

/// Smoothstep on `[a, b]`.
fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests;
