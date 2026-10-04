//! pYIN fundamental-frequency tracker (Mauch & Dixon, ICASSP 2014) — D7.
//!
//! Implementation notes (kept next to the code because this is the core IP
//! of the product):
//!
//! 1. **Difference function via FFT** — the YIN difference
//!    `d(τ) = Σ_{i=0..W-1} (x[i] − x[i+τ])²` is computed as
//!    `E0 + S(τ) − 2·ACF(τ)` where the cross-correlation of the fixed first
//!    window against the doubled buffer runs through realfft (3 transforms
//!    per frame, no `O(W²)` lags).
//! 2. **Cumulative-mean-normalized difference** `d'(τ)` exactly as in YIN.
//! 3. **Probabilistic thresholds** — 100 absolute thresholds on `d'` carry a
//!    Beta(2, 10) prior; the first local minimum below each threshold
//!    becomes a voiced candidate weighted by that threshold's prior mass.
//!    This yields a per-frame distribution over pitch *and* a voicing
//!    probability (no candidate below a threshold ⇒ that mass is unvoiced).
//! 4. **Viterbi smoothing** over 10-cent pitch bins plus one unvoiced
//!    state, with a note-continuation kernel (±32 bins), a small uniform
//!    jump mass for onsets, and voiced/unvoiced switching costs.
//!
//! Estimates are **center-aligned**: the value for frame `i` describes time
//! `(start + W) / sr`, the middle of the analysis buffer — this removes the
//! group delay that would otherwise smear vibrato/sweep accuracy gates.

use rustfft::num_complex::Complex;
use rustfft::num_traits::Zero;

use crate::error::{CoreError, Result};

/// Absolute thresholds on the normalized difference function.
const N_THRESHOLDS: usize = 100;
const THRESHOLD_MIN: f64 = 0.05;
const THRESHOLD_MAX: f64 = 0.95;

/// Pitch-bin grid: 10-cent bins from `FREF_HZ` up to `fmax`.
const BIN_CENTS: f64 = 10.0;
const FREF_HZ: f64 = 50.0;

/// Viterbi transition tuning (fractions of transition mass).
const P_STAY_VOICED: f64 = 0.985; // voiced→voiced (kernel + uniform escape)
const P_UNIFORM_JUMP: f64 = 0.010; // voiced→voiced, arbitrary bin (onsets)
const P_MOVE: f64 = 0.12; // share of kernel mass spread over |Δbin| > 0
const P_V_TO_UV: f64 = 0.002;
const P_UV_TO_V: f64 = 0.05;
const P_UV_STAY: f64 = 0.90;
/// Note-continuation kernel half-width in bins (±320 cents covers vibrato
/// slew of ±50 cents at 7 Hz with a 10 ms hop by a wide margin).
const KERNEL_HALF: i32 = 32;

/// Lower/upper F0 search bounds (Hz). Voice: baritone fundamentals to
/// soprano falsetto headroom.
pub const FMIN_HZ: f64 = 50.0;
pub const FMAX_HZ: f64 = 1100.0;

/// Per-frame pitch track produced by [`pyin`].
#[derive(Debug, Clone)]
pub struct PyinResult {
    /// Sample rate of the analysed signal.
    pub sample_rate: u32,
    /// Hop between frames in samples (10 ms nominal).
    pub hop: usize,
    /// Window length `W` in samples (analysis buffer is `2·W`).
    pub window: usize,
    /// F0 per frame in Hz; `0.0` where the Viterbi path chose unvoiced.
    pub f0: Vec<f32>,
    /// Raw voicing probability per frame (pre-Viterbi candidate mass).
    pub voiced_prob: Vec<f32>,
    /// Viterbi voiced flag per frame.
    pub voiced: Vec<bool>,
}

impl PyinResult {
    /// Time in seconds of frame `i`.
    ///
    /// Center-aligned on the *Hann analysis window*, which spans the first
    /// half of the doubled buffer: the estimate for frame `i` describes
    /// time `(start + W/2) / sr`. (The second half of the buffer exists
    /// only so lags up to `W` stay inside the frame data.)
    pub fn frame_time(&self, i: usize) -> f64 {
        (i * self.hop + self.window / 2) as f64 / f64::from(self.sample_rate)
    }

    /// Number of frames.
    pub fn len(&self) -> usize {
        self.f0.len()
    }

    /// True when the track has no frames.
    pub fn is_empty(&self) -> bool {
        self.f0.is_empty()
    }

