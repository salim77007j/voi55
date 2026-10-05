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

## 2026-10-04 — Phase 5.1/5.1b: CI matrix + nightly memory checks

- **5.1 (8f48ce2)** `.github/workflows/ci.yml`: 3-OS matrix (ubuntu/windows/
  macos), fmt+clippy(-D warnings)+build+test per target, headless UI smoke on
  Linux with screenshot artifact. **Fix (57237ab):** Linux runners also need
  `libfontconfig1-dev` (Slint font system) — first run failed on
  `yeslogic-fontconfig-sys`. Matrix green on all three OS.
- **5.1b (bde079c)** `.github/workflows/nightly.yml` (schedule + dispatch):
  ASan `cargo test -p mvl-core -p mvl-audio` + Valgrind memcheck of the real
  headless app run. **Fixes (57237ab/808aadd):** sanitizer must be scoped to
  the target triple (`CARGO_TARGET_*_RUSTFLAGS` + `--target`) because
  proc-macros cannot be instrumented; valgrind suppressions for third-party
  fontconfig charset caches (592 B definite, all via FcCharSetAddChar —
  documented in `.github/valgrind-suppress.supp`). ASan: all tests pass, zero
  first-party leaks.

## 2026-10-04 — Phase 5.2: release artifacts workflow

- **5.2 (73b79df)** `.github/workflows/release.yml`: 3-OS stripped release
  builds; gates = binary *runs* headlessly + `< 50 MB` size check; bundle =
  binary + README + LICENSE; GH Release on `v*` tags. **Fix (808aadd):**
  Windows bundle via PowerShell `Compress-Archive` (runners ship no `zip`).
- **Amendment (D14):** the "criterion benchmark in mvl-core" plan item is
  fulfilled by `crates/mvl-audio/examples/bench_render.rs` — same measurement
  (preview DSP load vs the 40 %-of-one-core budget), no extra dependency,
  exits non-zero on budget miss. The valgrind check moved from "local sandbox"
  to the nightly CI job (sandbox has no valgrind and no root); run logs are the
  recorded evidence.

## 2026-10-04 — Phase 5.3: runtime verification forced RAM-budget fixes

- First verification: **316 MB peak RSS** on a 3-min 48 kHz stereo session vs
  the 200 MB budget. Attribution via `crates/mvl-audio/examples/rss_probe.rs`:
  pYIN per-frame matrices +192 MB; stage scratch (acc/wsum/env + pipeline
  input copy) +182 MB; stereo preview + duplicate downmix +103 MB.
- **Fixes (c9ffd3f):** (1) streaming pYIN Viterbi — observation rows consumed
  per frame, u16 backpointer trellis + per-frame candidate taus retained
  (−170 MB); (2) new `mvl-core::ola` — `CarryOla` fixed-size ring overlap-add
  used by all three stages with on-the-fly `EnvStream` voicing envelope;
  additions per sample in the same order ⇒ bit-identical output (all gates
  green); (3) pipeline drops the unused `x.to_vec()` working copy; (4) app
  stores the A/B preview **mono** (Player maps 1→N at output; export keeps the
  original channel layout) and drops the stale preview before a re-render.
- **Result: 316 → 196 MB** (all-stages worst case), 146 MB loaded idle,
  single-stage interactive ≈ 180 MB; mono-session export ≈ 173 MB, stereo
  ≈ 206 MB (disclosed). Cold start 56–59 ms; binary 23.8 MB; preview render
  34× real-time = 3 % of one core. Harness committed:
  `scripts/verify-runtime.sh` + `scripts/make_session_fixture.py` +
  `MVL_TIMINGS` headless stage timing. Evidence: `docs/evidence/phase5/`.
- Honest gap recorded in the report: full-file preview round-trip (250 ms
  debounce + render at 34× real-time) does not meet the brief's < 20 ms
  streaming-preview target; chunked preview path is Phase 6+ work, the ring
  OLA is its groundwork.
- NEXT: Phase 6 — VALIDATION_REPORT.md, real-hardware numbers, native-Arabic
  reviewer checklist, tag v1.0.0 — awaits the explicit "continue".

## 2026-10-04 — Phase 6.1: chunk-driven streaming render (f2a25f8)

