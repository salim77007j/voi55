//! D8 verification for the TD-PSOLA pitch shifter.
//!
//! Measurement notes: the shifted output's pitch is verified with the
//! **cepstral comb-spacing** instrument (`measure::f0_ceps`). Two
//! periodicities coexist in a PSOLA output — the grain-placement comb
//! (the audible pitch) and the grain-internal content periodicity (the
//! input pitch) — and both plain autocorrelation arg-max and YIN-family
//! first-dip rules can latch onto the content instead. The cepstrum locks
//! onto the comb spacing. It does not affect the render path: the engines
//! analyze the *input* track once.

use super::*;
use crate::pyin::pyin;
use crate::synth::{self, Rng, VowelSpec};

#[test]
fn neutral_shift_is_bit_exact() {
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(220.0, sr as usize / 2);
    let x = synth::harmonic_tone(&track_f0, sr, 10);
    let t = pyin(&x, sr).expect("pyin");
    let out = pitch_shift(&x, sr, &t, 0.0);
    assert_eq!(out.len(), x.len());
    assert!(out.iter().zip(&x).all(|(a, b)| a == b));
}

#[test]
fn pitch_up_three_semitones_is_measured() {
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(220.0, sr as usize);
    let mut rng = Rng::new(21);
    let x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");

    let out = pitch_shift(&x, sr, &t, 3.0);
    assert_eq!(out.len(), x.len(), "duration must be preserved exactly");

    let target = 220.0f64 * 2.0f64.powf(3.0 / 12.0); // 261.63 Hz
    // A coherent comb must exist at the target pitch and stand well above
    // the off-comb neighbourhood. (The input-pitch comb also remains
    // present — grains are input material — a known TD-PSOLA
    // characteristic documented in the phase report.)
    let s_target = crate::measure::ceps_strength(&out, sr, target);
    let s_off1 = crate::measure::ceps_strength(&out, sr, target * 2.0f64.powf(3.5 / 12.0));
    let s_off2 = crate::measure::ceps_strength(&out, sr, target * 2.0f64.powf(-3.5 / 12.0));
    assert!(
        s_target > 2.0 * s_off1 && s_target > 2.0 * s_off2,
        "+3 st: no dominant comb at {target:.2} Hz ({s_target:.3} vs {s_off1:.3}/{s_off2:.3})"
    );
}

#[test]
fn pitch_down_seven_semitones_is_measured() {
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(220.0, sr as usize);
    let mut rng = Rng::new(22);
    let x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");

    let out = pitch_shift(&x, sr, &t, -7.0);
    assert_eq!(out.len(), x.len());

    let target = 220.0f64 * 2.0f64.powf(-7.0 / 12.0); // 146.83 Hz
    let s_target = crate::measure::ceps_strength(&out, sr, target);
    let s_off1 = crate::measure::ceps_strength(&out, sr, target * 2.0f64.powf(3.5 / 12.0));
    let s_off2 = crate::measure::ceps_strength(&out, sr, target * 2.0f64.powf(-3.5 / 12.0));
    assert!(
        s_target > 2.0 * s_off1 && s_target > 2.0 * s_off2,
        "−7 st: no dominant comb at {target:.2} Hz ({s_target:.3} vs {s_off1:.3}/{s_off2:.3})"
    );
}

