# Phase 3 Report — DSP Engines (pYIN · TD-PSOLA · Formant · Air)

**Status: COMPLETE.** Branch `main`, head `8559211` + this commit.
All sub-items were committed and pushed individually (3.1 → 3.7).

## What shipped

| Sub-item | Commit | Deliverable |
|---|---|---|
| 3.1 | `2d69a58` | pYIN tracker (D7) + synth fixtures + accuracy gates |
| 3.2 | `96c0639` | TD-PSOLA pitch shifter (D8) + gates |
| 3.3 | `7417f04` | Formant engine (D9, amended) + gates |
| 3.4 | `f543e5d` | Air/breath engine (D10, amended) + gates |
| 3.5 | `8031e06` | Offline pipeline (D11 order) + `mvl-audio` bridge |
| 3.6 | `8559211` | Evidence corpus (WAVs, spectrograms, metrics) |
| 3.7 | this | Report + README/worklog updates |

**Quality gates at HEAD:** 81 tests green (49 `mvl-core`, 32 `mvl-audio`), `cargo fmt` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean, `#![forbid(unsafe_code)]` in `mvl-core`, no `unwrap()`/`expect()` on user/audio data in production paths (test code only).

## D7 — pYIN pitch tracking

Implementation: FFT-accelerated YIN difference function computed with
**Hann-weighted** correlations (3 forward + 2 inverse real-FFTs per
frame); 100 absolute thresholds with a Beta(2, 10) prior; 10-cent pitch
bins + one unvoiced state; Viterbi with a note-continuation kernel
(stay ≈ 0.88, Laplacian p_move, uniform escape mass for onsets); sub-bin
F0 output from parabolically-refined periods; estimates center-aligned
on the analysis window (W/2).

**Measured accuracy (synthetic fixtures, D7 gate: median ≤ ±5 ¢,
p95 ≤ ±20 ¢, unvoiced ≥ 95 %):**

| Fixture | Median | p95 |
|---|---|---|
| Pure tones 82.4 / 110 / 220 / 440 / 880 Hz | ≈ 0.0 ¢ | ≤ 5 ¢ |
| Vibrato ±50 ¢ @ 4 Hz and 7 Hz | ≤ 5 ¢ | ≤ 20 ¢ |
| Sweep 110→440 Hz over 2 s | ≤ 5 ¢ | ≤ 20 ¢ |
| White noise + silence | ≥ 95 % unvoiced | — |
| 140 Hz vowel | ≤ 5 ¢ | ≤ 20 ¢ |

Two non-obvious defects were found and fixed during bring-up (both are
documented in code comments): rectangular-window correlation sums carry a
truncated-period wobble (`sin(Wω)/sin(ω)` amplification) that biased
low-F0 estimates by >10 ¢ — fixed by Hann weighting; and a Viterbi
initialized from a large negative constant ties every state in `f64`
(ULP 128 swamps |ln p| ≤ 23) — fixed by observation-based initialization.

## D8 — TD-PSOLA pitch shifting

Epochs from the pYIN track refined to local `x²` energy peaks; Hann
grains (≥ 2 synthesis periods); output grid spaced `T0/r` while the read
pointer advances the same `T0/r` — duration preserved by construction;
overlap-add normalized by the accumulated window sum; voicing envelope
crossfades unvoiced material **bit-exactly** (gate at p < 0.05).

**Measured (see `crates/mvl-core/src/psola/tests.rs`):** +3 st and
−7 st outputs carry a coherent comb at the target (ceps instrument);
vibrato mean-F0 comb at the shifted mean; envelope peaks within 12 %
(cepstral instrument); unvoiced interior bit-exact; duration exact.

**Known characteristic (disclosed):** a PSOLA output contains two
periodicities — the grain-placement comb (the shifted pitch) and the
grain-internal content comb (the input pitch) — plus a grain-reuse beat
pattern visible in the spectrograms after ~0.6 s. Non-integer ratios
produce overlap-cancellation ripple (~10 % F1 displacement measured at
+5 st). These are inherent to TD-PSOLA; perceptual quality is validated
in Phase 6 with real vocals. Render-path correctness is unaffected: the
engines analyze the **input** track once.

## D9 — Formant engine (amended)

