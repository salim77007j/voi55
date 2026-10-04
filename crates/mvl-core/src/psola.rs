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
//!
//! Phase 6 streaming: the grain loop lives in [`PsolaStage`], a
//! chunk-driven driver; [`pitch_shift`] is a thin wrapper that runs it to
//! completion. Grains are emitted chronologically and each stage flush is
//! cadence-free w.r.t. values, so driving in chunks is bit-identical to
//! the single-pass render (gate-tested in `crate::stream`).

use crate::ola::{CarryOla, EnvStream};
use crate::pyin::{FMAX_HZ, FMIN_HZ, PyinResult};

/// Frames with voicing probability below this do not spawn epochs.
pub(crate) const EPOCH_VOICED_GATE: f32 = 0.45;
/// Overlap-add window-sum floor; below it the input sample passes through.
pub(crate) const WSUM_FLOOR: f32 = 1e-6;

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

    let mut stage = PsolaStage::new(x, sample_rate, track, ratio, 0);
    if stage.is_passthrough() {
        return x.to_vec();
    }
    stage.advance_to(x, usize::MAX);
    stage.finish(x);
    stage.into_output()
}

/// Chunk-driven PSOLA stage (Phase 6). Owns the epoch schedule and the
/// carry overlap-add; [`PsolaStage::advance_to`] processes grains until
/// the flushed output frontier reaches `target`, so a caller can pull the
/// render in fixed-size chunks. With `target = usize::MAX` the driver
/// performs exactly the same flush/add sequence as the historical
/// single-pass loop — bit-identical output by construction.
///
/// `start > 0` (chain restart at the playhead) drops every grain whose
/// synthesis position is below `start`; output positions
/// `start..first_epoch + half` then miss their left-hand overlap
/// contributions — the disclosed restart artifact region (measured by the
/// `stream` gate tests).
pub(crate) struct PsolaStage<'a> {
    len: usize,
    /// Chain restart offset (writes are clipped to `>= start`).
    start: usize,
    ratio: f64,
    max_half: f64,
    epochs: Vec<usize>,
    periods: Vec<f64>,
    ola: CarryOla,
    env: EnvStream<'a>,
    epoch_idx: usize,
    in_pos: f64,
    out_pos: f64,
    out: Vec<f32>,
    /// Absolute position of `out[0]` (front-trimmed for streaming).
    out_base: usize,
    passthrough: bool,
    done: bool,
}

impl<'a> PsolaStage<'a> {
    /// Builds the epoch schedule from the full input `x` (epoch refinement
    /// is a whole-signal walk, so this stage is only ever the chain's
    /// *first* stage, fed by the complete session buffer).
    ///
    /// `start` positions the output frontier for chain restarts.
    pub(crate) fn new(
        x: &[f32],
        sample_rate: u32,
        track: &'a PyinResult,
        ratio: f64,
        start: usize,
    ) -> Self {
        let sr = f64::from(sample_rate);
        let len = x.len();
        let min_period = sr / FMAX_HZ;
        let max_period = sr / FMIN_HZ;

        // --- epochs: walk the voiced frames, refine to energy peaks ------
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

        // Local period per epoch from neighbour spacing (pitch-synchronous).
        // Computed on the *full* epoch list so kept epochs carry exactly the
        // periods the single-pass render would give them.
        let mut passthrough = false;
        let mut periods = vec![0.0f64; epochs.len()];
        for i in 0..epochs.len() {
            let p = if i + 1 < epochs.len() {
                (epochs[i + 1] - epochs[i]) as f64
            } else if i > 0 {
                (epochs[i] - epochs[i - 1]) as f64
            } else {
                passthrough = true; // single epoch: no pitch-synchronous grid
                break;
            };
            periods[i] = p.clamp(min_period, max_period);
        }
        passthrough |= epochs.len() < 2;

        // Restart: drop epochs below `start`; the first kept epoch seeds the
        // synthesis grid (its left window may reach below `start` — those
        // contributions are the disclosed restart artifact).
        if start > 0 && !passthrough {
            let begin = epochs.partition_point(|&e| e < start);
            epochs.drain(..begin);
            periods.drain(..begin);
            if epochs.len() < 2 {
                passthrough = true;
            }
        }

        let max_half = max_period.max(max_period / ratio) + 4.0;
        let ola = CarryOla::with_start(len, max_half as usize, start);
        let mut env = EnvStream::new(track, sample_rate);
        env.reset_to(start);
        let seed = if passthrough { 0.0 } else { epochs[0] as f64 };
        Self {
            len,
            start,
            ratio,
            max_half,
            epochs,
            periods,
            ola,
            env,
            epoch_idx: 0,
            in_pos: seed,
            out_pos: seed,
            out: Vec::with_capacity(len.saturating_sub(start)),
            out_base: start,
            passthrough,
            done: false,
        }
    }

