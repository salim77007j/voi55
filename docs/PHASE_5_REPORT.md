# Phase 5 Report — 3-OS CI, Release Artifacts, Nightly Memory Checks, Runtime Verification

**Status: COMPLETE.** Branch `main`, head `808aadd` (+ this commit).
All sub-items were committed and pushed individually.

## What shipped

| Sub-item | Commit | Deliverable |
|---|---|---|
| 5.1 | `8f48ce2` + fix `57237ab` | `.github/workflows/ci.yml` — 3-OS matrix (ubuntu/windows/macos): `fmt --check`, `clippy -D warnings`, build, full test suite; ALSA + fontconfig headers on Linux; headless UI smoke with screenshot artifact |
| 5.1b | `bde079c` + fixes | `.github/workflows/nightly.yml` — ASan tests (mvl-core + mvl-audio, LeakSanitizer at exit) + Valgrind memcheck of the real headless app run; scheduled nightly + manual dispatch |
| 5.2 | `73b79df` + fix `808aadd` | `.github/workflows/release.yml` — 3-OS stripped release builds, run-from-artifact gate (the binary must render a real frame), < 50 MB size gate, bundle (binary + README + LICENSE), GitHub Release attachment on `v*` tags |
| 5.3 | `c9ffd3f` | Runtime verification + the RAM-budget fixes it forced (below): `bench_render` throughput harness, `verify-runtime.sh` + 3-min fixture generator, `MVL_TIMINGS` headless stage timing, streaming carry-OLA DSP, streaming pYIN Viterbi, mono preview storage |
| 5.4 | this | Phase report + evidence corpus (`docs/evidence/phase5/`) |

**Quality gates at HEAD:** 97 tests green (50 `mvl-core` + 33 `mvl-audio` + 14 `mvl-app`),
`cargo fmt` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean,
MSRV pinned at 1.88 in `Cargo.toml`, lockfile committed (D15).

## 1. CI — full test matrix on three operating systems (D15)

- Matrix `ubuntu-latest` / `windows-latest` / `macos-latest`, `fail-fast: false`, 60-min
  job timeout. Every push to `main` and every PR runs: format check → clippy
  (`--workspace --all-targets -D warnings`) → build → `cargo test --workspace`.
