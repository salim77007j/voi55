# Phase 7.3 Report — Cross-Platform Verification

**Status: COMPLETE (to the honest limit of this sandbox)** · CI green on
5 runner images incl. real macOS 14 + 15 + 26 hardware · per-platform UI
render evidence for Windows / macOS / Ubuntu · real-GUI Linux run and a
real Windows-PE-under-Wine run captured · HEAD `this commit`

## 1. Mission and the honest constraints

The brief asks for end-to-end verification on Windows 10/11, macOS 14/15,
Ubuntu 24.04 and Fedora 41 with screenshots, plus the macOS microphone
bundle task flagged in 7.1. This workspace is a headless Linux sandbox:
no Windows, no macOS, no display server, **no microphone, no speakers, no
sound card of any kind**. Every verification below is therefore typed by
what it actually is — *real-hardware execution*, *CI-runner execution*,
*Wine proxy execution*, or *cannot be done here (human task)*. Nothing is
upgraded a category by wishful thinking.

## 2. What was executed, by evidence class

### 2.1 Real GitHub-runner execution (closest to "real machine")

A `workflow_dispatch` of the release pipeline at `52c1f02` (after the CI
fix below) built and ran the real binary on three runners, each of which
**executed the shipped binary** (not just compiled it) via the headless
render smoke gate, then uploaded the rendered UI frame:

| Runner image | Result | Evidence |
|---|---|---|
| `ubuntu-24.04` | build + smoke SUCCESS | `docs/evidence/phase7/platform/smoke-ubuntu-latest.png` |
| `windows-2025-vs2026` | build + smoke SUCCESS | `docs/evidence/phase7/platform/smoke-windows-latest.png` |
| `macos-26-arm64` | build + smoke SUCCESS | `docs/evidence/phase7/platform/smoke-macos-latest.png` |

The three frames are the redesigned Studio Console rendered by the real
binary on each OS. The status bars double as **device-enumeration
evidence** on machines that do have audio stacks: the macOS frame shows
`IN/OUT Apple Virtual Sound Device`, the Ubuntu frame
`IN/OUT Default Audio Device` — the 7.1 enumeration code paths running
live on foreign OSes. (Windows' frame shows the file/track segments; the
WASAPI endpoints of the CI runner do not surface through the smoke
capture and are exercised by the Wine run below instead.)

### 2.2 CI on real Apple hardware — macOS 14 and 15 (the task book's targets)

`macos-latest` now tracks macOS 26, which alone would not satisfy the
brief's "macOS 14/15". The CI matrix was therefore extended (`1e2c9c5`)
to pin `macos-15` and `macos-14`; on those real Apple runners the job
runs fmt + clippy + build + the full test suite. Result at HEAD:
**success on ubuntu-24.04, windows-2025, macos-26, macos-15, macos-14**
(5 images). Within CI's reach this closes the macOS 14/15 column:
the workspace compiles clean and 131 tests pass on Sonoma and Sequoia
themselves.

### 2.3 The macOS bundle task (7.1 gap → closed here)

- `.github/macos/Info.plist` (repo file, reviewable): bundle name
  "Micro-Vocal Lab", id `dev.microvocallab.MicroVocalLab`, version read
  from the workspace manifest (`1.0.0-rc.1` at bundle time),
  `LSMinimumSystemVersion 11.0` (honest floor for the arm64 slice),
  audio category, high-resolution flag, and
  **`NSMicrophoneUsageDescription`** — the string CoreAudio's permission
  prompt displays; without it an unsigned build's capture can be denied
  silently. This was 7.1's disclosed gap #3.
- The macOS release job now builds a **universal binary** (`lipo` of
  arm64 + x86_64, verified from the downloaded artifact:
  "Mach-O universal binary with 2 architectures"), ad-hoc codesigns it
  (disclosed: *not* a Developer ID signature; Gatekeeper still requires
  right-click-open — §3.7 human step), `plutil -lint`s the plist, and
  ships `Micro-Vocal Lab.app` + the raw binary + README + LICENSE in the
  tarball. All steps executed on the real macOS runner in the dispatch
  run — the bundle pipeline is verified, not just written.

### 2.4 Linux deep verification (this sandbox, real execution)

- Environment reset disclosure: the sandbox lost the Rust toolchain,
  repo and packages mid-phase. Recovered with rustup (user prefix),
  alsa-lib 1.2.14 built from source into `~/.local/alsa` (no root),
  `libxkbcommon-x11`/`libxcb-xkb` extracted from Debian .debs into a user
  library prefix — all reproducible, all recorded here.
- **131/131 tests green** at HEAD locally (matches CI exactly: 56 core +
  57 audio + 16 app + 2 shot suites).
- `--selftest-audio`: fails *honestly* ("audio device is not available")
  — the container's ALSA `default` is unopenable, and the error surfaces
  verbatim instead of a crash or a fake pass.
