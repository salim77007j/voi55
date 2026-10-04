# Micro-Vocal Lab — Architecture Plan

| | |
|---|---|
| **Document** | docs/ARCHITECTURE_PLAN.md |
| **Phase** | 1 (Research + Architecture) |
| **Date** | 2026-10-04 |
| **Status** | Approved for implementation (Phase 2+) |
| **Evidence** | `docs/research/crates_2026_snapshot.json` — live crates.io API data captured 2026-10-04 |

---

## 1. Product Summary and Hard Constraints

Micro-Vocal Lab is a free, lightweight, Rust-native desktop application for microscopic,
post-recording control of the human voice. It decomposes a vocal recording into three
perceptually independent components — **pitch**, **breath/air**, and **formant/vocal-tract
shape** — and lets the user reshape each with extreme precision while leaving timing and
articulation untouched. The target is a credible free alternative to Melodyne-class tools
for the global 2026 market.

Hard constraints inherited from the product brief (non-negotiable, verified each phase):

1. **Pure Rust**, native on Windows / macOS / Linux; no C++ application layer.
2. **English UI** by default, optional **RTL / Arabic** localization via a runtime switch.
3. Studio-grade capture (**192 kHz / 32-bit float** where hardware allows), import WAV+MP3,
   export WAV + MP3 (configurable bitrate).
4. Three precision, real-time controls:
   - **Pitch Shift**: ±12 semitones, 1-cent resolution, duration and formants preserved (no chipmunk effect).
   - **Air & Breath**: add airy warmth or remove breaths/sibilance, 0.1 dB resolution, clarity untouched (signature feature).
   - **Formant Shift**: perceived vocal-tract size in millimetres; words remain 100 % intelligible; pitch must not move.
5. Non-destructive workflow: export renders the current slider state to a *new* file.
6. Quality bars: real-time preview, binary < 50 MB, RAM < 200 MB, cold start < 1 s,
   `cargo clippy -D warnings` clean, no `unwrap()` on user/audio data, every claim in the
   validation report backed by a screenshot or an audio file.
7. **No-fake-UI**: every control is wired to real DSP. No mockups, no dead buttons.

---

## 2. Ecosystem Research Snapshot (captured 2026-10-04)

Raw evidence: `docs/research/crates_2026_snapshot.json` (26 crates queried live from the
crates.io API). Highlights:

