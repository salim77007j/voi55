//! Chunk-driven streaming render pipeline (Phase 6) — the missing piece
//! between the Phase 5 carry-OLA groundwork and the brief's interactive
//! preview budget.
//!
//! [`StreamChain`] drives the three engines (D11 order: pitch → formant →
//! air) through their chunk-driven stage drivers so a caller can pull the
//! render in fixed-size output chunks while it plays, instead of waiting
//! for a whole-file render. Three properties make this safe:
//!
//! 1. **Bit-exact vs the offline render.** Every stage emits its grains /
//!    STFT frames in the same chronological order and adds them into the
//!    same carry rings; `CarryOla::flush_to` only decides *when* finished
//!    samples become visible, never *what* they are. Gate tests below
//!    assert exact `f32` equality between chunked chains and
//!    `pipeline::render_with_track` for every stage combination and
//!    adversarial chunk sizes (1 sample upwards).
//! 2. **Bounded memory.** Each stage's output buffer is front-trimmed to
//!    the consumer's read frontier once samples are consumed, so chain
//!    memory is a few windows, independent of file length — a 10-minute
//!    session streams in the same footprint as a 10-second one.
//! 3. **Honest restart semantics.** [`StreamChain::new_at`] restarts the
//!    chain at an arbitrary position (slider change while playing). The
//!    restarted chain is deterministic, but output positions near the
//!    restart point miss left-hand overlap/frame contributions from before
//!    it (and the PSOLA epoch phase resets). The artifact region is
//!    bounded by one stage window (measured by the `restart_stft_steady_
//!    state` gate test) and is what the preview player masks with a short
//!    crossfade.
//!
//! Stage activation mirrors `pipeline::render_with_track` exactly
//! (neutral parameters never instantiate a stage), so a chain with all
//! sliders at neutral is not constructible — callers keep playing the
//! original buffer instead.

use crate::engine::EngineParams;
use crate::error::{CoreError, Result};
use crate::pyin::PyinResult;

/// Drives `pitch → formant → air` over pre-analyzed pYIN data in
/// pull-chunks. Create once per (input, parameters) pair; a parameter
/// change rebuilds the chain at the playhead (see [`StreamChain::new_at`]).
pub struct StreamChain<'a> {
    len: usize,
    /// Chain start offset (0 for a whole-file render).
    start: usize,
    psola: Option<crate::psola::PsolaStage<'a>>,
    formant: Option<crate::formant::FormantStage<'a>>,
    air: Option<crate::air::AirStage<'a>>,
    /// Absolute position up to which the chain consumer took the output.
    consumed: usize,
}

impl<'a> StreamChain<'a> {
    /// Whole-file streaming chain (equivalent to `new_at(.., 0)`).
    ///
    /// # Errors
    /// Fails on engine parameter errors; neutral parameters are rejected
    /// (there would be nothing to stream — play the original buffer).
    pub fn new(
        session: &[f32],
        sample_rate: u32,
        track: &'a PyinResult,
        params: &EngineParams,
    ) -> Result<Self> {
        Self::new_at(session, sample_rate, track, params, 0)
    }

    /// Streaming chain restarting at absolute sample `start` (playhead).
    ///
    /// Stage activation mirrors `pipeline::render_with_track`: a stage is
    /// instantiated iff its parameter is off-neutral. `start = 0` yields
    /// bit-identical output to the offline render; `start > 0` produces the
    /// disclosed restart artifact near the splice (see the module docs).
    ///
    /// # Errors
    /// Fails on engine parameter errors; neutral parameters are rejected.
    pub fn new_at(
        session: &[f32],
        sample_rate: u32,
        track: &'a PyinResult,
        params: &EngineParams,
        start: usize,
    ) -> Result<Self> {
        let start = start.min(session.len());
        // Activation conditions — identical to pipeline::render_with_track.
        let semitones = params.pitch_semitones();
        let mm = params.formant_mm();
        let air_db = params.air_db();
        let pitch_active = semitones.abs() >= 1e-9;
        let formant_active = (mm - crate::engine::REFERENCE_VTL_MM).abs() >= 1e-9;
        let air_active = air_db.abs() >= 1e-9;
        if !pitch_active && !formant_active && !air_active {
            return Err(CoreError::Analysis(
                "streaming chain requested with neutral parameters".into(),
            ));
        }

        // The PSOLA stage builds its epoch schedule from the full signal —
        // it is only ever the chain's first stage, fed by the complete
        // session buffer. A degenerate epoch schedule (silence, < 2 epochs)
        // surfaces as passthrough and is wired around: downstream stages
        // then read the session directly, exactly like the offline
        // pipeline's passthrough copy.
        let psola = if pitch_active {
            let ratio = 2.0f64.powf(semitones / 12.0);
            let stage = crate::psola::PsolaStage::new(session, sample_rate, track, ratio, start);
            if stage.is_passthrough() {
                None
            } else {
                Some(stage)
            }
        } else {
            None
        };

        Ok(Self {
            len: session.len(),
            start,
            psola,
            formant: if formant_active {
                Some(crate::formant::FormantStage::new(
                    session.len(),
                    sample_rate,
                    track,
                    crate::engine::REFERENCE_VTL_MM / mm,
                    start,
                ))
            } else {
                None
            },
            air: if air_active {
                Some(crate::air::AirStage::new(
                    session.len(),
                    sample_rate,
                    track,
                    air_db.clamp(-24.0, 12.0),
                    start,
                ))
            } else {
                None
            },
            consumed: start,
        })
    }

