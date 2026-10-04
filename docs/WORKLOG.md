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
