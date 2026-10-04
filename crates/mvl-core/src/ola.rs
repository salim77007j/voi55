//! Carry-based streaming overlap-add and the streaming voicing envelope.
//!
//! Phase 5 RAM fix: the three render stages (PSOLA, formant, air) used to
//! materialize two full-length scratch buffers (`acc` + `wsum`) and — for
//! pitch/formant — a full-length voicing envelope, on top of their output
//! vector. On a 3-min 48 kHz session that is ~170 MB of simultaneously
//! live buffers and blew the 200 MB budget. Every stage overlaps in a
//! *bounded* window (grain half-width, or the STFT frame), so the running
//! sums live in a fixed-size ring and finished samples are flushed to the
//! output as soon as no future grain/frame can touch them. The additions
//! per output sample happen in the same order as before, so results are
//! bit-identical (verified by the stage gate tests).
//!
//! Memory per stage after the change: one output vector + one small ring
//! (tens of KB) instead of four full-length buffers.
//!
//! Phase 6 streaming: `flush_to`/`flush_all` take an `x_base` offset so a
//! stage can be driven from a *windowed* input buffer (the previous stage's
//! trimmed output) while all bookkeeping stays in absolute sample
//! positions. `CarryOla::with_start` opens the head at a non-zero offset
//! (chain restart at the playhead). Flush cadence never changes values —
//! only when samples are emitted — so chunked driving is bit-identical to
//! the full-buffer render (gate-tested in `stream`).

use crate::pyin::PyinResult;

/// Voicing gate for the exact copy-through path (p below this → input).
pub(crate) const BYPASS_GATE: f32 = 0.05;

/// Ring overlap-add. `span` is the maximum number of output samples that
/// can be "alive" (written but not flushed) at any time; the ring is sized
/// to the next power of two above `2·span` so a slot is never reused while
/// its absolute position can still be written.
pub(crate) struct CarryOla {
    ring_acc: Vec<f32>,
    ring_wsum: Vec<f32>,
    mask: usize,
    /// Absolute index of the next sample to flush.
    head: usize,
    /// Total output length (samples beyond `len` are never written).
    len: usize,
}

impl CarryOla {
    /// Opens the ring with the flush head at sample `start`
    /// (streaming restart: positions below `start` are never emitted and
    /// callers clip their writes to `>= start`). Whole-signal stages pass 0.
    pub(crate) fn with_start(len: usize, span: usize, start: usize) -> Self {
        let ring = (2 * span + 16).next_power_of_two();
        Self {
            ring_acc: vec![0.0; ring],
            ring_wsum: vec![0.0; ring],
            mask: ring - 1,
            head: start,
            len,
        }
    }

    /// Absolute index of the next sample to flush (the stage's output
    /// frontier).
    pub(crate) fn head(&self) -> usize {
        self.head
    }

    /// Adds one weighted contribution at absolute position `pos`
    /// (`head <= pos < len` — callers clip their grain/frame bounds).
    #[inline]
    pub(crate) fn add(&mut self, pos: usize, value: f32, weight: f32) {
        debug_assert!(pos >= self.head && pos < self.len);
        let slot = pos & self.mask;
        self.ring_acc[slot] += value;
        self.ring_wsum[slot] += weight;
    }

    /// Flushes samples `head..watermark` into `out`, applying the stage's
    /// normalization (and voicing crossfade when `env` is `Some`).
    ///
    /// `x` is the stage's input window covering absolute positions
    /// `x_base..x_base + x.len()` (a full signal passes `x_base = 0`).
    ///
    /// `shifted = acc/wsum` where the window sum is above `floor`, else the
    /// input sample passes through; with an envelope, the result is
    /// `v·shifted + (1−v)·x[i]` — exactly the previous full-buffer loops.
    pub(crate) fn flush_to(
        &mut self,
        watermark: usize,
        x: &[f32],
        x_base: usize,
        mut env: Option<&mut EnvStream>,
        floor: f32,
        out: &mut Vec<f32>,
    ) {
        let watermark = usize::min(watermark, self.len);
        while self.head < watermark {
            let i = self.head;
            let slot = i & self.mask;
            let (acc, wsum) = (self.ring_acc[slot], self.ring_wsum[slot]);
            let src = x[i - x_base];
            if let Some(stream) = env.as_deref_mut() {
                let v = stream.value_at(i);
                let shifted = if wsum > floor { acc / wsum } else { src };
                out.push(v * shifted + (1.0 - v) * src);
            } else {
                out.push(if wsum > floor { acc / wsum } else { src });
            }
            // Reset so the slot is clean if the ring wraps onto it later.
            self.ring_acc[slot] = 0.0;
            self.ring_wsum[slot] = 0.0;
            self.head += 1;
        }
    }

    /// Flushes everything up to `len` (end of signal).
    pub(crate) fn flush_all(
        &mut self,
        x: &[f32],
        x_base: usize,
        env: Option<&mut EnvStream>,
        floor: f32,
        out: &mut Vec<f32>,
    ) {
        let watermark = self.len;
        self.flush_to(watermark, x, x_base, env, floor, out);
    }
}

/// Streaming equivalent of the old full-length `voicing_envelope` buffer:
/// 3-frame moving average of the pYIN voicing probability, linearly
/// nearest frame-center lookup, values below [`BYPASS_GATE`] snapped to
/// exact zero. `value_at` must be called with non-decreasing `i` (all
/// three stages flush strictly forward).
pub(crate) struct EnvStream<'a> {
    track: &'a PyinResult,
    sample_rate: f64,
    /// Nearest analysis frame for the last queried time.
    frame: usize,
    last_i: usize,
}

impl<'a> EnvStream<'a> {
    pub(crate) fn new(track: &'a PyinResult, sample_rate: u32) -> Self {
        Self {
            track,
            sample_rate: f64::from(sample_rate),
            frame: 0,
            last_i: 0,
        }
    }

    /// Restarts the stream at absolute sample `start` (chain restart at the
    /// playhead). `value_at` then only accepts `i >= start`.
    pub(crate) fn reset_to(&mut self, start: usize) {
        self.last_i = start;
    }

    pub(crate) fn value_at(&mut self, i: usize) -> f32 {
        debug_assert!(i >= self.last_i, "EnvStream requires monotonic samples");
        self.last_i = i;

        let t = i as f64 / self.sample_rate;
        let track_len = self.track.len();
        if track_len == 0 {
            return 0.0;
        }
        while self.frame + 1 < track_len
            && (self.track.frame_time(self.frame + 1) - t).abs()
                < (t - self.track.frame_time(self.frame)).abs()
        {
            self.frame += 1;
        }
        // 3-frame moving average (~30 ms at the 10 ms hop), same f32 order
        // as the old buffer version.
        let f = self.frame;
        let lo = f.saturating_sub(1);
        let hi = usize::min(f + 1, track_len - 1);
        let n = (hi - lo + 1) as f32;
        let p = self.track.voiced_prob[lo..=hi].iter().sum::<f32>() / n;
        if p < BYPASS_GATE { 0.0 } else { p }
    }
}
