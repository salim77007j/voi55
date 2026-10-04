//! TD-PSOLA pitch shifting (D8) — time-domain, duration-preserving.
//!
//! Analysis grains (Hann-windowed, `2·T0`–`4·T0` long) are cut at epochs
//! located on the energy peaks of the input, placed there by walking the
//! pYIN period track and refining each predicted position to the local
//! `x²` maximum. Synthesis re-places the grains on an output grid spaced
//! `T0/r` (r = the pitch ratio) while the *read* pointer advances by the
//! same `T0/r`, which keeps the input↔output time mapping 1:1 — duration
//! and articulation are preserved by construction and only the local
//! grain repetition rate changes. The overlap-add is energy-normalized
//! (dividing by the accumulated window sum), which keeps the amplitude
//! flat for any ratio including grain reuse (r > 1) and epoch skipping
//! (r < 1).
//!
//! Unvoiced / aperiodic material bypasses PSOLA: a voicing envelope
//! derived from the pYIN voicing probability crossfades between the
//! shifted signal and the untouched input over ~10 ms, so breath noises
//! and sibilants pass through bit-exactly (gated at p < 0.05) and note
//! boundaries stay click-free.

use crate::pyin::{FMAX_HZ, FMIN_HZ, PyinResult};

/// Voicing gate for the exact copy-through path (p below this → input).
const BYPASS_GATE: f32 = 0.05;
/// Frames with voicing probability below this do not spawn epochs.
const EPOCH_VOICED_GATE: f32 = 0.45;
/// Overlap-add window-sum floor; below it the input sample passes through.
const WSUM_FLOOR: f32 = 1e-6;

/// Shifts the pitch of `x` by `semitones` using the pitch track from pYIN.
///
/// The output has exactly `x.len()` samples. Formants ride along with the
/// grains (PSOLA copies the original spectra), so timbre is preserved;
/// duration is preserved by construction.
pub fn pitch_shift(x: &[f32], sample_rate: u32, track: &PyinResult, semitones: f64) -> Vec<f32> {
    let ratio = 2.0f64.powf(semitones / 12.0);
    if (semitones.abs() < 1e-9) || x.is_empty() || track.is_empty() {
        return x.to_vec();
    }

    let sr = sample_rate as f64;
    let min_period = sr / FMAX_HZ;
    let max_period = sr / FMIN_HZ;

    // --- 1. epochs: walk the voiced frames, refine to energy peaks ------
    let mut epochs: Vec<usize> = Vec::with_capacity(track.len());
    for (i, &voiced) in track.voiced.iter().enumerate() {
        if !voiced || track.voiced_prob[i] < EPOCH_VOICED_GATE || track.f0[i] <= 0.0 {
            continue;
        }
        let period = (sr / f64::from(track.f0[i])).clamp(min_period, max_period);
        let predicted = match epochs.last() {
            None => track.frame_time(i) * sr,
            Some(&last) => last as f64 + period,
        };
        let refined = refine_epoch(x, predicted, period);
        // Reject duplicates (min spacing half a period keeps epochs ordered).
        if let Some(&last) = epochs.last()
            && ((refined as f64) - (last as f64)) < 0.5 * period
        {
            continue;
        }
        epochs.push(refined);
    }
    if epochs.len() < 2 {
        return x.to_vec();
    }

    // Local period per epoch from neighbour spacing (pitch-synchronous).
    let mut periods = vec![0.0f64; epochs.len()];
    for i in 0..epochs.len() {
        let p = if i + 1 < epochs.len() {
            (epochs[i + 1] - epochs[i]) as f64
        } else if i > 0 {
            (epochs[i] - epochs[i - 1]) as f64
        } else {
            return x.to_vec();
        };
        periods[i] = p.clamp(min_period, max_period);
    }

    // --- 2. voicing envelope at sample rate ------------------------------
    let env = voicing_envelope(x.len(), sample_rate, track);

    // --- 3. grain overlap-add on the resampled epoch grid ----------------
    let len = x.len();
    let mut acc = vec![0.0f32; len];
    let mut wsum = vec![0.0f32; len];

    let mut epoch_idx = 0usize;
    let mut in_pos = epochs[0] as f64;
    let mut out_pos = epochs[0] as f64;
    while out_pos < len as f64 && epoch_idx < epochs.len() {
        // Advance the read pointer to the nearest epoch (monotonic).
        while epoch_idx + 1 < epochs.len()
            && (epochs[epoch_idx + 1] as f64 - in_pos).abs()
                <= (epochs[epoch_idx] as f64 - in_pos).abs()
        {
            epoch_idx += 1;
        }
        let m = epochs[epoch_idx];
        let t0 = periods[epoch_idx];
        // Window: ≥ 2·T0, stretched for downshifts so the overlap never
        // falls below ~50 % (two *synthesis* periods keep the window sum
        // flat and average the grain-reuse pattern).
        let half = ((2.0 * t0).max(t0 / ratio + 4.0)).ceil() as usize;
        let window = 2 * half;

        // Write the grain, clipping to the buffer.
        let start_out = out_pos as i64 - half as i64;
        let start_src = m as i64 - half as i64;
        for j in 0..window {
            let o = start_out + j as i64;
            let s = start_src + j as i64;
            if o < 0 || o >= len as i64 {
                continue;
            }
            // Hann over the (possibly clipped) grain; source reads outside
            // the signal contribute silence but still accumulate window
            // weight, keeping the normalization consistent.
            let w = hann_at(j, window);
            let sample = if s >= 0 && s < len as i64 {
                x[s as usize]
            } else {
                0.0
            };
            acc[o as usize] += sample * w;
            wsum[o as usize] += w;
        }

        let step = t0 / ratio;
        out_pos += step;
        in_pos += step;
        if epoch_idx + 1 < epochs.len() && in_pos > (epochs[epoch_idx + 1] as f64 + t0) {
            // Read pointer fell behind (deep downshift): jump to the next
            // epoch so the grain content keeps tracking the input.
            epoch_idx += 1;
        }
    }

    // --- 4. normalize and blend with the voicing envelope ----------------
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let v = env[i];
        let shifted = if wsum[i] > WSUM_FLOOR {
            acc[i] / wsum[i]
        } else {
            x[i]
        };
        out.push(v * shifted + (1.0 - v) * x[i]);
    }
    out
}