- Linux runners need `libasound2-dev` (cpal/alsa-sys) **and** `libfontconfig1-dev`
  (Slint's font system links fontconfig via pkg-config — the sandbox had it, the
  runners don't; found by the first CI run, fixed in `57237ab`).
- Runners have no audio device: `Player::connect()` returns the honest
  `AudioError::NoDevice` there — the deviceless code paths *are* the tested
  behaviour, not a gap (D2 honest-format disclosure).
- Runtime smoke on Linux: `micro-vocal-lab --screenshot --demo synth` renders a real
  frame headlessly (software renderer, no display server) and uploads it as a
  workflow artifact.
- **Result: green on all three OS** — CI runs [CI · 57237ab], [CI · c9ffd3f] and
  [CI · 808aadd] all passed (ubuntu ✓ windows ✓ macos ✓), the last two with the
  5.3 DSP memory refactor in the tree.

## 2. Nightly memory checks (D14)

- **ASan job**: nightly toolchain, `cargo test --target x86_64-unknown-linux-gnu
  -p mvl-core -p mvl-audio` with the sanitizer scoped to the target triple
  (`CARGO_TARGET_*_RUSTFLAGS`) — proc-macros cannot be instrumented, and scoping
  is what makes the build possible at all (first run failed on `thiserror-impl`).
  LeakSanitizer runs at process exit, so the whole DSP/I-O test suite doubles as a
  leak check. **Result: all tests pass under ASan, zero first-party leaks**
  (nightly dispatches on `c9ffd3f` and `808aadd`).
- **Valgrind job**: debug build of the real binary, memcheck of the full headless
  UI stack (platform init → demo load + pYIN → waveform render → PNG encode →
  teardown), `--error-exitcode=99` on definite leaks. Result: **no first-party
  leaks**; the only definite records are fontconfig internals — 336 B in charset
  caches (`FcCharSetAddChar`) and 256 B direct + 64 B indirect in the config-parse
  tree (via libexpat) — process-lifetime allocations by fontconfig's design,
  freed only in `FcFini()` which the font stack never calls. They are covered by
  the reviewed suppression file (`.github/valgrind-suppress.supp`) so the gate
  stays on real regressions. **Final nightly run [37213289805]: ASan ✓ +
  Valgrind ✓.**
- The dev sandbox has no valgrind and no root, so both checks live in CI where
  they are reproducible (D14 amendment recorded in `docs/WORKLOG.md`).

## 3. Release artifacts (D15)

- Triggers: `v*` tag pushes (bundles attach to the GitHub Release via
  `softprops/action-gh-release@v2`) and manual dispatch (artifact-only
  verification).
- Per target: `cargo build --release -p mvl-app` (profile already `lto=thin`,
  `codegen-units=1`, `strip=true`) → **run the binary** (`--screenshot` headless
  render — the artifact must run, not just link) → **size gate** (< 50 MB, hard
  fail) → bundle `binary + README + LICENSE` (GPL-3.0 §4 license copy) → upload.
  Windows bundles via `Compress-Archive` (runners ship no `zip` — found by the
  first release run, fixed in `808aadd`).
- **Result: green on all three OS** (run [37212516729], dispatch on `808aadd`):
  every release binary *rendered a real frame on its target* (smoke PNGs
  uploaded per platform) and the bundles landed at **8.05 MB** (windows zip),
  **7.34 MB** (macos tar.gz), **10.90 MB** (ubuntu tar.gz) — uncompressed Linux
  binary 23.8 MB locally. `github-release` correctly skipped on dispatch runs
  (tag-only). Local verification on this machine matches: release binary runs
  headless from `target/release/` at **23.8 MB**.

## 4. Runtime verification — every brief budget measured (D14 evidence policy)

Harness: `scripts/verify-runtime.sh` (release binary, Linux), fixture generator
`scripts/make_session_fixture.py` (3-min 48 kHz stereo vocal). Raw evidence:
`docs/evidence/phase5/runtime-verification.txt`, `bench-render.txt`,
`cold-timings.log`.

| Budget (from the brief) | Target | Measured | Verdict |
|---|---|---|---|
| Binary size (release, stripped) | < 50 MB | **23.8 MB** | ✅ |
| Cold start | < 1 s | **56–59 ms** (median of 5, headless proxy: whole screenshot run incl. process start, UI init, first frame, PNG encode) | ✅ |
| RAM, 3-min 48 kHz session | < 200 MB | **146 MB** loaded idle · **196 MB** with an all-stages preview rendered | ✅ (was **316 MB** before 5.3 — see §5) |
| Preview DSP load | < 40 % of one core @ 48 kHz | **3 %** (34× real-time; 30-s male-vocal fixture, all three stages engaged, cached pYIN — the per-slider-move path) | ✅ |

Supporting numbers (`docs/evidence/phase5/bench-render.txt`): pYIN cold analysis
98× real-time; full cold render (pYIN + all stages) 25× real-time. The
criterion-benchmark plan item (D14) is fulfilled by the committed
`cargo run --release -p mvl-audio --example bench_render` harness instead — same
measurement, no extra dependency, exits non-zero when the budget is missed
(amendment in `docs/WORKLOG.md`).

## 5. The RAM budget was failing — verification forced real fixes (5.3)

The first verification run measured **316 MB peak RSS** on the 3-min session
against the 200 MB budget. Attribution (via the committed
`crates/mvl-audio/examples/rss_probe.rs`):

| Source | Before | After |
|---|---|---|
| pYIN analysis (per-frame obs f64 + τ f64 + bp u32 matrices) | +192 MB | **+22 MB** — streaming Viterbi (`ViterbiTrellis`): observation rows consumed per frame, u16 backpointer trellis + handful of candidate taus retained |
| Stage scratch during render (acc + wsum + voicing env + stage hand-off) | +148 MB | **+50 MB** — `ola::CarryOla`: fixed-size ring overlap-add in all three stages (PSOLA grains, formant frames, air frames) with on-the-fly voicing envelope; additions per sample happen in the same order, results bit-identical (all 50 core gate tests unchanged) |
| Pipeline input copy (`x.to_vec()` that no stage needed) | +34 MB | **0** — stages borrow; first active stage reads the caller's slice |
| A/B preview storage (stereo buffer + duplicate display downmix) | +103 MB | **+34 MB** — preview stored mono (the render is mono-derived; `Player` maps 1→N at output; export still renders the original channel layout) and the stale preview is dropped before a re-render starts |

Net: **316 → 196 MB**, with the interactive single-stage case (one slider moved
from neutral — the typical operating point) peaking around ~180 MB, and the
loaded-idle session at 146 MB. All 97 tests, the pYIN accuracy gates, the PSOLA
bit-exact bypass tests and the pixel-asserting screenshot tests stayed green
throughout, so the memory work is behaviour-preserving.

## 6. Honest gaps

- **Preview round-trip latency budget (< 20 ms typical) is not met by the
  current architecture.** The brief's number presumes a streaming preview
  pipeline; Micro-Vocal Lab renders the *whole file* per slider change
  (correctness-first, non-destructive, one shared pYIN pass — D11). Round-trip
  is debounce (250 ms) + full-file render at 34× real-time: ≈ 0.3 s for a 10-s
  take, ≈ 1.1 s for 30 s, ≈ 3.5 s for a 2-min song — the status bar shows the
  real render time every time. PSOLA's own physics floor (≥ 2·T0, ~35 ms worst
  case at low F0, D8) remains disclosed as before. A chunked/streaming preview
  path is the natural Phase 6+ engineering item; the DSP refactor of 5.3
  (bounded-window OLA) is the groundwork for it.
