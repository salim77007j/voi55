//! D7 accuracy gates for the pYIN tracker, run on synthetic fixtures.

use super::*;

/// Collects (measured_f0, true_f0) pairs over interior frames, skipping
/// `edge` frames at both ends (window boundary effects).
fn paired(track: &PyinResult, true_f0_at: impl Fn(f64) -> f64, edge: usize) -> Vec<(f64, f64)> {
    (edge..track.len().saturating_sub(edge))
        .filter(|&i| track.voiced[i])
        .map(|i| {
            let t = track.frame_time(i);
            (f64::from(track.f0[i]), true_f0_at(t))
        })
        .collect()
}

fn cents_errors(pairs: &[(f64, f64)]) -> Vec<f64> {
    let mut errs: Vec<f64> = pairs
        .iter()
        .map(|&(fm, ft)| cents_between(fm, ft))
        .collect();
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    errs
}

fn assert_gates(errs: &[f64], median_max: f64, p95_max: f64, ctx: &str) {
    assert!(!errs.is_empty(), "{ctx}: no voiced frames to evaluate");
    let median = errs[errs.len() / 2];
    let p95 = errs[(errs.len() as f64 * 0.95) as usize].abs();
    assert!(
        median.abs() <= median_max,
        "{ctx}: median |{median:.2}| cents > {median_max}"
    );
    assert!(p95 <= p95_max, "{ctx}: p95 |{p95:.2}| cents > {p95_max}");
}

#[test]
fn clean_tone_within_5_cents_median() {
    let sr = 48_000;
    let track_f0 = crate::synth::f0_track_const(220.0, sr as usize);
    let x = crate::synth::harmonic_tone(&track_f0, sr, 12);
    let t = pyin(&x, sr).expect("pyin");
    let errs = cents_errors(&paired(&t, |_| 220.0, 4));
    assert_gates(&errs, 5.0, 20.0, "220 Hz tone");
}

#[test]
fn low_and_high_tones_hold_gates() {
    let sr = 48_000;
    for &f0 in &[82.4f32, 110.0, 440.0, 880.0] {
        let track_f0 = crate::synth::f0_track_const(f0, sr as usize / 2);
        let x = crate::synth::harmonic_tone(&track_f0, sr, 16);
        let t = pyin(&x, sr).expect("pyin");
        let errs = cents_errors(&paired(&t, |_| f64::from(f0), 4));
        assert_gates(&errs, 5.0, 20.0, &format!("{f0} Hz tone"));
    }
}

#[test]
fn vibrato_4_and_7_hz_within_gates() {
    let sr = 48_000;
    for &rate in &[4.0f32, 7.0] {
        // ±50 cents around 220 Hz (fixture per D7 verification gate).
        let frames = sr as usize; // 1 s
        let track_f0 = crate::synth::vibrato_f0_track(220.0, 50.0, rate, frames, sr);
        let x = crate::synth::harmonic_tone(&track_f0, sr, 12);
        let t = pyin(&x, sr).expect("pyin");
        let errs = cents_errors(&paired(
            &t,
            |time| {
                let idx = usize::min((time * f64::from(sr)) as usize, track_f0.len() - 1);
                f64::from(track_f0[idx])
            },
            6,
        ));
        assert_gates(&errs, 5.0, 20.0, &format!("vibrato {rate} Hz ±50c"));
    }
}

#[test]
fn sweep_tracks_within_gates() {
    let sr = 48_000;
    // Vocal glissando: 110 → 440 Hz over 2 s (0.5 octave/s).
    let frames = 2 * sr as usize;
    let track_f0 = crate::synth::sweep_f0_track(110.0, 440.0, frames);
    let x = crate::synth::harmonic_tone(&track_f0, sr, 12);
    let t = pyin(&x, sr).expect("pyin");
    let errs = cents_errors(&paired(
        &t,
        |time| {
            let k = (time * f64::from(sr) / frames as f64).clamp(0.0, 1.0);
            110.0f64 * (440.0f64 / 110.0).powf(k)
        },
        6,
    ));
    assert_gates(&errs, 5.0, 20.0, "sweep 110→440");
}

#[test]
fn noise_and_silence_flagged_unvoiced() {
    let sr = 48_000;
    let seg = sr as usize * 2 / 5; // 0.4 s
    let mut rng = crate::synth::Rng::new(99);
    let mut x = crate::synth::white_noise(seg, &mut rng);
    x.extend(std::iter::repeat_n(0.0f32, seg));
    let more = crate::synth::white_noise(seg, &mut rng);
    x.extend(more);

    let t = pyin(&x, sr).expect("pyin");
    let interior = &t.voiced[6..t.len() - 6];
    let unvoiced = interior.iter().filter(|&&v| !v).count();
    let ratio = f64::from(unvoiced as u32) / interior.len() as f64;
    assert!(
        ratio >= 0.95,
        "unvoiced accuracy {ratio:.3} < 0.95 (interior frames {})",
        interior.len()
    );
}

#[test]
fn vowel_fixture_is_mostly_voiced() {
    let sr = 48_000;
    let track_f0 = crate::synth::f0_track_const(140.0, sr as usize);
    let mut rng = crate::synth::Rng::new(5);
    let x = crate::synth::vowel(&track_f0, sr, &crate::synth::VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");
    let interior = &t.voiced[6..t.len() - 6];
    let voiced = interior.iter().filter(|&&v| v).count();
    let ratio = f64::from(voiced as u32) / interior.len() as f64;
    assert!(ratio >= 0.9, "voiced ratio {ratio:.3} < 0.9 on vowel");
    let errs = cents_errors(&paired(&t, |_| 140.0, 6));
    assert_gates(&errs, 5.0, 20.0, "140 Hz vowel");
}

#[test]
fn estimate_is_scale_invariant() {
    let sr = 48_000;
    let track_f0 = crate::synth::f0_track_const(220.0, sr as usize / 2);
    let x = crate::synth::harmonic_tone(&track_f0, sr, 8);
    let quiet: Vec<f32> = x.iter().map(|&v| v * 0.1).collect();
    let a = pyin(&x, sr).expect("pyin a");
    let b = pyin(&quiet, sr).expect("pyin b");
    for (fa, fb) in a.f0.iter().zip(&b.f0) {
        if *fa > 0.0 && *fb > 0.0 {
            let d = cents_between(f64::from(*fa), f64::from(*fb)).abs();
            assert!(d < 5.0, "scale changed estimate by {d:.2} cents");
        }
    }
}

#[test]
fn empty_input_is_an_error() {
    assert!(pyin(&[], 48_000).is_err());
}
