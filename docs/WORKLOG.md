# Micro-Vocal Lab — Worklog

Running log, updated after every sub-item. Newest entries at the bottom.
This file exists so that any agent (or a fresh environment) can resume from
the last commit without losing context.

---

## 2026-10-04 — Phase 1: Research + Architecture Plan

**Done:**
- Cloned `salim77007j/voi55` (empty); configured git identity; branch `main`.
- Live crates.io research snapshot — 26 crates, evidence at
  `docs/research/crates_2026_snapshot.json` (captured 2026-10-04).
- Wrote `docs/ARCHITECTURE_PLAN.md`: 15 decision records (D1–D15) with rejected
  alternatives, performance budget, risk register, phase mapping.
- Key decisions: Slint 1.18 UI · cpal 0.18 I/O · hound WAV · symphonia MP3 decode ·
  LAME (mp3lame-encoder) MP3 encode · rustfft/realfft FFT · in-house pYIN +
  TD-PSOLA pitch · LPC + allpass Bark-warp formants (mm) · STFT harmonic/residual
  air engine (0.1 dB) · Inter + IBM Plex Sans Arabic · "Studio Graphite" design system.
- Added README.md, LICENSE (GPL-3.0-or-later), .gitignore, PHASE_1_REPORT.md.
- Committed and pushed to `origin/main`.

**Decisions future phases must honor:**
- Engine lives in `mvl-core` (no I/O deps, no unsafe); I/O in `mvl-audio`;
  Slint app in `mvl-app` (see D15).
- Processing order fixed: pitch → formant → air (D11).
- Every "works/natural" claim needs a screenshot or audio file (D14).

**Next up (Phase 2):** workspace scaffold; cpal 192 kHz/f32 capture; hound WAV
import/export; symphonia MP3 import; LAME MP3 export; play/pause/stop; I/O
round-trip unit tests. Commit after each sub-item.

## 2026-10-04 — Phase 2.1: Workspace scaffold

- Workspace `Cargo.toml` (resolver 3, edition 2024, release profile: lto=thin, strip).
- `crates/mvl-core`: engine parameter contract (pitch ±12 st/1 cent, air ±dB/0.1 dB, formant 130–190 mm) with saturating setters + 6 unit tests. DSP arrives Phase 3.
- `crates/mvl-audio`: `AudioBuffer` (interleaved f32, invariant-enforced) + `AudioError` + 6 unit tests. Deps declared: cpal 0.18, hound, symphonia (mp3), mp3lame-encoder.
- `crates/mvl-app`: `micro-vocal-lab` binary stub (version report; Slint UI in Phase 4).
- Sandbox note: no sudo/ALSA headers → local ALSA 1.2.14 prefix extracted from Debian debs at `/home/z/my-project/.alsa-prefix`, wired via `scripts/env.sh` (PKG_CONFIG_PATH + LD_LIBRARY_PATH). Verified cpal 0.18 links against it. Real Linux machines need only `libasound2-dev`.
- cargo fmt + clippy -D warnings clean; 12 tests green.

## 2026-10-04 — Phase 2.4: MP3 import/export + offline resampler

- `mp3.rs`: import via symphonia 0.6 (probe handles ID3v2; planar→interleaved f32 for F32/F64/S16/S32/U8/S8 outputs), export via LAME 0.2.5 (`Builder`, planar Mono/DualPcm, CBR 128–320, `FlushNoGap`, explicit byte-budget reserve because `encode_to_vec` writes into spare capacity only).
- `resample.rs`: windowed-sinc offline resampler (Hann, 24 zero-crossings, f64 accumulate) — needed because MP3 tops out at 48 kHz. Round-trip SNR + exact-length tests.
- mp3_target_rate(): 192k/96k→48k mapping, unit-tested.
- Tests: MP3 mono 44.1k round-trip (offset-search SNR > 20 dB), stereo 192k→48k decode verification, garbage-file safety, rate mapping.
- Key API learnings recorded for Phase 3: symphonia 0.6 = enum CodecParameters + registry factory + planar AudioBuffer<f32>; LAME = spare-capacity output.
- 33 tests green; fmt + clippy -D warnings clean.