- **196 vs 200 MB is a thin margin**, measured on the worst case (3-min stereo,
  all stages, preview held). Sessions ≤ ~2:30 clear the budget with room; the
  floor is two full-length buffers (render source + result) inherent to the
  current stage hand-off plus ~40 MB of Slint/runtime overhead. Stereo-session
  *export* renders transiently higher (~206 MB, mono-session exports ~173 MB)
  because the channel-restore copy is inherently an extra full-length buffer.
- **Cold start is a headless proxy** (spawn → first frame → exit, no display
  server, no GPU init). Real-hardware numbers belong to Phase 6 validation on
  machines with audio devices and displays.
- **Valgrind suppressions cover a third-party library** (fontconfig charset
  caches, 656 B total). The suppression file documents the reasoning; first-party
  allocations are leak-free under both LeakSanitizer and memcheck.
- **This machine has no microphone and no audio device** — capture/playback
  behaviour is verified only through the deviceless code paths, transport state
  machines, and the Phase 2 null-device self-test. Phase 6 real-hardware
  validation is still the gate for "sounds right" claims.
- **Windows/macOS results exist only as CI evidence** (tests + headless smoke +
  release builds); the interactive GUI experience on those platforms is
  Phase 6 validation work.

## 7. Evidence index (D14 — every claim carries an artifact)

- `docs/evidence/phase5/runtime-verification.txt` — binary size, cold start ×5,
  RSS idle + render-engaged, all budget verdicts
- `docs/evidence/phase5/bench-render.txt` — throughput harness output
  (preview/cold/pyrin factors, budget verdict)
- `docs/evidence/phase5/cold-timings.log` — per-stage `MVL_TIMINGS` stderr
- `.github/workflows/{ci,nightly,release}.yml` — the gates themselves
- `.github/valgrind-suppress.supp` — reviewed third-party suppressions
- `scripts/verify-runtime.sh`, `scripts/make_session_fixture.py` — reproducible
  harness
- `crates/mvl-audio/examples/{bench_render,rss_probe}.rs` — budget + attribution
  tools
- GitHub Actions runs: CI matrix green on `57237ab` / `c9ffd3f` / `808aadd`;
  nightly ASan ✓ + Valgrind ✓ (final run 37213289805); release artifacts ×3 OS
  green (run 37212516729, bundles 8.05 / 7.34 / 10.90 MB)

**Next up (Phase 6):** `VALIDATION_REPORT.md` — real-vocal validation on hardware,
native-Arabic reviewer checklist, performance numbers on real machines, honest
gaps ledger, tag `v1.0.0`. Awaits the explicit "continue".