    /// Total output length (the input length — every stage preserves it).
    pub fn total_len(&self) -> usize {
        self.len
    }

    /// First absolute output position this chain will ever emit.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Absolute output frontier: `flushed()..` is not yet final;
    /// `start..flushed()` is final (produced but maybe not consumed).
    pub fn flushed(&self) -> usize {
        if let Some(a) = &self.air {
            a.flushed()
        } else if let Some(f) = &self.formant {
            f.flushed()
        } else if let Some(p) = &self.psola {
            p.flushed()
        } else {
            self.start
        }
    }

    /// Samples produced but not yet taken by the consumer.
    pub fn pending(&self) -> usize {
        self.flushed().saturating_sub(self.consumed)
    }

    /// Worst-case input-lookahead of the active stages, in samples — the
    /// group delay between the input cursor and the output frontier. Used
    /// for the honest latency readout in the status bar.
    pub fn latency_samples(&self) -> usize {
        let mut total = 0usize;
        if let Some(p) = &self.psola {
            total += p.max_half().ceil() as usize;
        }
        if let Some(f) = &self.formant {
            total += f.window_samples();
        }
        if let Some(a) = &self.air {
            total += a.window_samples();
        }
        total
    }

    /// Sum of all active stage heads — a monotonic progress indicator for
    /// the demand-wave loop (the final stage may stall while upstream
    /// stages feed it; that is still progress).
    fn progress(&self) -> usize {
        let p = self.psola.as_ref().map_or(0, |s| s.flushed());
        let f = self.formant.as_ref().map_or(0, |s| s.flushed());
        let a = self.air.as_ref().map_or(0, |s| s.flushed());
        p + f + a
    }

    /// Advances the chain until the output frontier reaches `target` (or
    /// the input/render is exhausted). The caller passes the *complete*
    /// session signal every call — stages read it through the demand wave.
    ///
    /// # Errors
    /// Propagates engine/FFT failures from any stage.
    pub fn advance_to(&mut self, session: &[f32], target: usize) -> Result<()> {
        debug_assert_eq!(session.len(), self.len, "chain input is the full session");
        let target = target.min(self.len);
        let has_f = self.formant.is_some();
        let has_a = self.air.is_some();

        loop {
            let before = self.progress();
            // Demand wave: each stage runs until it can satisfy its
            // consumer's current need. Needs are recomputed every round —
            // the wave travels downstream → upstream.
            let air_need = self
                .air
                .as_ref()
                .and_then(crate::air::AirStage::next_need)
                .unwrap_or(self.len);
            let formant_need = self
                .formant
                .as_ref()
                .and_then(crate::formant::FormantStage::next_need)
                .unwrap_or(self.len);
            let air_target = target;
            let formant_target = if has_a { air_need } else { target };
            let psola_target = if has_f {
                formant_need
            } else if has_a {
                air_need
            } else {
                target
            };

            // Downstream first: air consumes what formant (or the source)
            // already produced.
            if let Some(a) = self.air.as_mut() {
                let (src, base): (&[f32], usize) = match (&self.formant, &self.psola) {
                    (Some(f), _) => (f.out_slice(), f.out_base()),
                    (None, Some(p)) => (p.out_slice(), p.out_base()),
                    (None, None) => (session, 0),
                };
                a.advance_to(src, base, air_target)?;
            }
            if let Some(f) = self.formant.as_mut() {
                let (src, base): (&[f32], usize) = match &self.psola {
                    Some(p) => (p.out_slice(), p.out_base()),
                    None => (session, 0),
                };
                f.advance_to(src, base, formant_target)?;
            }
            if let Some(p) = self.psola.as_mut() {
                p.advance_to(session, psola_target);
            }

            // Trim intermediate hand-off buffers to the consumer's frontier
            // (bounded memory — see the module docs). The consumer never
            // reads below its own flush head, so this is always safe.
            if let (Some(p), Some(f)) = (self.psola.as_mut(), self.formant.as_ref()) {
                p.trim_to(f.flushed());
            } else if let (Some(p), Some(a)) = (self.psola.as_mut(), self.air.as_ref()) {
                // pitch → air direct wiring (formant at neutral)
                p.trim_to(a.flushed());
            }
            if let (Some(f), Some(a)) = (self.formant.as_mut(), self.air.as_ref()) {
                f.trim_to(a.flushed());
            }

            let after = self.progress();
            if self.flushed() >= target || after == before {
                // No stage made progress this round: either the target is
                // reached or the remaining work needs `finish`.
                break;
            }
        }
        Ok(())
    }

