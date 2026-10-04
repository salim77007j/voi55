//! Streaming preview (Phase 6) — closes the full-file preview latency gap
//! disclosed in Phase 5.
//!
//! [`PreviewStream`] owns a worker thread that pulls the render out of
//! [`mvl_core::stream::StreamChain`] in fixed-size chunks and pushes it
//! into a bounded [`StreamFifo`]; the `Player` callback consumes the FIFO
//! on the audio thread. A slider change is a *restart*: the worker rebuilds
//! the chain at the playhead and splices the new render onto the unplayed
//! tail of the old one through a short linear crossfade, so the parameter
//! change is audible within roughly one FIFO depth (~10–20 ms) instead of
//! the debounce + whole-file render round trip.
//!
//! Honesty notes (D2/D14):
//! - The splice region (one stage window past the restart point) is
//!   rendered with partial overlap history and blended away by the
//!   crossfade — the steady-state stream is bit-identical to the offline
//!   render (gate-tested in `mvl-core::stream`) **for STFT stages**. With
//!   the pitch stage active, a restart also resets the PSOLA read-pointer
//!   phase: the epoch grid and grain content are the same, but the
//!   fractional read-pointer phase differs from the offline render, so
//!   post-restart samples are perceptually equivalent yet not sample-exact
//!   (gate-tested with a comb-strength assert). Exports always use the
//!   offline render.
//! - The FIFO carries mono at the *session* rate; the caller must ensure
//!   the output device runs at the same rate (the app falls back to the
//!   debounced offline-render preview otherwise). No resampler rides on
//!   the real-time path.
//! - Without an audio device the worker/FIFO state machine still runs and
//!   is unit-tested; the cpal callback itself is exercised only on
//!   real hardware (same disclosure as Phase 2/5).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvl_core::engine::EngineParams;
use mvl_core::pyin::PyinResult;
use mvl_core::stream::StreamChain;

use crate::error::{AudioError, Result};

/// Commands the UI thread sends to the preview worker.
enum Command {
    /// Rebuild the chain with new parameters at `position` (slider change
    /// or seek) and splice onto the unplayed tail.
    Restart {
        params: EngineParams,
        position: usize,
    },
    Stop,
}

/// Worker statistics surfaced in the status bar (honest numbers).
#[derive(Debug, Default, Clone)]
pub struct PreviewStats {
    /// Wall-clock µs from accepting a restart to the first post-splice
    /// chunk landing in the FIFO.
    pub last_restart_us: Option<u64>,
    /// Completed restarts.
    pub restarts: u64,
    /// Samples pushed into the FIFO.
    pub produced: u64,
    /// Engine error text, if the chain failed.
    pub error: Option<String>,
}

struct WorkerShared {
    fifo: Arc<StreamFifo>,
    command: Mutex<Option<Command>>,
    stats: Mutex<PreviewStats>,
}

/// Bounded single-producer/single-consumer sample ring with absolute
/// sample positions and a splice primitive for parameter-change restarts.
///
/// The producer (worker thread) pushes rendered chunks; the consumer (the
/// cpal callback) pulls mono frames and maps 1→N channels. All operations
/// take the inner mutex briefly and never allocate on the callback path.
///
/// Splice contract (`StreamFifo::splice`), covering every restart
/// geometry:
/// - `read ≤ at ≤ write` (parameter change at the playhead — the normal
///   case, the worker renders ~100 ms ahead): the unplayed audio ahead of
///   `at` stays valid, the write cursor rewinds to `at`, and up to
///   `max_fade` samples of it are captured as the crossfade fade-out
///   source. Playback continues gap-free through the splice.
/// - `at` outside `[read, write]` (seek beyond production, or backward):
///   hard jump — cursor and write both move to `at` and the consumer is
///   *held* (silence) until the worker's first post-splice push, so it can
///   never read stale ring content. The discarded material is the skipped
///   region — the same thing a DAW does on a hard seek.
pub struct StreamFifo {
    inner: Mutex<FifoInner>,
    underrun: AtomicBool,
    /// Set by a hard-jump splice; cleared by the producer's first push.
    hold: AtomicBool,
    total: AtomicU64,
}

