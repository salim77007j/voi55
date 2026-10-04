//! Offline render pipeline — D11: pitch → formant → air, in that order,
//! over a single analysis pass.
//!
//! The fixed order exists because each stage expects the stage before it:
//! PSOLA works on clean excitation, the formant warp on pitch-stabilized
//! material, and the air engine on the finished voice. All three stages
//! share ONE pYIN analysis of the input — the stages that follow operate
//! on time-aligned material (PSOLA preserves duration and the formant
//! warp preserves pitch), so re-analysis would be redundant.
//!
//! Real-time preview (control-rate parameters, ~10 ms crossfades) lands in
//! Phase 4 on top of the same engines; this module is the quality-maximal
//! offline path.

use crate::engine::EngineParams;
use crate::error::Result;
use crate::pyin::{PyinResult, pyin};

/// Summary of one offline render ( surfaced in the UI status bar and in
/// the validation report).
#[derive(Debug, Clone, PartialEq)]
pub struct RenderReport {
    /// pYIN frames analysed.
    pub frames_analyzed: usize,
    /// Fraction of frames the Viterbi path marked voiced.
    pub voiced_ratio: f32,
    /// Median F0 over voiced frames (0.0 if nothing voiced).
    pub median_f0_hz: f32,
    /// Which stages actually ran: [pitch, formant, air].
    pub stages_applied: [bool; 3],
}

/// Renders `x` (mono) with the three engines in D11 order.
///
/// Returns the rendered samples, the shared pitch track (for UI display)
/// and a report. Neutral parameters yield a bit-exact copy.
///
/// # Errors
/// Propagates engine/FFT failures.
pub fn render(
    x: &[f32],
    sample_rate: u32,
    params: &EngineParams,
) -> Result<(Vec<f32>, PyinResult, RenderReport)> {
    let track = pyin(x, sample_rate)?;

    let mut samples = x.to_vec();
    let mut stages = [false; 3];

    // 1. Pitch (TD-PSOLA).
    let semitones = params.pitch_semitones();
    if semitones.abs() >= 1e-9 {
        samples = crate::psola::pitch_shift(&samples, sample_rate, &track, semitones);
        stages[0] = true;
    }

    // 2. Formant (vocal-tract length).
    let mm = params.formant_mm();
    if (mm - crate::engine::REFERENCE_VTL_MM).abs() >= 1e-9 {
        samples = crate::formant::shift_formants(&samples, sample_rate, &track, mm)?;
        stages[1] = true;
    }

    // 3. Air / breath.
    let air_db = params.air_db();
    if air_db.abs() >= 1e-9 {
        samples = crate::air::process_air(&samples, sample_rate, &track, air_db)?;
        stages[2] = true;
    }

    let report = RenderReport {
        frames_analyzed: track.len(),
        voiced_ratio: track.voiced_ratio(),
        median_f0_hz: track.median_f0(),
        stages_applied: stages,
    };
    Ok((samples, track, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineParams;
    use crate::measure;
    use crate::synth::{self, Rng, VowelSpec};

    fn vowel_fixture(sr: u32) -> Vec<f32> {
        let track_f0 = synth::f0_track_const(160.0, sr as usize);
        let mut rng = Rng::new(71);
        synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng)
    }

    #[test]
    fn neutral_params_yield_bit_exact_copy() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let (out, _track, report) = render(&x, sr, &EngineParams::default()).expect("render");
        assert_eq!(out.len(), x.len());
        assert!(out.iter().zip(&x).all(|(a, b)| a == b));
        assert_eq!(report.stages_applied, [false, false, false]);
        assert!(report.frames_analyzed > 0);
    }

    #[test]
    fn stages_apply_in_declared_order_and_each_leaves_a_mark() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let mut params = EngineParams::default();
        params.set_pitch_semitones(3.0);
        params.set_formant_mm(130.0);
        params.set_air_db(6.0);

        let (out, _track, report) = render(&x, sr, &params).expect("render");
        assert_eq!(out.len(), x.len());
        assert_eq!(report.stages_applied, [true, true, true]);

        // Pitch: a coherent comb at the target (ceps instrument).
        let target = 160.0f64 * 2.0f64.powf(3.0 / 12.0);
        let s_target = measure::ceps_strength(&out, sr, target);
        let s_off = measure::ceps_strength(&out, sr, target * 2.0f64.powf(3.5 / 12.0));
        assert!(s_target > 2.0 * s_off, "no comb at {target:.1} Hz");

        // Formant: F1 moved up (cepstral envelope peak in the warped band).
        let nfft = 8192usize;
        let bin_hz = sr as f64 / nfft as f64;
        let env = measure::cepstral_envelope(&out, nfft, sr as usize / 300);
        let f1 = measure::envelope_peak(&env, bin_hz, 500.0, 900.0);
        assert!(f1 > 560.0, "F1 did not move up: {f1:.1} Hz");

        // Air: high-band noise bed boosted vs the neutral render.
        let (neutral, _, _) = render(&x, sr, &EngineParams::default()).expect("neutral");
        let e_in = band_power(&neutral, sr, 3000.0, 4500.0);
        let e_out = band_power(&out, sr, 3000.0, 4500.0);
        assert!(
            e_out > e_in * 1.4,
            "air stage did not boost the high band ({e_in:.3} -> {e_out:.3})"
        );
    }

    /// Raw band power (no windowing finesse needed for an A/B comparison).
    fn band_power(x: &[f32], sr: u32, lo: f64, hi: f64) -> f64 {
        use realfft::RealFftPlanner;
        let nfft = 8192usize;
        let bin_hz = sr as f64 / nfft as f64;
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(nfft);
        let mut inp = r2c.make_input_vec();
        let seg_end = usize::min(x.len(), nfft);
        inp[..seg_end].copy_from_slice(&x[..seg_end]);
        inp[seg_end..].fill(0.0);
        let mut spec = r2c.make_output_vec();
        r2c.process(&mut inp, &mut spec).expect("fft");
        let (lo_b, hi_b) = ((lo / bin_hz) as usize, (hi / bin_hz) as usize);
        spec[lo_b..=hi_b]
            .iter()
            .map(|c| f64::from(c.norm_sqr()))
            .sum()
    }

    #[test]
    fn report_describes_the_track() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let (_out, _track, report) = render(&x, sr, &EngineParams::default()).expect("render");
        assert!(
            report.voiced_ratio > 0.8,
            "voiced ratio {}",
            report.voiced_ratio
        );
        assert!(
            (report.median_f0_hz - 160.0).abs() < 5.0,
            "median f0 {} Hz",
            report.median_f0_hz
        );
    }
}