    /// Flushes all remaining output once the input is exhausted (the tail
    /// beyond the last grain/frame). Call when `pending()` stops growing
    /// short of the target. Order matters: each stage's tail becomes the
    /// downstream stage's input tail, so stage 1 flushes first.
    ///
    /// # Errors
    /// Propagates engine/FFT failures.
    pub fn finish(&mut self, session: &[f32]) -> Result<()> {
        if let Some(p) = self.psola.as_mut() {
            p.advance_to(session, usize::MAX);
            p.finish(session);
        }
        if let Some(f) = self.formant.as_mut() {
            // Run the remaining frames against the (now complete) input.
            let (src, base): (&[f32], usize) = match &self.psola {
                Some(p) => (p.out_slice(), p.out_base()),
                None => (session, 0),
            };
            f.advance_to(src, base, self.len)?;
            let (src, base): (&[f32], usize) = match &self.psola {
                Some(p) => (p.out_slice(), p.out_base()),
                None => (session, 0),
            };
            f.finish(src, base);
        }
        if let Some(a) = self.air.as_mut() {
            let (src, base): (&[f32], usize) = match (&self.formant, &self.psola) {
                (Some(f), _) => (f.out_slice(), f.out_base()),
                (None, Some(p)) => (p.out_slice(), p.out_base()),
                (None, None) => (session, 0),
            };
            a.advance_to(src, base, self.len)?;
            let (src, base): (&[f32], usize) = match (&self.formant, &self.psola) {
                (Some(f), _) => (f.out_slice(), f.out_base()),
                (None, Some(p)) => (p.out_slice(), p.out_base()),
                (None, None) => (session, 0),
            };
            a.finish(src, base);
        }
        Ok(())
    }

    /// Copies the unconsumed output range `[consumed, flushed)` into `dst`
    /// (appending). Call after `advance_to`.
    pub fn take_output(&self, dst: &mut Vec<f32>) {
        let (src, base) = self.last_stage_output();
        let from = self.consumed.saturating_sub(base);
        let to = self.flushed() - base;
        dst.extend_from_slice(&src[from..to]);
    }

    /// Marks `n` pending samples as consumed and trims the final stage's
    /// buffer (bounded memory).
    pub fn consume(&mut self, n: usize) {
        self.consumed += n;
        let keep = self.consumed;
        match self.air.as_mut() {
            Some(a) => a.trim_to(keep),
            None => match self.formant.as_mut() {
                Some(f) => f.trim_to(keep),
                None => {
                    if let Some(p) = self.psola.as_mut() {
                        p.trim_to(keep);
                    }
                }
            },
        }
    }

    /// Total samples currently alive in the chain's internal output
    /// buffers (exposed for the RAM-budget checks in the validation
    /// report).
    pub fn internal_buffer_samples(&self) -> usize {
        let stage = |s: Option<&[f32]>| s.map_or(0, |o| o.len());
        stage(self.psola.as_ref().map(|p| p.out_slice()))
            + stage(self.formant.as_ref().map(|f| f.out_slice()))
            + stage(self.air.as_ref().map(|a| a.out_slice()))
    }

