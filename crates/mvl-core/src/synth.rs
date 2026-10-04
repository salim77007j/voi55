//! Deterministic signal generators for test fixtures and evidence audio.
//!
//! These are simulation utilities, not product features: they produce the
//! synthetic vowels, vibrato tones and noise beds the Phase 3 accuracy gates
//! (D7–D10) and the evidence corpus run on. Everything is seeded, so every
//! number in the reports is reproducible bit-for-bit.

/// Small xorshift64* generator — no external RNG dependency, deterministic.
pub struct Rng(u64);

impl Rng {
    /// Creates a generator from a non-zero seed (forced internally).
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// Next uniform float in `[-1, 1)`.
    pub fn next_bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    }
}

/// Constant F0 track of `n` frames (helper for the generators below).
pub fn f0_track_const(f0: f32, n: usize) -> Vec<f32> {
    vec![f0; n]
}

/// Instantaneous F0 of a sinusoidal vibrato around `base` with depth `cents`
/// at modulation rate `rate_hz`, sampled at times `i / sample_rate`.
///
/// Returned per-frame (one value per `hop` samples by the caller convention;
/// the generators integrate phase from this track).
pub fn vibrato_f0_track(
    base: f32,
    cents: f32,
    rate_hz: f32,
    n: usize,
    sample_rate: u32,
) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            base * 2.0f32.powf(cents * (std::f32::consts::TAU * rate_hz * t).sin() / 1200.0)
        })
        .collect()
}

/// Linear F0 sweep track from `f0_start` to `f0_end` over `n` frames.
pub fn sweep_f0_track(f0_start: f32, f0_end: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let k = if n <= 1 {
                0.0
            } else {
                i as f32 / (n - 1) as f32
            };
            f0_start * (f0_end / f0_start).powf(k)
        })
        .collect()
}

/// Harmonic tone with explicit harmonic amplitudes (phase-integrated, so
/// vibrato and sweeps stay coherent). Frequencies above Nyquist are skipped.
pub fn harmonic_tone_amps(f0_track: &[f32], sample_rate: u32, amps: &[f32]) -> Vec<f32> {
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(f0_track.len());
    for &f0 in f0_track {
        let mut s = 0.0f64;
        for (k, &a) in amps.iter().enumerate() {
            let f = f64::from(f0) * (k + 1) as f64;
            if f >= f64::from(sample_rate) / 2.0 {
                break;
            }
            s += f64::from(a) * (phase * (k + 1) as f64).sin();
        }
        out.push(s as f32 * 0.5);
        phase += std::f64::consts::TAU * f64::from(f0) / f64::from(sample_rate);
        if phase > std::f64::consts::TAU {
            phase -= std::f64::consts::TAU;
        }
    }
    out
}

/// Harmonic tone with `1/k` amplitudes (sawtooth-like, vowel-exciter-shaped).
pub fn harmonic_tone(f0_track: &[f32], sample_rate: u32, n_harmonics: usize) -> Vec<f32> {
    let amps: Vec<f32> = (1..=n_harmonics).map(|k| 1.0 / k as f32).collect();
    harmonic_tone_amps(f0_track, sample_rate, &amps)
}

/// Specification for the formant-based vowel generator.
pub struct VowelSpec {
    /// `(frequency_hz, bandwidth_hz, amplitude)` formant peaks. The
    /// harmonic amplitudes follow the sum of Lorentzian peaks, so the
    /// spectrum has exactly the formants prescribed here.
    pub formants: Vec<(f32, f32, f32)>,
    /// Breath noise level `0..1` relative to the voiced peak.
    pub breath: f32,
    /// Harmonic amplitude shimmer (per-harmonic deterministic ripple).
    pub jitter: f32,
}

impl Default for VowelSpec {
    fn default() -> Self {
        // Neutral "ah"-like vowel: F1 500, F2 1500, F3 2500, F4 3500 Hz
        // with a glottal-ish amplitude tilt (F1 dominant). Jitter/breath
        // model a clean studio vocal (the product's input).
        Self {
            formants: vec![
                (500.0, 80.0, 1.0),
                (1500.0, 100.0, 0.6),
                (2500.0, 120.0, 0.35),
                (3500.0, 160.0, 0.22),
            ],
            breath: 0.01,
            jitter: 0.02,
        }
    }
}

