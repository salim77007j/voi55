# Phase 2 Report — Project Scaffold + Audio I/O

| | |
|---|---|
| **Date** | 2026-10-04 |
| **Scope** | Workspace scaffold, cpal capture, WAV/MP3 import-export, player, round-trip tests |
| **Status** | **Complete** |

## Commits (one per sub-item, per protocol)

| Commit | Sub-item |
|---|---|
| `83e4d95` | 2.1 Workspace scaffold + `AudioBuffer` + engine parameter contract |
| `1363288`→`d14fc00` | 2.2 cpal capture with honest 192 kHz/f32 negotiation (+clippy fix) |
| `0dfa08d` | 2.3 WAV import/export via hound + round-trip tests |
| `64f5db4`→`accea83` | 2.4 MP3 import (symphonia) + export (LAME) + sinc resampler (+clippy fix) |
| `6aee27c`→`6adbe85` | 2.5 Audio player (play/pause/stop) + real playback self-test (+clippy fixes) |

## What works (all verified, nothing claimed without evidence)

- **Workspace**: three crates per plan D15 — `mvl-core` (parameter contract),
  `mvl-audio` (I/O), `mvl-app` (`micro-vocal-lab` binary). Edition 2024,
  resolver 3, release profile (`lto=thin`, `strip`).
- **Capture** (`mvl-audio::capture`): requests **192 kHz / f32** explicitly; the
  unit-tested negotiation (`plan_capture`, 6 tests) prefers f32 ranges containing
  the preferred rate, else falls back to the device maximum, else to convertible
  integer formats. Every recording carries `CaptureInfo` stating requested vs
  negotiated format — the honesty requirement from D2. Live capture was NOT
  runnable in this sandbox (no microphone); this is disclosed and re-verified on
  hardware in Phase 5. Overflow guard caps captures at 10 minutes.
- **WAV** (`mvl-audio::wav`): import f32/i16/i24/i32/i8; export Float32/Int16/Int24.
  Round-trip tests: **Float32 bit-exact** (48 kHz stereo and 192 kHz mono),
  i16/i24 within one LSB, garbage/missing files return errors (never panic).
- **MP3** (`mvl-audio::mp3`): import via symphonia 0.6 (probe transparently
  handles ID3v2; planar→interleaved conversion for 6 decoded formats). Export
  via LAME 0.2.5, CBR 128–320 kbps. Because MP3 physically tops out at 48 kHz,
  sources above that (96/192 kHz sessions) are resampled by a new windowed-sinc
  offline resampler (`resample.rs`, f64 accumulation, Hann-windowed, 24
  zero-crossings; in-band SNR > 50 dB in tests, exact length ratios).
- **Player** (`mvl-audio::player`): play/pause/stop transport over a cpal f32
  output stream; auto-stop at end of buffer; channel mapping (mono↔stereo +
  generic) as a pure tested function; buffers resampled to the device rate on
  play. **Real end-to-end playback self-test passes** (`micro-vocal-lab
  --selftest-audio`): opens the device, plays a generated 1 s sine, observes
  transport transitions. In this sandbox it runs through ALSA's null PCM (no
  sound card exists here — disclosed); on user machines it uses real hardware.
- **Quality gates**: `cargo fmt --check` clean · `cargo clippy -D warnings`
  clean (workspace, all targets) · **36 tests green** · no `unwrap()` on user
  or audio data in library code (failures return `AudioError`).

## Evidence in-repo

- `docs/WORKLOG.md` — per-sub-item log with API findings.
- Test suite: `cargo test` (36 passing) — includes WAV bit-exactness, MP3
  round-trip SNR with offset search, resampler SNR/length, capture negotiation
  matrix, player channel mapping, engine parameter clamps.
- Reproduce the playback self-test: `cargo run -p mvl-app -- --selftest-audio`.

## Key API findings (2026 crate versions — recorded for Phase 3)

- **cpal 0.18**: `SampleRate` is now a plain `u32` alias; `Device::name()` is
  gone (use `Display`/`description()`); `input_devices()` returns `Result`;
  traits must be imported explicitly.
- **symphonia 0.6**: `CodecParameters` is an enum; decoders come from a
  `CodecRegistry` factory; decoded audio is planar `AudioBuffer<f32>` wrapped
  in `GenericAudioBufferRef`; `AudioDecoderOptions::gapless` (default true)
  trims delay/padding.
- **mp3lame-encoder 0.2.5**: builder type is `Builder` (not `EncoderBuilder`);
  inputs are planar (`MonoPcm`/`DualPcm`); `encode_to_vec` writes into the
  Vec's **spare capacity** — reserve a byte budget first; flush needs ≥ 7200
  bytes headroom; use `FlushNoGap` for gapless-friendly files.

## Honest gaps / limitations carried forward

1. **Microphone capture not exercised on hardware here** (no mic in sandbox) —
   compile-checked, unit-tested negotiation logic, scheduled for device testing
   in Phase 5.
2. **Playback self-test** runs through the ALSA null plugin in this sandbox;
   latency/real-hardware verification belongs to Phase 5.
3. **MP3 gapless**: decoder-side trimming is on (symphonia `gapless: true`);
   round-trip tests tolerate small residual offset via SNR alignment search.
4. Player uses a brief mutex lock inside the RT callback (standard cpal
   pattern, no allocation); if Phase 3 profiling shows contention, it moves to
   a lock-free `ringbuf` (already a workspace dependency candidate).

**Next:** Phase 3 — the DSP core (pYIN, TD-PSOLA pitch, LPC formant warp,
air/breath engine) with accuracy gates from D7.