#[test]
fn vibrato_pitch_shift_tracks_depth() {
    let sr = 48_000u32;
    let frames = sr as usize;
    let track_f0 = synth::vibrato_f0_track(220.0, 50.0, 5.0, frames, sr);
    let x = synth::harmonic_tone(&track_f0, sr, 10);
    let t = pyin(&x, sr).expect("pyin");

    let semis = 5.0f64;
    let out = pitch_shift(&x, sr, &t, semis);

    // A coherent comb must exist at the shifted mean pitch, above the
    // off-comb neighbourhood (whole-signal measure over full vibrato
    // cycles, so the mean comb is well defined; the ±50-cent vibrato
    // smears the comb, hence the modest dominance requirement).
    let expected = 220.0f64 * 2.0f64.powf(semis / 12.0); // 293.66 Hz
    let s_target = crate::measure::ceps_strength(&out, sr, expected);
    let s_off1 = crate::measure::ceps_strength(&out, sr, expected * 2.0f64.powf(3.5 / 12.0));
    let s_off2 = crate::measure::ceps_strength(&out, sr, expected * 2.0f64.powf(-3.5 / 12.0));
    assert!(
        s_target > 1.5 * s_off1 && s_target > 1.5 * s_off2,
        "vibrato shift: no comb at {expected:.2} Hz ({s_target:.3} vs {s_off1:.3}/{s_off2:.3})"
    );
}

#[test]
fn unvoiced_interior_bypasses_bit_exactly() {
    let sr = 48_000u32;
    let seg = sr as usize * 3 / 10;
    let track_f0 = synth::f0_track_const(200.0, seg);
    let mut rng = Rng::new(23);
    let vowel = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let noise = synth::white_noise(seg, &mut rng);
    let mut x = vowel;
    x.extend(noise);
    let track_f0b = synth::f0_track_const(210.0, seg);
    let vowel_b = synth::vowel(&track_f0b, sr, &VowelSpec::default(), &mut rng);
    let mut vowel_b = vowel_b;
    x.append(&mut vowel_b);

    let t = pyin(&x, sr).expect("pyin");
    let out = pitch_shift(&x, sr, &t, 4.0);

    // Interior of the noise segment (well past the crossfades).
    let lo = seg + seg / 5;
    let hi = seg + (4 * seg) / 5;
    for i in lo..hi {
        assert_eq!(
            out[i], x[i],
            "unvoiced sample {i} altered: {} vs {}",
            out[i], x[i]
        );
    }
}

#[test]
fn spectral_envelope_is_preserved_on_shift() {
    // The spectral envelope must not move when the pitch shifts (D8
    // contract). Instrument: cepstrally-smoothed LTAS (comb removed by
    // quefrency liftering) → F1/F2 peak positions must agree within 12 %.
    // The residual displacement (~10 % measured on +5 st) is the known
    // TD-PSOLA overlap-cancellation ripple at non-integer ratios — see
    // the phase report's honest-gaps section.
    let sr = 48_000u32;
    let track_f0 = synth::f0_track_const(150.0, sr as usize);
    let mut rng = Rng::new(24);
    let x = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
    let t = pyin(&x, sr).expect("pyin");

    let out = pitch_shift(&x, sr, &t, 5.0);
    let before = cepstral_formants(&x, sr);
    let after = cepstral_formants(&out, sr);
    assert_eq!(before.len(), after.len());
    for (b, a) in before.iter().zip(&after) {
        let rel = (a - b).abs() / b;
        assert!(
            rel <= 0.12,
            "envelope peak moved {b:.0} -> {a:.0} Hz ({:.1} %)",
            rel * 100.0
        );
    }
}

/// F1/F2 positions (Hz) from the shared cepstral-envelope instrument.
fn cepstral_formants(x: &[f32], sr: u32) -> Vec<f64> {
    let nfft = 8192usize;
    let bin_hz = sr as f64 / nfft as f64;
    let env = crate::measure::cepstral_envelope(x, nfft, sr as usize / 300);
    vec![
        crate::measure::envelope_peak(&env, bin_hz, 300.0, 700.0),
        crate::measure::envelope_peak(&env, bin_hz, 1200.0, 1900.0),
    ]
}

#[test]
fn silence_input_returns_copy() {
    let sr = 48_000u32;
    let x = vec![0.0f32; sr as usize / 4];
    let t = pyin(&x, sr).expect("pyin");
    let out = pitch_shift(&x, sr, &t, 3.0);
    assert!(out.iter().all(|&v| v == 0.0));
}