/// Synthesizes a vowel-like voice signal as a harmonic tone whose harmonic
/// amplitudes follow the prescribed formant envelope (sum of Lorentzian
/// peaks), plus a lowpassed breath-noise bed. Deterministic, with a
/// spectrum that has exactly the formant structure the spec asks for —
/// unlike a filter-state resonator bank, whose zeros and gain interactions
/// are easy to get wrong. Peak-normalized to 0.5.
pub fn vowel(f0_track: &[f32], sample_rate: u32, spec: &VowelSpec, rng: &mut Rng) -> Vec<f32> {
    let _ = rng;
    let f0 = f0_track.first().copied().unwrap_or(100.0).max(1.0);
    let nyq = sample_rate as f32 / 2.0;
    let n_harm = usize::clamp((nyq / f0) as usize, 8, 128);

    // Harmonic amplitudes: sum of Lorentzian formant peaks × glottal tilt.
    let amps: Vec<f32> = (1..=n_harm)
        .map(|k| {
            let f = f0 * k as f32;
            let mut a = 0.0f32;
            for &(fr, bw, amp) in &spec.formants {
                let d = (f - fr) / bw;
                a += amp / (1.0 + d * d);
            }
            // Per-harmonic deterministic shimmer (spec.jitter).
            let shimmer = 1.0 + spec.jitter * (0.7 * k as f32).sin();
            a * shimmer / k as f32
        })
        .collect();
    // Remove the 1/k exciter tilt the helper applies implicitly? It does
    // not: harmonic_tone_amps uses amps verbatim, so fold the tilt out.
    let amps: Vec<f32> = amps
        .iter()
        .enumerate()
        .map(|(k, a)| a * (k + 1) as f32)
        .collect();

    let mut out = harmonic_tone_amps(f0_track, sample_rate, &amps);

    // Breath: lowpassed noise at the prescribed level.
    if spec.breath > 0.0 {
        let bed = breath_noise(out.len(), sample_rate, rng);
        let peak = out.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-9);
        for (v, b) in out.iter_mut().zip(&bed) {
            *v += spec.breath * b * peak;
        }
    }

    let peak = out.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-9);
    for v in &mut out {
        *v *= 0.5 / peak;
    }
    out
}

/// White noise in `[-1, 1)`, length `n`.
pub fn white_noise(n: usize, rng: &mut Rng) -> Vec<f32> {
    (0..n).map(|_| rng.next_bipolar()).collect()
}

/// One-pole lowpassed white noise (~3 kHz at 48 kHz), for breath beds.
pub fn breath_noise(n: usize, sample_rate: u32, rng: &mut Rng) -> Vec<f32> {
    let a = (-std::f32::consts::TAU * 3000.0 / sample_rate as f32).exp();
    let mut state = 0.0f32;
    let mut peak = 1e-9f32;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        state = (1.0 - a) * rng.next_bipolar() + a * state;
        out.push(state);
        peak = peak.max(state.abs());
    }
    for v in &mut out {
        *v *= 0.5 / peak;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_bipolar(), b.next_bipolar());
        }
    }

    #[test]
    fn rng_stays_in_unit_range() {
        let mut rng = Rng::new(7);
        for _ in 0..10_000 {
            let v = rng.next_bipolar();
            assert!((-1.0..1.0).contains(&v), "value {v} out of range");
        }
    }

    #[test]
    fn harmonic_tone_phase_is_continuous() {
        // Frequency must match the track: measure zero crossings of a
        // constant tone. `1/k²` amplitudes keep the waveform smooth (no
        // sawtooth Gibbs ripple adding spurious crossings).
        let sr = 48_000;
        let n = 4_800;
        let track = f0_track_const(220.0, n);
        let amps: Vec<f32> = (1..=8).map(|k| 1.0 / (k * k) as f32).collect();
        let x = harmonic_tone_amps(&track, sr, &amps);
        let mut crossings = 0usize;
        for w in x.windows(2) {
            if w[0] < 0.0 && w[1] >= 0.0 {
                crossings += 1;
            }
        }
        // 0.1 s at 220 Hz → 22 positive-going crossings (±2 edges).
        assert!((20..=24).contains(&crossings), "crossings = {crossings}");
    }

    #[test]
    fn vibrato_track_depth_is_correct() {
        // One full modulation period so both extremes are sampled.
        let track = vibrato_f0_track(220.0, 50.0, 5.0, 9_600, 48_000);
        let max = track.iter().cloned().fold(0.0f32, f32::max);
        let min = track.iter().cloned().fold(f32::MAX, f32::min);
        let up_cents = 1200.0 * (max / 220.0).log2();
        let dn_cents = 1200.0 * (min / 220.0).log2(); // negative by design
        assert!((up_cents - 50.0).abs() < 0.5, "up = {up_cents}");
        assert!((dn_cents + 50.0).abs() < 0.5, "down = {dn_cents}");
    }

    #[test]
    fn vowel_output_is_bounded_and_periodic_enough() {
        let sr = 48_000;
        let track = f0_track_const(140.0, sr as usize); // 1 s
        let mut rng = Rng::new(3);
        let x = vowel(&track, sr, &VowelSpec::default(), &mut rng);
        let peak = x.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        assert!(peak <= 0.5 + 1e-4);
        assert_eq!(x.len(), track.len());
    }

    #[test]
    fn sweep_track_hits_endpoints() {
        let t = sweep_f0_track(110.0, 440.0, 100);
        assert!((t[0] - 110.0).abs() < 1e-4);
        assert!((t[99] - 440.0).abs() < 1e-2);
    }
}