Two amendments, both justified in `docs/ARCHITECTURE_PLAN.md` and in the
module docs: (1) exact frequency-axis envelope resampling replaces the
allpass Bark approximation (the allpass ratio varies per formant, which
would violate the contractual `F·175/L` mapping); (2) the per-frame
envelope is estimated by **cepstral liftering** (quefrency `sr/300 Hz`)
instead of LPC — LPC's least-squares fit follows the spectral tilt on
strongly periodic synthetic excitations and starves the warp of formant
evidence. The corrective ratio `R(f) = exp(env(f/r) − env(f))` is
smoothed, ±18 dB limited, band-edge tapered, loudness-compensated, and
applied voiced-only (unvoiced passes bit-exactly — no lisping).

**Measured:** 130 mm moves F1/F2/F3 by ×1.346 within 8 % (the tolerance
is the harmonic-grid envelope resolution at 140 Hz); 190 mm ×0.921;
pitch unchanged ≤ 5 ¢ median; unvoiced interior bit-exact; loudness
within ±1 dB.

## D10 — Air & breath engine (amended)

Two-path STFT (2048 @ 48 kHz equivalent, 75 % overlap). Implementation
note: the Fitzgerald time-median separation was evaluated and **rejected**
— stationary noise is as time-stable as harmonics, so the median
classifies it as harmonic. The shipped discriminator is the **pYIN comb
mask × peak-above-floor prominence** (local median floor over ±10 bins):
a bin is harmonic only near k·F0 *and* sticking out of the local floor,
which also keeps pYIN's rare voiced-misclassified noise frames from
leaking. Unvoiced frames are 100 % residual. Positive slider: band
weight 0.25@200 Hz → 1.0@4 kHz + tilt (+6 dB cap, slider-scaled).
Negative: de-ess concentration 5–13 kHz + downward expansion (≤ 0.8
deeper) during detected breath (1–8 kHz spectral flatness, no voicing).
The harmonic path gain is exactly 1.0 for any slider position.

**Measured:** +6 dB air → +5.8 dB in the 4–6.5 kHz residual band;
−12 dB → −11.5 dB; de-ess band ≥ 3 dB deeper than mid band; comb-bin
energy within ±0.5 dB at ±12 dB; breath burst −18 dB expanded to
−19.3 dB end-to-end (evidence fixture B); neutral bit-exact.

## D11 — Pipeline

`mvl-core::pipeline::render` runs one pYIN pass shared by all stages in
the fixed order **pitch → formant → air** and returns a `RenderReport`
(frames, voiced ratio, median F0, applied stages). `mvl-audio::engine::
render_offline` bridges `AudioBuffer` ⇄ engine (mono downmix, channel
restore, `AudioError::Dsp` propagation). Neutral parameters yield a
bit-exact copy at every layer.

## Evidence

`docs/evidence/phase3/` — 7 WAVs (16-bit/48 kHz), 5 before/after
spectrograms, `metrics.txt`, and the generators
(`cargo run -p mvl-core --example phase3_evidence` +
`python3 make_spectrograms.py`). Spectrogram checks confirm: harmonic
spacing widens for +4 st with an identical envelope; the formant warp
moves the energy bands with unchanged spacing; breath bursts vanish at
−18 dB.

## Honest gaps

- **Synthetic fixtures only.** No microphone in the build sandbox; all
  accuracy claims run on deterministic synthetic vowels/tones. Real-vocal
  validation is planned for Phase 5/6 with user-run recordings.
- **TD-PSOLA sub-harmonic content** at non-integer ratios (documented
  above) — audible as slight roughness at extreme shifts; the
  phase-coherentvocoded alternative was rejected in D8 and stays rejected.
- **Formant-warp measurement resolution** is harmonic-grid-limited
  (±1 harmonic) on the fixtures; the engine's envelope warp itself is
  exact by construction.
- **Vibrato beyond ±~3 % F0 smear** leaks harmonics into the air
  engine's residual (mask width trade-off); acceptable v1, revisited if
  Phase 6 real-vocal checks demand it.
- **Breath detector is a heuristic** (spectral flatness + voicing
  absence); no ML, no training data — behavior is deterministic and
  reproducible.

## Carried into Phase 4

- `mvl-core::pipeline::render` + `RenderReport` are the exact seams the
  Slint UI drives; the pitch track is returned for the waveform/pitch
  display.
- `mvl-audio::engine::render_offline` is ready to back the UI's render
  button (No-Fake-UI: every control must reach these functions).
- Real-time preview architecture (control-rate params, crossfades) is a
  Phase 4 concern on top of the same engines.