    /// Voiced fraction of frames.
    pub fn voiced_ratio(&self) -> f32 {
        if self.f0.is_empty() {
            return 0.0;
        }
        self.voiced.iter().filter(|&&v| v).count() as f32 / self.f0.len() as f32
    }

    /// Median F0 over voiced frames (Hz), or 0.0 if nothing is voiced.
    pub fn median_f0(&self) -> f32 {
        let mut v: Vec<f32> = self
            .f0
            .iter()
            .zip(&self.voiced)
            .filter(|&(_, &voiced)| voiced)
            .map(|(&f, _)| f)
            .collect();
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    }
}

/// Runs pYIN on a mono signal.
///
/// # Errors
/// Fails on empty input or if the FFT backend reports an error.
pub fn pyin(x: &[f32], sample_rate: u32) -> Result<PyinResult> {
    if x.is_empty() {
        return Err(CoreError::Analysis("empty signal".into()));
    }
    let sr = f64::from(sample_rate);

    // Geometry: window W (pow2) must hold two periods of FMIN; the analysis
    // buffer doubles it; FFT size equals the buffer length.
    let min_window = (2.0 * sr / FMIN_HZ).ceil() as usize;
    let w = usize::max(2048, min_window).next_power_of_two();
    let tau_min = usize::max(2, (sr / FMAX_HZ).floor() as usize);
    let tau_max = usize::min(w - 2, (sr / FMIN_HZ).ceil() as usize);
    let hop = usize::clamp(sample_rate as usize / 100, 128, 4096);

    let n_bins = (1200.0 * (FMAX_HZ / FREF_HZ).log2() / BIN_CENTS).ceil() as usize;
    let thresholds = threshold_grid();
    let prior = beta_prior(&thresholds, 2.0, 10.0);

    // FFT plans for the Hann-weighted difference function (three forward
    // transforms per frame: full buffer, windowed first half, squared
    // buffer; plus two inverse transforms). Hann weighting suppresses the
    // truncated-period wobble of rectangular sums (`sin(Wω)/sin(ω)`
    // amplification) that otherwise biases low-F0 period estimates by
    // >10 cents; its sidelobes sit ≈31 dB below the main lobe.
    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(2 * w);
    let c2r = planner.plan_fft_inverse(2 * w);

    // Hann window over the first half (analysis window), cached spectrum.
    let hann: Vec<f32> = (0..w)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / w as f64).cos() as f32)
        .collect();
    let mut fft_w_in = r2c.make_input_vec();
    for (dst, &h) in fft_w_in.iter_mut().zip(&hann) {
        *dst = h;
    }
    let mut fft_w = r2c.make_output_vec();
    r2c.process(&mut fft_w_in, &mut fft_w)
        .map_err(|e| CoreError::Fft(e.to_string()))?;

    let mut buf = vec![0.0f32; 2 * w];
    let mut spectrum_b = r2c.make_output_vec();
    let mut spectrum_u = r2c.make_output_vec();
    let mut spectrum_x2 = r2c.make_output_vec();
    let mut corr = r2c.make_output_vec();
    let mut corr_a = c2r.make_output_vec();
    let mut corr_b = c2r.make_output_vec();
    let mut in_b = r2c.make_input_vec();
    let mut in_u = r2c.make_input_vec();
    let mut in_x2 = r2c.make_input_vec();

    // Prefix sums are replaced by the windowed energy terms below.
    // Difference function + CMND scratch.
    let mut dprime = vec![0.0f64; w];
    let mut minima: Vec<(usize, f64)> = Vec::with_capacity(64); // (τ, d')
    let mut bin_weight = vec![0.0f64; n_bins];

    let n_frames = (x.len() - 1) / hop + 1;
    let mut result = PyinResult {
        sample_rate,
        hop,
        window: w,
        f0: Vec::with_capacity(n_frames),
        voiced_prob: Vec::with_capacity(n_frames),
        voiced: Vec::with_capacity(n_frames),
    };
    let mut obs_v: Vec<Vec<f64>> = Vec::with_capacity(n_frames);
    let mut obs_uv: Vec<f64> = Vec::with_capacity(n_frames);
    // Sub-bin period estimates: per frame, per bin, the parabolic-refined
    // τ of the deepest candidate that landed in that bin.
    let mut bin_tau = vec![0.0f64; n_bins];
    let mut obs_tau: Vec<Vec<f64>> = Vec::with_capacity(n_frames);

    for frame in 0..n_frames {
        let start = frame * hop;
        let take = usize::min(2 * w, x.len().saturating_sub(start));
        buf[..take].copy_from_slice(&x[start..start + take]);
        buf[take..].fill(0.0);

        // Remove mean (DC invariance).
        let mean = buf.iter().sum::<f32>() / buf.len() as f32;
        for v in &mut buf {
            *v -= mean;
        }

        // --- Hann-weighted difference function ------------------------
        // d(τ) = Σ_{i<W} w[i]·(x[i] − x[i+τ])²
        //      = E0 + B(τ) − 2·A(τ), all terms FFT-accelerated.
        for (i, v) in buf.iter().enumerate() {
            in_x2[i] = *v * *v;
        }
        // realfft uses input buffers as scratch space, so every element is
        // rewritten each frame (the tail beyond the Hann window must be
        // re-zeroed explicitly, not just left from the previous frame).
        in_u[..w].copy_from_slice(&buf[..w]);
        for (dst, &h) in in_u[..w].iter_mut().zip(&hann) {
            *dst *= h;
        }
        in_u[w..].fill(0.0);
        in_b.copy_from_slice(&buf);
        r2c.process(&mut in_b, &mut spectrum_b)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        r2c.process(&mut in_u, &mut spectrum_u)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        r2c.process(&mut in_x2, &mut spectrum_x2)
            .map_err(|e| CoreError::Fft(e.to_string()))?;

        // A(τ) = Σ w[i]·x[i]·x[i+τ] = IFFT(FFT(x)·conj(FFT(w·x)))[τ]
        for (c, (b, u)) in corr.iter_mut().zip(spectrum_b.iter().zip(&spectrum_u)) {
            *c = *b * u.conj();
        }
        // f32 rounding leaves stray imaginary parts at DC/Nyquist; the
        // inverse real FFT requires exactly Hermitian input.
        corr[0].im = 0.0;
        corr[w].im = 0.0;
        c2r.process(&mut corr, &mut corr_a)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        // B(τ) = Σ w[i]·x[i+τ]²      = IFFT(FFT(x²)·conj(FFT(w)))[τ]
        for (c, (x2, wf)) in corr.iter_mut().zip(spectrum_x2.iter().zip(&fft_w)) {
            *c = *x2 * wf.conj();
        }
        corr[0].im = 0.0;
        corr[w].im = 0.0;
        c2r.process(&mut corr, &mut corr_b)
            .map_err(|e| CoreError::Fft(e.to_string()))?;
        let inv_n = 1.0 / (2 * w) as f64;
        let e0: f64 = hann
            .iter()
            .zip(&buf)
            .map(|(&h, &v)| f64::from(h) * f64::from(v) * f64::from(v))
            .sum();

        let mut running = 0.0f64;
        dprime[0] = 1.0;
        for tau in 1..w {
            let d = e0 + f64::from(corr_b[tau]) * inv_n - 2.0 * f64::from(corr_a[tau]) * inv_n;
            running += d;
            dprime[tau] = if running > 1e-12 {
                d * tau as f64 / running
            } else {
                1.0
            };
        }

        // --- candidates ------------------------------------------------
        minima.clear();
        for tau in tau_min..tau_max {
            if dprime[tau] < dprime[tau - 1] && dprime[tau] <= dprime[tau + 1] {
                minima.push((tau, dprime[tau]));
            }
        }

        bin_weight.fill(0.0);
        bin_tau.fill(0.0);
        let mut p_voiced = 0.0f64;
        for (t, &prior_w) in thresholds.iter().zip(&prior) {
            // First local minimum (ascending τ) below this threshold.
            let mut found: Option<usize> = None;
            for &(tau, dp) in &minima {
                if dp < *t {
                    found = Some(tau);
                    break;
                }
            }
            if let Some(tau) = found {
                let tau_star = parabolic(&dprime, tau);
                let f = sr / tau_star;
                let bin = f_to_bin(f, n_bins);
                if bin_weight[bin] == 0.0 {
                    bin_tau[bin] = tau_star;
                }
                bin_weight[bin] += prior_w;
                p_voiced += prior_w;
            }
        }

        let mut ob = vec![0.0f64; n_bins];
        ob.copy_from_slice(&bin_weight);
        obs_v.push(ob);
        let mut ot = vec![0.0f64; n_bins];
        ot.copy_from_slice(&bin_tau);
        obs_tau.push(ot);
        obs_uv.push((1.0 - p_voiced).clamp(1e-10, 1.0));

        if frame == 20 && std::env::var("MVL_PYIN_DEBUG").is_ok() {
            eprintln!(
                "[dbg] w={w} tau=[{tau_min},{tau_max}] n_bins={n_bins} minima={} p_voiced={p_voiced:.4}",
                minima.len()
            );
            for (tau, dp) in minima.iter().take(12) {
                eprintln!("[dbg]   min tau={tau} d'={dp:.4}");
            }
            let lo = usize::saturating_sub(10, 1);
            eprintln!(
                "[dbg]   d'[205..230] = {:?}",
                &dprime[205usize.max(lo)..230.min(w)]
            );
            let best = bin_weight
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal));
            eprintln!(
                "[dbg]   best bin = {best:?} -> f0={:?}",
                best.map(|(b, _)| bin_to_f0(b))
            );
        }
    }

    // --- Viterbi over (n_bins voiced states + 1 unvoiced state) ---------
    let (path, uv_path) = viterbi(&obs_v, &obs_uv, n_bins);

    for t in 0..n_frames {
        let voiced = !uv_path[t];
        let f0 = if voiced {
            // Sub-bin precision: prefer the parabolic-refined period of the
            // winning bin over the 10-cent bin center.
            let tau_star = obs_tau[t][path[t]];
            if tau_star > 0.0 {
                (sr / tau_star) as f32
            } else {
                bin_to_f0(path[t])
            }
        } else {
            0.0
        };
        result.f0.push(f0);
        let pv = obs_v[t].iter().sum::<f64>();
        result.voiced_prob.push(pv as f32);
        result.voiced.push(voiced);
    }
    Ok(result)
}

