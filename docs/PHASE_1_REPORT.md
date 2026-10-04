# Phase 1 Report — Research + Architecture Plan

| | |
|---|---|
| **Date** | 2026-10-04 |
| **Scope** | Ecosystem research, technology selection, architecture document |
| **Status** | **Complete** |

## What was done

1. **Repository secured.** Cloned `salim77007j/voi55` (found empty — fresh start),
   git identity configured, `main` branch initialized.
2. **Live ecosystem research executed.** Queried the crates.io API for 26 candidate
   crates; raw evidence committed at `docs/research/crates_2026_snapshot.json`
   (captured 2026-10-04). Key findings:
   - Slint 1.18.1 (2026-09-21) — freshest release among UI candidates; only candidate
     with first-class RTL/bidi + runtime language switching.
   - cpal 0.18.2, symphonia 0.6.1, rubato 5.0.1, ringbuf 0.5.2 — all actively maintained.
   - `mp3lame-encoder` 0.2.5 (2026-08-20) — maintained LAME bindings exist; no
     quality-competitive pure-Rust MP3 encoder exists (`shine-mp3` absent from crates.io).
   - Pitch-tracking crates are stale/minimal (2021–2022) → in-house pYIN justified.
3. **All technology decisions made and documented** in `docs/ARCHITECTURE_PLAN.md`
   (477 lines, 15 decision records, each with rejected alternatives and fallbacks):
   - UI: **Slint 1.18** (GPLv3 path) — rejected Iced, egui, Tauri, Qt-FFI.
   - Audio I/O: **cpal 0.18** — rejected rodio, portaudio-rs, raw platform APIs.
   - WAV: **hound 3.5** · MP3 decode: **symphonia 0.6** · MP3 encode: **LAME bindings**.
   - FFT: **rustfft + realfft** — rejected FFTW, fundsp runtime, dasp.
   - Pitch shift: **in-house pYIN + TD-PSOLA** — rejected varispeed, phase vocoder,
     WSOLA, neural (v1); formants: **LPC + allpass Bark-scale envelope warping** in mm;
     air/breath: **STFT harmonic/residual decomposition**, 0.1 dB, de-esser + de-breath.
   - Fonts: **Inter + IBM Plex Sans Arabic** (OFL). Design language: **"Studio Graphite"**
     with full token table, 8 pt grid, WCAG AA contrast targets.
4. **Latency honesty documented up front:** PSOLA algorithmic floor ≈ 2·T0 → preview
   < 20 ms for F0 ≥ ~120 Hz, up to ~35 ms for low male F0; offline export always exact.
   Disclosed in D8/D11 rather than overpromised.
5. **Repo hygiene:** README with phase status table, GPL-3.0-or-later LICENSE,
   `.gitignore`, `docs/WORKLOG.md`.

## Evidence

- `docs/research/crates_2026_snapshot.json` — raw API responses, timestamps, versions.
- `docs/ARCHITECTURE_PLAN.md` — decision records D1–D15, performance budget,
  risk register, phase mapping, references.

## Acceptance criteria for Phase 1

| Brief requirement | Status |
|---|---|
| Clone/init repo | Done (empty repo initialized, pushed) |
| Research 2026 Rust audio ecosystem | Done — live snapshot with 26 crates |
| Research DSP approaches (phase vocoder / PSOLA / LPC / neural) | Done — D8/D9/D10 with rejections |
| Research UI frameworks (Slint / Iced / egui / Tauri / Qt) | Done — D1 with rejections |
| Choose + justify: UI, audio I/O, WAV/MP3, FFT, DSP per slider, font, colors | Done — D1–D13 |
| docs/ARCHITECTURE_PLAN.md with decisions + rejected alternatives | Done |
| Commit and push | Done (this commit) |

## Known risks carried into Phase 2

- LAME (`mp3lame-encoder`) must compile on all three CI targets — vendor-build fallback
  documented (D5).
- Slint Arabic shaping must pass a Phase 4 acceptance matrix — fallback documented (D1).
- 192 kHz is a device negotiation, not a guarantee — honest display + rubato
  correction designed in (D2).

**Next:** Phase 2 — project scaffold + audio I/O (workspace, cpal capture at
192 kHz/32-bit float, hound WAV, symphonia/LAME MP3, player, round-trip tests).