| Crate | Latest stable | Newest release | Downloads | Role in this project |
|---|---|---|---|---|
| `slint` | 1.18.1 | 2026-09-21 | 1.82 M | **Chosen** — declarative UI toolkit |
| `iced` | 0.14.0 | 2025-12-07 | 2.88 M | Rejected — see D1 |
| `egui` | 0.36.2 | 2026-09-08 | 25.1 M | Rejected — see D1 |
| `tauri` | 2.12.1 | 2026-10-01 | 34.1 M | Rejected — see D1 |
| `cpal` | 0.18.2 | 2026-08-16 | 22.8 M | **Chosen** — audio I/O |
| `rodio` | 0.22.2 | 2026-03-05 | 12.2 M | Rejected — see D2 |
| `hound` | 3.5.1 | 2023-09-25 | 19.9 M | **Chosen** — WAV read/write (mature & frozen; WAV format does not move) |
| `symphonia` | 0.6.1 | 2026-08-13 | 15.7 M | **Chosen** — MP3 decode, pure Rust |
| `mp3lame-encoder` | 0.2.5 | 2026-08-20 | 1.11 M | **Chosen** — MP3 encode via LAME bindings |
| `rustfft` | 6.4.1 | 2025-09-18 | 30.6 M | **Chosen** — FFT engine |
| `realfft` | 3.5.0 | 2025-06-12 | 17.7 M | **Chosen** — real-FFT wrapper over rustfft |
| `rubato` | 5.0.1 | 2026-10-01 | 12.3 M | **Chosen** — high-quality async resampling (device-rate adaptation) |
| `ringbuf` | 0.5.2 | 2026-09-13 | 20.1 M | **Chosen** — lock-free SPSC queues for the real-time graph |
| `biquad` | 0.6.0 | 2026-03-22 | 0.41 M | **Chosen** — IIR filter primitives (air shelf, DC block) |
| `fundsp` | 0.23.0 | 2026-01-07 | 0.22 M | Evaluated; not adopted (see D6) |
| `dasp` | 0.11.0 | 2020-05-29 | 5.2 M | Evaluated; stale, not adopted |
| `pitch-detection` | 0.3.0 | 2022-06-30 | 0.07 M | Rejected — YIN/MPM only, unmaintained → in-house pYIN (D7) |
| `pitchy` | 0.2.0 | 2025-05-30 | 0.003 M | Rejected — note/frequency utilities, not a tracker |
| `aubio-rs` | 0.2.0 | 2021-04-30 | 0.02 M | Rejected — stale bindings to a C library |
| `rustybuzz` | 0.20.1 | 2024-11-12 | 32.7 M | Indirect — full HarfBuzz shaping port (Arabic) |
| `cosmic-text` | 0.19.0 | 2026-04-22 | 9.64 M | Indirect — bidi + complex-script text stack (used by Slint) |
| `rfd` | 0.17.2 | 2026-01-12 | 33.0 M | **Chosen** — native open/save file dialogs |
| `crossbeam-channel` | 0.5.17 | 2026-09-05 | 621 M | **Chosen** — UI ↔ engine message passing |

Reading of the 2026 landscape: the Rust audio stack is mature and actively maintained at
the I/O, container, FFT, and resampling layers. The only genuinely weak spot is
**monophonic pitch tracking / PSOLA** — existing crates are stale or minimal. That layer is
the product's core IP anyway, so it is implemented in-house (D7–D10) on top of the mature
primitives (`realfft`, `biquad`, `rubato`, `ringbuf`).

---

## 3. Decision Records

Each record states the decision, the justification, the rejected alternatives, and the
fallback if the decision fails a Phase-gate check.

### D1 — UI framework: **Slint 1.18** (declarative, native)

**Decision.** Slint 1.18.x with the FemtoVG renderer, `.slint` declarative markup, and the
GPLv3 licensing path (project is GPL-3.0-or-later; see §5 licensing note).

**Why Slint wins for this product:**
- **First-class RTL + internationalization.** Slint has built-in bidirectional text and a
  runtime `layout-direction` flip, plus a translation workflow — the Arabic/RTL toggle is a
  supported core feature, not a bolt-on. Its text stack is `cosmic-text` + `rustybuzz`
  (full HarfBuzz shaping port), so Arabic letters connect correctly.
- **Native and light.** Compiles to a small native binary (~10–15 MB range with our
  feature set) with low RAM — directly satisfying the "small binary, low RAM, fast
  startup" constraint. No webview, no browser engine.
- **Declarative + designable.** Property bindings map 1:1 onto our slider/parameter model;
  styling supports the dark, precise "studio hardware" look (custom slider widgets are a
  documented Slint use case).
- **Actively maintained** (1.18.1 released 2026-09-21 — freshest release in its class).

**Rejected alternatives:**
- *Iced 0.14* — solid Elm-style toolkit, but complex-script/RTL support and runtime
  language switching are weaker; the right-to-left requirement is a hard gate, so a weaker
  RTL story is disqualifying.
- *egui 0.36* — excellent for tooling, but immediate-mode text rendering and generic
  widget aesthetics make it hard to hit the "$200 professional audio tool" visual bar, and
  RTL/shaping quality is insufficient for Arabic.
- *Tauri 2.12* — web UI would give the best CSS-level design freedom, but it violates
  "entirely written in Rust" in spirit, adds a webview (RAM + tens of MB binary), and
  forces audio-parameter traffic across an IPC boundary. Wrong trade for a lean native tool.
