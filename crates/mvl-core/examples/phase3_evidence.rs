//! Phase 3 evidence corpus: synthesizes deterministic vocal fixtures,
//! renders them through the three engines and writes before/after WAVs
//! plus a metrics summary to `docs/evidence/phase3/`.
//!
//! Run from the repo root:
//!   cargo run -p mvl-core --example phase3_evidence -- <output-dir>
//! (defaults to `docs/evidence/phase3` when no argument is given)

use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

use mvl_core::engine::EngineParams;
use mvl_core::measure;
use mvl_core::pipeline::render;
use mvl_core::synth::{self, Rng, VowelSpec, VowelSpec as VS};

const SR: u32 = 48_000;

fn write_wav(path: &Path, x: &[f32]) -> std::io::Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SR,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    for &s in x {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(v).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    }
    writer.finalize().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/evidence/phase3".to_string());
    std::fs::create_dir_all(&out_dir)?;
    let out = Path::new(&out_dir);

    // ---- fixture A: clean studio vowel, 160 Hz --------------------------
    let track_f0 = synth::f0_track_const(160.0, SR as usize);
    let mut rng = Rng::new(1601);
    let vowel_a = synth::vowel(&track_f0, SR, &VowelSpec::default(), &mut rng);
    write_wav(&out.join("vocal_a.wav"), &vowel_a)?;

    // ---- fixture B: phrase with breath bursts between vowels ------------
    let seg = SR as usize * 2 / 5;
    let mut rng_b = Rng::new(1602);
    let mut vowel_b = synth::vowel(&synth::f0_track_const(175.0, seg), SR, &VS::default(), &mut rng_b);
    for &f in &[190.0f32, 165.0] {
        let burst = synth::white_noise(seg / 2, &mut rng_b);
        vowel_b.extend(burst.iter().map(|v| v * 0.25));
        vowel_b.append(&mut synth::vowel(&synth::f0_track_const(f, seg), SR, &VS::default(), &mut rng_b));
    }
    write_wav(&out.join("vocal_b_breathy.wav"), &vowel_b)?;

    // ---- renders ---------------------------------------------------------
    let mut p_pitch = EngineParams::default();
    p_pitch.set_pitch_semitones(4.0);
    let (a_pitch, _, rep_pitch) = render(&vowel_a, SR, &p_pitch)?;
    write_wav(&out.join("vocal_a_pitch_plus4.wav"), &a_pitch)?;

    let mut p_formant = EngineParams::default();
    p_formant.set_formant_mm(130.0);
    let (a_formant, _, rep_formant) = render(&vowel_a, SR, &p_formant)?;
    write_wav(&out.join("vocal_a_formant_130mm.wav"), &a_formant)?;

    let mut p_air = EngineParams::default();
    p_air.set_air_db(6.0);
    let (a_air, _, rep_air) = render(&vowel_a, SR, &p_air)?;
    write_wav(&out.join("vocal_a_air_plus6.wav"), &a_air)?;

    let mut p_b = EngineParams::default();
    p_b.set_air_db(-18.0);
    let (b_air, _, rep_b_air) = render(&vowel_b, SR, &p_b)?;
    write_wav(&out.join("vocal_b_air_minus18.wav"), &b_air)?;

    let mut p_all = EngineParams::default();
    p_all.set_pitch_semitones(4.0);
    p_all.set_formant_mm(130.0);
    p_all.set_air_db(6.0);
    let (a_all, _, rep_all) = render(&vowel_a, SR, &p_all)?;
    write_wav(&out.join("vocal_a_combined.wav"), &a_all)?;

    // ---- metrics ----------------------------------------------------------
    let nfft = 8192usize;
    let bin_hz = SR as f64 / nfft as f64;
    let mut m = String::new();
    m.push_str("Micro-Vocal Lab — Phase 3 evidence metrics\n");
    m.push_str("==========================================\n");
    m.push_str("fixture A: 160 Hz vowel (Lorentzian formants 500/1500/2500/3500)\n");
    m.push_str("fixture B: 175/190/165 Hz vowels with white-noise breath bursts\n\n");

    let f1 = |x: &[f32]| {
        let env = measure::cepstral_envelope(x, nfft, SR as usize / 300);
        measure::envelope_peak(&env, bin_hz, 350.0, 750.0)
    };
    m.push_str(&format!(
        "A: cepstral F1 = {:.1} Hz (prescribed 500)\n",
        f1(&vowel_a)
    ));
    m.push_str(&format!(
        "A formant 130mm: cepstral F1 = {:.1} Hz (prescribed 500×1.346 = 673)\n",
        f1(&a_formant)
    ));

    let pitch_comb = |x: &[f32], f: f64| measure::ceps_strength(x, SR, f);
    let target_a = 160.0f64 * 2.0f64.powf(4.0 / 12.0);
    m.push_str(&format!(
        "A pitch +4st: cepstral comb strength at {:.1} Hz = {:.2} (target), at 160 Hz = {:.2} (input)\n",
        target_a,
        pitch_comb(&a_pitch, target_a),
        pitch_comb(&a_pitch, 160.0)
    ));

    let band = |x: &[f32]| -> f64 {
        // RMS in the 3–4.5 kHz residual band (A has no harmonics above 2.6 kHz
        // of note: A's harmonics run to Nyquist, so we measure the residual
        // between harmonics: bins excluded by ±1 bin of k·160).
        let mut acc = 0.0f64;
        let mut cnt = 0usize;
        for (k, &s) in x.iter().enumerate().take(x.len() / 2) {
            let _ = k;
            acc += f64::from(s) * f64::from(s);
            cnt += 1;
        }
        (acc / cnt as f64).sqrt()
    };
    let _ = band;
    // Residual band measure via band power ratio (500–4000 Hz on the
    // breath-burst fixture B, inter-phrase segment only).
    let b_mid = &vowel_b[seg..seg + seg / 2];
    let b_mid_out = &b_air[seg..seg + seg / 2];
    let rms = |x: &[f32]| (x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len() as f64).sqrt();
    m.push_str(&format!(
        "B air −18dB: inter-phrase breath RMS {:.4} → {:.4} ({:+.1} dB)\n",
        rms(b_mid),
        rms(b_mid_out),
        20.0 * (rms(b_mid_out) / rms(b_mid)).log10()
    ));

    for (name, rep) in [
        ("A pitch +4st", &rep_pitch),
        ("A formant 130mm", &rep_formant),
        ("A air +6dB", &rep_air),
        ("B air −18dB", &rep_b_air),
        ("A combined", &rep_all),
    ] {
        m.push_str(&format!(
            "{name}: frames={} voiced={:.2} medianF0={:.1}Hz stages[pitch,formant,air]={:?}\n",
            rep.frames_analyzed, rep.voiced_ratio, rep.median_f0_hz, rep.stages_applied
        ));
    }
    std::fs::write(out.join("metrics.txt"), &m)?;
    print!("{m}");
    println!("evidence written to {out_dir}/");
    Ok(())
}