- `mvl-core::stream`: each stage loop (PSOLA grains, formant frames, air
  frames) moved into a chunk-driven driver; the offline functions are thin
  wrappers over the same math. `CarryOla::with_start` (restart head) +
  `x_base` input windows; `EnvStream::reset_to`. `StreamChain` drives
  pitch→formant→air through a demand wave (progress = sum of stage heads,
  so mid-pipeline rounds don't stall), front-trims hand-off buffers to the
  consumer frontier (memory window-sized, file-length-independent).
- GOTCHA: a stage advance that requested work but yields nothing means the
  chain is exhausted (OLA tails never flush on their own) — the caller must
  run `finish` once. GOTCHA 2: a restart seeds a fresh PSOLA read-pointer
  phase — the epoch grid is identical but the output is not sample-exact vs
  the full render (perceptually equivalent; disclosed).
- Gates: chunked == offline render (exact f32) for 7 stage combos × chunk
  sizes {1,7,479,4095,2²⁰}; bounded buffers; STFT restart convergence
  (measured bound ≤ 2 windows + slack); neutral rejected; latency readout.

## 2026-10-04 — Phase 6.2a: streaming preview player (e244fd5)

- `mvl-audio::preview`: PreviewStream worker (10 ms chunks → bounded FIFO,
  100 ms), Player gains a FIFO source (mono→N mapping in the callback,
  transport identical for both sources). Slider change = restart at the
  playhead + 10 ms crossfade splice onto the unplayed tail.
- FIFO splice contract: continuity (read ≤ at ≤ write: tail capture +
  rewind, gap-free) and hard jump (seek beyond production / backward:
  cursor reset + consumer hold until the first push — stale ring content
  unreachable). GOTCHA: consume only what `push` accepted — the blended
  remainder parks in a worker-local pending buffer (dropping it loses
  audio; re-serving it double-crossfades). GOTCHA: restart position may
  equal the cursor → empty advance ≠ exhaustion; only "requested work and
  got nothing" fires the tail flush. Completion signal for consumers is the
  FIFO read cursor reaching the end (hard jumps discard regions — a
  pulled-sample counter never terminates).
- Disclosures (module docs): splice region = partial overlap history
  (masked by the crossfade); pitch-active restart resets PSOLA phase
  (perceptual, not sample-exact; exports use the offline render); FIFO is
  mono session-rate (no RT resampler); cpal callback needs hardware.
- Gates (42 audio tests): streamed == offline render bit-exact through the
  worker; STFT restart converges exactly beyond the artifact window; pitch
  restart moves the comb to the new target (ceps gate — window ≥ 8192
  samples, the instrument's Welch FFT); FIFO wrap/underrun/hold; backpressure.

## 2026-10-04 — Phase 6.2b: app wiring (f6f18e2)

- Live lifecycle: started on first play (device probe via
  `devices::default_output_rate()`, session rate == device rate, track
  available, non-neutral params); slider moves restart at the playhead and
  show the last measured restart latency (new EN/AR status string);
  neutral params stop the live path (original plays); stop/rewind
  hard-jump the stream cursor to 0; new sessions stop the old worker. The
  debounced offline render still feeds the A/B view; exports unchanged
  (offline). Headless/screenshot paths untouched (12 app tests green).

## 2026-10-04 — Phase 6.3/6.4: validation close-out (d84d297 + this)

- `docs/VALIDATION_REPORT.md`: 17-row verified-claims matrix (every brief
  requirement → evidence artifact); real-hardware protocols (§3: round
  trip, named sound-quality verdicts, soak, 192 kHz capture, live-latency
  loopback, cold start, per-OS notes); native-Arabic reviewer checklist
  (§4); performance table (§5); honest gaps ledger (§6, incl. the new
  restart artifacts and the rate-match constraint). Final `v1.0.0` =
  sign-off on those protocols.
- README refreshed (status table, features, verified budgets, 112 tests).
- Quality at HEAD: 112 tests green (56 core + 42 audio + 12 app + 2),
  fmt + clippy `-D warnings` clean workspace-wide.
- NEXT: tag `v1.0.0-rc.1` (fires the release workflow); then the hardware
  protocols on real machines → final `v1.0.0`.

## 2026-10-04 — Phase 6.5: version 1.0.0-rc.1 (57b232b)

- Workspace version → `1.0.0-rc.1` (Cargo.toml + Cargo.lock). The rc
  marks the complete feature set with every sandbox gate green; final
  `v1.0.0` signs off on the real-hardware protocols (§3/§4) — this
  environment has no microphone, audio device or display, and the
  honesty policy forbids a final tag before someone has heard it.

## 2026-10-04 — Release workflow fix + rc.1 publish confirmation (f381b49)

- First `v1.0.0-rc.1` tag push failed the release job at the
  `GH Release` step (missing `contents:write`); the workflow now grants
  it and the re-run succeeded.
- Publish state verified via the GitHub API: release `v1.0.0-rc.1`
  exists, marked prerelease, with all three artifacts attached —
  `micro-vocal-lab-ubuntu-latest.tar.gz` (10.4 MB),
  `micro-vocal-lab-windows-latest.zip` (7.7 MB),
  `micro-vocal-lab-macos-latest.tar.gz` (7.0 MB) — all far under the
  50 MB budget.
- Out-of-CI smoke of the published Linux artifact: downloaded the asset
  via the API (Accept: application/octet-stream), unpacked
  (binary 23.8 MB + LICENSE + README), ran `--screenshot --demo synth`
  headlessly — exit 0, valid 1280×800 PNG, full Studio Graphite shell
  (three precision sliders, waveform + F0 trace, transport, عربي
  switch, "v1.0.0-rc.1" title). The published artifact runs.
- REMAINING for `v1.0.0`: run the §3 real-hardware protocols and the §4
  native-Arabic review on real machines (user-side), triage any
  failure, then tag `v1.0.0`. No further sandbox-side code work is
  planned unless a hardware run finds something.

## 2026-10-04 — Phase 7.1: P0 fixes (a: symphonia, b/c: devices, d: robustness)

User feedback on real machines: import/DSP work, but the UI reads
student-grade and real-machine audio-device handling is fragile. Phase 7
opened: 7.1 = the four P0 fixes, 7.2 = professional UI redesign against
the uploaded reference (repo root `reference.png`, "Audioprecise Pro"
style — analyzed, becomes the 7.2 visual target).

- **7.1a (7848903)** — symphonia 0.6 → **0.5.5** (mature line; 0.6 is a
  self-declared preview). Import ported to the 0.5 API (`probe.format()`,
  `CodecParams` value, `CodecRegistry::make`, `Cow<AudioBuffer<S>>`
  variants, `Signal::chan`, `FromSample` i24/u24). Robustness gain: torn
  packets are skipped (DecodeError → warn) instead of failing the file;
  EOF handled via UnexpectedEof IoError. New tests: decode determinism
  (bit-identical), truncated file, 64 seeded random blobs. 45 audio tests
  green.
- **7.1b+c — device management (inputs AND outputs)**
  - `resolve_input_device` / `resolve_output_device` (mvl-audio): honest
    fallback chain — explicit name → platform default → first working
    device; returns whether it fell back so the UI can say so.
  - `Recorder::start_named` / `Player::connect_named`; the app stores the
    user's pick per direction and passes it on every connection.
  - `list_output_devices` (name, config count, max rate, granted formats)
    mirrors the existing input enumeration; the startup banner lists both.
  - Player negotiation rewritten: **any granted sample format**
    (f32 > i32 > i16 > 8-bit — generic `build_output_stream::<T>` with a
    reused f32 scratch, clamped `FromF32` conversion) and **any channel
    count** (previous hard cap of f32/stereo removed).
  - Slint **Devices dialog** (headphones button in the header, RTL-aware
    layout, EN/AR strings in i18n with template tests): real-enumerated
    input/output lists with the effective device highlighted, per-row
    pick retargets the next capture/playback, output pick re-homes the
    player immediately, **Test output** plays a real 440 Hz / 0.3 s tone
    (flag follows the actual transport in the 40 ms poller — no timers).
  - Headless evidence: `--devices` flag renders the dialog through the
    same software-renderer path (`docs/evidence/phase7/devices-dialog-
    {en,ar-rtl}.png`); the sandbox's ALSA null device shows up, proving
    the enumeration is real.
  - Platform notes documented in `devices.rs` (WASAPI shared-mode,
    CoreAudio permission-on-first-capture + NSMicrophoneUsageDescription
    bundle task, PipeWire/PulseAudio via the ALSA plugin stack, loopback
    not enumerated — honest scope).
- **7.1d — robustness**
  - Zero-frame capture guard: `Recorder::stop` → `finalize_capture`
    returns the new named `AudioError::EmptyCapture` ("the device
    produced no audio") instead of an empty buffer the UI would accept;
    unit-tested for empty/torn/one-frame inputs.
  - `catch_unwind` around the two worker threads: the offline render
    thread (panic → `RenderDone::Failed` with the payload text) and the
    Phase 6 preview worker (panic → FIFO stalls, stats.error set, app
    keeps running). RT callbacks stay panic-free by construction; the
    global panic hook prints thread/location context.
- Quality at HEAD: **118 tests green** (56 core + 48 audio + 12 app +
  2 integration), fmt + clippy `-D warnings` clean workspace-wide.
- 7.1b/c/d share files (capture.rs/app.rs/main.rs carry both the device
  and the robustness changes), so they land as one commit with this
  three-part record; 7.1a went separately as 7848903.
- NEXT: 7.2 — the professional studio UI redesign against the reference
  image (dense channel-strip panels, RTA spectrum analyzer from real
  FFTs, transport + meters, before/after), awaits "continue".

## 2026-10-05 — Phase 7.2 start: professional UI redesign (reference-driven)

- Environment reset again (fresh container): ALSA dev headers missing →
  non-root recovery: `apt-get download libasound2-dev` extracted to
  ~/.local/alsa-dev, local `alsa.pc` in ~/.local/pkgconfig, runtime
  libasound.so.2 copied over the dangling .so symlink; env helper in
  ~/.local/voi55-env.sh. Baseline build green at 975bdd2.
- Mandatory visual analysis of `reference.png` (1280×698, uploaded at
  repo root): AUDIOPRECISE PRO — medium-dark gray panel chrome with
  beveled borders, inset near-black displays (waveform/EQ/RTA/comp),
  dark radial-gradient knobs + mono readouts below, green→yellow
  vertical meters with dB scales, saturated transport buttons (green
  play / red record), LCD project-time display, colored track tabs,
  dense small-caps labeling. This is the Phase 7.2 visual target.
- Design synthesis (report will disclose): task-book accent/state
  palette (pitch #00B4D8, air #06FFA5, formant #B388FF, play #4CAF50,
  record #E53935, warn/rewind #FFC107, well #0A0A0A) + the reference's
  chrome language (panel borders, inset wells, LCD readouts, meters).
- Fonts: task book mandates IBM Plex family → downloaded IBM Plex Sans
  (Latin, 3 weights) + IBM Plex Mono (3 weights, OFL.txt) to
  assets/fonts; Arabic stays IBM Plex Sans Arabic (already embedded).
- 7.2 sub-item plan: a) tokens+fonts+waveform palette b) appwindow
  layout rebuild (menubar/toolbar/3-column/transport/statusbar) c)
  knob control strips d) real metering (output tap, volume, seek,
  DSP duty) e) real-FFT spectrum analyzer f) i18n+status metrics+RTL
  audit g) evidence + PHASE_7_2_REPORT. Commit per sub-item.