- *Qt via FFI (CxxQt / qmetaobject-rs)* — polished and RTL-perfect, but drags a C++
  toolchain and Qt licensing complexity into a pure-Rust, free product. Rejected.

**Fallback.** If Arabic shaping or RTL flip fails acceptance in Phase 4 (visual test
matrix), re-evaluate: (a) Slint with pre-shaped text via custom rendering, (b) Iced 0.14 +
`rustybuzz` integration. Decision checkpoint is Phase 4, before UI polish begins.

### D2 — Audio I/O: **cpal 0.18** (capture + playback device layer)

**Decision.** `cpal` 0.18.x for microphone capture and output-stream playback.

**Why:** cpal is the de-facto Rust standard for device-level audio: WASAPI (Windows),
CoreAudio (macOS), ALSA/PulseAudio/JACK (Linux). It exposes native device sample formats
(including f32) and lets us *request* 192 kHz capture with explicit negotiation and
reporting of what the device actually granted — which is exactly the honest behavior we
need for the "192 kHz / 32-bit float" requirement (many consumer mics cap at 48/96 kHz;
the UI will show the true negotiated format, and `rubato` upsamples to the project rate
when the device caps below it — see D8 pipeline). cpal's 22.8 M downloads and steady
2026 release cadence make it the lowest-risk choice.

**Rejected alternatives:**
- *rodio 0.22* — playback-oriented convenience layer over cpal; hides the device control
  we need for precise capture format negotiation and low-latency output.
- *portaudio-rs* — stale bindings to a C library; violates the pure-Rust preference.
- *Raw WASAPI/CoreAudio/ALSA bindings* — three platform layers to maintain by hand; no.

**Fallback.** If a specific platform misbehaves at 192 kHz in Phase 2 testing, fall back
to the device's native rate + `rubato` correction (already the designed path), and file an
upstream issue. No alternate crate needed.

### D3 — WAV I/O: **hound 3.5**

**Decision.** `hound` for all WAV reading and writing (32-bit float and 24/16-bit PCM,
rates up to 192 kHz).

**Why:** hound is the canonical Rust WAV library (19.9 M downloads). It is API-stable and
feature-complete for exactly our needs: f32 samples, arbitrary bit depths, streaming
read/write. The 2023 last-release date is *not* a risk: the RIFF/WAVE format is frozen and
hound is complete for it. Rediscovering this wheel would add risk for zero gain.

**Rejected:** *symphonia for WAV too* — overkill for a format we also need to *write*
(symphonia is decode-only); hand-rolled RIFF writer — needless risk around extensible-WAVE
headers at 192 kHz.

### D4 — MP3 decode: **symphonia 0.6**

**Decision.** `symphonia` 0.6.x (MPEG Layer III feature) for MP3 import, decoding
straight to f32 at the file's native rate.

**Why:** pure Rust, actively maintained (0.6.1, 2026-08-13), no C dependencies to
cross-compile on three platforms, professional-grade decoder accuracy, and it future-proofs
import (FLAC/OGG can be enabled later by adding features, not new crates).

**Rejected:** *minimp3* — C-based bindings; fine, but symphonia is pure Rust and more
maintainable; *rodio's decoder stack* — couples import to playback concerns.

### D5 — MP3 encode: **LAME via `mp3lame-encoder` 0.2**

**Decision.** `mp3lame-encoder` 0.2.5 (safe Rust bindings to LAME) for MP3 export,
CBR 128–320 kbps configurable, default 320.

**Why:** LAME remains the reference MP3 encoder in 2026 — the MP3 patents expired in 2017,
so it is patent-clean and license-viable (LGPL; we satisfy it by shipping source + build
instructions and noting the link in every release). The bindings crate is actively
maintained (0.2.5, 2026-08-20). Audio quality at 320 kbps CBR is transparent for voice.

