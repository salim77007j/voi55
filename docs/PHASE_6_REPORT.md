# Phase 6 Report — Streaming Live Preview, Validation Close-out, v1.0.0-rc.1

**Status: COMPLETE.** Branch `main`; all sub-items committed and pushed
individually. The tree is tagged `v1.0.0-rc.1`; the final `v1.0.0` signs off
on the real-hardware protocols in `docs/VALIDATION_REPORT.md` (§3/§4) — this
environment has no microphone, no audio device and no display server, and the
project's honesty policy (D14) forbids tagging "heard and verified" without
having heard it.

## What shipped

| Sub-item | Commit | Deliverable |
|---|---|---|
| 6.1 | `f2a25f8` | `mvl-core::stream` — chunk-driven streaming render engine (the Phase 5 groundwork put to work) |
| 6.2a | `e244fd5` | `mvl-audio::preview` — PreviewStream worker, bounded FIFO, crossfade restart; `Player` streaming source |
| 6.2b | `f6f18e2` | App wiring — live lifecycle, status-bar latency, honest fallbacks |
| 6.3 | `d84d297` | `docs/VALIDATION_REPORT.md` — claims matrix, hardware protocols, Arabic reviewer checklist, honest gaps |
| 6.4 | this | Phase report + worklog + README refresh |
| 6.5 | tag | `v1.0.0-rc.1` — fires the release workflow (3-OS bundles attach to the GitHub Release) |

**Quality gates at HEAD:** 112 tests green (56 `mvl-core` + 42 `mvl-audio`
+ 12 `mvl-app` + 2 doc/example), `cargo fmt` clean,
`cargo clippy --workspace --all-targets -- -D warnings` clean.

## 1. The preview-latency gap is closed (the Phase 5 honest gap)

Phase 5 disclosed that the < 20 ms preview budget was not met: the app
rendered the *whole file* per slider change (250 ms debounce + render at
34× real-time ≈ 0.3 s for 10 s, 3.5 s for 2 min). Phase 6 replaces the
audible path with a true streaming render:

- **6.1** — every stage loop (PSOLA grains, formant frames, air frames)
  moved into a chunk-driven driver (`PsolaStage`/`FormantStage`/`AirStage`);
  the offline functions are thin wrappers over the same math (single source
  of truth). `CarryOla` gained a restart head (`with_start`) and input
  windows (`x_base`). `StreamChain` drives pitch→formant→air through a
  demand wave, front-trims hand-off buffers to the consumer frontier, and
  mirrors the offline pipeline's stage activation exactly.
  **Gate: chunked == offline render, exact `f32` equality**, all 7 stage
  combos × chunk sizes {1, 7, 479, 4095, 2²⁰}. Internal buffers stay
  window-sized (≤ ~4 MB) independent of file length — a 10-minute session
  streams in the same footprint as a 10-second one.
- **6.2** — `PreviewStream`: a worker pulls 10 ms chunks into a bounded
  FIFO (100 ms); the cpal callback consumes it. A slider change is a
  *restart*: the chain rebuilds at the playhead and the new render splices
  onto the unplayed tail through a 10 ms linear crossfade — audible within
  ~one output callback (≈10–25 ms). The FIFO splice handles all restart
  geometries (continuity splice at the playhead; hard jump + consumer hold
  for seeks beyond production or backward — stale ring content is
  unreachable). Worker discipline: consume only what the FIFO accepted;
  blended remainder parks in a pending buffer (no lost audio, no double
  crossfade); the tail flush fires when an advance that requested work
  yields nothing.
- **6.2b** — the app starts the live path on first play (device present,
  session rate == device rate, track available, non-neutral params), shows
  the last measured restart latency in the status bar, rewinds on
  stop/rewind, stops on new sessions and neutral parameters. The debounced
  offline render still feeds the A/B waveform view; **exports always use
  the offline render**.

## 2. Disclosed engineering facts (new)

- **Restart artifacts.** The splice region (≤ one stage window past the
  restart point) is rendered with partial overlap history and masked by the
  crossfade. With the **pitch** stage active, a restart also resets the
  PSOLA read-pointer phase: the epoch grid and grain content are identical,
  but the fractional read-pointer phase differs from the offline render, so
  post-restart samples are perceptually equivalent yet not sample-exact.
  Gate tests prove both halves: STFT-stage restarts converge *exactly*;
  the pitch restart moves the comb to the new target (ceps gate).
- **Rate match.** The streaming path carries no resampler: it runs when the
  session rate equals the device rate; otherwise the app keeps the offline
  preview (disclosed fallback, not an error).
- **Headless CI is deviceless by design** — the FIFO/worker state machines
  are unit-tested without a device; the cpal callback itself is hardware
  validation (same disclosure as Phases 2/5).

## 3. Validation close-out

`docs/VALIDATION_REPORT.md` maps all 17 brief requirements to their
evidence, specifies the real-hardware protocols (§3: round trip, named
sound-quality verdicts, 10-minute soak, 192 kHz capture, live-latency
loopback, desktop cold start, per-OS platform notes), the native-Arabic
reviewer checklist (§4), the performance table (§5) and the honest gaps
ledger (§6). The final `v1.0.0` tag is a sign-off on those protocols —
each pass criterion is concrete and each failure must be fixed or
disclosed before tagging.

## 4. Honest gaps (carried forward / new)

See `docs/VALIDATION_REPORT.md` §6. Headlines: real-hardware validation
pending (the rc's whole point); 196 vs 200 MB stays thin on the 3-min
stereo worst case; cold start remains a headless proxy; PSOLA's ≥ 2·T0
physics floor is unchanged; the restart artifacts above are new and
disclosed; vibrato ±3 % leakage and the heuristic breath detector carry
over from Phase 3.

## 5. Evidence index

- `crates/mvl-core/src/stream.rs` — chain gate tests (bit-exactness,
  bounded buffers, restart convergence, latency readout)
- `crates/mvl-audio/src/preview.rs` — worker/FIFO gates (stream == offline
  render through the worker; splice/hold units; backpressure)
- `docs/VALIDATION_REPORT.md` — the validation contract for v1.0.0
- `docs/evidence/phase{3,4,5}/` — DSP spectrograms, UI screenshots,
  runtime verification (unchanged, still cited)
- GitHub Actions — CI matrix green per push; nightly ASan/Valgrind; release
  workflow re-runs the 3-OS artifact gates on the `v1.0.0-rc.1` tag

**Next:** run the §3/§4 protocols on real machines, fix or disclose any
failure, then tag `v1.0.0`.