## 2026-10-04 — Phase 3: DSP engines complete

- **3.1 (2d69a58) pYIN (D7):** Hann-weighted FFT difference function (3 fwd + 2 inv real-FFTs/frame) — kills the truncated-period wobble that biased low F0 by >10¢; Beta(2,10) threshold prior; 10-cent bins + unvoiced state; Viterbi with note-continuation kernel; sub-bin F0 from parabolic τ*; center-aligned on W/2. GOTCHA: a −1e18-initialized Viterbi ties every state in f64 (ULP 128 > |ln p| ≤ 23). Gates: tones 82.4–880 Hz ≈0¢ median; vibrato 4/7 Hz ±50¢ ≤5¢/≤20¢; sweep ≤5¢/≤20¢; noise+silence ≥95% unvoiced.
- **3.2 (96c0639) TD-PSOLA (D8):** epochs refined to x² peaks; grains ≥2 synthesis periods; read pointer advances T0/r (1:1 time map → duration exact); window-sum-normalized OLA; voicing envelope with exact bypass (<0.05). Measurement lesson: PSOLA outputs carry TWO combs (placement = target, grain content = input) — verified with a cepstral comb-spacing instrument (`measure::f0_ceps`); non-integer ratios give ~10% F1 ripple (disclosed).
- **3.3 (7417f04) Formant (D9 amended):** exact envelope resampling E(f/r) replaces the allpass approximation (contract F·175/L); envelope estimated by cepstral liftering (sr/300) — LPC follows the tilt on periodic excitations and starves the warp; log-domain ratio, ±18 dB limiter, band-edge taper, RMS compensation, voiced-only. synth::vowel rewritten as harmonic amps following a prescribed Lorentzian envelope (filter-bank synths kept failing). Gates: ×1.346/×0.921 within 8% (harmonic-grid resolution); pitch ≤5¢; unvoiced bit-exact; loudness ±1 dB.
- **3.4 (f543e5d) Air (D10 amended):** time-median HPSS rejected (stationary noise is time-stable too); discriminator = pYIN comb mask × peak-above-floor prominence (local median floor) — also plugs pYIN's rare voiced-misclassified noise frames. Positive = band weight 0.25→1 (200 Hz→4 kHz) + tilt; negative = de-ess 5–13 kHz + breath downward expansion (flatness 1–8 kHz, ≤0.8 deeper). Harmonic path gain exactly 1.0. TEST BUG FOUND: band helper used 5·log10 instead of 10·log10 (half-dB). Gates: +6→+5.8 dB; −12→−11.5 dB; de-ess ≥3 dB deeper; comb bins ±0.5 dB; breath burst −19.3 dB end-to-end.
- **3.5 (8031e06) Pipeline + bridge:** `pipeline::render` (one pYIN pass, D11 order pitch→formant→air, RenderReport); `mvl-audio::engine::render_offline` (mono downmix → engine → channel restore; `AudioError::Dsp`). End-to-end gates: all three stages leave measurable marks; stereo neutral bit-exact; +2 st duration/coherence.
- **3.6 (8559211) Evidence:** docs/evidence/phase3 — 7 WAVs, 5 spectrogram pairs (verified visually: spacing widens for pitch, bands move for formant, bursts vanish for air), metrics.txt; generators committed (`examples/phase3_evidence.rs`, `make_spectrograms.py`).
- **Quality at HEAD:** 81 tests green (49 core + 32 audio); fmt + clippy -D warnings clean workspace-wide; forbid(unsafe) in core; no unwrap/expect on user/audio data in production paths.
- **Honest gaps:** synthetic fixtures only (no mic in sandbox — real-vocal validation deferred to Phase 5/6); PSOLA sub-harmonic content at non-integer ratios (disclosed); vibrato >±3% leaks into the residual; breath detector is a heuristic.
- NEXT: Phase 4 — Slint UI (EN LTR + AR RTL, waveform zoom, three sliders wired to pipeline::render, No-Fake-UI audit) awaits the explicit "continue".