fn threshold_grid() -> Vec<f64> {
    (0..N_THRESHOLDS)
        .map(|i| {
            THRESHOLD_MIN + (THRESHOLD_MAX - THRESHOLD_MIN) * i as f64 / (N_THRESHOLDS - 1) as f64
        })
        .collect()
}

/// Beta(a, b) density evaluated on the grid, normalized to sum 1.
fn beta_prior(grid: &[f64], a: f64, b: f64) -> Vec<f64> {
    let pdf: Vec<f64> = grid
        .iter()
        .map(|&t| t.powf(a - 1.0) * (1.0 - t).powf(b - 1.0))
        .collect();
    let sum: f64 = pdf.iter().sum();
    pdf.into_iter().map(|v| v / sum).collect()
}

/// Parabolic refinement of a local minimum: returns the interpolated
/// (fractional) lag.
fn parabolic(d: &[f64], tau: usize) -> f64 {
    if tau == 0 || tau + 1 >= d.len() {
        return tau as f64;
    }
    let (a, b, c) = (d[tau - 1], d[tau], d[tau + 1]);
    let denom = a - 2.0 * b + c;
    if denom.abs() < 1e-12 {
        return tau as f64;
    }
    let delta = 0.5 * (a - c) / denom;
    tau as f64 + delta.clamp(-1.0, 1.0)
}

