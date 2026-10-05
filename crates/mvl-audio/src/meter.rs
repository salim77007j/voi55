//! Real-time level metering (Phase 7.2d).
//!
//! The audio callbacks publish per-channel peaks and the callback duty
//! cycle into lock-free atomics; the UI poller drains them on its 40 ms
//! tick. The real-time path never allocates, never locks, and never
//! blocks: a few relaxed-order atomic stores per callback.
//!
//! This is the honest source of the transport L/R meters, the record
//! input LED and the status-bar DSP figure — No-Fake-UI.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// F32 bits in an atomic (levels cross the RT boundary lock-free).
#[derive(Default)]
struct LevelCell(AtomicU32);

impl LevelCell {
    /// Real-time side: keep the peak (max) since the last drain.
    #[inline]
    fn push(&self, v: f32) {
        let bits = v.to_bits();
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            let cur_v = f32::from_bits(cur);
            // NaN never wins; negative zero fine (peak of silence).
            if v <= cur_v {
                break;
            }
            match self
                .0
                .compare_exchange_weak(cur, bits, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// UI side: swap-and-reset (read the held peak, clear the cell).
    #[inline]
    fn drain(&self) -> f32 {
        f32::from_bits(self.0.swap(0.0f32.to_bits(), Ordering::Relaxed))
    }
}

/// Shared level tap. One instance per stream (output or capture).
#[derive(Default)]
pub struct MeterTap {
    l: LevelCell,
    r: LevelCell,
    /// Callback duty cycle, EMA-smoothed, in parts per million
    /// (10_000 ppm = 1 % of real time spent inside the callback; the
    /// figure the status bar divides by 10_000 to show a percentage).
    duty_ppm: AtomicU32,
}

impl MeterTap {
    /// Creates a tap (wrapped in `Arc` for the stream callback).
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Real-time side: publish one interleaved callback buffer. `ch`
    /// channels; channel 0 → L, channel 1 → R (a mono source lights
    /// both; extra channels are not metered — disclosed).
    pub fn push_interleaved(&self, data: &[f32], ch: usize) {
        if ch == 0 || data.is_empty() {
            return;
        }
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for frame in data.chunks_exact(ch) {
            let a = frame[0].abs();
            if a > peak_l {
                peak_l = a;
            }
            if ch > 1 {
                let b = frame[1].abs();
                if b > peak_r {
                    peak_r = b;
                }
            }
        }
        self.l.push(peak_l);
        if ch > 1 {
            self.r.push(peak_r);
        } else {
            // Mono: the single channel is both L and R (honest mirror).
            self.r.push(peak_l);
        }
    }

    /// Real-time side: publish from a non-`f32` interleaved buffer with
    /// a per-sample converter (the capture path — no intermediate
    /// allocation on the RT thread). Reflects the true incoming signal
    /// even when the capture ring overflows and drops samples.
    pub fn push_converted<S: Copy>(&self, data: &[S], ch: usize, conv: impl Fn(S) -> f32) {
        if ch == 0 || data.is_empty() {
            return;
        }
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for frame in data.chunks_exact(ch) {
            let a = conv(frame[0]).abs();
            if a > peak_l {
                peak_l = a;
            }
            if ch > 1 {
                let b = conv(frame[1]).abs();
                if b > peak_r {
                    peak_r = b;
                }
            }
        }
        self.l.push(peak_l);
        if ch > 1 {
            self.r.push(peak_r);
        } else {
            self.r.push(peak_l);
        }
    }

    /// Real-time side: publish the measured callback cost against the
    /// callback's wall-clock budget (`cost_secs / budget_secs`), EMA-
    /// smoothed (α = 1/8) so the figure is stable but current.
    pub fn push_duty(&self, cost_secs: f64, budget_secs: f64) {
        if budget_secs <= 0.0 {
            return;
        }
        let ppm = ((cost_secs / budget_secs) * 1_000_000.0).clamp(0.0, 1_000_000.0) as u32;
        let prev = self.duty_ppm.load(Ordering::Relaxed);
        let ema = (prev - (prev >> 3)) + ppm / 8;
        self.duty_ppm.store(ema, Ordering::Relaxed);
    }

    /// UI side: (level-l, level-r, duty-ppm); drains the peak cells.
    pub fn drain(&self) -> (f32, f32, u32) {
        (
            self.l.drain(),
            self.r.drain(),
            self.duty_ppm.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_is_max_since_last_drain_and_resets() {
        let tap = MeterTap::new();
        tap.push_interleaved(&[0.1, -0.1, 0.5, 0.5, -0.3, -0.3], 2);
        let (l, r, _) = tap.drain();
        assert!((l - 0.5).abs() < 1e-6, "l peak {l}");
        assert!((r - 0.5).abs() < 1e-6, "r peak {r}");
        // Drained cells read silence until new audio arrives.
        let (l2, r2, _) = tap.drain();
        assert_eq!(l2, 0.0);
        assert_eq!(r2, 0.0);
    }

    #[test]
    fn mono_source_lights_both_channels() {
        let tap = MeterTap::new();
        tap.push_interleaved(&[0.25, -0.75], 1);
        let (l, r, _) = tap.drain();
        assert!((l - 0.75).abs() < 1e-6);
        assert!((r - 0.75).abs() < 1e-6);
    }

    #[test]
    fn duty_ema_is_smoothed_and_bounded() {
        let tap = MeterTap::new();
        // 10 % duty (= 100_000 ppm) for 16 callbacks: the EMA approaches
        // ~88_000 ppm (1 - (7/8)^16 of the target).
        for _ in 0..16 {
            tap.push_duty(0.1, 1.0);
        }
        let (_, _, ppm) = tap.drain();
        assert!(ppm > 80_000 && ppm <= 100_000, "ppm {ppm}");
        // Over-budget callbacks clamp at 100 %.
        tap.push_duty(5.0, 1.0);
        let (_, _, hot) = tap.drain();
        assert!(hot <= 1_000_000);
    }

    #[test]
    fn empty_and_degenerate_input_are_safe() {
        let tap = MeterTap::new();
        tap.push_interleaved(&[], 2);
        tap.push_interleaved(&[0.5], 0);
        tap.push_duty(0.1, 0.0);
        let (l, r, _) = tap.drain();
        assert_eq!(l, 0.0);
        assert_eq!(r, 0.0);
    }
}
