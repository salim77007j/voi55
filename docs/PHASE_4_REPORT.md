# Phase 4 Report — Slint UI (EN LTR · AR RTL · Waveform · Wired Sliders · No-Fake-UI Audit)

**Status: COMPLETE.** Branch `main`, head `02583b1` + this commit.
All sub-items were committed and pushed individually (4.1 → 4.6).

## What shipped

| Sub-item | Commit | Deliverable |
|---|---|---|
| setup | `4231102` | Reproducible sandbox env scripts (rustup path + local ALSA prefix) |
| 4.1 | `bfda300` | Slint scaffold: Studio Graphite shell (D13 tokens), embedded Inter + IBM Plex Sans Arabic, headless screenshot platform + PNG evidence pipeline |
| 4.2 | `bbc7a64` | Waveform engine: min/max/power pyramid, voiced tint + F0 trace, wheel-zoom/drag-pan, playhead, session module, `--demo/--open/--window/--playhead` flags |
| 4.3 | `fa9eabc` | Three precision sliders wired to the engine (1 ¢ / 0.1 dB / 1 mm), EngineParams clamp+echo, debounced background render, A/B preview toggle |
| 4.4 | `6d0672a` | Transport, import/export, recording (No-Fake-UI): cpal Player/Recorder, WAV f32/24/16 + MP3 128–320 export panel, rfd import with automation overrides |
| 4.5 | `02583b1` | i18n: runtime EN LTR ↔ AR RTL switch, exact-mirror layout scheme, localized status/labels, LRM-isolated technical fragments, evidence corpus |
| 4.6 | this | No-Fake-UI audit + phase report |

**Quality gates at HEAD:** 97 tests green (50 `mvl-core` + 33 `mvl-audio` + 14 `mvl-app` incl. the pixel-asserting screenshot integration test), `cargo fmt` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean, no `unwrap()`/`expect()` on user/audio data in production paths.

## 1. UI architecture

- **Slint 1.18 (D1)** with `mvl-app` split into lib + bin: the GUI binary, the
  headless screenshot renderer and the integration tests drive the exact same
  `App` code path. Slint owns layout/widgets; Rust owns all state — the
  generated bindings expose only properties/callbacks, and every callback
  terminates in real logic (audit below).
- **Studio Graphite (D13)** design tokens in `tokens.slint`: dark surfaces,
  three parameter accents (pitch/air/formant), 2 px focus rings, hover/press
  surface lightness. The screenshot test asserts rendered pixels against the
  token spec, not against widget constants.
- **Fonts (D12)** embedded at compile time: Inter (Regular/Medium/SemiBold) for
  the Latin UI, IBM Plex Sans Arabic (Regular/Medium/SemiBold) for the RTL UI.
  The active font family follows the language table at runtime.

## 2. Waveform engine (4.2)

The waveform is not an image decal: `WaveformPyramid` is built from the actual
`AudioBuffer` (adaptive bin count from duration), and every frame Rust renders
peak/RMS columns for the current viewport from the pyramid + mono cache, with:

- voiced-region tint and an F0 trace taken from the **shared pYIN analysis**
  (one analysis, many consumers — D11);
- wheel-zoom anchored at the pointer, drag-pan, −/+ and fit controls, zoom
  span readout (`view 2.50 s` → `view 120 ms` → `view 50 µs`);
- a playhead overlay driven by the real `Player::position_secs` while playing;
- an A/B chip that swaps the view to the *rendered* preview (dropping the
  input-derived tint/trace, because they describe the input, not the render).

Session state (project buffer, name, view, pyramid, pYIN track) lives in
`session.rs`; `--demo synth` provides a deterministic built-in vocal for
evidence and tests.

## 3. Precision sliders → engine (4.3)

Each slider is a custom widget with studio-hardware interaction:

- drag anywhere on the groove, click-to-jump, double-click → neutral
  (0 st / 0 dB / 175 mm);
- Left/Right/Up/Down → one fine step (**1 ¢ / 0.1 dB / 1 mm**), PageUp/PageDown
  → coarse (**1 st / 1 dB / 10 mm**);
- every change writes `EngineParams` — the **same clamping contract the DSP
  reads** — and the widget echoes the canonical snapped value back, so the UI
  can never display a value the engine did not apply;
