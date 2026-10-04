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
| 4 | Professional UI (English LTR + Arabic RTL), waveform, wired controls | Pending |
| 5 | CI (Windows / macOS / Linux), integration testing, artifacts | Pending |
| 6 | Final validation report + tag v1.0.0 | Pending |

Architecture and all technology decisions (with rejected alternatives and
evidence): see **[docs/ARCHITECTURE_PLAN.md](docs/ARCHITECTURE_PLAN.md)**.

## Technology (decided in Phase 1)

- **UI**: Slint 1.18 — declarative, native, small binary, first-class RTL/Arabic support
- **Audio I/O**: cpal 0.18 (WASAPI / CoreAudio / ALSA-Pulse-JACK)
- **Files**: hound (WAV) · symphonia (MP3 decode) · LAME via mp3lame-encoder (MP3 encode)
- **DSP**: in-house pYIN pitch tracking · TD-PSOLA pitch shift (duration-preserving) ·
  LPC source–filter formant warping in millimetres · STFT harmonic/residual air engine (0.1 dB)
- **Design language**: "Studio Graphite" — dark, hardware-referential, WCAG AA, Inter + IBM Plex Sans Arabic

## Building (Linux dev note)

Standard Rust toolchain (1.88+). On Linux, `cpal` needs ALSA development
headers (`libasound2-dev`) at build time.

```bash
cargo build --release
cargo test                            # 36 tests: I/O round-trips, negotiation, DSP contract
cargo run -p mvl-app                  # environment report
cargo run -p mvl-app -- --selftest-audio   # real end-to-end playback exercise
```

## License

GPL-3.0-or-later — see [LICENSE](LICENSE). Fonts are licensed under the SIL OFL;
MP3 encoding uses LAME (LGPL), whose patents expired in 2017.