**Rejected:** *pure-Rust MP3 encoders* — no maintained, quality-competitive pure-Rust
encoder exists in 2026 (checked live: no `shine-mp3` on crates.io; remaining options are
abandoned or quality-deficient). A niche C dependency for *encode only* is the correct
compromise; all other layers stay pure Rust.

**Fallback.** If LAME cross-compilation fails on any CI target in Phase 2/5, options in
order: vendor LAME via `cc` crate build script (it is small, autoconf-free build available),
or ship WAV-first export and MP3 as best-effort per platform — both documented honestly.

### D6 — FFT / DSP primitives: **rustfft 6.4 + realfft 3.5**, plus `biquad`, `rubato`, `ringbuf`

**Decision.** `rustfft` (SIMD-accelerated planning) wrapped by `realfft` for real-valued
STFT work; `biquad` for IIR shelf/DC filters; `rubato` for rate adaptation; `ringbuf` for
lock-free SPSC real-time queues; `crossbeam-channel` for control-rate messaging.

**Why:** this is the mature, boring, correct 2026 stack. rustfft's SIMD planning is
competitive with FFTW for our block sizes without licensing pain.

**Rejected:** *FFTW bindings* — GPL + C toolchain friction across 3 OSes for a marginal
gain at our window sizes (1024–4096); *fundsp 0.23* — elegant live-DSP graph, but our
algorithms are custom analysis-driven processes (PSOLA/LPC) that don't map onto its
operator model; we use its ideas, not its runtime. *dasp* — effectively dormant since 2020.

### D7 — Pitch detection: **in-house pYIN** in `mvl-core`

**Decision.** Implement **pYIN** (probabilistic YIN, Mauch & Dixon 2014) in `mvl-core`,
on top of `realfft`. Outputs per-frame F0 + periodicity (voicing probability) + candidate
T0s. Used by all three engines (PSOLA grain placement, harmonic/residual split, LPC
voicing gating).

**Why:** pYIN is the 2026 reference for monophonic voice pitch tracking: median error of
a few cents on clean vocals, graceful unvoiced handling via the Viterbi transition prior.
No maintained Rust crate provides it (verified live: `pitch-detection` 2022, `aubio-rs`
2021 C bindings, `pitchy` is a utility not a tracker). This is the one place the brief's
"don't reinvent" rule yields to necessity — and it is the core IP of a vocal tool.

**Verification gate (Phase 3):** on synthetic signals (swept sines, vibrato 4–7 Hz ±50
cents, formant-rich vowel stacks) pYIN must hold ≤ ±5 cents median error, ≤ ±20 cents at
95th percentile, and unvoiced frames must be flagged with ≥ 95 % accuracy. Real-vocal
spot checks documented with spectrograms.

### D8 — Pitch Shift engine: **TD-PSOLA (time-domain pitch-synchronous overlap-add)**

**Decision.** ±12 semitones in 1-cent steps via **TD-PSOLA**: epochs from pYIN T0 +
local energy-peak refinement; analysis grains of 2·T0 with Hann windows; synthesis places
grains on a grid spaced by `T0_new = T0 · 2^(−cents/1200)` at *unchanged time positions*.

**Why PSOLA is the most natural choice for voice in 2026:**
- **Duration is preserved by construction** — synthesis time positions equal analysis
  positions, so timing and articulation are bit-stable; only the local repetition rate of
  grains changes. No time-stretch side effect, no chipmunk effect.
- **Formants are preserved** — grain spectra are copied from the original signal around
  each epoch, so the vocal-tract envelope rides along unchanged; pitch moves *inside* the
  same envelope. This is exactly the perceptual independence the brief demands.
- Time-domain grain work means **no phase-vocoder smearing** on voice: no metallic ring,
  no transient loss — the documented weaknesses of phase-vocoder pitch shifting on speech.

