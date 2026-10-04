# Phase 7.1 Report — P0 Fixes

**Scope**: the four P0 issues from the real-machine feedback round:
input-device detection, output-device handling, general robustness, and
the symphonia preview-line dependency. UI redesign is **7.2** (next),
cross-platform verification is **7.3**.

**Commits**: `7848903` (7.1a), `dee2145` (7.1b+c+d).
**Quality at HEAD**: 118 tests green (56 core + 48 audio + 12 app +
2 integration), `cargo fmt` + `cargo clippy -D warnings` clean
workspace-wide.

---

## 7.1a — symphonia 0.6 → 0.5.5 (ISSUE 4)

The 0.6 series is a self-declared "v2 preview — never use in
production". The workspace now pins `symphonia = "0.5.5"` (default
features off, `mp3` on) and the import path was ported to the 0.5 API:

| 0.6 (preview)                       | 0.5.5 (now)                          |
|-------------------------------------|--------------------------------------|
| `probe.probe(...) -> Box<dyn Format>` | `probe.format(...) -> ProbeResult` |
| `CodecParameters::Audio(p)` enum    | `Track.codec_params: CodecParameters`|
| `CodecRegistry` + decoder factory   | `get_codecs().make(&params, &opts)`  |
| `GenericAudioBufferRef` slice-based | `AudioBufferRef` (`Cow<AudioBuffer<S>>`) |
| `Audio` trait (`frames`/`spec`)     | inherent methods + `Signal::chan`    |

Robustness gains made while porting (disclosed in module docs):

- **Torn/corrupt packets are skipped** with a warning
  (`Error::DecodeError` → `log::warn!`) instead of failing the whole
  file. A file with zero decodable audio is still a clean error.
- **EOF** is handled via symphonia's `UnexpectedEof` IoError;
  `ResetRequired` stops cleanly.

Verification (all in `crates/mvl-audio/src/mp3.rs` tests):

- `mp3_roundtrip_mono_44k1` — SNR > 20 dB floor (existing gate).
- `mp3_roundtrip_stereo_192k_resamples_to_48k` — rate conversion intact.
- **`mp3_decode_is_deterministic`** — the same file decoded twice
  produces bit-identical samples (the "bit-exact decode" claim in the
  brief maps here: MP3 is lossy, so the invariant that *can* be honest
  is decode determinism + the SNR gate above).
- **`truncated_mp3_does_not_panic`** — file cut at 60 % and 5 %: either
  decodes with fewer frames or errors cleanly; never panics/hangs.
- **`random_blobs_never_panic`** — 64 seeded random blobs through
  `import_mp3` (the in-CI slice of the 1000-file fuzz gate; the full
  count belongs to the 7.3/quality-gate protocol).

## 7.1b — input devices (ISSUE 1)

- **Full enumeration** was already present (`list_input_devices`); it is
  now joined by `list_output_devices` (name, config-range count, max
  rate, granted formats) and both are printed in the startup banner.
- **Resolution fallback chain** — `resolve_input_device(Option<&str>)`
  returns `(device, fell_back)`:
  1. exact-name match from the full enumeration (when a name is given),
  2. the platform default input,
  3. the first enumerable input device.
  A vanished picked device or a missing "default" therefore degrades
  honestly (the UI discloses "fell back") instead of failing.
- `Recorder::start_named(Some/None)` passes the pick through; the app
  stores the selection and feeds it on every arm.
- **Devices dialog** (Slint): header headphones-button opens a centered
  RTL-aware dialog with two real-enumerated lists; the device the next
  capture will use is highlighted. See 7.1c for the shared plumbing.

Platform behaviour is documented in `crates/mvl-audio/src/devices.rs`
module docs (honest scope):

- **Windows** — cpal's default host is WASAPI; every capture/render
  endpoint enumerates; shared mode only.
- **macOS** — CoreAudio prompts when the first input *stream is built*,
  not at enumeration; denial surfaces as `Stream`/`EmptyCapture` in the
  status line. The release bundle must carry
  `NSMicrophoneUsageDescription` (tracked as a release-bundle task for
  7.3 — a task, not a claim).