## 2026-10-05 — Phase 7.2a+7.2b: tokens/fonts/palette + full layout rebuild

- **7.2a (e00fcca)**: "Studio Console" tokens (task-book palette + reference
  chrome language); IBM Plex Sans/Mono added (3 weights each + OFL), Inter
  removed; waveform rasterizer gains a vertical cyan→coral peak gradient
  (lerp per row), amber 1px playhead, voiced tint softened; screenshot
  test constants re-pinned to the new spec.
- **7.2b (this commit)**: appwindow rebuilt to the task-book layout —
  - menu bar (File/View/Audio/Help; Edit omitted honestly — no real edit
    commands exist; every entry dispatches a real command: import/export/
    devices/zoom/fit/A-B/zoom-to-selection/test-output/about), dropdown
    panels with explicit open-id state (no PopupWindow; deterministic in
    the headless renderer), RTL mirrors via layout-order incl. spring.
  - toolbar: brand block + Record/Import/Export icon-caption actions +
    devices + language chip (same real callbacks).
  - three-column body: info rail (TRACK/FORMAT/ANALYSIS panels fed from
    the real session: name/rate/frames/f32 domain/channels/median F0/
    voiced %; em-dash when analysis missing), time ruler (nice 1-2-5
    steps from the real view span; the same tick model draws the well
    grid), waveform well; right column keeps PrecisionSliders until 7.2c.
  - drag selection (SEL mode chip → select callback → overlay + duration
    status) and View→"Zoom to selection" (real set_view_secs).
  - transport: colored buttons (amber rewind-to-start, 56px green play,
    stop, red record with pulse, amber FF +5s), LCD time display (split
    current/total, IBM Plex Mono, "PROJECT TIME" engineering label),
    MASTER volume slider wired to a real output gain, L/R meters (honest
    silence until the 7.2d tap lands).
  - status bar segments: engine line | file | format | IN device | OUT
    device — all fed from real state; empty segments hide.
  - mvl-audio: Player::set_volume/volume (atomic f32 bits, applied in
    the callback post-mapping pre-conversion, clamp ±1.0), Player::
    seek_to_frame (buffered source; streaming seeks via PreviewStream::
    restart), apply_volume extracted + unit-tested (scale/clamp/mute).
  - App: menu_action dispatch, seek_to/seek_relative (streaming vs
    buffered paths), volume persistence across reconnects, selection in
    project seconds, ruler model rebuild in refresh(), LCD init at zero.