- changes debounce 250 ms and render on a background thread through
  `mvl_audio::engine::render_mono_to_buffer` with the cached pYIN track; the
  finished render lands via an mpsc channel polled by a UI-thread timer (Slint
  work stays on the Slint thread). The status bar shows real render time.

## 4. Transport, import/export, recording (4.4)

- **Playback**: `play/pause/stop/rewind` on `mvl_audio::Player` (cpal output
  stream, connected lazily — a machine without a device gets an honest status
  message, and export still works). Playing with dirty parameters renders
  synchronously first, so playback always matches the sliders. Rewind shares
  `stop()`'s semantics (position resets to 0).
- **Recording**: `Recorder` (cpal capture) requests 192 kHz/f32 and discloses
  the negotiated rate when the device caps it; the capture ring reports
  overflow honestly. Stopping loads the take into the session like any import.
- **Export**: the panel selects f32/24-bit/16-bit WAV or MP3 (bitrate
  128–320, D5); confirming always **renders fresh from the current sliders**
  and writes a *new* file (non-destructive, brief §5), reporting path + time.
  File dialogs come from `rfd` (xdg-portal on Linux, native elsewhere), with
  `MVL_OPEN_FILE`/`MVL_SAVE_FILE` env overrides so CI/evidence drives the same
  downstream code a user's dialog selection would. An export round-trip
  (render → write → re-import → frame-exact compare) runs in the test suite.
- **Import**: WAV/MP3 dispatch by extension; decode errors surface verbatim in
  the status bar.

## 5. i18n — runtime EN LTR ↔ AR RTL (4.5)

- **Explicit string tables** (`i18n.rs`, no gettext): every user-visible
  string exists in EN and AR in one testable place; table completeness and
  template-fill are unit-tested (a `{key}` left unfilled fails the test).
- **Language switch** is a header chip (`toggle-language`) and `--lang` CLI:
  it swaps all strings, the embedded font family, and the `rtl` layout flag at
  runtime, then rebuilds composed lines from session state. The chip shows the
  *target* language as the affordance (EN UI → «عربي», AR UI → "EN").
- **RTL mirroring**: Slint 1.18 shapes bidi text natively but does not mirror
  layouts; every container binds its children's `layout-order` to the `rtl`
  flag. Header, transport bar, status bar, the slider row and each slider's
  label/readout row carry exact-mirror schemes (fixed collisions from the WIP
  draft: duplicated order values would have relied on declaration-order
  tiebreaks). The export panel anchors under the export button in both
  directions.
- **Disclosed scope decisions** (D12, consistent with DAW conventions):
  - the audio **time axis, groove geometry and zoom controls stay LTR** in
    both languages;
  - transport **positions mirror**; glyphs keep their playback-direction
    meaning (the play glyph points the way audio moves on the LTR timeline);
  - **Western digits** and Latin unit symbols (`st`/`dB`/`mm`/`Hz`) remain in
    engineering readouts in both languages; numeric fragments are wrapped in
    LTR marks (U+200E) inside Arabic text so bidi never reorders them;
  - OS file-dialog chrome (rfd) stays English — it is OS surface, not app UI.

## 6. No-Fake-UI audit (phase gate)

Every control in the window traced to its implementation:

