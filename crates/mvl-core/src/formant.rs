//! Formant (vocal-tract length) engine — D9.
//!
//! **Implementation note (Phase 3, amends D9).** D9 sketched LPC with an
//! allpass Bark warp. Two amendments, both verified against the fixtures:
//!
//! 1. **Exact envelope resampling instead of the allpass approximation.**
//!    The allpass map's effective ratio varies per formant (up to ~30 % at
//!    F4), violating the contractual mapping `F_new ≈ F_old · 175/L` that
//!    `EngineParams::formant_scale` exposes. The warp is the exact
//!    frequency-axis resampling of the envelope.
//! 2. **Cepstral envelope estimation instead of LPC.** The per-frame
//!    spectral envelope is measured by quefrency liftering (comb removed,
//!    features ≥ 300 Hz kept). LPC's least-squares fit proved fragile on
//!    strongly periodic synthetic excitations — its poles follow the
//!    spectral tilt rather than the narrow formant bumps, which starves
//!    the warp of formant evidence. Cepstral lifting is the robust
//!    standard for envelope estimation and needs no excitation-model
//!    assumptions. `lpc.rs` remains for diagnostics.
//!
//! Pipeline per Hann-STFT frame: log-spectrum → cepstrum → lifter →
//! smooth envelope `env(f)` (log domain) → warped envelope
//! `env(f/r)` with `r = 175/target_mm` → corrective ratio
//! `R(f) = exp(env(f/r) − env(f))` → smooth, ±18 dB limit, band-edge
//! taper → `Y = X·R` (loudness-compensated) → weighted overlap-add.
//! Voiced-only processing: unvoiced frames pass through bit-exactly, so
//! sibilance never lisps (D9 guardrail). Pitch is untouched: the ratio is
//! a fixed linear filter per frame and the excitation periodicity is
//! preserved (verified by the pYIN gate below).
//!
//! Phase 6 streaming: the frame loop lives in [`FormantStage`], a
//! chunk-driven driver; [`shift_formants`] is a thin wrapper. Frames are
//! processed strictly in order and each flush is value-free w.r.t. the
//! arithmetic, so chunked driving is bit-identical to the single-pass
//! render (gate-tested in `crate::stream`).

use crate::error::{CoreError, Result};
use crate::ola::{CarryOla, EnvStream};
use crate::pyin::PyinResult;
use rustfft::num_complex::Complex;
use std::sync::Arc;

/// Reference adult vocal-tract length (mm) — mirrors `engine::REFERENCE_VTL_MM`.
const REFERENCE_VTL_MM: f64 = 175.0;
/// Hard guardrails for the raw mm argument (the UI clamp is ±[130, 190]).
const MM_HARD_LIMITS: (f64, f64) = (100.0, 250.0);
/// Envelope-ratio limiter (±18 dB).
const RATIO_LIMIT: f64 = 7.943; // 10^(18/20)
/// Analysis band edge (Hz); envelope is estimated and warped below it.
const BAND_LIMIT_HZ: f64 = 8000.0;
/// Envelope feature floor (Hz) — sets the cepstral lifter length.
const ENV_FEATURE_HZ: f64 = 300.0;
/// Overlap-add window-sum floor.
const WSUM_FLOOR: f32 = 1e-6;

/// Rescales the vocal tract to `target_mm` (formants scale by
/// `175/target_mm`), driven by the pYIN voicing track.
///
/// # Errors
/// Fails when `target_mm` is outside the hard limits or on FFT errors.
pub fn shift_formants(
    x: &[f32],
    sample_rate: u32,
    track: &PyinResult,
    target_mm: f64,
) -> Result<Vec<f32>> {
    if !(MM_HARD_LIMITS.0..=MM_HARD_LIMITS.1).contains(&target_mm) {
        return Err(CoreError::Analysis(format!(
            "vocal-tract length {target_mm} mm outside {}..{} mm",
            MM_HARD_LIMITS.0, MM_HARD_LIMITS.1
        )));
    }
    let ratio = REFERENCE_VTL_MM / target_mm;
    if (ratio - 1.0).abs() < 1e-9 || x.is_empty() {
        return Ok(x.to_vec());
    }

    let mut stage = FormantStage::new(x.len(), sample_rate, track, ratio, 0);
    stage.advance_to(x, 0, usize::MAX)?;
    stage.finish(x, 0);
    match stage.take_error() {
        Some(e) => Err(e),
        None => Ok(stage.into_output()),
    }
}

