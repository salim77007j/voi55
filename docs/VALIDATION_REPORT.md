# Micro-Vocal Lab — Validation Report (Phase 6)

**Status:** all sandbox-verifiable claims carry evidence (D14). Real-hardware
items are specified as executable protocols below and are **pending** — this
machine has no microphone, no audio device and no display server, and CI
runners are equally deviceless. Nothing in this report claims "sounds right"
without a human having heard it; that is exactly what the protocol in §3 is
for.

This report is the gate for the `v1.0.0` final tag. The tree is tagged
`v1.0.0-rc.1` with every sandbox gate green.

---

## 1. Verified-claims matrix

Every row is a product-brief requirement. "Evidence" points at the artifact
that proves the claim. Claims that require hardware are marked **HW** and
carry a protocol row in §3 instead of an evidence pointer.

| # | Requirement (brief) | Claim | Evidence | Status |
|---|---|---|---|---|
| 1 | 192 kHz / 32-bit float recording | `Recorder` requests f32 at the preferred rate, negotiates honestly, discloses overflow | `crates/mvl-audio/src/capture.rs` + unit tests; deviceless paths tested | ✅ code + tests, **HW §3.4** for real mics |
| 2 | WAV import/export | f32/i16/i24 round-trips are bit-exact at 192 kHz | `wav::tests` (bit-exact round-trips, garbage-file safety) | ✅ |
| 3 | MP3 import | symphonia decodes ID3-carrying MP3s, planar→interleaved, resamples >48 kHz sources | `mp3::tests` (round-trip SNR > 20 dB, 192k→48k decode) | ✅ |
| 4 | MP3 export | LAME CBR 128–320, `FlushNoGap`, byte-budget reserve | `mp3::tests` + Phase 2 report | ✅ |
| 5 | Pitch ±12 st / 1 cent | pYIN + TD-PSOLA: ≈0 ¢ median on 82.4–880 Hz tones, vibrato/sweep gates, duration preserved | `mvl-core` pYIN/PSOLA gate tests (56 core tests); `docs/evidence/phase3/` | ✅ |
| 6 | Air & Breath ±0.1 dB | STFT harmonic/residual engine: +6→+5.8 dB measured, de-ess ≥ 3 dB, breath burst −19.3 dB, harmonic path gain exactly 1.0 | `air` gate tests; `docs/evidence/phase3/` spectrograms | ✅ |
| 7 | Formant 130–190 mm | Exact envelope resampling: ×1.346/×0.921 within 8 %, pitch untouched ≤ 5 ¢, unvoiced bit-exact | `formant` gate tests | ✅ |
| 8 | Processing order pitch→formant→air (D11) | `pipeline::render` + `StreamChain` share stage activation and order | `stream::tests::chain_chunked_equals_offline_render_all_combos` — exact `f32` equality, all 7 stage combos | ✅ |
| 9 | EN default UI + AR RTL switch | Runtime EN↔AR, embedded fonts, exact-mirror layouts, LRM isolation | Phase 4.5 evidence PNGs (`docs/evidence/phase4/`), screenshot tests incl. RTL pixel-diff | ✅ code, **§4** for native review |
| 10 | Non-destructive export | Export renders fresh from the original with current params; the session buffer is never modified | export round-trip tests; Phase 4.4 report | ✅ |
| 11 | Preview latency < 20 ms typical | **Now met by the live streaming preview** (Phase 6.2): slider change → chain restart at the playhead + 10 ms crossfade; audible within ~one output callback (≈10–25 ms). Old path (debounce + whole-file render) remains for the A/B view | `preview::tests` (bit-exact stream vs offline render; restart convergence; comb gate); `mvl-core::stream` chunk gates | ✅ mechanism, **HW §3.5** for loopback numbers |
| 12 | Binary < 50 MB | Release, stripped: 23.8 MB (Linux); bundles 8.05/7.34/10.90 MB | `docs/evidence/phase5/runtime-verification.txt`; release CI size gate | ✅ |
| 13 | RAM < 200 MB (3-min session) | 146 MB idle / 196 MB worst case (was 316 MB before Phase 5.3) | `docs/evidence/phase5/runtime-verification.txt` | ✅ |
| 14 | Cold start < 1 s | 56–59 ms (headless proxy) | `docs/evidence/phase5/cold-timings.log` | ✅ proxy, **HW §3.6** for desktop numbers |
| 15 | Preview DSP < 40 % of a core | 3 % (34× real-time) for the offline path; the streaming worker renders 10 ms chunks in ~50 µs of DSP each | `docs/evidence/phase5/bench-render.txt`; `mvl-core::stream` timing in tests | ✅ |
| 16 | No memory leaks | ASan nightly (LeakSanitizer) + Valgrind memcheck of the real UI stack | nightly run 37213289805: ASan ✓ + Valgrind ✓ (only fontconfig internals, suppressed with rationale) | ✅ |
| 17 | Every control wired to the engine (No-Fake-UI) | Sliders→engine params→live restart; transport→Player/FIFO; export→fresh render; language chip→i18n | `app.rs` audit trail in worklog; screenshot tests assert rendered state | ✅ |