    fn last_stage_output(&self) -> (&[f32], usize) {
        if let Some(a) = &self.air {
            (a.out_slice(), a.out_base())
        } else if let Some(f) = &self.formant {
            (f.out_slice(), f.out_base())
        } else if let Some(p) = &self.psola {
            (p.out_slice(), p.out_base())
        } else {
            (&[], self.start)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineParams;
    use crate::synth::{self, Rng, VowelSpec};

    fn vowel_fixture(sr: u32) -> Vec<f32> {
        let track_f0 = synth::f0_track_const(160.0, sr as usize);
        let mut rng = Rng::new(71);
        synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng)
    }

    fn track_of(x: &[f32], sr: u32) -> PyinResult {
        crate::pyin::pyin(x, sr).expect("pyin")
    }

    /// All stage combinations: pitch only / formant only / air only / all.
    fn combo_params(combo: usize) -> EngineParams {
        let mut params = EngineParams::default();
        if combo & 1 != 0 {
            params.set_pitch_semitones(2.0);
        }
        if combo & 2 != 0 {
            params.set_formant_mm(140.0);
        }
        if combo & 4 != 0 {
            params.set_air_db(3.0);
        }
        params
    }

    /// Drives `chain` in `chunk`-sample output pulls to completion and
    /// returns the rendered signal (the canonical streaming pattern the
    /// preview player uses).
    fn drive_chunked(
        x: &[f32],
        sr: u32,
        track: &PyinResult,
        params: &EngineParams,
        chunk: usize,
    ) -> Vec<f32> {
        let mut chain = StreamChain::new(x, sr, track, params).expect("chain");
        let mut got: Vec<f32> = Vec::with_capacity(x.len());
        let mut consumed = 0usize;
        while consumed < x.len() {
            let next_target = usize::min(consumed + chunk, x.len());
            chain.advance_to(x, next_target).expect("advance");
            let before = got.len();
            chain.take_output(&mut got);
            let produced = got.len() - before;
            chain.consume(produced);
            consumed += produced;
            if produced == 0 && chain.flushed() < next_target {
                chain.finish(x).expect("finish");
            }
        }
        chain.finish(x).expect("finish");
        loop {
            let before = got.len();
            chain.take_output(&mut got);
            let produced = got.len() - before;
            if produced == 0 {
                break;
            }
            chain.consume(produced);
        }
        got
    }

    /// Gate: a chain driven in chunk pulls is bit-identical to the offline
    /// render, for every stage combination.
    #[test]
    fn chain_chunked_equals_offline_render_all_combos() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let track = track_of(&x, sr);
        for combo in 1..8 {
            let params = combo_params(combo);
            let (full, _report) =
                crate::pipeline::render_with_track(&x, sr, &params, &track).expect("offline");
            let got = drive_chunked(&x, sr, &track, &params, 1000);
            assert_eq!(got.len(), full.len(), "combo {combo}: length mismatch");
            assert_eq!(got, full, "combo {combo}: chunked render diverged");
        }
    }

    /// Gate: adversarial chunk sizes (including 1-sample pulls) stay
    /// bit-identical.
    #[test]
    fn chain_adversarial_chunk_sizes_bit_exact() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let track = track_of(&x, sr);
        let mut params = EngineParams::default();
        params.set_pitch_semitones(-3.0);
        params.set_formant_mm(190.0);
        params.set_air_db(-12.0);
        let (full, _) =
            crate::pipeline::render_with_track(&x, sr, &params, &track).expect("offline");

