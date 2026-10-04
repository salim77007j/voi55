//! RSS probe — Phase 5 memory investigation. Loads the 3-min fixture the
//! same way the app does and reports peak RSS after each stage, so the
//! dominant allocation is measured, not guessed.
//!
//! Run: cargo run --release -p mvl-audio --example rss_probe -- /tmp/mvl-session-3min.wav

use mvl_audio::{AudioBuffer, wav::import_wav};
use mvl_core::pyin::pyin;
use std::time::Instant;

fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| {
                l.split_whitespace()
                    .nth(1)
                    .and_then(|v| v.parse::<u64>().ok())
            })
        })
        .unwrap_or(0)
}

fn stage(name: &str) {
    eprintln!("peak RSS after {name:<28}: {:>8} kB", peak_rss_kb());
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: rss_probe WAV");
    stage("start");

    let t = Instant::now();
    let buf = import_wav(std::path::Path::new(&path)).expect("import");
    eprintln!(
        "import_wav: {} frames, {} ch, {} Hz in {:?}",
        buf.frames(),
        buf.channels(),
        buf.sample_rate(),
        t.elapsed()
    );
    stage("import_wav");

    let mono: Vec<f32> = if buf.channels() == 1 {
        buf.samples().to_vec()
    } else {
        buf.samples()
            .chunks(buf.channels() as usize)
            .map(|c| c.iter().sum::<f32>() / buf.channels() as f32)
            .collect()
    };
    eprintln!("mono cache: {} samples", mono.len());
    stage("mono downmix");

    let t = Instant::now();
    let track = pyin(&mono, buf.sample_rate()).expect("pyin");
    eprintln!("pyin: {} frames in {:?}", track.len(), t.elapsed());
    stage("pyin");

    // The app's preview path: render with the cached track, mono storage.
    let mut params = mvl_core::engine::EngineParams::default();
    params.set_pitch_semitones(2.0);
    params.set_air_db(6.0);
    params.set_formant_mm(140.0);

    // Per-stage anatomy: where do the render transients live?
    let shifted = mvl_core::psola::pitch_shift(&mono, buf.sample_rate(), &track, 2.0);
    stage("psola pitch_shift");
    drop(shifted);
    stage("dropped psola out");

    let formed = mvl_core::formant::shift_formants(&mono, buf.sample_rate(), &track, 140.0)
        .expect("formant");
    stage("formant shift_formants");
    drop(formed);
    stage("dropped formant out");

    let aired = mvl_core::air::process_air(&mono, buf.sample_rate(), &track, 6.0).expect("air");
    stage("air process_air");
    drop(aired);
    stage("dropped air out");

    let t = Instant::now();
    let preview = mvl_audio::engine::render_mono_to_buffer(
        &mono,
        buf.sample_rate(),
        1,
        &params,
        Some(&track),
    )
    .expect("preview render");
    eprintln!(
        "preview render (mono): {} frames in {:?}",
        preview.frames(),
        t.elapsed()
    );
    stage("preview render (mono)");
    drop(preview);
    stage("dropped preview");

    // What a stereo restore costs (export path).
    let stereo = mvl_audio::engine::render_mono_to_buffer(
        &mono,
        buf.sample_rate(),
        2,
        &params,
        Some(&track),
    )
    .expect("stereo render");
    stage("stereo render (export)");
    drop(stereo);
    stage("dropped stereo");

    drop(mono);
    drop(track);
    stage("dropped mono+track");

    let _ = AudioBuffer::silence(0.001, 48_000, 1).expect("warm");
    stage("end");
}