    pub(crate) fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    /// Worst-case grain half-width (the stage's input-lookahead bound).
    pub(crate) fn max_half(&self) -> f64 {
        self.max_half
    }

    /// Absolute output frontier (samples `start..flushed()` are final).
    pub(crate) fn flushed(&self) -> usize {
        self.ola.head()
    }

    /// Borrowed output window (absolute positions `out_base..`).
    pub(crate) fn out_slice(&self) -> &[f32] {
        &self.out
    }

    pub(crate) fn out_base(&self) -> usize {
        self.out_base
    }

    /// Drops the output prefix below `keep_from` (streaming trim: the
    /// consumer's frontier). Positions below `keep_from` are never read
    /// again by the consumer.
    pub(crate) fn trim_to(&mut self, keep_from: usize) {
        let keep_from = keep_from.clamp(self.out_base, self.out.len() + self.out_base);
        let k = keep_from - self.out_base;
        if k > 0 {
            self.out.drain(..k);
            self.out_base = keep_from;
        }
    }

    /// Processes grains until the flushed frontier reaches `target` (or the
    /// grain schedule is exhausted). `x` is the full input signal
    /// (`x.len() == len`); `target = usize::MAX` runs to exhaustion.
    pub(crate) fn advance_to(&mut self, x: &[f32], target: usize) {
        if self.passthrough || self.done {
            return;
        }
        while self.out_pos < self.len as f64 && self.epoch_idx < self.epochs.len() {
            if self.flushed() >= target {
                break;
            }
            // Flush everything no future grain can reach (a grain's left
            // edge is `out_pos − half ≥ out_pos − max_half`).
            let flush_mark = (self.out_pos - self.max_half).max(0.0) as usize;
            self.ola.flush_to(
                flush_mark,
                x,
                0,
                Some(&mut self.env),
                WSUM_FLOOR,
                &mut self.out,
            );

            // Advance the read pointer to the nearest epoch (monotonic).
            while self.epoch_idx + 1 < self.epochs.len()
                && (self.epochs[self.epoch_idx + 1] as f64 - self.in_pos).abs()
                    <= (self.epochs[self.epoch_idx] as f64 - self.in_pos).abs()
            {
                self.epoch_idx += 1;
            }
            let m = self.epochs[self.epoch_idx];
            let t0 = self.periods[self.epoch_idx];
            // Window: ≥ 2·T0, stretched for downshifts so the overlap never
            // falls below ~50 % (two *synthesis* periods keep the window sum
            // flat and average the grain-reuse pattern).
            let half = ((2.0 * t0).max(t0 / self.ratio + 4.0)).ceil() as usize;
            let window = 2 * half;

            // Write the grain, clipping to the buffer (and to the restart
            // offset — positions below `start` are never emitted).
            let start_out = self.out_pos as i64 - half as i64;
            let start_src = m as i64 - half as i64;
            for j in 0..window {
                let o = start_out + j as i64;
                let s = start_src + j as i64;
                if o < self.start as i64 || o >= self.len as i64 {
                    continue;
                }
                // Hann over the (possibly clipped) grain; source reads outside
                // the signal contribute silence but still accumulate window
                // weight, keeping the normalization consistent.
                let w = hann_at(j, window);
                let sample = if s >= 0 && s < self.len as i64 {
                    x[s as usize]
                } else {
                    0.0
                };
                self.ola.add(o as usize, sample * w, w);
            }

            let step = t0 / self.ratio;
            self.out_pos += step;
            self.in_pos += step;
            if self.epoch_idx + 1 < self.epochs.len()
                && self.in_pos > (self.epochs[self.epoch_idx + 1] as f64 + t0)
            {
                // Read pointer fell behind (deep downshift): jump to the next
                // epoch so the grain content keeps tracking the input.
                self.epoch_idx += 1;
            }
        }
    }

    /// Flushes the tail (normalize + voicing crossfade per sample).
    pub(crate) fn finish(&mut self, x: &[f32]) {
        if self.passthrough || self.done {
            self.done = true;
            return;
        }
        self.done = true;
        self.ola
            .flush_all(x, 0, Some(&mut self.env), WSUM_FLOOR, &mut self.out);
    }

    /// Consumes the driver after a full run (`advance_to(usize::MAX)` +
    /// `finish`): yields the complete output vector.
    pub(crate) fn into_output(self) -> Vec<f32> {
        debug_assert_eq!(self.flushed(), self.len, "full run must flush everything");
        self.out
    }
}

/// Refines a predicted epoch position to the `x²` maximum within
/// ±quarter period (glottal-closure alignment).
pub(crate) fn refine_epoch(x: &[f32], predicted: f64, period: f64) -> usize {
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
pub(crate) fn hann_at(j: usize, n: usize) -> f32 {
    if n <= 1 {
        return 1.0;
    }
    (0.5 - 0.5 * (std::f64::consts::TAU * j as f64 / n as f64).cos()) as f32
}

#[cfg(test)]
mod tests;
