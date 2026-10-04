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

use crate::ola::{CarryOla, EnvStream};
use crate::pyin::{FMAX_HZ, FMIN_HZ, PyinResult};

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

    // --- 3. grain overlap-add on the resampled epoch grid ----------------
    // Carry-based: grains are emitted chronologically and span at most
    // `half ≤ max_half` on either side of their synthesis position, so a
    // fixed ring replaces the two full-length accumulation buffers, and
    // finished samples flush straight into the output (bit-identical adds
    // in the same order — see `ola`).
    let max_half = max_period.max(max_period / ratio) + 4.0;
    let mut ola = CarryOla::new(x.len(), max_half as usize);
    let mut env = EnvStream::new(track, sample_rate);
    let mut out: Vec<f32> = Vec::with_capacity(x.len());

    let mut epoch_idx = 0usize;
    let mut in_pos = epochs[0] as f64;
    let mut out_pos = epochs[0] as f64;
    while out_pos < x.len() as f64 && epoch_idx < epochs.len() {
        // Flush everything no future grain can reach (a grain's left edge
        // is `out_pos − half ≥ out_pos − max_half`).
        let flush_mark = (out_pos - max_half).max(0.0) as usize;
        ola.flush_to(flush_mark, x, Some(&mut env), WSUM_FLOOR, &mut out);

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
            if o < 0 || o >= x.len() as i64 {
                continue;
            }
            // Hann over the (possibly clipped) grain; source reads outside
            // the signal contribute silence but still accumulate window
            // weight, keeping the normalization consistent.
            let w = hann_at(j, window);
            let sample = if s >= 0 && s < x.len() as i64 {
                x[s as usize]
            } else {
                0.0
            };
            ola.add(o as usize, sample * w, w);
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

    // --- 4. flush the tail (normalize + voicing crossfade happen per
    // sample inside the carry flush) -------------------------------------
    ola.flush_all(x, Some(&mut env), WSUM_FLOOR, &mut out);
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

/// Periodic Hann value for position `j` in a window of `n` samples.
fn hann_at(j: usize, n: usize) -> f32 {
    if n <= 1 {
        return 1.0;
    }
    (0.5 - 0.5 * (std::f64::consts::TAU * j as f64 / n as f64).cos()) as f32
}

#[cfg(test)]
mod tests;