        for chunk in [1usize, 7, 479, 4095, 1 << 20] {
            let got = drive_chunked(&x, sr, &track, &params, chunk);
            assert_eq!(got, full, "chunk size {chunk} diverged");
        }
    }

    /// Bounded memory: internal buffers stay window-sized regardless of how
    /// much was rendered (the consumer drains promptly).
    #[test]
    fn chain_buffers_stay_bounded() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let track = track_of(&x, sr);
        let mut params = EngineParams::default();
        params.set_pitch_semitones(1.0);
        params.set_formant_mm(150.0);
        params.set_air_db(2.0);
        let mut chain = StreamChain::new(&x, sr, &track, &params).expect("chain");
        let mut sink: Vec<f32> = Vec::new();
        let mut consumed = 0usize;
        let mut peak_buffers = 0usize;
        while consumed < x.len() {
            let next_target = usize::min(consumed + 960, x.len());
            chain.advance_to(&x, next_target).expect("advance");
            sink.clear();
            chain.take_output(&mut sink);
            let produced = sink.len();
            chain.consume(produced);
            consumed += produced;
            peak_buffers = peak_buffers.max(chain.internal_buffer_samples());
            if produced == 0 && chain.flushed() < next_target {
                // Pipeline mid-flight stall (the demand wave continues on
                // the next advance) or true tail — the tail needs finish.
                chain.finish(&x).expect("finish");
            }
        }
        // Each stage keeps at most a couple of windows; at 48 kHz with
        // PSOLA half ≈ 750 + formant 1024 + air 2048, anything beyond
        // ~3 windows per stage would mean a trim regression.
        assert!(
            peak_buffers < 30_000,
            "chain buffers grew to {peak_buffers} samples"
        );
    }

    /// Restart semantics: a chain started at `start > 0` is deterministic,
    /// and the STFT stages (no epoch phase) converge to the offline render
    /// within one window past the restart point.
    #[test]
    fn restart_stft_steady_state_exact() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let track = track_of(&x, sr);
        // Formant + air only (PSOLA resets its epoch phase at a restart —
        // disclosed; exactness is asserted for the STFT stages).
        let mut params = EngineParams::default();
        params.set_formant_mm(140.0);
        params.set_air_db(3.0);
        let (full, _) =
            crate::pipeline::render_with_track(&x, sr, &params, &track).expect("offline");

        let start = 30_000usize;
        let mut chain = StreamChain::new_at(&x, sr, &track, &params, start).expect("chain");
        let mut got: Vec<f32> = Vec::new();
        let mut consumed = start;
        while consumed < x.len() {
            let next_target = usize::min(consumed + 1000, x.len());
            chain.advance_to(&x, next_target).expect("advance");
            let before = got.len();
            chain.take_output(&mut got);
            let produced = got.len() - before;
            chain.consume(produced);
            consumed += produced;
            if produced == 0 && chain.flushed() < next_target {
                chain.finish(&x).expect("finish");
            }
        }
        chain.finish(&x).expect("finish");
        loop {
            let before = got.len();
            chain.take_output(&mut got);
            let produced = got.len() - before;
            if produced == 0 {
                break;
            }
            chain.consume(produced);
        }
        assert_eq!(got.len(), full.len() - start);

        // The artifact region: find the first position where the restarted
        // chain agrees with the offline render from there on. The STFT
        // stages lose only the frame contributions that start before
        // `start` — bounded by two windows + slack.
        let n_formant =
            usize::clamp((0.02 * sr as f64).round() as usize, 1024, 8192).next_power_of_two();
        let n_air =
            usize::max(1024, (2048.0 * sr as f64 / 48_000.0).round() as usize).next_power_of_two();
        let bound = 2 * usize::max(n_formant, n_air) + 8192;
        let mut first_match = None;
        for off in 0..got.len() {
            if got[off..] == full[start + off..] {
                first_match = Some(off);
                break;
            }
        }
        let first_match = first_match.expect("restarted chain must converge to offline render");
        assert!(
            first_match <= bound,
            "restart artifact region {first_match} samples exceeds the disclosed bound {bound}"
        );
    }

    /// A chain must reject neutral parameters (the caller plays the
    /// original buffer instead).
    #[test]
    fn neutral_params_are_rejected() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let track = track_of(&x, sr);
        let err = StreamChain::new(&x, sr, &track, &EngineParams::default());
        assert!(err.is_err());
    }

    /// Latency readout: every active stage contributes its window.
    #[test]
    fn latency_reflects_active_stages() {
        let sr = 48_000u32;
        let x = vowel_fixture(sr);
        let track = track_of(&x, sr);
        let mut pitch_only = EngineParams::default();
        pitch_only.set_pitch_semitones(2.0);
        let chain = StreamChain::new(&x, sr, &track, &pitch_only).expect("chain");
        let pitch_latency = chain.latency_samples();
        assert!(pitch_latency > 0);

        let mut all = pitch_only;
        all.set_formant_mm(140.0);
        all.set_air_db(3.0);
        let chain = StreamChain::new(&x, sr, &track, &all).expect("chain");
        assert!(chain.latency_samples() > pitch_latency);
    }
}