struct FifoInner {
    buf: Vec<f32>,
    /// Absolute sample index of `buf[0]`'s slot content owner… both
    /// counters are absolute and wrap through the capacity mask.
    read: u64,
    write: u64,
}

impl StreamFifo {
    /// `capacity` in samples (mono).
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(FifoInner {
                buf: vec![0.0; capacity.max(64)],
                read: 0,
                write: 0,
            }),
            underrun: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            total: AtomicU64::new(0),
        }
    }

    /// Declares the total material length (for end-of-stream detection).
    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Release);
    }

    /// Total material length in samples.
    pub fn total_frames(&self) -> u64 {
        self.total.load(Ordering::Acquire)
    }

    /// Playback cursor (absolute sample index of the next frame to play).
    pub fn read_frame(&self) -> u64 {
        self.inner.lock().map(|f| f.read).unwrap_or(0)
    }

    /// Buffered frames available to the consumer.
    pub fn available(&self) -> usize {
        self.inner
            .lock()
            .map(|f| (f.write - f.read) as usize)
            .unwrap_or(0)
    }

    /// Free frames available to the producer.
    pub fn free(&self) -> usize {
        self.inner
            .lock()
            .map(|f| f.buf.len() as u64 - (f.write - f.read))
            .unwrap_or(0) as usize
    }

    /// Producer: pushes samples, returns how many were accepted (the
    /// worker checks `free()` first, so this is normally everything). The
    /// first push after a hard-jump splice releases the consumer hold.
    pub fn push(&self, samples: &[f32]) -> usize {
        let Ok(mut f) = self.inner.lock() else {
            return 0;
        };
        let cap = f.buf.len() as u64;
        let used = f.write - f.read;
        let space = (cap - used) as usize;
        let n = samples.len().min(space);
        let cap_us = cap as usize;
        for (k, &s) in samples.iter().take(n).enumerate() {
            let slot = ((f.write as usize) + k) % cap_us;
            f.buf[slot] = s;
        }
        f.write += n as u64;
        if n > 0 {
            self.hold.store(false, Ordering::Release);
        }
        n
    }

    /// Consumer (real-time callback): pulls up to `out.len() /
    /// out_channels` mono frames and maps them 1→N. Returns the number of
    /// *frames* written; the rest of `out` stays zero-filled (the caller
    /// zero-fills before). An empty pull flags an underrun; a held FIFO
    /// (hard-jump splice) returns silence without flagging.
    pub fn pull_mono_into(&self, out: &mut [f32], out_channels: u32) -> usize {
        if self.hold.load(Ordering::Acquire) {
            return 0;
        }
        let Ok(mut f) = self.inner.lock() else {
            self.underrun.store(true, Ordering::Relaxed);
            return 0;
        };
        let channels = usize::try_from(out_channels).unwrap_or(1).max(1);
        let want_frames = out.len() / channels;
        let have = (f.write - f.read) as usize;
        let frames = want_frames.min(have);
        let cap = f.buf.len();
        for k in 0..frames {
            let slot = ((f.read as usize) + k) % cap;
            let s = f.buf[slot];
            let dst = &mut out[k * channels..(k + 1) * channels];
            dst[0] = s;
            for d in &mut dst[1..] {
                *d = s;
            }
        }
        f.read += frames as u64;
        if frames == 0 && want_frames > 0 {
            self.underrun.store(true, Ordering::Relaxed);
        }
        frames
    }

    /// Worker: splices the stream at `at`. See the type-level contract for
    /// the three geometries. Returns the captured crossfade tail (empty on
    /// a hard jump).
    pub fn splice(&self, at: u64, max_fade: usize) -> Vec<f32> {
        let Ok(mut f) = self.inner.lock() else {
            return Vec::new();
        };
        if at > f.write || at < f.read {
            // Hard jump outside the buffered window: discard everything and
            // hold the consumer until the first post-splice push lands.
            f.read = at;
            f.write = at;
            self.hold.store(true, Ordering::Release);
            return Vec::new();
        }
        // Continuity splice: unplayed audio ahead of `at` stays valid; the
        // write cursor rewinds so the worker overwrites from `at` onward.
        let mut tail = Vec::new();
        let buffered = (f.write - at) as usize;
        let n = buffered.min(max_fade);
        let cap = f.buf.len();
        tail.reserve(n);
        for k in 0..n {
            let slot = ((at as usize) + k) % cap;
            tail.push(f.buf[slot]);
        }
        f.write = at;
        tail
    }

    /// Consumer diagnostics: takes the underrun flag (clears it).
    pub fn take_underrun(&self) -> bool {
        self.underrun.swap(false, Ordering::Relaxed)
    }
}