**Guardrails engineered in:** unvoiced/aperiodic frames (breath consonants, sibilants,
silence) bypass PSOLA via copy-through with crossfades (aperiodicity from pYIN decides);
overlap-add is energy-normalized; grain-boundary fades eliminate clicks; ±12 st extremes
use grain resampling to stay artifact-light.

**Rejected alternatives:**
- *Resampling/varispeed* — changes duration; chipmunk effect. Disqualifying.
- *Phase vocoder (Laroche–Dolson peak locking)* — good on polyphonic music, but on solo
  voice it smears transients and adds the classic "phasiness" at large shifts.
- *WSOLA/SPS* — time-scale oriented; timing blur violates the articulation constraint.
- *Neural (DDSP / voice conversion)* — best-in-class timbre work exists, but requires
  multi-MB models, inference time or GPU, training-set bias, and nondeterminism; violates
  "lightweight, stable, 1-cent deterministic precision" for v1. Documented as a future
  optional module behind a plugin boundary (D14), not part of the core engine.

**Latency honesty:** PSOLA's algorithmic latency floor is ≈ 2·T0 (about 10 ms at 200 Hz,
~20 ms at 100 Hz, ~25 ms at 80 Hz male F0). The < 20 ms preview target is met for F0 ≳
120 Hz in low-latency mode (512-sample cpal buffer + short-window analysis); below that,
physics wins and preview latency tops out around 30–35 ms. Offline export is always exact.
This trade-off is disclosed in the UI status bar and the validation report rather than
hand-waved.

### D9 — Formant Shift engine: **LPC source–filter decomposition + allpass envelope warping**

**Decision.** Per-frame **LPC analysis** (autocorrelation + Levinson–Durbin, order 24–40
scaled by sample rate, pre-emphasis) splits the signal into *excitation* (residual) and
*spectral envelope*. The envelope is resampled on a Bark-scale grid and frequency-warped
with a first-order allpass warping chain whose coefficient maps to the vocal-tract length
ratio `r = 175 mm / target_mm` (average adult VTL ≈ 175 mm; formants scale ≈ 1/L). The
excitation is re-filtered through the warped envelope. Pitch is untouched because the
excitation's periodicity is untouched; words stay intelligible because the envelope *shape*
is preserved, only its frequency placement warps.

**Control mapping.** Slider range **130–190 mm** (child ↔ large male), 1 mm resolution,
displayed as mm with the implied formant scaling factor. F ≈ F₀ · (175 / L): 130 mm →
×1.35 (about +5.1 semitone-equivalent formant lift), 190 mm → ×0.92 (about −1.5). This
covers the "child ↔ large man ↔ woman" transformations in the brief without leaving the
natural regime.

**Guardrails engineered in:** per-frame envelope smoothing + crossfaded block processing
(no timbral zipper); warp limiter keeps F1–F3 in anatomically plausible bands; voiced/
unvoiced-dependent LPC gain compensation keeps loudness stable; sibilance bands (unvoiced)
are warped gently to avoid lisping artifacts.

**Rejected alternatives:**
- *Cepstral liftering envelope* — more ripple-sensitive and less stable on breathy voice.
- *Formant tracking + explicit peak shifting* — brittle when F1/F2 merge or on breathy
  onsets; the allpass-warp method needs no discrete formant peaks, which is why it wins.
- *Phase-vocoder formant moving* — inherits vocoder phasiness; also couples to pitch path.
- *Neural voice conversion* — same objections as D8 for v1.

### D10 — Air & Breath engine: **STFT harmonic/residual decomposition with independent gain** (signature feature)

**Decision.** A two-path STFT processor (window 2048 @ ≤ 48 kHz equivalents, scaled at
higher rates; 75 % overlap, Hann):

1. **Harmonic path** — pitch-synchronous comb tracking driven by pYIN F0 + time-frequency
   median filtering (harmonic-percussive style, Fitzgerald 2010) isolates the voiced
   component. This path is passed through with ≤ 0.01 dB deviation when the slider is
   centered — clarity is contractually untouched.