- **Real X11 event-loop run**: the app under `Xvfb` (winit X11 backend —
  a different code path from the `--screenshot` software renderer) ran
  its full Slint event loop for 10 s: **0 panics**, honest empty state
  ("No audio loaded — import a WAV / MP3, record, or run with --demo
  synth"), status line "Engine v1.0.0-rc.1 ready". Captured via
  `x11grab`: `docs/evidence/phase7/platform/linux-gui-xvfb-real.png`.
- A first attempt *did* panic (missing `libxkbcommon-x11`) — and the
  7.1d global panic hook caught and printed it with thread + location,
  which is exactly the behavior it exists for. Recorded as a passed
  robustness check, then the library was supplied and the run went clean.

### 2.5 Windows execution — real PE under Wine (proxy, disclosed)

- The CI-built `micro-vocal-lab.exe` (PE32+ x86-64, 18.4 MB, integrity
  verified) was executed under **Wine 10.0 (win64 prefix)** on this
  sandbox with Xvfb. The real Windows binary booted, printed its banner,
  enumerated (and honestly failed to open) audio through the container's
  deviceless stack, and rendered the **complete Studio Console UI** in a
  real Win32 window: `docs/evidence/phase7/platform/windows-exe-under-wine.png`.
- Stability: 0 panics; the process stayed alive 35+ s until the sandbox
  reaped Xvfb between tool calls (log ends with the X-connection break,
  not an application fault).
- Honest Wine-environment quirks, *not* claimed as product bugs: the
  Arabic chip shows tofu (Wine's bare font environment), and the capture
  clips the status bar behind the emulated title bar. Both artifacts of
  the proxy environment; the macOS/Ubuntu runner frames show the same
  strings rendering correctly.

### 2.6 A red CI nobody had noticed — found and fixed here

Release workflow was green but **CI had been failing on macOS clippy
since the 7.2 headline commit**: libc deprecated `mach_task_self` and
`-D warnings` turned that into an error on Apple targets. Fixed with the
libc-recommended `mach2` crate (`52c1f02`; `mach2::traps::mach_task_self`
returns `mach_port_t = c_uint`, identical to libc's `task_info` parameter
type — verified in both crates' sources). CI green again on all images.
Process lesson adopted in this phase: *check CI at every HEAD*, not only
locally green trees.

## 3. The §3 real-hardware protocol — what still stands

`docs/VALIDATION_REPORT.md` §3 remains **the human gate** and this phase
cannot discharge it from a sound-less sandbox. Per row:

| §3 row | Status after 7.3 |
|---|---|
| 3.1 import → edit → export round trip | DSP/logic covered by tests incl. bit-exact round-trips; **human listen pass still required** |
| 3.2 sound-quality verdicts (named signer) | **human-only — standing blocker for v1.0.0 sign-off** |
| 3.3 10-min stability soak | 7.4 gate; sandbox soak planned there (no audio device: transport without sound) |
| 3.4 192 kHz capture honesty | needs a real mic — human task |
| 3.5 loopback latency < 20 ms | needs real out/in — human task |
| 3.6 cold start | sandbox proxy only (headless); desktop numbers need real HW |
| 3.7 platform specifics | Windows zip structure verified; macOS Gatekeeper right-click flow **documented, needs human run**; Linux build deps documented in module docs |

## 4. Platform matrix vs the task book (honest)

| Target | Verified how | Residual gap |
|---|---|---|
| Ubuntu 24.04 | **exact**: CI green on `ubuntu-24.04` runner + smoke render + sandbox deep run | no sound device here; PipeWire playback unexercised (ALSA plugin path compiled) |
| Fedora 41 | **not verified** — no runner, no container | binary links glibc + ALSA stack (distro-agnostic in principle); from-source deps documented. Suggest a Fedora container CI job as 7.4 follow-up |
| Windows 10/11 | compile+tests on `windows-2025` runner; PE boots full UI under Wine | WASAPI on real Win10/11 hardware — human pass pending |
| macOS 14/15 | **CI green on real macos-14 & macos-15 runners** (compile+clippy+131 tests); universal binary covers arm64+Intel | audible playback/CoreAudio prompt on those machines — human pass pending (plist now carries the mic string) |

## 5. Evidence index (this phase)

`docs/evidence/phase7/platform/smoke-ubuntu-latest.png` ·
`smoke-windows-latest.png` · `smoke-macos-latest.png` (runner renders) ·
`linux-gui-xvfb-real.png` (real X event loop) ·
`windows-exe-under-wine.png` (PE under Wine) · CI/release runs
`37307297896` (5-image CI, green) · `37312400391` (release dispatch at
`52c1f02`, three-platform bundle+smoke, universal verified) ·
integrity checks recorded in WORKLOG (ELF / PE32+ / Mach-O universal).

## 6. Verdict

Everything verifiable without a human, a microphone, or a sound card has
been verified, several items on more platforms than the brief asked for
(macOS 14 *and* 15 *and* 26; a Windows PE actually executed). The macOS
bundle task is closed. One real CI failure was discovered and fixed.
What remains is precisely the part that requires ears and hardware —
§3.2's signed sound-quality verdicts above all — plus the 7.4 machine
gates (fuzzer/soak/RAM/latency). Per the honest-reporting discipline,
**v1.0.0 must not be tagged until a human has run §3 on real hardware**;
7.4 will do everything else and leave that signature line explicitly
open. Phase 7.3 is done; awaiting "continue" for 7.4.