- Evidence: ui72b-layout-en.png, ui72b-layout-ar-devices.png (full RTL
  mirror incl. menus/transport/status), ui72b-layout-export.png.
- Tests: 120 app+audio green (volume tests added), fmt+clippy clean.

## 2026-10-05 — Phase 7.2d+7.2e: real metering + real-FFT spectrum analyzer

- **7.2d (c612da5)**: lock-free `MeterTap` (peak cells + duty EMA,
  atomics only on the RT path) — output tap in the player callback
  (post-gain, pre-conversion; measures what the DAC actually gets) and
  input tap in the capture callback (reflects the true incoming signal
  even when the ring overflows). Transport L/R meters + peak-hold ticks
  driven per 40 ms poll with ballistics (fall 20 %/tick, 2 s peak hold,
  slow decay); recording switches the meters to the input source.
  Status bar gains the RSS + DSP(%) segment (Linux /proc statm,
  macOS task_info via libc, Windows GetProcessMemoryInfo via
  windows-sys — target-gated deps; segment hides when unknown).
  Tests: tap peak/drain, mono mirror, duty EMA bounds, degenerate
  inputs, RSS sanity. +6 tests.
- **7.2e (this commit)**: `SpectrumTap` — fixed 4096-slot atomic ring
  (mono downmix, monotonic cursor, overwrites safe), drained by the UI
  into a 2048-sample window → Hann → rustfft forward → 48 log-spaced
  bands (40 Hz–16 kHz, band = max bin, like an RTA) → dB mapping
  (floor −64 dB) → bars + peak hold/decay rendered into a
  SharedPixelBuffer in the panel's physical pixels. Panel sits in the
  right column under the three strips; live LED follows the transport;
  decay-to-silence when stopped (never a frozen frame). Analysis core
  extracted as a pure module + tests: 440 Hz sine peaks in band 19
  (≈429–485 Hz), zero leakage above, silence → silent bars, renderer
  pixel assertions. +3 tests. Headless evidence shows the honest empty
  well (no audio device in the sandbox — the FFT path is exercised by
  the unit tests and will show live bars on real hardware in 7.3).