fn f_to_bin(f: f64, n_bins: usize) -> usize {
    let cents = 1200.0 * (f / FREF_HZ).log2();
    let bin = (cents / BIN_CENTS).round() as i64;
    usize::try_from(bin.clamp(0, n_bins as i64 - 1)).unwrap_or(0)
}

fn bin_to_f0(bin: usize) -> f32 {
    (FREF_HZ * 2.0f64.powf(bin as f64 * BIN_CENTS / 1200.0)) as f32
}

/// Max-product (Viterbi) decoding. Returns `(bin_path, uv_flag)`.
///
/// Scores are initialized from the *observations* of frame 0 (not from a
/// large negative constant): in `f64`, a −1e18 offset has an ULP of 128,
/// which would swamp every log-probability difference (|ln p| ≤ 23) and
/// collapse all states into ties.
fn viterbi(obs_v: &[Vec<f64>], obs_uv: &[f64], n_bins: usize) -> (Vec<usize>, Vec<bool>) {
    let n_frames = obs_v.len();
    if n_frames == 0 {
        return (Vec::new(), Vec::new());
    }
    let floor = 1e-10f64;
    let neg_inf = f64::NEG_INFINITY;

    // Note-continuation kernel: strong stay-probability at Δ=0, the rest
    // of `P_MOVE` spread over a Laplacian decay (covers vibrato slew and
    // moderate glissando without taxing straight notes).
    let mut kernel = [0.0f64; 2 * KERNEL_HALF as usize + 1];
    let mut lap_sum = 0.0;
    for (i, k) in kernel.iter_mut().enumerate() {
        let d = (i as i32 - KERNEL_HALF).abs() as f64;
        *k = (-d / 3.0).exp();
        lap_sum += *k;
    }
    for (i, k) in kernel.iter_mut().enumerate() {
        let d = (i as i32 - KERNEL_HALF) as f64;
        *k = if d == 0.0 {
            (1.0 - P_MOVE) + P_MOVE * *k / lap_sum
        } else {
            P_MOVE * *k / lap_sum
        };
    }
    let log_kernel: Vec<f64> = kernel.iter().map(|&k| (P_STAY_VOICED * k).ln()).collect();
    let log_uniform = (P_UNIFORM_JUMP / n_bins as f64).ln();

    let mut score_v = vec![0.0f64; n_bins];
    for (b, s) in score_v.iter_mut().enumerate() {
        *s = obs_v[0][b].max(floor).ln();
    }
    let mut score_uv = obs_uv[0].max(floor).ln();
    let mut next_v = vec![0.0f64; n_bins];

    // Backpointers: per frame, per voiced bin → predecessor bin, or the
    // `n_bins` sentinel meaning "came from unvoiced"; uv → 0 = stayed
    // unvoiced, else best voiced bin + 1.
    let mut bp_v: Vec<Vec<u32>> = Vec::with_capacity(n_frames);
    let mut bp_uv: Vec<u32> = Vec::with_capacity(n_frames);
    bp_v.push(vec![n_bins as u32; n_bins]); // frame 0 has no predecessor
    bp_uv.push(0);

    for t in 1..n_frames {
        let mut best_bin = 0usize;
        let mut best_v = neg_inf;
        for (b, &s) in score_v.iter().enumerate() {
            if s > best_v {
                best_v = s;
                best_bin = b;
            }
        }

        let mut bp = vec![0u32; n_bins];
        for b in 0..n_bins {
            let mut best = neg_inf;
            let mut best_src = 0usize;
            for (ki, &lk) in log_kernel.iter().enumerate() {
                let src = b as i32 - (ki as i32 - KERNEL_HALF);
                if src < 0 || src >= n_bins as i32 {
                    continue;
                }
                let s = score_v[src as usize] + lk;
                if s > best {
                    best = s;
                    best_src = src as usize;
                }
            }
            let via_uniform = best_v + log_uniform;
            if via_uniform > best {
                best = via_uniform;
                best_src = best_bin;
            }
            let via_uv = score_uv + P_UV_TO_V.ln();
            if via_uv > best {
                best = via_uv;
                best_src = n_bins; // sentinel: from unvoiced
            }
            next_v[b] = best + obs_v[t][b].max(floor).ln();
            bp[b] = best_src as u32;
        }
        let uv_stay = score_uv + P_UV_STAY.ln();
        let uv_from_v = best_v + P_V_TO_UV.ln();
        score_uv = uv_stay.max(uv_from_v) + obs_uv[t].max(floor).ln();
        bp_uv.push(if uv_stay >= uv_from_v {
            0
        } else {
            best_bin as u32 + 1
        });

        std::mem::swap(&mut score_v, &mut next_v);
        bp_v.push(bp);
    }

    // Backtrack from the globally best final state.
    let mut path = vec![0usize; n_frames];
    let mut uv_path = vec![false; n_frames];
    let mut best_v = neg_inf;
    let mut best_bin = 0usize;
    for (b, &s) in score_v.iter().enumerate() {
        if s > best_v {
            best_v = s;
            best_bin = b;
        }
    }
    let mut cur_voiced = best_v >= score_uv;
    let mut cur_bin = best_bin;
    for t in (0..n_frames).rev() {
        if cur_voiced {
            path[t] = cur_bin;
            uv_path[t] = false;
            if t == 0 {
                break;
            }
            let src = bp_v[t][cur_bin] as usize;
            if src == n_bins {
                cur_voiced = false;
            } else {
                cur_bin = src;
            }
        } else {
            uv_path[t] = true; // path[t] value is ignored for unvoiced frames
            if t == 0 {
                break;
            }
            let src = bp_uv[t] as usize;
            if src > 0 {
                cur_voiced = true;
                cur_bin = src - 1;
            }
        }
    }
    (path, uv_path)
}

/// Convenience: cents error between two frequencies.
pub fn cents_between(f_meas: f64, f_true: f64) -> f64 {
    1200.0 * (f_meas / f_true).log2()
}

// `Complex`/`Zero` are used through realfft's vector types; keep the imports
// honest by referencing them in a compile-time check.
const _: fn() = || {
    let _ = Complex::<f32>::zero;
};

#[cfg(test)]
mod tests;