/// Chunk-driven formant stage (Phase 6). Processes the STFT frame grid in
/// order; [`FormantStage::advance_to`] runs frames until the flushed
/// frontier reaches `target` **and** the input window covers the frame
/// (mid-stream the caller feeds the previous stage's output as it is
/// produced — the gate breaks instead of reading not-yet-rendered input as
/// silence). Input is a window `[x_base, x_base + x.len())` of absolute
/// positions, so the caller can front-trim its hand-off buffer.
///
/// `start > 0` (chain restart) begins the frame grid at the first frame
/// whose window starts at or after `start`; output positions below that
/// frame's coverage miss their left-hand frame contributions — part of the
/// disclosed restart artifact region.
pub(crate) struct FormantStage<'a> {
    len: usize,
    n: usize,
    hop: usize,
    ratio: f64,
    band_bins: usize,
    lifter: usize,
    r2c: Arc<dyn realfft::RealToComplex<f32>>,
    c2r: Arc<dyn realfft::ComplexToReal<f32>>,
    window: Vec<f32>,
    ola: CarryOla,
    env: EnvStream<'a>,
    frame: usize,
    frames: usize,
    // per-frame scratch (reused, exactly as the historical locals)
    spectrum: Vec<Complex<f32>>,
    in_buf: Vec<f32>,
    y_out: Vec<f32>,
    cep_in: Vec<Complex<f32>>,
    cep: Vec<f32>,
    env_spec: Vec<f32>,
    env_bins: Vec<Complex<f32>>,
    env_ln: Vec<f64>,
    env_w: Vec<f64>,
    out: Vec<f32>,
    /// Absolute position of `out[0]` (front-trimmed for streaming).
    out_base: usize,
    err: Option<CoreError>,
    done: bool,
}

impl<'a> FormantStage<'a> {
    /// `target_mm` must already be validated (hard limits) and `ratio ≠ 1`.
    pub(crate) fn new(
        len: usize,
        sample_rate: u32,
        track: &'a PyinResult,
        ratio: f64,
        start: usize,
    ) -> Self {
        let n = usize::clamp((0.02 * sample_rate as f64).round() as usize, 1024, 8192)
            .next_power_of_two();
        let hop = n / 2;
        let bin_hz = sample_rate as f64 / n as f64;
        let band_bins = usize::min(n / 2, (BAND_LIMIT_HZ / bin_hz) as usize);
        let lifter = usize::max(8, (sample_rate as f64 / ENV_FEATURE_HZ) as usize);

        let mut planner = realfft::RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(n);
        let c2r = planner.plan_fft_inverse(n);

        let window: Vec<f32> = (0..n)
            .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
            .collect();

        // Scratch buffers, sized once (order matters: create before the
        // plans move into the struct).
        let spectrum = r2c.make_output_vec();
        let in_buf = r2c.make_input_vec();
        let y_out = c2r.make_output_vec();
        let cep_in = c2r.make_input_vec();
        let cep = c2r.make_output_vec();
        let env_spec = r2c.make_input_vec();
        let env_bins = r2c.make_output_vec();

        // Carry-based weighted OLA: frame t writes [t·hop, t·hop + n), so a
        // fixed ring of span n replaces the two full-length buffers; finished
        // samples flush to the output before each frame (bit-identical adds —
        // see `ola`).
        let ola = CarryOla::with_start(len, n, start);
        let mut env = EnvStream::new(track, sample_rate);
        env.reset_to(start);
        let frames = (len - 1) / hop + 1;
        let frame = if start > 0 {
            usize::min(start.div_ceil(hop), frames)
        } else {
            0
        };
        Self {
            len,
            n,
            hop,
            ratio,
            band_bins,
            lifter,
            r2c,
            c2r,
            window,
            ola,
            env,
            frame,
            frames,
            spectrum,
            in_buf,
            y_out,
            cep_in,
            cep,
            env_spec,
            env_bins,
            env_ln: vec![0.0f64; band_bins + 2],
            env_w: vec![0.0f64; band_bins + 2],
            out: Vec::with_capacity(len.saturating_sub(start)),
            out_base: start,
            err: None,
            done: false,
        }
    }