| Control | Callback / source | Real implementation |
|---|---|---|
| Language chip | `toggle-language` → `App::set_language` | Swaps string table + font + `rtl`; recomposes status; screenshot-tested (AR header pixel-diff > 500 px) |
| Import button | `import-clicked` → `dialogs::pick_audio_file` → `load_any` | rfd / `MVL_OPEN_FILE` → WAV/MP3 decode → `Session::load` + pYIN analysis |
| Export button | `export-clicked` | Opens/closes the export panel (real panel state) |
| Format chips f32/24/16/MP3 | `export-format-changed` | `export_format` drives `WavBitDepth` vs LAME branch in `export_confirm` |
| Bitrate chips 128–320 | `export-bitrate-changed` | `export_bitrate` is the LAME CBR argument |
| “Choose location & export” | `export-confirm` → `App::export_confirm` | Fresh render from current sliders → `export_wav`/`export_mp3` → path+ms status; round-trip test |
| Waveform wheel / drag | `wave-zoom` / `wave-pan` → `View::zoom_at`/`pan` | Viewport math on the real session view; columns re-rendered per frame |
| −/+ / fit / A/B chips | `zoom`, `fit`, `toggle_preview` | Same view/preview state machine the GUI gestures use |
| Waveform image + F0 trace | Rust-rendered each frame | `WaveformPyramid` + `waveform::draw` from the actual buffer/pYIN |
| Playhead | `set_playhead` from the 40 ms poller | `Player::position_secs` — real transport position |
| Slider ×3 (drag/keys/dbl-click) | `pitch/air/formant-changed` → `EngineParams::set_*` | Canonical clamp/snap echoed back → debounced background render (real DSP) |
| Slider readouts | `fmt_pitch/air/formant` | Composed from `EngineParams`, not widget state |
| Play/Pause | `play-pause` → `App::play_pause` | cpal `Player` connect/play/pause/resume; honest no-device status |
| Stop / Rewind | `transport-stop` / `transport-rewind` | `Player::stop()` (transport Stopped, position 0) |
| REC | `record-toggle` → `Recorder::start/stop` | Real cpal capture; negotiated-rate + overflow disclosures; take loads into session |
| Time readout / REC clock | 40 ms poller | `Player::position_secs` / capture `Instant` |
| Status bar | `compose_status` | Live state: loaded line from session+pYIN stats, render timing, verbatim errors |

**Informational elements** (brand, version, empty-state hint, track label,
zoom readout) are not controls; each is composed from live state in the active
language. **No dead controls remain.**

## 7. Bugs found and fixed during Phase 4

1. **Export panel painted under the waveform well** (found in 4.5): the panel
   was a header child overflowing the 56 px header, but later layout siblings
   painted over it — invisible ever since 4.4 despite working exports. Moved
   to a window-level overlay (last child paints on top); verified in EN and AR
   evidence PNGs.
2. **`layout-order` collisions in the WIP RTL draft**: duplicated order values
   (spring/language chip, play/stop) would have relied on declaration-order
   tiebreaks; replaced with exact-mirror schemes that are symmetric by
   construction.

## 8. Evidence (D14 — claims carry pixels)

`docs/evidence/phase4/`, all rendered headlessly by the real binary
(`--screenshot`, software renderer, deterministic clock):

| File | Shows |
|---|---|
| `ui-en-shell.png` | Empty EN shell: header, sliders, transport, status, empty-state hint |
| `ui-en-demo.png` | Demo vocal: waveform + voiced tint + F0 trace, sliders at +4 st/+5.5 dB/140 mm, render timing in status |
| `ui-en-preview-ab.png` | A/B view: rendered preview (formant accent color) |
| `ui-en-export.png` | Export panel open (f32 active; bitrate row correctly hidden) |
| `ui-ar-rtl.png` | Same state as `ui-en-demo.png` in Arabic: mirrored header/sliders/transport, Arabic font shaping, RTL status with LTR numeric islands |
| `ui-ar-export.png` | Arabic export panel anchored under the mirrored export button |

The automated form lives in `tests/screenshot.rs`: background tokens, empty-state
visibility, accent-pitch slider fill, status text, export round-trip, Arabic
status line, and an LTR-vs-RTL header pixel-diff are all asserted on real
rendered pixels.

## 9. Honest gaps

- **Synthetic audio only in evidence** — the sandbox has no microphone; the
  demo vocal and exported renders are synthetic. Real-vocal capture and
  screenshots on real hardware are deferred to Phase 5/6 (as planned).
- **Playback/recording not exercised in-sandbox** (no audio device): covered by
  the Phase 2 null-device self-test, the honest-status code paths, and unit
  tests of the underlying transport state machines.
- **No native-Arabic reviewer sign-off yet** — the plan's risk register calls
  for a native-reviewer checklist (D1 fallback checkpoint). The mirroring and
  shaping are verified by screenshots and pixel tests; a native speaker should
  review wording quality in Phase 6 validation.
- **Dialog chrome and unit symbols stay English/Latin** — deliberate, disclosed
  scope (OS surface + engineering notation), not omissions.
- Performance budgets (binary < 50 MB, cold start < 1 s, RSS < 200 MB) are
  Phase 5/6 verification items; debug-build renders here take ~550 ms for a
  2 s demo (release profile is substantially faster and is what ships).

**Next up (Phase 5):** 3-OS CI (GitHub Actions matrix), release artifacts,
full test matrix per target, runtime verification — awaits the explicit "continue".