2. **Residual path** — the difference signal: breath noise, aspiration, fricatives,
   sibilance, room air. The slider applies a *gain in dB, 0.1 dB resolution* to this path:
   - **Positive (add air):** residual boosted with a gentle spectral tilt (+6 dB/octave
     above ~6 kHz) so added air reads as *warm* breathiness, not hiss; optional
     pitch-locked modulation keeps the noise organically tied to phonation.
   - **Negative (remove):** de-esser-style attenuation concentrated in 5–12 kHz plus
     downward expansion of the residual during inter-phrase breath detection (spectral
     flatness + energy envelope signature), enabling surgical breath removal without
     touching the voiced tone.

**Why:** harmonic/residual separation is the only architecture that satisfies "musical,
not binary": it is continuous (0.1 dB), per-frequency-band adaptive, and provably cannot
affect the harmonic clarity path. Static EQ cannot separate breath from tone; broadband
gates are choppy; neural enhancement is overkill and opaque.

**Rejected:** *static high-shelf EQ* — dulls consonants along with everything else;
*expander/gate* — binary-sounding, choppy onsets; *spectral subtraction* — musical noise
artifacts; *neural speech enhancement* — heavy, nondeterministic, and trained-taste bias.

### D11 — Real-time pipeline and latency budget

Processing graph (single `mvl-core::Engine`, control-rate parameters via crossbeam, audio
via `ringbuf` SPSC):

```
[cpal capture] → ringbuf → [Engine: pYIN → PSOLA ⇄ LPC-warp ⇄ Air/Breath] → ringbuf → [cpal playback]
[project buffer (f32, 24–192 kHz)] ↖ offline render path uses the same Engine, analysis-quality windows
```

Order is fixed: **pitch first** (PSOLA works on clean excitation), **formant second**
(LPC on pitch-stabilized signal), **air last** (operates on the finished voice). Sliders
are hot-swappable in preview; the engine crossfades parameter changes over ~10 ms.

| Latency component (preview, 48 kHz) | Budget |
|---|---|
| cpal device buffer (512 samples, negotiated smaller where possible) | ~5–10 ms |
| Engine block + crossfades | ~5 ms |
| PSOLA algorithmic (2·T0) | 10 ms @ 200 Hz … 25 ms @ 80 Hz |
| **Total preview** | **~20 ms (F0 ≥ 120 Hz) … ~35 ms (low male F0)** |

Offline export latency: unbounded, quality-maximal windows. This is the honest physics of
the chosen algorithms, disclosed up front (see D8 honesty note).

### D12 — Fonts and text rendering: **Inter + IBM Plex Sans Arabic**, shaped by cosmic-text

**Decision.** Bundle **Inter** (SIL OFL) for Latin UI with *tabular numerals* enabled for
all value readouts (1-cent and 0.1 dB displays must not jitter while dragging). Bundle
**IBM Plex Sans Arabic** (SIL OFL) as the Arabic UI face. Rendering/shaping via Slint's
`cosmic-text` + `rustybuzz` stack (correct Arabic joining, bidi mirroring, RTL line
order).

**Why Inter:** designed for screens (tall x-height, excellent hinting), a 2026 industry
standard for tool UIs, full weight range for a precise type hierarchy, OFL license with
no redistribution friction. **Why IBM Plex Sans Arabic:** harmonized x-height with
humanist sans faces, genuine Naskh-influenced structure that stays legible at UI sizes,
OFL. Numerals stay Western digits in both directions for engineering readouts.

**Rejected:** *Roboto/System UI* — platform-inconsistent across the three OSes;
*Noto Sans* family — fine but less distinctive at display sizes; *SF Pro/Segoe* — license
and platform-locked.

### D13 — Design language: **"Studio Graphite"** (dark, hardware-referential)