- Tests at HEAD: 121 app+audio green (57 audio, 18 app lib, 2 shot,
  ...), fmt + clippy `-D`-clean (0 warnings).

## 2026-10-05 — Phase 7.2f+g: RTL/angle fixes, evidence matrix, phase report

- Knob sweep corrected to industry standard (min 7:30 → max 4:30, 270°
  clockwise from 12; sin/cos screen mapping fixed — was rotated 90°).
- Ruler labels at sub-ms steps now µs-precision ("0.0123 s") — ms-rounded
  labels collided at 100 µs steps.
- Headless `--about` flag added (About dialog evidence; same property the
  Help menu sets).
- Evidence matrix: 17 screenshots in docs/evidence/phase7/ covering EN/AR
  × empty/demo/params/zoom-subms/preview/devices/export/about.
- docs/PHASE_7_2_REPORT.md: reference analysis, sub-item log, No-Fake-UI
  audit, 6 honest deviations, gaps → 7.3/7.4, verdict.
- Final state: 131 tests green (56 core + 57 audio + 16 app + 2 shot),
  fmt + clippy clean, all sub-item commits pushed.

## 2026-10-05 — Phase 7.3: cross-platform verification (sandbox honesty edition)

- Environment reset mid-phase (toolchain+repo gone): recovered via rustup
  user install, alsa-lib 1.2.14 source build (~/.local/alsa), xkbcommon-
  x11/libxcb-xkb user-prefix debs; repo re-cloned; git history remained
  authoritative (7.1+7.2 already done).