## 2. What Phase 6 added to the evidence base

- **`mvl-core::stream`** — chunk-driven render engine. Gate tests assert
  *exact `f32` equality* between chunked streaming and the offline render
  for every stage combination and chunk sizes {1, 7, 479, 4095, 2²⁰};
  bounded internal buffers (window-sized, independent of file length);
  restart convergence for the STFT stages (measured artifact bound);
  neutral-parameter rejection.
- **`mvl-audio::preview`** — worker + bounded FIFO + splice machinery.
  Gate tests: the *streamed* preview equals the offline render bit-exactly
  through the worker path; STFT-stage restarts converge exactly beyond the
  artifact window; with pitch active the comb moves to the new target
  (ceps gate); FIFO wrap/underrun/hold units; production backpressure.
- **App wiring** — live lifecycle (device probe → rate match → play),
  status-bar latency readout, honest fallbacks. Screenshot suite unchanged
  (12 app tests).

## 3. Real-hardware validation protocol (pending — run before final v1.0.0)

Run on one machine per OS with a real microphone and speakers/headphones.
Record results in `docs/evidence/phase6/hardware-<os>.md`. A row passes when
its criterion is met; any failure becomes a filed issue before tagging.

### 3.1 Import → edit → export round trip (all OS)
1. Import a real monophonic vocal take (WAV, 48 kHz or 96 kHz).
2. Move each slider; confirm the A/B view renders and the status bar shows
   real render times.
3. Export WAV f32 + MP3 320; re-import the exports; confirm duration and
   channel count match the session and the audio is not clipped.

### 3.2 Sound-quality verdicts (the claims no benchmark can make)
For each engine, A/B the original vs the rendered preview at a moderate
setting (pitch +2 st, formant 160 mm, air −6 dB) on the *same* take:
- Pitch: no phasing/metallic ring; formants preserved; duration identical.
- Formant: timbre moves as expected; no lisp on sibilants (bit-exact bypass).
- Air: positive reads as breathiness, negative cleans breath without dulling
  the voice (harmonic path untouched).
**Pass criterion:** the tester signs each of the three verdicts by name.

### 3.3 Stability soak
10 minutes of continuous slider dragging during playback on a 2-minute take.
**Pass:** no crash, no unbounded memory growth (RSS visible in a task
manager stays within ~1.5× the 3-minute budget numbers), no audio-device
errors in the console/log.

### 3.4 Capture at 192 kHz
Record 10 s via the built-in mic and an external interface if available.
**Pass:** the status line reports the *actual* negotiated rate/format (honest
caps display); the recording plays back at correct speed and level; the
overflow disclosure appears only when drops actually occur.

### 3.5 Live-preview latency loopback (the brief's < 20 ms number)
1. Play the take; drag the pitch slider to a new value and hold.
2. Screen-record at ≥ 60 fps (phone camera at 240 fps is better).
3. Count frames between the slider visibly settling and the audible pitch
   change; convert to ms.
4. Repeat 10× across pitch/formant/air.
**Pass:** median ≤ 20 ms typical voice (≥ 120 Hz F0); report the worst case
with the F0 of the material (low F0 ⇒ larger PSOLA grains ⇒ disclosed).
**Disclosed artifacts to listen for:** a ≤ 10 ms crossfade at the splice;
with the pitch stage active the restarted render's grain alignment resets
(perceptually equivalent, not sample-exact — exports always use the offline
render).

### 3.6 Desktop cold start
Launch the app 5× from the desktop shell (not a terminal pre-warmed run);
time to interactive waveform.
**Pass:** median < 1 s.