- **Linux** — cpal talks ALSA; PipeWire/PulseAudio desktops expose their
  devices through the ALSA plugin stack. Loopback capture of system
  audio is **not** enumerated (a PulseAudio/PipeWire module feature, not
  an ALSA capture endpoint — disclosed rather than faked).

## 7.1c — output devices (ISSUE 2)

- **Any granted sample format**: the previous negotiation rejected
  anything but `f32` mono/stereo. `Player::connect_on_device` now ranks
  every convertible configuration (f32 > i32 > i16 > 8-bit, then rate
  proximity to 48 kHz, then channel count) and builds the stream in the
  device's own format through a generic `build_output_stream::<T>`.
  The transport state machine and channel mapping stay in the `f32`
  domain (reused scratch buffer; the final `FromF32` conversion clamps).
  Unit tests cover full-scale mapping and the i16 round-trip within one
  LSB.
- **Any channel count**: the mono→N FIFO map and the generic N→M fold in
  `copy_into_output` were already channel-agnostic; the artificial
  `channels > 2` cap is gone, so 4/6/8-channel endpoints can play.
- **Named connect**: `Player::connect_named` uses the same resolve
  chain; picking a device in the dialog re-homes the player immediately
  (old stream dropped, new one built — the change is audible at once).
- **Test output button**: real 440 Hz / 0.3 s sine through the actual
  `Player`. The "playing" affordance follows the *real* transport in the
  40 ms poller (no timers, no fake state). Failures (no device, denied
  output) surface verbatim in the status line.

## 7.1d — robustness (ISSUE 3)

- **Zero-frame recordings are a named error**: `Recorder::stop` now
  finalizes through `finalize_capture`, which returns the new
  `AudioError::EmptyCapture` ("zero-frame recording — the device
  produced no audio") for empty *or* torn captures instead of handing
  the UI a silent empty project. Unit-tested for empty / half-a-frame /
  one-complete-frame inputs.
- **`catch_unwind` around all worker threads**:
  - the offline render thread (app.rs): a panic is converted into the
    normal `RenderDone::Failed` path — the status line shows
    "render thread panicked: <payload>" and the app keeps running;
  - the Phase 6 preview worker: a panic stops the worker, the FIFO
    stalls at its last chunk (playback ends there), `stats.error` is
    set, and the app keeps running.
  RT audio callbacks stay panic-free by construction (no unwrap on the
  callback paths; the callbacks recover from poisoned locks by emitting
  silence).
- **Global panic hook**: thread name + file:line + payload to stderr for
  every panic (GUI and screenshot modes) — nothing dies silently.

## Evidence

- `docs/evidence/phase7/devices-dialog-en.png` — the dialog rendering
  the sandbox's real (ALSA null) device through the software renderer.
- `docs/evidence/phase7/devices-dialog-ar-rtl.png` — the same dialog in
  Arabic: mirrored layout, translated labels, Latin device names kept as
  LTR islands.
- `--selftest-audio` still exercises connect → play → position-advance →
  pause → stop on whatever device the machine grants.

## Honest gaps (carried into 7.2/7.3)

1. **Devices dialog styling is functional, not final** — it uses the
   Phase 4 shell; 7.2 restyles it against the reference image (dense
   panel look, accent strips, meters).
2. **No live input/output meters yet** — the meters the brief wants are
   part of the 7.2 redesign, wired to real peak/RMS from the recorder
   and player callbacks (API groundwork exists; the UI plumbing is 7.2
   work).
3. **`NSMicrophoneUsageDescription`** needs to be added to the macOS
   release bundle step before a signed .app can prompt for the mic
   (7.3 bundle task).
4. **The 1000-file fuzz gate** runs here at 64 seeded blobs in unit CI;
   the full protocol belongs to the 7.3 verification phase.
5. **Real-machine verification is 7.3** — this phase verified the code
   paths, the tests and the headless evidence; hearing a tone on a real
   laptop is explicitly the next phase's job.

## Verdict

The four P0 issues are fixed with regression tests, the fallback chains
and the picker make device handling honest instead of lucky, and the
app survives worker panics by design. Sandboxed verification is green
(118 tests, clippy clean, headless evidence). Cross-platform
verification on real machines remains 7.3 work, as planned.