/// Refines a predicted epoch position to the `x²` maximum within
/// ±quarter period (glottal-closure alignment).
fn refine_epoch(x: &[f32], predicted: f64, period: f64) -> usize {
    let center = predicted.round() as i64;
    let reach = (0.25 * period).round() as i64;
    let lo = (center - reach).max(0);
    let hi = (center + reach).min(x.len() as i64 - 1);
    let mut best = center.clamp(0, x.len() as i64 - 1) as usize;
    let mut best_e = -1.0f64;
    for i in lo..=hi {
        let e = f64::from(x[i as usize]) * f64::from(x[i as usize]);
        if e > best_e {
            best_e = e;
            best = i as usize;
        }
    }
    best
}

/// Sample-rate voicing envelope: smoothed p_v per frame, linearly
/// interpolated; frames below [`BYPASS_GATE`] snap to exact zero so
/// clearly unvoiced material bypasses bit-exactly.
pub(crate) fn voicing_envelope(len: usize, sample_rate: u32, track: &PyinResult) -> Vec<f32> {
    if track.is_empty() {
        return vec![0.0; len];
    }

    // 3-frame moving average (~30 ms at the 10 ms hop).
    let mut sm = vec![0.0f32; track.len()];
    for (i, sm_i) in sm.iter_mut().enumerate() {
        let lo = i.saturating_sub(1);
        let hi = usize::min(i + 1, track.len() - 1);
        let n = (hi - lo + 1) as f32;
        *sm_i = track.voiced_prob[lo..=hi].iter().sum::<f32>() / n;
    }

    let mut env = Vec::with_capacity(len);
    let mut frame = 0usize;
    for i in 0..len {
        let t = i as f64 / sample_rate as f64;
        // Walk to the frame whose window-center time is nearest.
        while frame + 1 < track.len()
            && (track.frame_time(frame + 1) - t).abs() < (t - track.frame_time(frame)).abs()
        {
            frame += 1;
        }
        let p = sm[frame];
        env.push(if p < BYPASS_GATE { 0.0 } else { p });
    }
    env
}

/// Periodic Hann value for position `j` in a window of `n` samples.
fn hann_at(j: usize, n: usize) -> f32 {
    if n <= 1 {
        return 1.0;
    }
    (0.5 - 0.5 * (std::f64::consts::TAU * j as f64 / n as f64).cos()) as f32
}

#[cfg(test)]
mod tests;