### 3.7 Platform-specific
- **Windows:** bundle installs/runs; WASAPI shared-mode playback OK; the
  release zip opens with Explorer.
- **macOS:** Gatekeeper right-click-open flow documented and works;
  CoreAudio playback OK.
- **Linux:** ALSA + PipeWire playback OK; note the `libasound2-dev` /
  `libfontconfig1-dev` build deps for from-source users.

## 4. Native-Arabic reviewer checklist (D1 acceptance — pending)

To be executed by a native Arabic reader on the `--lang ar` UI. Pass = every
box checked; any failure reopens the Phase 4 RTL work.

- [ ] All Arabic text shapes correctly (no disconnected letters, no boxed
      glyphs) — IBM Plex Sans Arabic embedded and applied.
- [ ] Bidi is correct in mixed lines: numbers/units inside Arabic sentences
      read naturally (LRM isolation working); no reversed punctuation.
- [ ] Header, transport bar, status bar and slider rows mirror exactly
      (layout-order schemes); the play button keeps its mirrored slot.
- [ ] The waveform groove and time axis stay LTR by design and read as
      engineering displays, not as layout errors.
- [ ] Western digits in engineering readouts are intentional and legible
      (D12 policy).
- [ ] The language chip affordance is understandable (EN UI shows «عربي»,
      AR UI shows "EN").
- [ ] Terminology sign-off for the three engines and all transport verbs
      (pitch/formant/air/play/export…): natural for audio work, consistent.
- [ ] The export panel renders on top in RTL (the Phase 4.5 overlay fix)
      and its labels read correctly.

## 5. Performance numbers (current tree)

| Metric | Target | Measured | Environment caveat |
|---|---|---|---|
| Binary (release, stripped) | < 50 MB | 23.8 MB (Linux) | sandbox; release CI re-checks on every tag |
| Cold start | < 1 s | 56–59 ms | headless proxy — no display/GPU init |
| RAM, 3-min 48 kHz session | < 200 MB | 146 MB idle / 196 MB all-stages | Linux, glibc; stereo export transients ~206 MB (disclosed) |
| Preview DSP load | < 40 % core | 3 % offline path; streaming chunks ≈ 50 µs DSP per 10 ms | cached pYIN, all stages engaged |
| Live slider→ear latency | < 20 ms typical | ≈ 10–25 ms by construction (splice at the playhead + one callback); loopback protocol §3.5 | needs hardware for the recorded number |
| Streaming RAM overhead | — | FIFO 100 ms mono + a few stage windows (≤ ~4 MB) | bounded, independent of file length |
| Test suite | all green | 112 tests (56 core + 42 audio + 12 app + 2 doc/examples) | fmt + clippy `-D warnings` clean |

## 6. Honest gaps ledger (current)

1. **Real-hardware validation pending** — §3/§4 protocols exist, not yet run
   (no mic/audio/display in this environment; CI runners are deviceless too).
2. **196 vs 200 MB is a thin margin** on the 3-min stereo worst case;
   stereo-session export transients reach ~206 MB (inherent extra
   full-length buffer; disclosed in Phase 5).
3. **Cold start is a headless proxy** — real desktop numbers belong to §3.6.
4. **PSOLA physics floor** — ≥ 2·T0 grain delay (~35 ms worst case at low
   F0) remains disclosed (D8); the live path does not change it.
5. **Restart artifacts (new, disclosed)** — a live restart renders the splice
   region with partial overlap history (masked by the 10 ms crossfade) and,
   with pitch active, resets the PSOLA read-pointer phase: post-restart
   audio is perceptually equivalent but not sample-exact vs the export
   render. Exports always use the offline render (bit-exact by gate tests).
6. **Streaming preview requires session rate == device rate** — other rates
   fall back to the offline preview path (no resampler on the real-time
   path; disclosed).
7. **Vibrato beyond ±3 % leaks into the residual** (air engine trade-off,
   Phase 3 disclosure); the breath detector is a heuristic.
8. **Synthetic fixtures only** for DSP accuracy gates — real-vocal
   confirmation is §3.2's job.

## 7. Sign-off

- [ ] §3 protocols executed on Windows / macOS / Linux, evidence committed
- [ ] §4 native-Arabic review signed
- [ ] All failures triaged and either fixed or disclosed
- [ ] Final tag `v1.0.0` cut (the `v1.0.0-rc.1` release artifacts are already
      built by CI on the tag push)