impl std::fmt::Debug for StreamFifo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamFifo")
            .field("read", &self.read_frame())
            .field("available", &self.available())
            .field("total", &self.total_frames())
            .finish_non_exhaustive()
    }
}

/// A running streaming preview: worker thread + shared FIFO. Clone-free
/// handle; the FIFO `Arc` is handed to the `Player`.
pub struct PreviewStream {
    shared: Arc<WorkerShared>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PreviewStream {
    /// Starts the worker rendering `session` (mono, `sample_rate`) with
    /// `params` from `start_sample`, using `track` as the shared pYIN
    /// analysis. `fifo_capacity` bounds the ring (samples).
    ///
    /// # Errors
    /// Fails when the initial chain cannot be built (engine parameter
    /// error, e.g. neutral parameters).
    pub fn start(
        session: Arc<Vec<f32>>,
        sample_rate: u32,
        track: Arc<PyinResult>,
        params: &EngineParams,
        start_sample: usize,
        fifo_capacity: usize,
    ) -> Result<Self> {
        let fifo = Arc::new(StreamFifo::new(fifo_capacity));
        fifo.set_total(session.len() as u64);
        let shared = Arc::new(WorkerShared {
            fifo,
            command: Mutex::new(None),
            stats: Mutex::new(PreviewStats::default()),
        });
        let worker_shared = Arc::clone(&shared);
        let params = *params;
        let start = start_sample.min(session.len());
        let handle = std::thread::Builder::new()
            .name("mvl-preview".into())
            .spawn(move || {
                // Phase 7 ISSUE 3: a panicking DSP worker must never take
                // the whole app down. The worker exits; the FIFO stalls at
                // its last chunk (the player stops at the end of buffered
                // audio) and the error is visible in the stats.
                let panic_shared = Arc::clone(&worker_shared);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    worker_loop(worker_shared, session, sample_rate, track, params, start);
                }));
                if outcome.is_err() {
                    log::error!("preview worker panicked — live preview stopped");
                    if let Ok(mut stats) = panic_shared.stats.lock() {
                        stats.error = Some("preview worker panicked".into());
                    }
                }
            })
            .map_err(|e| AudioError::Stream(format!("preview worker spawn failed: {e}")))?;
        Ok(Self {
            shared,
            worker: Some(handle),
        })
    }

    /// The FIFO for the `Player` (consumer side).
    pub fn fifo(&self) -> &Arc<StreamFifo> {
        &self.shared.fifo
    }

    /// UI thread: queues a parameter-change restart at `position`
    /// (non-blocking; the worker applies it before its next chunk).
    pub fn restart(&self, params: &EngineParams, position: usize) {
        if let Ok(mut cmd) = self.shared.command.lock() {
            *cmd = Some(Command::Restart {
                params: *params,
                position,
            });
        }
    }

    /// Current worker statistics (for the status bar).
    pub fn stats(&self) -> PreviewStats {
        self.shared
            .stats
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// Buffered frames in the FIFO (forwarded for convenience).
    pub fn available(&self) -> usize {
        self.shared.fifo.available()
    }

    /// Playback cursor (absolute sample).
    pub fn read_frame(&self) -> u64 {
        self.shared.fifo.read_frame()
    }

    /// Stops the worker and waits for it. The FIFO becomes frozen; drop
    /// the `Player`'s copy afterwards.
    pub fn stop(mut self) {
        if let Ok(mut cmd) = self.shared.command.lock() {
            *cmd = Some(Command::Stop);
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl std::fmt::Debug for PreviewStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewStream")
            .field("available", &self.available())
            .field("read_frame", &self.read_frame())
            .finish_non_exhaustive()
    }
}

/// 10 ms crossfade at 48 kHz (scaled to the session rate by the worker).
const FADE_MS: u64 = 10;
/// 10 ms worker chunk at 48 kHz (scaled likewise).
const CHUNK_MS: u64 = 10;

/// Builds a streaming chain at `pos` (standalone fn: closures cannot
/// express the returned borrow of `track`).
fn build_chain<'a>(
    session: &[f32],
    sample_rate: u32,
    track: &'a PyinResult,
    params: &EngineParams,
    pos: usize,
) -> std::result::Result<StreamChain<'a>, mvl_core::error::CoreError> {
    StreamChain::new_at(session, sample_rate, track, params, pos)
}

