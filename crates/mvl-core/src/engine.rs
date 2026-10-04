//! Engine parameter contract.
//!
//! These are the exact ranges and resolutions the UI sliders enforce
//! (`docs/ARCHITECTURE_PLAN.md`, brief §B and D13). Keeping them here means
//! the UI and the DSP engine can never drift apart: both read the same
//! clamps. The processing itself is implemented in Phase 3.

/// Pitch shift, in semitones: ±12.
pub const PITCH_RANGE_SEMITONES: f64 = 12.0;
/// Pitch resolution: 1 cent.
pub const PITCH_STEP_CENTS: f64 = 1.0;
/// Air/breath gain range, dB.
pub const AIR_RANGE_DB: (f64, f64) = (-24.0, 12.0);
/// Air/breath resolution: 0.1 dB.
pub const AIR_STEP_DB: f64 = 0.1;
/// Formant (vocal-tract length) range, millimetres.
pub const FORMANT_RANGE_MM: (f64, f64) = (130.0, 190.0);
/// Formant resolution: 1 mm.
pub const FORMANT_STEP_MM: f64 = 1.0;
/// Reference adult vocal-tract length used for the mm ↔ scaling mapping (D9).
pub const REFERENCE_VTL_MM: f64 = 175.0;

/// The three user-facing parameters of Micro-Vocal Lab, clamped at rest.
///
/// All values are stored already clamped to their documented range; setting
/// an out-of-range value saturates instead of panicking, so a UI bug can
/// never push the engine into an invalid state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineParams {
    pitch_semitones: f64,
    air_db: f64,
    formant_mm: f64,
}

impl Default for EngineParams {
    fn default() -> Self {
        Self {
            pitch_semitones: 0.0,
            air_db: 0.0,
            formant_mm: REFERENCE_VTL_MM,
        }
    }
}

impl EngineParams {
    /// Saturating setter for the pitch slider (semitones, ±12).
    pub fn set_pitch_semitones(&mut self, semitones: f64) {
        self.pitch_semitones = clamp_step(
            semitones,
            -PITCH_RANGE_SEMITONES,
            PITCH_RANGE_SEMITONES,
            PITCH_STEP_CENTS / 100.0,
        );
    }

    /// Saturating setter for the air/breath slider (dB, 0.1 steps).
    pub fn set_air_db(&mut self, db: f64) {
        self.air_db = clamp_step(db, AIR_RANGE_DB.0, AIR_RANGE_DB.1, AIR_STEP_DB);
    }

    /// Saturating setter for the formant slider (vocal-tract length in mm).
    pub fn set_formant_mm(&mut self, mm: f64) {
        self.formant_mm = clamp_step(mm, FORMANT_RANGE_MM.0, FORMANT_RANGE_MM.1, FORMANT_STEP_MM);
    }

    /// Current pitch shift in semitones.
    pub fn pitch_semitones(&self) -> f64 {
        self.pitch_semitones
    }

    /// Current pitch shift in cents (semitones × 100).
    pub fn pitch_cents(&self) -> f64 {
        self.pitch_semitones * 100.0
    }

    /// Current air/breath gain in dB.
    pub fn air_db(&self) -> f64 {
        self.air_db
    }

    /// Current vocal-tract length in millimetres.
    pub fn formant_mm(&self) -> f64 {
        self.formant_mm
    }

    /// Formant frequency scaling factor implied by the current VTL
    /// (`F_new ≈ F_old · 175 / L`, see D9).
    pub fn formant_scale(&self) -> f64 {
        REFERENCE_VTL_MM / self.formant_mm
    }

    /// True when no parameter deviates from neutral (used by the UI to
    /// decide whether a render would be a no-op).
    pub fn is_neutral(&self) -> bool {
        self.pitch_semitones == 0.0 && self.air_db == 0.0 && self.formant_mm == REFERENCE_VTL_MM
    }
}

/// Clamp `value` into `[min, max]` and quantize to `step` (snapping from the
/// `min` edge). Used for every engine parameter.
fn clamp_step(value: f64, min: f64, max: f64, step: f64) -> f64 {
    let clamped = value.clamp(min, max);
    let steps = ((clamped - min) / step).round();
    min + steps * step
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_clamps_to_twelve_semitones() {
        let mut p = EngineParams::default();
        p.set_pitch_semitones(50.0);
        assert_eq!(p.pitch_semitones(), 12.0);
        p.set_pitch_semitones(-50.0);
        assert_eq!(p.pitch_semitones(), -12.0);
    }

    #[test]
    fn pitch_snaps_to_one_cent_grid() {
        let mut p = EngineParams::default();
        p.set_pitch_semitones(0.123456); // 12.3456 cents -> snaps to 12 cents = 0.12 st
        assert!((p.pitch_cents() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn air_clamps_and_snaps_to_tenth_db() {
        let mut p = EngineParams::default();
        p.set_air_db(3.549); // -> 3.5
        assert!((p.air_db() - 3.5).abs() < 1e-9);
        p.set_air_db(999.0);
        assert!((p.air_db() - 12.0).abs() < 1e-9);
        p.set_air_db(-999.0);
        assert!((p.air_db() + 24.0).abs() < 1e-9);
    }

    #[test]
    fn formant_clamps_to_mm_range() {
        let mut p = EngineParams::default();
        p.set_formant_mm(100.0);
        assert_eq!(p.formant_mm(), 130.0);
        p.set_formant_mm(300.0);
        assert_eq!(p.formant_mm(), 190.0);
    }

    #[test]
    fn formant_scale_matches_vtl_mapping() {
        let mut p = EngineParams::default();
        assert!((p.formant_scale() - 1.0).abs() < 1e-12);
        p.set_formant_mm(175.0 / 1.35); // ~129.6 clamps to 130
        let expected = 175.0 / p.formant_mm();
        assert!((p.formant_scale() - expected).abs() < 1e-12);
    }

    #[test]
    fn neutral_state_detected() {
        let p = EngineParams::default();
        assert!(p.is_neutral());
        let mut q = EngineParams::default();
        q.set_pitch_semitones(0.01);
        assert!(!q.is_neutral());
    }
}