    /// Absolute output frontier.
    pub(crate) fn flushed(&self) -> usize {
        self.ola.head()
    }

    pub(crate) fn out_slice(&self) -> &[f32] {
        &self.out
    }

    pub(crate) fn out_base(&self) -> usize {
        self.out_base
    }

    /// Drops the output prefix below `keep_from` (the consumer's frontier).
    pub(crate) fn trim_to(&mut self, keep_from: usize) {
        let keep_from = keep_from.clamp(self.out_base, self.out.len() + self.out_base);
        let k = keep_from - self.out_base;
        if k > 0 {
            self.out.drain(..k);
            self.out_base = keep_from;
        }
    }

    /// What the *next* unprocessed frame needs from the input (absolute
    /// end position, exclusive), or `None` when the frame grid is done.
    pub(crate) fn next_need(&self) -> Option<usize> {
        if self.frame >= self.frames {
            return None;
        }
        let start = self.frame * self.hop;
        let take = usize::min(self.n, self.len.saturating_sub(start));
        Some(start + take)
    }

    /// STFT window size in samples (the stage's input-lookahead bound).
    pub(crate) fn window_samples(&self) -> usize {
        self.n
    }

    /// Processes frames until the flushed frontier reaches `target`, the
    /// frame grid is exhausted, or the input window does not yet cover the
    /// next frame. `x` covers absolute positions `x_base..x_base + x.len()`.
    /// `target = usize::MAX` requires the *complete* input up front (the
    /// offline path).
    pub(crate) fn advance_to(&mut self, x: &[f32], x_base: usize, target: usize) -> Result<()> {
        if self.done {
            return Ok(());
        }
        while self.frame < self.frames {
            if self.flushed() >= target {
                break;
            }
            let start = self.frame * self.hop;
            let take = usize::min(self.n, self.len.saturating_sub(start));
            // Input readiness gate: never read not-yet-produced input as
            // silence (the offline path passes the complete buffer, so the
            // gate never fires there and the sequence is identical).
            if x_base + x.len() < start + take {
                break;
            }
            // Everything before this frame's write window is complete: the
            // previous frame wrote up to (frame−1)·hop + n, which is covered.
            self.ola.flush_to(
                start,
                x,
                x_base,
                Some(&mut self.env),
                WSUM_FLOOR,
                &mut self.out,
            );
            let visible = &x[(start - x_base)..(start - x_base + take)];
            if let Err(e) = self.process_frame(start, take, visible) {
                self.err = Some(e);
                self.done = true;
                break;
            }
            self.frame += 1;
        }
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        Ok(())
    }