**Decision.** A dark, matte, studio-rack-inspired design system, specified before any
slint file is written:

| Token | Value | Usage |
|---|---|---|
| `bg/base` | `#12151A` | window background (never pure black) |
| `bg/surface` | `#1A1F26` | panels, waveform well |
| `bg/elevated` | `#212831` | toolbar, dialog chrome |
| `border/subtle` | `#2C3440` | 1 px hairlines, dividers |
| `text/primary` | `#E7EBF0` | primary text (≥ 12.4:1 on base) |
| `text/muted` | `#8C97A6` | secondary text (≥ 4.9:1) |
| `accent/pitch` | `#5AA7FF` | pitch slider, pitch trace |
| `accent/air` | `#46D6A5` | air slider, residual trace |
| `accent/formant` | `#B78CFF` | formant slider, envelope trace |
| `state/record` | `#FF4D4F` | record state, clip meters |
| `state/play` | `#3DDC84` | play state, transport |
| `state/warn` | `#FFB020` | device fallback notices |

Geometry and motion: 8 pt spacing grid; radius scale 6/10/14 px; focus ring = 2 px accent
outline + 1 px offset (keyboard-visible on every control); hover = +4 % surface
lightness; press = +8 %; slider drag and zoom transitions 120–160 ms
`cubic-bezier(0.2, 0.7, 0.3, 1)`. Custom icon set: 20 px, 1.75 px stroke, geometric —
microphone, folder-import, export, play/stop, reset, language — drawn as SVG paths
compiled into the app (no emoji, no placeholder glyphs). Waveform: per-pixel peak/RMS
columns, voiced regions tinted by pitch confidence, sub-millisecond zoom with decimated
min/max pyramids for instant redraw at any zoom depth.

Accessibility: WCAG AA contrast on all text tokens (checked in table above); every
control exposes a tooltip; all interactive elements reachable and operable by keyboard
(Tab order mirrors visual order in LTR *and* RTL).

### D14 — Testing, QA, and evidence policy

- **Unit (mvl-core):** pYIN accuracy gates (D7); PSOLA duration invariance (±0 samples);
  pitch-only shift leaves spectral-centroid-of-envelope within tolerance; formant warp
  leaves F0 within ±1 cent; air gain linearity verified at 0.1 dB steps; no NaN/Inf on
  pathological input (silence, DC, full-scale noise, 192 kHz chirps).
- **Round-trips (mvl-audio):** WAV f32/24/16-bit @ 44.1–192 kHz write→read equality;
  MP3 encode→decode sanity (SNR bounds); rubato rate-conversion accuracy.
- **Golden-audio regression:** committed small fixtures (`assets/audio/`) with SNR /
  spectral-distance thresholds so quality regressions fail CI, not ears.
- **Integration:** engine end-to-end on real vocal samples; before/after WAVs committed
  to `assets/audio/before_after/` as *required evidence* for any "natural output" claim.
- **Static quality:** `cargo fmt --check`, `cargo clippy -D warnings`, no `unwrap()` on
  user/audio paths (enforced by review + grep gate in CI), documented public APIs.
- **Memory:** ASan build in nightly CI; Valgrind run recorded in the Phase 5 report.
- **Evidence policy (from the brief):** every validation-report claim must cite a
  screenshot or audio file; "works" without evidence is treated as "missing".

### D15 — Workspace layout, licensing, and CI

**Workspace** (single repository, three crates — engine stays UI-agnostic and testable):

```
voi55/
├── crates/
│   ├── mvl-core/    # DSP: pYIN, PSOLA, LPC warp, air/breath — no I/O deps, no unsafe
│   ├── mvl-audio/   # cpal I/O, hound/symphonia/LAME codecs, rubato, playback graph
│   └── mvl-app/     # Slint UI, app state, engine wiring, i18n (en + ar), rfd dialogs
├── assets/          # audio fixtures, fonts, icons
├── docs/            # this plan, phase reports, worklog
└── .github/workflows/ci.yml
```