fn worker_loop(
    shared: Arc<WorkerShared>,
    session: Arc<Vec<f32>>,
    sample_rate: u32,
    track: Arc<PyinResult>,
    initial_params: EngineParams,
    start: usize,
) {
    let ms_samples = u64::from(sample_rate) / 1000;
    let fade = usize::try_from(ms_samples * FADE_MS).unwrap_or(480).max(64);
    let chunk = usize::try_from(ms_samples * CHUNK_MS)
        .unwrap_or(480)
        .max(64);

    let mut splice: Option<(Vec<f32>, usize)> = None;
    let mut params = initial_params;
    let mut finished = false;
    // Blended samples the FIFO has not accepted yet (bounded by the chunk
    // + the finish tail; drained before new production).
    let mut pending: Vec<f32> = Vec::new();
    let start = start.min(session.len());

    let mut chain = match build_chain(&session, sample_rate, &track, &params, start) {
        Ok(c) => c,
        Err(e) => {
            if let Ok(mut stats) = shared.stats.lock() {
                stats.error = Some(e.to_string());
            }
            return;
        }
    };
    let mut cursor = chain.flushed();

    loop {
        // ── commands ────────────────────────────────────────────────────
        if let Ok(mut cmd) = shared.command.lock() {
            match cmd.take() {
                Some(Command::Stop) => return,
                Some(Command::Restart {
                    params: new_params,
                    position,
                }) => {
                    params = new_params;
                    let pos = position.min(session.len());
                    let t0 = Instant::now();
                    match build_chain(&session, sample_rate, &track, &params, pos) {
                        Ok(c) => {
                            chain = c;
                            cursor = chain.flushed();
                            finished = false;
                            pending.clear();
                            let old_tail = shared.fifo.splice(pos as u64, fade);
                            splice = Some((old_tail, 0));
                            if let Ok(mut stats) = shared.stats.lock() {
                                stats.last_restart_us =
                                    Some(u64::try_from(t0.elapsed().as_micros()).unwrap_or(0));
                                stats.restarts += 1;
                            }
                        }
                        Err(e) => {
                            if let Ok(mut stats) = shared.stats.lock() {
                                stats.error = Some(e.to_string());
                            }
                        }
                    }
                }
                None => {}
            }
        }

        // ── produce ────────────────────────────────────────────────────
        let mut progressed = false;
        // 1. Drain already-blended samples into the FIFO (any room helps).
        if !pending.is_empty() {
            let pushed = shared.fifo.push(&pending);
            pending.drain(..pushed);
            if let Ok(mut stats) = shared.stats.lock() {
                stats.produced += pushed as u64;
            }
            if pushed > 0 {
                progressed = true;
            }
        }
        // 2. Produce a new chunk when the remainder is delivered and the
        //    FIFO has room for a full chunk plus splice headroom.
        let free = shared.fifo.free();
        if pending.is_empty() && !finished && free > chunk + fade {
            let mut out: Vec<f32> = Vec::with_capacity(chunk * 2);
            if cursor < session.len() {
                // Ask the chain for the next chunk.
                let target = usize::min(cursor + chunk, session.len());
                match chain.advance_to(&session, target) {
                    Ok(()) => chain.take_output(&mut out),
                    Err(e) => {
                        if let Ok(mut stats) = shared.stats.lock() {
                            stats.error = Some(e.to_string());
                        }
                        return;
                    }
                }
                if out.is_empty() {
                    // Work was requested and the chain still produced
                    // nothing — it is exhausted (the overlap-add tails
                    // never flush on their own): run the tail flush once.
                    finished = true;
                    match chain.finish(&session) {
                        Ok(()) => chain.take_output(&mut out),
                        Err(e) => {
                            if let Ok(mut stats) = shared.stats.lock() {
                                stats.error = Some(e.to_string());
                            }
                            return;
                        }
                    }
                }
            } else {
                // Cursor at the end: only the tail flush remains.
                finished = true;
                match chain.finish(&session) {
                    Ok(()) => chain.take_output(&mut out),
                    Err(e) => {
                        if let Ok(mut stats) = shared.stats.lock() {
                            stats.error = Some(e.to_string());
                        }
                        return;
                    }
                }
            }
            if !out.is_empty() {
                blend_splice(&mut out, &mut splice, fade);
                // The whole chunk is blended and owned here — consumed
                // from the chain in full; whatever the FIFO cannot take
                // yet parks in `pending` for step 1 of the next rounds.
                chain.consume(out.len());
                cursor = chain.flushed();
                let pushed = shared.fifo.push(&out);
                pending.extend_from_slice(&out[pushed..]);
                if let Ok(mut stats) = shared.stats.lock() {
                    stats.produced += pushed as u64;
                }
                if pushed > 0 {
                    progressed = true;
                }
            }
        }

        if !progressed {
            // Backpressured (FIFO full), finished, or waiting — idle briefly.
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Blends the first samples of `out` against the captured old tail (linear
/// crossfade over `fade`); positions beyond the captured tail fade the new
/// audio in from zero, so a short (or empty) tail never clicks.
fn blend_splice(out: &mut [f32], splice: &mut Option<(Vec<f32>, usize)>, fade: usize) {
    let Some((old_tail, taken)) = splice.as_mut() else {
        return;
    };
    let fade = fade.max(1);
    for (i, sample) in out.iter_mut().enumerate() {
        let j = *taken + i;
        if j >= fade {
            break;
        }
        let t = j as f32 / fade as f32;
        *sample = match old_tail.get(j) {
            Some(&old) => old * (1.0 - t) + *sample * t,
            None => *sample * t, // fade-in when the old tail ran short
        };
    }
    *taken += out.len();
    if *taken >= fade {
        *splice = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::AudioBuffer;
    use mvl_core::synth::{self, Rng, VowelSpec};

    fn vowel_session(sr: u32) -> (Arc<Vec<f32>>, Arc<PyinResult>) {
        let track_f0 = synth::f0_track_const(170.0, sr as usize / 2);
        let mut rng = Rng::new(81);
        let mono = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
        let track = mvl_core::pyin::pyin(&mono, sr).expect("pyin");
        (Arc::new(mono), Arc::new(track))
    }

    fn params_active() -> EngineParams {
        let mut params = EngineParams::default();
        params.set_pitch_semitones(2.0);
        params.set_air_db(3.0);
        params
    }

    fn wait_until(mut check: impl FnMut() -> bool, timeout_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        check()
    }

    #[test]
    fn fifo_ring_wraps_and_tracks_positions() {
        let fifo = StreamFifo::new(8);
        fifo.set_total(100);
        assert_eq!(fifo.push(&[1.0, 2.0, 3.0, 4.0, 5.0]), 5);
        assert_eq!(fifo.available(), 5);
        assert_eq!(fifo.read_frame(), 0);

        let mut out = vec![0.0f32; 6]; // 3 frames stereo
        let frames = fifo.pull_mono_into(&mut out, 2);
        assert_eq!(frames, 3);
        assert_eq!(out, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        assert_eq!(fifo.read_frame(), 3);
        // The ring floors its capacity at 64 slots.
        assert_eq!(fifo.free(), 64 - 2);

        // Wrap-around push/pull: after the first pull (read=3, write=5),
        // six samples fit; read then trails write by the full capacity.
        assert_eq!(fifo.push(&[6.0, 7.0, 8.0, 9.0, 10.0, 11.0]), 6);
        let mut out2 = vec![0.0f32; 8];
        let frames2 = fifo.pull_mono_into(&mut out2, 1);
        assert_eq!(frames2, 8);
        assert_eq!(out2, vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
        assert_eq!(fifo.read_frame(), 11);
    }

    #[test]
    fn fifo_underrun_flags_and_splice_rewinds() {
        let fifo = StreamFifo::new(16);
        fifo.set_total(100);
        let mut out = vec![0.0f32; 4];
        assert_eq!(fifo.pull_mono_into(&mut out, 1), 0);
        assert!(fifo.take_underrun());
        assert!(!fifo.take_underrun(), "flag must clear on take");

        fifo.push(&[1.0, 2.0, 3.0, 4.0]);
        // Continuity splice at 2: keeps positions 2..4 as the fade source.
        let tail = fifo.splice(2, 8);
        assert_eq!(tail, vec![3.0, 4.0]);
        assert_eq!(fifo.available(), 2);
        assert_eq!(fifo.read_frame(), 0);
        // A second splice inside the buffered window is still continuous.
        let tail2 = fifo.splice(0, 8);
        assert_eq!(tail2, vec![1.0, 2.0]);
        assert_eq!(fifo.read_frame(), 0);
    }

    #[test]
    fn fifo_hard_jump_holds_consumer_until_push() {
        let fifo = StreamFifo::new(16);
        fifo.set_total(100);
        fifo.push(&[1.0, 2.0]);
        // Hard jump beyond the produced frontier.
        let tail = fifo.splice(10, 8);
        assert!(tail.is_empty());
        let mut out = [0.0f32; 4];
        assert_eq!(fifo.pull_mono_into(&mut out, 1), 0, "consumer must be held");
        assert!(!fifo.take_underrun(), "a hold is not an underrun");
        // The producer's first push releases the hold.
        fifo.push(&[9.0, 10.0, 11.0]);
        let frames = fifo.pull_mono_into(&mut out, 1);
        assert_eq!(frames, 3);
        assert_eq!(&out[..3], &[9.0, 10.0, 11.0]);
        assert_eq!(fifo.read_frame(), 13);
    }

    #[test]
    fn fifo_backward_jump_discards_played_region() {
        let fifo = StreamFifo::new(16);
        fifo.set_total(100);
        fifo.push(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = [0.0f32; 2];
        assert_eq!(fifo.pull_mono_into(&mut out, 1), 2); // read = 2
        let tail = fifo.splice(0, 8); // backward jump below the cursor
        assert!(tail.is_empty());
        assert_eq!(fifo.available(), 0);
        assert_eq!(fifo.pull_mono_into(&mut out, 1), 0, "held after jump");
        fifo.push(&[7.0, 8.0]);
        assert_eq!(fifo.pull_mono_into(&mut out, 1), 2);
        assert_eq!(out, [7.0, 8.0]);
        assert_eq!(fifo.read_frame(), 2);
    }

    /// End-to-end gate: what the worker produces (pulled from the FIFO) is
    /// bit-identical to the offline render of the same parameters.
    #[test]
    fn streamed_preview_matches_offline_render() {
        let sr = 48_000u32;
        let (session, track) = vowel_session(sr);
        let params = params_active();
        let (full, _, _) =
            mvl_core::pipeline::render(&session, sr, &params).expect("offline render");

        let stream = PreviewStream::start(
            Arc::clone(&session),
            sr,
            Arc::clone(&track),
            &params,
            0,
            4800,
        )
        .expect("start");
        let mut pulled: Vec<f32> = Vec::new();
        assert!(
            wait_until(
                || {
                    let mut buf = [0.0f32; 480];
                    let frames = stream.fifo().pull_mono_into(&mut buf, 1);
                    pulled.extend_from_slice(&buf[..frames]);
                    pulled.len() >= full.len()
                },
                10_000
            ),
            "worker did not produce the full render in time"
        );
        stream.stop();
        assert_eq!(pulled.len(), full.len());
        assert_eq!(
            pulled, full,
            "streamed preview diverged from offline render"
        );
    }

    /// Restart semantics (formant + air — the deterministic STFT stages):
    /// after a parameter change, the FIFO content beyond the disclosed
    /// artifact window is **bit-identical** to the offline render of the
    /// new parameters. The restart here lands beyond the produced frontier
    /// (hard jump + hold) — the seek geometry. Uses a 1-second fixture so
    /// the restart lands mid-session.
    #[test]
    fn restart_converges_to_new_params() {
        let sr = 48_000u32;
        let track_f0 = synth::f0_track_const(170.0, sr as usize);
        let mut rng = Rng::new(81);
        let mono = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
        let track = Arc::new(mvl_core::pyin::pyin(&mono, sr).expect("pyin"));
        let session = Arc::new(mono);
        let first = params_active();
        let mut second = EngineParams::default();
        // STFT stages only: the pitch stage resets its read-pointer phase
        // on a restart (disclosed) — its gate is the comb test below.
        second.set_formant_mm(140.0);
        second.set_air_db(-6.0);
        let (full_new, _, _) =
            mvl_core::pipeline::render(&session, sr, &second).expect("offline render new");

        let pos = 24_000usize;
        let stream = PreviewStream::start(
            Arc::clone(&session),
            sr,
            Arc::clone(&track),
            &first,
            0,
            4800,
        )
        .expect("start");

        // Position-aware drain: pulled samples land at their absolute FIFO
        // positions (the cursor may jump on hard splices).
        let mut rendered = vec![0.0f32; session.len()];
        let drain = |stream: &PreviewStream, rendered: &mut [f32]| -> usize {
            let mut furthest = 0usize;
            loop {
                let mut buf = [0.0f32; 480];
                let frames = stream.fifo().pull_mono_into(&mut buf, 1);
                if frames == 0 {
                    break;
                }
                let start = (stream.fifo().read_frame() as usize).saturating_sub(frames);
                let end = usize::min(start + frames, rendered.len());
                if start < rendered.len() {
                    rendered[start..end].copy_from_slice(&buf[..end - start]);
                }
                furthest = furthest.max(end);
            }
            furthest
        };
        assert!(
            wait_until(|| drain(&stream, &mut rendered) > 960, 10_000),
            "no initial production"
        );
        stream.restart(&second, pos);
        // Completion signal: the playback cursor reached the end (a hard
        // jump discards a region, so a pulled-sample counter would never
        // reach the session length).
        assert!(
            wait_until(
                || {
                    drain(&stream, &mut rendered);
                    stream.read_frame() >= session.len() as u64
                },
                20_000
            ),
            "worker did not re-produce the full render in time"
        );
        let stats = stream.stats();
        stream.stop();

        // Everything past the artifact window must match the new render.
        let n_air =
            usize::max(1024, (2048.0 * sr as f64 / 48_000.0).round() as usize).next_power_of_two();
        let bound = pos + 2 * n_air + 8192;
        assert_eq!(
            &rendered[bound..],
            &full_new[bound..],
            "restarted preview did not converge to the new parameters"
        );
        assert!(stats.restarts >= 1);
        assert!(stats.last_restart_us.is_some());
    }

    /// Restart with the **pitch** stage active: the PSOLA read-pointer
    /// phase resets (disclosed — not sample-exact vs the offline render),
    /// but the comb at the *new* target pitch must dominate from beyond
    /// the artifact window onward.
    #[test]
    fn restart_with_pitch_moves_the_comb() {
        let sr = 48_000u32;
        let track_f0 = synth::f0_track_const(170.0, sr as usize);
        let mut rng = Rng::new(81);
        let mono = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
        let track = Arc::new(mvl_core::pyin::pyin(&mono, sr).expect("pyin"));
        let session = Arc::new(mono);
        let first = params_active();
        let mut second = EngineParams::default();
        second.set_pitch_semitones(-2.0);
        second.set_air_db(3.0);

        let pos = 24_000usize;
        let stream = PreviewStream::start(
            Arc::clone(&session),
            sr,
            Arc::clone(&track),
            &first,
            0,
            4800,
        )
        .expect("start");
        let mut rendered = vec![0.0f32; session.len()];
        let drain = |stream: &PreviewStream, rendered: &mut [f32]| -> usize {
            let mut furthest = 0usize;
            loop {
                let mut buf = [0.0f32; 480];
                let frames = stream.fifo().pull_mono_into(&mut buf, 1);
                if frames == 0 {
                    break;
                }
                let start = (stream.fifo().read_frame() as usize).saturating_sub(frames);
                let end = usize::min(start + frames, rendered.len());
                if start < rendered.len() {
                    rendered[start..end].copy_from_slice(&buf[..end - start]);
                }
                furthest = furthest.max(end);
            }
            furthest
        };
        assert!(
            wait_until(|| drain(&stream, &mut rendered) > 960, 10_000),
            "no initial production"
        );
        stream.restart(&second, pos);
        assert!(
            wait_until(
                || {
                    drain(&stream, &mut rendered);
                    stream.read_frame() >= session.len() as u64
                },
                20_000
            ),
            "worker did not finish the restarted render"
        );
        stream.stop();

        // The comb at the new target (-2 st from 170 Hz) dominates late in
        // the restarted render. The cepstral instrument needs ≥ 8192
        // samples (its Welch FFT size), hence the 12 000-sample window.
        let target = 170.0f64 * 2.0f64.powf(-2.0 / 12.0);
        let window = &rendered[36_000..];
        let s_target = mvl_core::measure::ceps_strength(window, sr, target);
        let s_off = mvl_core::measure::ceps_strength(window, sr, target * 2.0f64.powf(3.5 / 12.0));
        assert!(
            s_target > 2.0 * s_off,
            "restarted pitch stage did not take over (target {s_target:.3} vs off {s_off:.3})"
        );
    }

    /// The worker honours the FIFO bound (no unbounded production when the
    /// consumer is silent) — the RAM story for long sessions.
    #[test]
    fn production_is_backpressured() {
        let sr = 48_000u32;
        let (session, track) = vowel_session(sr);
        let params = params_active();
        let capacity = 4800usize;
        let stream = PreviewStream::start(session, sr, track, &params, 0, capacity).expect("start");
        assert!(
            wait_until(|| stream.available() > capacity - 960, 10_000),
            "worker should fill up to the high-water mark"
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            stream.available() <= capacity,
            "FIFO exceeded its capacity bound"
        );
        stream.stop();
    }

    /// Sanity: `AudioBuffer` round-trip of the pulled preview (the Player
    /// path maps 1→N; ensure the samples survive an interleaved wrap).
    #[test]
    fn pulled_preview_maps_to_stereo() {
        let buffer =
            AudioBuffer::from_interleaved(48_000, 1, vec![0.25, -0.5, 0.75]).expect("buffer");
        let mut out = vec![0.0f32; 6];
        let fifo = StreamFifo::new(8);
        fifo.push(buffer.samples());
        let frames = fifo.pull_mono_into(&mut out, 2);
        assert_eq!(frames, 3);
        assert_eq!(out, vec![0.25, 0.25, -0.5, -0.5, 0.75, 0.75]);
    }
}
