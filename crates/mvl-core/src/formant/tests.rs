//! D9 verification for the formant (vocal-tract length) engine.

use super::*;
use crate::measure;
use crate::pyin::pyin;
use crate::synth::{self, Rng, VowelSpec};

/// F1/F2/F3 positions (Hz) from the shared cepstral-envelope instrument.
/// Bands are wide enough to cover both the neutral and the warped peak.
fn formants(x: &[f32], sr: u32, warped: bool) -> Vec<f64> {
    let nfft = 8192usize;
    let bin_hz = sr as f64 / nfft as f64;
    let env = measure::cepstral_envelope(x, nfft, sr as usize / 300);
    // With the tract shortened to 130 mm the comb spacing grows to ~188 Hz
    // and every formant moves up by ×1.346; wide bands catch both cases.
    let (f1b, f2b, f3b) = if warped {
        (500.0, 1550.0, 2700.0)
    } else {
        (350.0, 1150.0, 2050.0)
    };
    vec![
        measure::envelope_peak(&env, bin_hz, f1b, f1b + 400.0),
        measure::envelope_peak(&env, bin_hz, f2b, f2b + 900.0),
        measure::envelope_peak(&env, bin_hz, f3b, f3b + 900.0),
    ]
}

fn fixture(sr: u32) -> Vec<f32> {
    let track_f0 = synth::f0_track_const(140.0, sr as usize * 6 / 5);
    let mut rng = Rng::new(41);
    synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng)
}

#[test]
fn neutral_tract_length_is_bit_exact() {
    let sr = 48_000u32;
    let x = fixture(sr);
    let t = pyin(&x, sr).expect("pyin");
    let out = shift_formants(&x, sr, &t, 175.0).expect("render");
    assert_eq!(out.len(), x.len());
    assert!(out.iter().zip(&x).all(|(a, b)| a == b));
}

#[test]
fn mm_out_of_hard_limits_is_an_error() {
    let sr = 48_000u32;
    let x = fixture(sr);
    let t = pyin(&x, sr).expect("pyin");
    assert!(shift_formants(&x, sr, &t, 50.0).is_err());
    assert!(shift_formants(&x, sr, &t, 400.0).is_err());
}

#[test]
fn shortened_tract_moves_all_formants_by_the_contracted_ratio() {
    let sr = 48_000u32;
    let x = fixture(sr);
    let t = pyin(&x, sr).expect("pyin");

    let out = shift_formants(&x, sr, &t, 130.0).expect("render");
    assert_eq!(out.len(), x.len());

    let expected = 175.0 / 130.0; // 1.346
    let before = formants(&x, sr, false);
    let after = formants(&out, sr, true);
    for (i, (&b, &a)) in before.iter().zip(&after).enumerate() {
        let measured = a / b;
        // 8 % tolerance: the envelope peak is sampled on the harmonic grid
        // (140 Hz here), so its measurable position is ±~1 harmonic wide.
        assert!(
            (measured - expected).abs() / expected < 0.08,
            "F{} moved ×{measured:.3}, expected ×{expected:.3} ({b:.0} -> {a:.0} Hz)",
            i + 1
        );
    }
}

#[test]
fn lengthened_tract_moves_formants_down() {
    let sr = 48_000u32;
    let x = fixture(sr);
    let t = pyin(&x, sr).expect("pyin");

    let out = shift_formants(&x, sr, &t, 190.0).expect("render");
    let expected = 175.0 / 190.0; // 0.921

    let nfft = 8192usize;
    let bin_hz = sr as f64 / nfft as f64;
    let env_b = measure::cepstral_envelope(&x, nfft, sr as usize / 300);
    let env_a = measure::cepstral_envelope(&out, nfft, sr as usize / 300);
    let f1b = measure::envelope_peak(&env_b, bin_hz, 350.0, 700.0);
    let f1a = measure::envelope_peak(&env_a, bin_hz, 300.0, 650.0);
    let measured = f1a / f1b;
    // 8 % tolerance: harmonic-grid envelope-peak resolution (see above).
    assert!(
        (measured - expected).abs() / expected < 0.08,
        "F1 moved ×{measured:.3}, expected ×{expected:.3} ({f1b:.0} -> {f1a:.0} Hz)"
    );
}

#[test]
fn warp_leaves_pitch_untouched() {
    let sr = 48_000u32;
    let x = fixture(sr);
    let t = pyin(&x, sr).expect("pyin");

    let out = shift_formants(&x, sr, &t, 130.0).expect("render");
    let t_out = pyin(&out, sr).expect("pyin out");

    let mut errs: Vec<f64> = (6..t_out.len().saturating_sub(6))
        .filter(|&i| t_out.voiced[i] && t.voiced[i])
        .map(|i| crate::pyin::cents_between(f64::from(t_out.f0[i]), f64::from(t.f0[i])))
        .collect();
    assert!(!errs.is_empty(), "no voiced frames after warp");
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let med = errs[errs.len() / 2];
    assert!(
        med.abs() <= 5.0,
        "warp shifted pitch by {med:+.2} cents median"
    );
}

#[test]
fn unvoiced_interior_bypasses_bit_exactly() {
    let sr = 48_000u32;
    let seg = sr as usize * 2 / 5;
    let track_f0 = synth::f0_track_const(160.0, seg);
    let mut rng = Rng::new(42);
    let mut x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    x.extend(synth::white_noise(seg, &mut rng));
    let track_f0b = synth::f0_track_const(170.0, seg);
    x.append(&mut synth::vowel(
        &track_f0b,
        sr,
        &VowelSpec::default(),
        &mut rng,
    ));

    let t = pyin(&x, sr).expect("pyin");
    let out = shift_formants(&x, sr, &t, 130.0).expect("render");

    let lo = seg + seg / 5;
    let hi = seg + (4 * seg) / 5;
    for i in lo..hi {
        assert_eq!(out[i], x[i], "unvoiced sample {i} altered");
    }
}

#[test]
fn overall_loudness_stays_stable() {
    // The loudness-compensation guardrail: total RMS within ±1 dB.
    let sr = 48_000u32;
    let x = fixture(sr);
    let t = pyin(&x, sr).expect("pyin");
    let out = shift_formants(&x, sr, &t, 130.0).expect("render");

    let rms =
        |s: &[f32]| s.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / s.len() as f64;
    let delta_db = 10.0 * (rms(&out) / rms(&x)).log10();
    assert!(
        delta_db.abs() <= 1.0,
        "loudness changed by {delta_db:+.2} dB"
    );
}
