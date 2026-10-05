# Phase 7.2 Report — Professional Studio UI Redesign

**Status: COMPLETE** · All 131 tests green · fmt/clippy `-D`-clean · HEAD `this commit`

## 1. Mission

Real users accepted the DSP but called the rc.1 UI "a student project." Phase 7.2
redesigns every screen to professional studio grade against the mandatory visual
target: `reference.png` (1280×698 DAW screenshot uploaded to the repo root),
fused with the task-book palette and typography spec.

## 2. Reference analysis (mandatory visual step, performed first)

`reference.png` (JPEG payload, 1280×698) shows "AUDIOPRECISE PRO v3.5". The
visual grammar extracted and adopted:

| Element | Reference language | Adopted here |
|---|---|---|
| Chrome | Medium-dark gray panels, 1 px beveled borders | bg #1E1E1E / panels #252526 / borders #3C3C3C |
| Displays | Inset near-black wells (waveform/EQ/RTA) | wells #0A0A0A + grid #2A2A2A |
| Knobs | Dark radial-gradient circles, pointer + readout | 76 px knobs, LED position ring, pointer dot |
| Meters | Green→yellow vertical scales, tick labels | L/R meters, 3-zone scale, peak-hold tick |
| Transport | Filled colored buttons (green play / red record) | 56 px green play, 48 px red record, amber rew/FF |
| Time | Dark LCD, large mono digits | LCD #101418, IBM Plex Mono 20 px |
| Labels | Dense small-caps, engineering readouts | 10 px letter-spaced captions + mono values |

## 3. What was built (sub-items a–e, each committed)

- **7.2a** `e00fcca` — "Studio Console" tokens (single global; no hardcoded
  colors anywhere), IBM Plex Sans/Mono/Arabic embedded (Inter retired),
  waveform rasterizer: vertical cyan→coral peak gradient (#00B4D8→#FF6B6B),
  amber playhead, softened voiced tint.
- **7.2b** `cd7d074` — full layout rebuild: menu bar (File/View/Audio/Help,
  dropdown panels with deterministic open-id state), toolbar (brand + Record/
  Import/Export icon-caption actions + devices + language), three-column body
  (info rail / ruler+well / control column), LCD transport, segmented status
  bar. `Player::set_volume` (atomic gain applied in the RT callback),
  `Player::seek_to_frame`, drag selection + zoom-to-selection, ruler tick model
  (nice 1-2-5 steps from the real view span; same model draws the well grid).
- **7.2c** `c471a87` — control strips: 76 px knobs with 21-dot LED value ring
  (270° sweep, min 7:30 / max 4:30), vertical-drag + wheel + double-click-reset
  + full keyboard support (arrows = 1 step, PageUp/Down = coarse), reset chips,
  neutral LEDs, accent edge bars, precision groove kept.
- **7.2d** `c612da5` — real metering: lock-free `MeterTap` (output post-gain,
  input during recording), meter ballistics with 2 s peak hold, RSS+DSP status
  segment (proc/statm · mach task_info · GetProcessMemoryInfo, target-gated).
- **7.2e** `68adff7` — real-FFT spectrum: 4096-slot atomic ring tap → 2048-pt
  Hann → rustfft → 48 log bands (40 Hz–16 kHz) → dB floor −64 → bars + peak
  hold/decay rendered per-panel-size; decays honestly to silence when stopped.

## 4. No-Fake-UI audit

Every visible control drives or displays real state: menus→commands the
toolbars also expose; volume→RT gain; FF→real cursor jump (streaming path
re-renders via worker restart); meters→atomic taps drained at 40 ms;
spectrum→FFT of the actual output signal; info strip→session/pYIN values;
status segments→real state, hidden when unknown. The headless spectrum well is
honestly empty (sandbox has no audio device; the analysis core is unit-tested:
440 Hz sine peaks in band 19 = 429–485 Hz, silence → silent bars).

## 5. Honest deviations from the task book

1. **No Edit menu** — there are no real edit commands (one-track engine); an
   empty menu would violate No-Fake-UI. File/View/Audio/Help ship.
2. **No solo buttons / per-strip VU arcs** — one track: nothing to solo. The
   LED ring shows parameter position (the only per-strip quantity that exists);
   real signal meters live in the transport where signal actually flows.
3. **No bypass toggles** — `EngineParams` has no bypass stage; faking one with
   a neutral-value trick would misrepresent engine state. Reset-to-neutral is
   provided instead (double-click / reset chip).
4. **"CPU" reads DSP duty** — the audio callback's measured cost vs budget
   (EMA). Per-process total CPU would need OS APIs per platform; the DSP
   figure is the honest, cross-platform audio-load metric.
5. **Recording-state screenshots** — the sandbox has no microphone; REC-state
   visuals (pulse + LCD clock) are code-verified but not screenshot-evidenced
   here; platform runs (7.3) will capture them on real hardware.
6. **Selection by drag** requires toggling the SEL chip (Slint TouchArea
   exposes no keyboard-modifier state, so Ctrl+drag is not detectable).

## 6. Evidence (docs/evidence/phase7/, all headless renders of the real app)

`ui72b-layout-en.png` (main view) · `ui72b-layout-ar-devices.png` (full RTL +
devices) · `ui72b-layout-export.png` · `ui72c-knobs-en/ar.png` ·
`ui72d-meters-en/ar.png` · `ui72e-spectrum-en/ar.png` · `ui72-shell-empty.png`
· `ui72-zoom-subms.png` (800 µs view, µs ruler) · `ui72-preview-ab.png` ·
`ui72-about-en.png` · `ui72-devices-en.png` · `ui72-export-ar.png` ·
`ui72-ar-zoom.png` — plus the rc.1 "before" shots in `phase4/` and
`release/rc1-linux-artifact-smoke.png` for the before/after comparison.

## 7. Gaps → next phases

- **7.3**: per-platform end-to-end runs (Windows 10/11, macOS 14/15, Ubuntu
  24.04, Fedora 41) + screenshots incl. live playback/spectrum/REC states;
  the analyzer and meters must be shown with a real device (sandbox cannot).
- **7.4**: fuzzer/stability/RAM/latency quality gates, final report, v1.0.0.

## 8. Verdict

The UI now matches the reference's visual grammar with the mandated palette
and IBM Plex typography; every pixel shown traces to engine state. 131 tests
green, warnings clean. Phase 7.2 is done; awaiting "continue" for 7.3.
