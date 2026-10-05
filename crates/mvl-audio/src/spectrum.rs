//! Real-time spectrum tap (Phase 7.2e).
//!
//! The output callback downmixes each buffer to mono and pushes the
//! samples into a fixed lock-free ring (atomic f32-bit cells + a
//! monotonic write cursor). The UI side drains the newest 2048-sample
//! window, applies a Hann window + FFT, and renders log-frequency
//! bars with peak hold — the same signal the DAC receives, never a
//! simulation (No-Fake-UI). Overwrites are expected and safe: the tap
//! is advisory analysis data, not transport state.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

const RING: usize = 4096;

/// One atomic f32 cell per ring slot.
struct Cells(Box<[AtomicU32; RING]>);

impl Default for Cells {
    fn default() -> Self {
        let arr: [AtomicU32; RING] = core::array::from_fn(|_| AtomicU32::new(0));
        Self(Box::new(arr))
    }
}

/// Shared spectrum tap.
pub struct SpectrumTap {
    cells: Cells,
    /// Monotonic count of samples ever pushed (wraps not modeled: at
    /// 48 kHz it takes ~3 million years of audio to overflow u64).
    written: AtomicUsize,
}

impl Default for SpectrumTap {
    fn default() -> Self {
        Self {
            cells: Cells::default(),
            written: AtomicUsize::new(0),
        }
    }
}

impl SpectrumTap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Real-time side: push an interleaved buffer, downmixed to mono.
    /// Lock-free, allocation-free.
    pub fn push_interleaved(&self, data: &[f32], ch: usize) {
        if ch == 0 {
            return;
        }
        let frames = data.len() / ch;
        let mut w = self.written.load(Ordering::Relaxed);
        for f in 0..frames {
            let frame = &data[f * ch..][..ch];
            let mut mono = 0.0f32;
            for s in frame {
                mono += s;
            }
            mono /= ch as f32;
            let bits = mono.to_bits();
            self.cells.0[w % RING].store(bits, Ordering::Relaxed);
            w = w.wrapping_add(1);
        }
        self.written.store(w, Ordering::Relaxed);
    }

    /// UI side: copies the newest `out.len()` samples into `out`
    /// (chronological order). Fills zeros when fewer samples have ever
    /// been pushed. Returns true when any real audio was present.
    pub fn drain_window(&self, out: &mut [f32]) -> bool {
        let total = self.written.load(Ordering::Relaxed);
        let n = out.len();
        let have = n.min(total);
        let start = total.saturating_sub(n);
        for (i, slot) in out.iter_mut().enumerate() {
            let idx = start + i;
            let v = if idx < total {
                f32::from_bits(self.cells.0[idx % RING].load(Ordering::Relaxed))
            } else {
                0.0
            };
            *slot = v;
        }
        have > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_drains_in_order_and_wraps() {
        let tap = SpectrumTap::new();
        // Push 8192 samples of a ramp (wraps the 4096 ring twice).
        let mono: Vec<f32> = (0..8192).map(|i| (i % 1000) as f32 * 0.001).collect();
        tap.push_interleaved(&mono, 1);
        // The newest 16 samples must be the ramp's tail, in order.
        let mut win = vec![0.0f32; 16];
        assert!(tap.drain_window(&mut win));
        for (i, v) in win.iter().enumerate() {
            let expect = (8192 - 16 + i) % 1000;
            assert!((v - expect as f32 * 0.001).abs() < 1e-6, "i={i} v={v}");
        }
    }

    #[test]
    fn downmix_is_the_channel_mean() {
        let tap = SpectrumTap::new();
        tap.push_interleaved(&[0.2, 0.4], 2);
        let mut win = vec![0.0f32; 1];
        tap.drain_window(&mut win);
        assert!((win[0] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn short_history_pads_with_zeros_honestly() {
        let tap = SpectrumTap::new();
        let mut win = vec![9.0f32; 8];
        assert!(!tap.drain_window(&mut win), "no audio pushed yet");
        assert!(win.iter().all(|&v| v == 0.0));
    }
}