- 7.3c macOS bundle: .github/macos/Info.plist (NSMicrophoneUsageDescription,
  version from workspace manifest, LSMinimumSystemVersion 11.0) + universal
  arm64+x86_64 lipo build + ad-hoc codesign + plutil -lint; verified end-to-
  end on a real macos-26-arm64 runner (release dispatch 37312400391).
- Per-platform render evidence (real runners executing the binary):
  smoke-{ubuntu,windows,macos}-latest.png -> docs/evidence/phase7/platform/;
  macOS frame shows live CoreAudio enumeration ("Apple Virtual Sound
  Device"), Ubuntu frame ALSA default.
- Sandbox deep runs: 131/131 tests green locally; real X11 event-loop run
  under Xvfb (0 panics, honest empty state; first attempt panicked on
  missing libxkbcommon-x11 and the 7.1d panic hook caught it as designed);
  --selftest-audio honest failure without a device; windows-latest PE
  executed under Wine 10.0 (0 panics, full UI render, 35 s+ alive).
- Found CI red at the 7.2 headline commit (macOS clippy: libc deprecated
  mach_task_self) -> fixed with mach2 (52c1f02); CI matrix extended to
  macos-15 + macos-14 (task book targets) -> CI green on 5 images
  (ubuntu-24.04, windows-2025, macos-26, macos-15, macos-14) at 1e2c9c5.
- Honest gaps: Fedora 41 unverified (no runner); WASAPI/CoreAudio audible
  playback, mic capture, loopback latency, Gatekeeper flow = human tasks
  (VALIDATION_REPORT §3 stands); Wine font/clip quirks disclosed as proxy
  artifacts. Report: docs/PHASE_7_3_REPORT.md.

## 2026-10-05 — Phase 7.4: quality gates + v1.0.0

- De-rc: workspace version 1.0.0-rc.1 -> 1.0.0.
- Fuzzer upgrade (the phase's one real code change): the 64-blob unit
  slice was VACUOUS (telemetry 0/1000 pure-random blobs reached a
  decoder). New crates/mvl-audio/tests/fuzz_1000.rs = seeded-mutation
  fuzzing (40 exporter-produced seeds, 1000 mutants; 330/1000 reached
  the decoders; >10% reached-assertion; runs in CI permanently).
- Runtime gates re-run at 1.0.0: cold start 62 ms median, RAM 149/198 MB
  (documented screenshot protocol), binary < 50 MB -> PASSED.
- Soak: 6 x 9.5-min segments (~57 min; sandbox reaps detached daemons ->
  segmented, disclosed), 108/108 alive, 0 crashes/panics, ~460 stress
  renders, 0 failures; AR segments exercise RTL. Evidence:
  docs/evidence/phase7/soak/.
- RAM audit: first long-lived-GUI measurement 252 MB steady, flat (no
  leak); smaps anatomy = 99 MB session + ~50 MB libLLVM/llvmpipe
  (sandbox software-GL) + 39 MB heap -> app-attributable ~150 MB.
  malloc_trim experiment tried and reverted (no effect).
- docs/PHASE_7_REPORT.md written (before/after, 10-gate scorecard, open
  human items, verdict). Tag v1.0.0 follows this commit.

## 2026-10-05 — v1.0.0 tagged and published

- Tag v1.0.0 at 33a0b63 (CI 5-image green). Release run 37330924094:
  three platforms built + ran the binary + uploaded bundles; GitHub
  Release v1.0.0 (prerelease=false) carries macOS (universal + .app,
  30 MB), Ubuntu (11 MB), Windows (8 MB). rc.1 retained as prerelease.
- Phase 7 complete. Open human items live in docs/PHASE_7_REPORT.md §5.
