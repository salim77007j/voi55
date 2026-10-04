//! Render-throughput harness — Phase 5 runtime verification.
//!
//! Budget from the brief: **preview DSP load < 40 % of one core @ 48 kHz**,
//! i.e. the preview render must run at ≥ 2.5× real-time on a single core.
//!
//! Two paths are measured, mirroring the app's real usage (Phase 4.3/4.4):
//! - **preview** — pYIN analysed once and cached; every slider move renders
//!   through `render_mono_to_buffer(.., Some(&track))`. This is the number
//!   the budget applies to (all three stages engaged: pitch + formant + air).
//! - **cold** — one pass including pYIN (first render after load / export
//!   render). Informational: the status bar shows this cost to users.
//!
//! Run: `cargo run --release -p mvl-audio --example bench_render`
//! Exits non-zero if the preview budget is missed.

use mvl_audio::AudioBuffer;
use mvl_audio::engine::render_mono_to_buffer;
use mvl_core::engine::EngineParams;
use mvl_core::pyin::pyin;
use mvl_core::synth::{self, Rng, VowelSpec};
use std::time::{Duration, Instant};

const SECS: usize = 30;
const SR: u32 = 48_000;
/// 40 % of one core ⇔ 2.5× real-time.
const MIN_PREVIEW_FACTOR: f64 = 2.5;

/// Male "ah" fixture: 220 Hz with ±60 ¢ vibrato at 5.5 Hz, F1–F4 formants,
/// slight breath and shimmer — the demanding-but-typical solo-vocal case
/// the latency budget in the brief is written for.
fn vocal_fixture() -> Vec<f32> {
    let f0_track = synth::vibrato_f0_track(220.0, 60.0, 5.5, SECS * SR as usize, SR);
    let spec = VowelSpec {
        formants: vec![
            (730.0, 90.0, 1.0),
            (1090.0, 90.0, 0.5),
            (2440.0, 120.0, 0.3),
            (3400.0, 150.0, 0.2),
        ],
        breath: 0.15,
        jitter: 0.05,
    };
    synth::vowel(&f0_track, SR, &spec, &mut Rng::new(2026))
}

fn median(v: &mut [Duration]) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn main() {
    println!(
        "bench_render: {SECS} s mono vocal @ {SR} Hz, pitch +2 st / air +6 dB / formant 140 mm \
         (all stages engaged)"
    );

    let mono = vocal_fixture();
    let buf = AudioBuffer::from_interleaved(SR, 1, mono.clone()).expect("fixture buffer");
    assert_eq!(buf.duration_secs(), SECS as f64);

    let mut params = EngineParams::default();
    params.set_pitch_semitones(2.0);
    params.set_air_db(6.0);
    params.set_formant_mm(140.0);

    // pYIN once, as the app does on load (cached for every later preview).
    let t_pyin = Instant::now();
    let track = pyin(&mono, SR).expect("pyin on fixture");
    let pyin_secs = t_pyin.elapsed().as_secs_f64();
    println!(
        "pyin (cold analysis)      : {pyin_secs:8.3} s  ({:.1}× real-time)",
        SECS as f64 / pyin_secs
    );

    // Preview path: render with the cached track (what a slider move costs).
    let mut preview = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let rendered =
            render_mono_to_buffer(&mono, SR, 1, &params, Some(&track)).expect("preview render");
        preview.push(t.elapsed());
        assert_eq!(rendered.frames(), buf.frames(), "duration must be exact");
    }
    let p = median(&mut preview);
    let p_factor = SECS as f64 / p.as_secs_f64();
    println!(
        "preview (cached pYIN)     : {:8.3} s  ({:.1}× real-time, median of 5)",
        p.as_secs_f64(),
        p_factor
    );

    // Cold path: first render / export render (pYIN + all stages).
    let mut cold = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        render_mono_to_buffer(&mono, SR, 1, &params, None).expect("cold render");
        cold.push(t.elapsed());
    }
    let c = median(&mut cold);
    println!(
        "cold (pYIN + render)      : {:8.3} s  ({:.1}× real-time, median of 3)",
        c.as_secs_f64(),
        SECS as f64 / c.as_secs_f64()
    );

    println!();
    println!(
        "BENCH preview_factor={p_factor:.2} pyin_factor={:.2} cold_factor={:.2}",
        SECS as f64 / pyin_secs,
        SECS as f64 / c.as_secs_f64()
    );
    if p_factor < MIN_PREVIEW_FACTOR {
        eprintln!(
            "BUDGET FAILED: preview {p_factor:.2}× < {MIN_PREVIEW_FACTOR}× \
             (40 % of one core)"
        );
        std::process::exit(1);
    }
    println!(
        "BUDGET OK: preview load = {:.0} % of one core (< 40 %)",
        100.0 / p_factor
    );
}