    /// One STFT frame — moved verbatim from the historical single-pass
    /// loop (`x` → the frame's visible window).
    fn process_frame(&mut self, start: usize, take: usize, x: &[f32]) -> Result<()> {
        let n = self.n;
        self.in_buf[..take].fill(0.0);
        for (dst, (&src, &w)) in self.in_buf[..take]
            .iter_mut()
            .zip(x.iter().zip(&self.window))
        {
            *dst = src * w;
        }
        self.r2c
            .process(&mut self.in_buf, &mut self.spectrum)
            .map_err(|e| CoreError::Fft(e.to_string()))?;

        // --- cepstral log-envelope of this frame ------------------------
        // IFFT of the even log-spectrum → real cepstrum → lifter → FFT
        // back. Quefrencies above `sr/ENV_FEATURE_HZ` (the harmonic comb
        // and finer ripple) are removed; what remains is the spectral
        // envelope in log magnitude.
        let band_bins = self.band_bins;
        self.cep_in[..=band_bins].copy_from_slice(&self.spectrum[..=band_bins]);
        for c in self.cep_in[..=band_bins].iter_mut() {
            *c = Complex::new((c.norm() + 1e-12).ln(), 0.0);
        }
        for c in self.cep_in[band_bins + 1..].iter_mut() {
            *c = Complex::new(0.0, 0.0);
        }
        self.c2r
            .process(&mut self.cep_in, &mut self.cep)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        let cep_len = self.cep.len();
        for (q, v) in self.cep.iter_mut().enumerate() {
            if q > self.lifter && q + 1 < cep_len - self.lifter {
                *v = 0.0;
            }
        }
        for (dst, &v) in self.env_spec.iter_mut().zip(&self.cep) {
            *dst = v / n as f32;
        }
        self.r2c
            .process(&mut self.env_spec, &mut self.env_bins)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        for (k, slot) in self.env_ln.iter_mut().enumerate().take(band_bins + 2) {
            let k_c = usize::min(k, band_bins);
            *slot = f64::from(self.env_bins[k_c].re);
        }

        // --- warped envelope and corrective ratio (log domain) ----------
        for (k, slot) in self.env_w.iter_mut().enumerate().take(band_bins + 2) {
            let src = k as f64 / self.ratio;
            let i0 = usize::min(src.floor() as usize, band_bins);
            let frac = (src - i0 as f64).clamp(0.0, 1.0);
            *slot = self.env_ln[i0] * (1.0 - frac) + self.env_ln[i0 + 1] * frac;
        }
        let taper_start = (band_bins as f64 * 0.85) as usize;
        let mut applied = vec![1.0f64; band_bins + 2];
        for (k, slot) in applied.iter_mut().enumerate().take(band_bins + 2) {
            *slot = self.env_w[k] - self.env_ln[k]; // log-domain ratio
        }
        smooth5(&mut applied, band_bins + 1);
        let limit = RATIO_LIMIT.ln();
        for (k, slot) in applied.iter_mut().enumerate().take(band_bins + 2) {
            let mut rk = slot.clamp(-limit, limit);
            if k > taper_start {
                let t = (k - taper_start) as f64 / (band_bins + 1 - taper_start) as f64;
                let fade = 1.0 - t * t * (3.0 - 2.0 * t);
                rk *= fade;
            }
            *slot = rk.exp();
        }

        // --- loudness compensation, apply, overlap-add ------------------
        let mut e_in = 0.0f64;
        let mut e_out = 0.0f64;
        for (k, spec) in self.spectrum.iter().enumerate() {
            let rk = if k <= band_bins + 1 { applied[k] } else { 1.0 };
            e_in += f64::from(spec.norm_sqr());
            e_out += f64::from(spec.norm_sqr()) * rk * rk;
        }
        let gain = if e_out > 1e-12 {
            (e_in / e_out).sqrt().clamp(0.5, 2.0)
        } else {
            1.0
        };

        for (k, spec) in self.spectrum.iter_mut().enumerate() {
            let rk = if k <= band_bins + 1 { applied[k] } else { 1.0 };
            *spec *= (rk * gain) as f32;
        }
        self.c2r
            .process(&mut self.spectrum, &mut self.y_out)
            .map_err(|e| CoreError::Fft(e.to_string()))?;

        let inv_n = 1.0 / n as f32;
        for i in 0..take {
            let w = self.window[i];
            self.ola.add(start + i, self.y_out[i] * inv_n * w, w * w);
        }
        Ok(())
    }

    /// Weighted-OLA normalize, then voicing crossfade (exact bypass where
    /// the envelope is zero — sibilance and breath never warp). Requires
    /// the input window to cover up to `len`.
    pub(crate) fn finish(&mut self, x: &[f32], x_base: usize) {
        self.done = true;
        self.ola
            .flush_all(x, x_base, Some(&mut self.env), WSUM_FLOOR, &mut self.out);
    }

    pub(crate) fn take_error(&mut self) -> Option<CoreError> {
        self.err.take()
    }

    /// Consumes the driver after a full error-free run.
    pub(crate) fn into_output(self) -> Vec<f32> {
        debug_assert_eq!(self.flushed(), self.len, "full run must flush everything");
        self.out
    }
}

/// In-place 5-tap moving average over `0..=limit` of `r`.
pub(crate) fn smooth5(r: &mut [f64], limit: usize) {
    if limit < 2 {
        return;
    }
    let mut scratch = vec![0.0f64; limit + 1];
    for (k, slot) in scratch.iter_mut().enumerate().take(limit + 1) {
        let lo = k.saturating_sub(2);
        let hi = usize::min(k + 2, limit);
        let n = (hi - lo + 1) as f64;
        *slot = r[lo..=hi].iter().sum::<f64>() / n;
    }
    r[..=limit].copy_from_slice(&scratch);
}

#[cfg(test)]
mod tests;
