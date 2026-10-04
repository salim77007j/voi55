# Micro-Vocal Lab

Precision microscopic control over the human voice — after recording.

Micro-Vocal Lab decomposes a vocal recording into its natural components
(**pitch**, **breath/air**, **formant/vocal-tract shape**) and lets you reshape
each element with extreme precision — without distorting the voice, without
robotic artifacts, and without altering natural timing or articulation.

A free, lightweight, Rust-powered alternative to expensive proprietary tools
like Melodyne. Native on **Windows**, **macOS**, and **Linux**.

## Status

| Phase | Scope | Status |
|---|---|---|
| 1 | Research + architecture plan | **Complete** |
| 2 | Project scaffold + audio I/O (capture / import / export / playback) | **Complete** |
| 3 | DSP engine: pYIN + PSOLA pitch, cepstral formant warp, air/breath control | **Complete** |
| 4 | Professional UI (English LTR + Arabic RTL), waveform, wired controls | **Complete** |
| 5 | CI (Windows / macOS / Linux), artifacts, nightly ASan/Valgrind, runtime verification | **Complete** |
| 6 | Streaming live preview + validation report + `v1.0.0-rc.1` | **Complete** (real-hardware protocols in `docs/VALIDATION_REPORT.md` pending — they gate the final `v1.0.0`) |

## Features

- **Pitch** — ±12 semitones in 1-cent steps. In-house pYIN tracking +
  TD-PSOLA shifting: duration- and formant-preserving by construction,
  no phase-vocoder smearing.
- **Air & Breath** — ±24/+12 dB in 0.1-dB steps. STFT harmonic/residual
  engine driven by the pitch comb: positive adds natural breathiness,
  negative is a de-esser + breath-removal downward expander. The harmonic
  path is untouched (gain exactly 1.0).
- **Formant** — vocal-tract length 130–190 mm. Exact frequency-axis
  envelope resampling (cepstral envelope, loudness-compensated);
  sibilance passes through bit-exactly.
- **Live streaming preview** — slider changes re-render at the playhead
  and crossfade in ~10–25 ms while playing; the A/B view uses the
  quality-maximal offline render; exports always render offline
  (bit-exact vs the streaming engine by gate tests).
- **Waveform** — zoom to sub-millisecond, min/max/power pyramid, F0 trace,
  playhead; A/B compare against the rendered preview.
- **Recording** — requests 192 kHz / 32-bit float and reports honestly what
  the device actually granted.
- **Files** — WAV import/export (f32/24/16) · MP3 import/export (CBR
  128–320, automatic resampling above 48 kHz).
- **English LTR + Arabic RTL** — runtime switch, embedded fonts, exact
  layout mirroring; numeric readouts stay Latin (engineering policy).

## Verified budgets (evidence in `docs/evidence/`, protocol in `docs/VALIDATION_REPORT.md`)

| Metric | Target | Measured |
|---|---|---|
| Binary (release, stripped) | < 50 MB | 23.8 MB |
| Cold start | < 1 s | 56–59 ms |
| RAM, 3-min 48 kHz session | < 200 MB | 146–196 MB |
| Preview DSP load | < 40 % of one core | 3 % |
| Live slider→ear latency | < 20 ms typical | ≈ 10–25 ms (loopback protocol pending on hardware) |
| Memory leaks | none | ASan + Valgrind nightly clean |

## Architecture and all technology decisions (with rejected alternatives and
evidence): see **[docs/ARCHITECTURE_PLAN.md](docs/ARCHITECTURE_PLAN.md)**.
Validation status and the real-hardware sign-off protocol:
**[docs/VALIDATION_REPORT.md](docs/VALIDATION_REPORT.md)**.

## Technology (decided in Phase 1)

- **UI**: Slint 1.18 — declarative, native, small binary, first-class RTL/Arabic support
- **Audio I/O**: cpal 0.18 (WASAPI / CoreAudio / ALSA-Pulse-JACK)
- **Files**: hound (WAV) · symphonia (MP3 decode) · LAME via mp3lame-encoder (MP3 encode)
- **DSP**: in-house pYIN pitch tracking · TD-PSOLA pitch shift (duration-preserving) ·
  cepstral-envelope formant warping in millimetres · STFT harmonic/residual air engine (0.1 dB) ·
  chunk-driven streaming render (bit-identical to the offline path)
- **Design language**: "Studio Graphite" — dark, hardware-referential, WCAG AA, Inter + IBM Plex Sans Arabic

## Building (Linux dev note)

Standard Rust toolchain (1.88+). On Linux, `cpal` needs ALSA development
headers (`libasound2-dev`) and Slint's font system needs
`libfontconfig1-dev` at build time.

```bash
cargo build --release
cargo test                            # 112 tests: I/O round-trips, DSP accuracy gates,
                                      # streaming bit-exactness, transport, screenshots
cargo run -p mvl-app                  # the app
cargo run -p mvl-app -- --screenshot --demo synth out.png   # headless render
```

## License

GPL-3.0-or-later — see [LICENSE](LICENSE). Fonts are licensed under the SIL OFL;
MP3 encoding uses LAME (LGPL), whose patents expired in 2017.