## 2026-10-04 — Phase 4.1–4.4 (entries backfilled at 4.5; per-sub-item commits in git log)

- **4.1 (bfda300)** Slint scaffold: Studio Graphite shell (D13 tokens), embedded
  Inter + IBM Plex Sans Arabic (D12), headless screenshot platform + PNG evidence
  pipeline; `mvl-app` split lib+bin. 83 tests.
- **4.2 (bbc7a64)** Waveform engine: min/max/power pyramid (adaptive bin), peak/RMS
  columns, voiced tint + F0 trace from shared pYIN, wheel-zoom/drag-pan + zoom
  controls, playhead overlay, session module, `--demo/--open/--window/--playhead`
  evidence flags. 91 tests.
- **4.3 (fa9eabc)** Precision sliders → engine: drag/keyboard/double-click-reset,
  1¢/0.1 dB/1 mm steps, EngineParams clamp+echo, 250 ms debounced background
  render via cached pYIN, A/B preview toggle, render timing in status bar. 93 tests.
- **4.4 (6d0672a)** Transport + import/export + recording (No-Fake-UI):
  play/pause/stop/rewind via cpal Player (lazy connect, honest no-device status),
  Recorder with overflow disclosure, WAV f32/24/16 + MP3 128–320 export panel with
  fresh non-destructive render, import via rfd (xdg-portal) with
  `MVL_OPEN_FILE`/`MVL_SAVE_FILE` automation override, export round-trip test.

## 2026-10-04 — Phase 4.5: i18n — runtime EN LTR ↔ AR RTL switch (D1/D12)

- `i18n.rs`: explicit EN/AR string tables (no gettext dep) — every user-visible
  string, per-language UI font (Inter / IBM Plex Sans Arabic), `rtl` flag,
  localized render-report stage names; `{key}` template fill; LRM (U+200E) LTR
  isolation for numeric/technical fragments inside Arabic text; device-cap and
  overflow suffixes localized. Western digits kept for engineering readouts in
  both directions (D12); unit symbols st/dB/mm/Hz stay Latin (disclosed scope).
- `app.rs`: `Lang` state + `set_language`/`apply_language`; all status lines,
  track labels, zoom readout, REC clock composed through the active table;
  language switch rebuilds base status from the session and drops transient
  extras (documented); `toggle-language` chip wired (affordance shows the target
  language: EN UI shows "عربي", AR UI shows "EN").
- RTL mirroring: exact-mirror `layout-order` schemes fixed in header, transport
  bar (play button stays on its mirrored slot; glyphs keep playback-direction
  meaning), status bar, slider row, and slider label/readout rows. Groove and
  DAW timeline stay LTR by policy (disclosed). Export panel anchoring now
  direction-aware.
- **Bug found & fixed (No-Fake-UI):** the export panel rendered *under* the
  waveform well (later layout siblings paint over a header child that overflows
  the 56 px header) — invisible since 4.4. Moved to a window-level overlay
  (last child → paints on top); EN + AR verified in evidence PNGs.
- `--lang en|ar` for GUI and `--screenshot` (+ `--export-panel` evidence flag);
  screenshot test gains an AR step (Arabic status line asserted, RTL header
  pixel-diff vs LTR > 500 px).
- Evidence: `docs/evidence/phase4/` — 6 PNGs (EN shell/demo/preview/export,
  AR RTL demo/export).
- Quality at HEAD: 97 tests green (50 core + 33 audio + 14 app incl. 4-step
  screenshot integration); fmt + clippy `-D warnings` clean workspace-wide.