**Licensing:** project code **GPL-3.0-or-later** — chosen because Slint's zero-cost path
for free desktop apps is GPLv3, and copyleft protects the "free Melodyne alternative"
mission from closed forks. LAME (LGPL) is linked per its terms; fonts are OFL.

**CI (Phase 5):** GitHub Actions matrix `windows-latest`, `macos-latest`,
`ubuntu-latest` (ALSA dev packages + Xvfb for headless UI smoke tests); build +
test + clippy + fmt per target; release artifacts uploaded per platform; nightly ASan job.
MSRV pinned in `Cargo.toml`; lockfile committed for reproducible builds.

---

## 4. Performance Budget and Verification Plan

| Metric | Target | Verification method |
|---|---|---|
| Preview round-trip latency | < 20 ms typical (≥ 120 Hz F0), ≤ 35 ms worst case | loopback measurement documented in Phase 5 |
| Binary size (release, stripped) | < 50 MB | CI artifact size check |
| RAM (typical 3-min 48 kHz session) | < 200 MB | runtime RSS sampling in integration test |
| Cold start | < 1 s | CLI timing harness (Phase 5) |
| Preview DSP load | < 40 % of one core @ 48 kHz | criterion benchmark in `mvl-core` |
| Memory leaks | none | ASan nightly + Valgrind Phase 5 |

## 5. Risk Register

| Risk | P | Impact | Mitigation |
|---|---|---|---|
| Arabic shaping/RTL edge cases in Slint | M | High (hard requirement) | Phase 4 acceptance matrix with native-reviewer checklist; fallback D1 |
| LAME build friction on a CI target | M | Medium | vendor-build fallback (D5); WAV-first export honesty |
| Device caps below 192 kHz | H | Low (expectation) | honest negotiated-format display + rubato correction (D2) |
| Low-F0 preview latency > 20 ms | H | Low (physics) | disclosed trade-off, offline exact (D8/D11) |
| pYIN on breathy/rough voice | M | Medium | voicing-gated bypass, Phase 3 gates on hard fixtures |
| Scope creep in UI polish | M | Medium | design tokens fixed in D13 before implementation |

## 6. Phase Mapping

| Phase | Delivers | Key gates |
|---|---|---|
| 2 | Workspace + cpal capture (192 kHz/f32) + WAV/MP3 import-export + player + round-trip tests | I/O round-trips green on dev machine |
| 3 | pYIN, PSOLA pitch, LPC formant warp, air/breath engine + fixtures + before/after audio | D7 accuracy gates met |
| 4 | Slint UI (EN LTR + AR RTL), waveform zoom, wired sliders, status bar, screenshots | No-fake-UI audit: every control traced to engine |
| 5 | 3-OS CI + artifacts + full test matrix + runtime verification + screenshots | all green on CI, app runs from artifact |
| 6 | VALIDATION_REPORT.md, performance numbers, honest gaps, tag v1.0.0 | every claim carries evidence |

## 7. References

- Moulines & Charpentier, *Pitch-synchronous waveform processing techniques for
  text-to-speech synthesis using diphones* (Speech Communication, 1990) — PSOLA.
- Mauch & Dixon, *pYIN: A Fundamental Frequency Estimator Using Probabilistic Threshold
  Distributions* (ICASSP 2014) — pitch tracking.
- Härmä & Laine, *Warping the unit circle to approximate Bark scale* — allpass frequency
  warping for envelope manipulation.
- Fitzgerald, *Harmonic-Percussive Source Separation using Median Filtering* (DAFx 2010)
  — harmonic/residual split for the air engine.
- Titze, *Principles of Voice Production* — vocal-tract length and formant scaling.
- Laroche & Dolson (1999) — phase-vocoder peak locking; reviewed and rejected for solo
  voice (see D8).
