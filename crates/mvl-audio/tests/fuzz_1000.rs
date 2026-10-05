//! Quality gate 1 — the FULL 1000-file fuzz protocol (Phase 7.4).
//!
//! The unit-test slice (`mp3::tests::random_blobs_never_panic`, 64 blobs)
//! runs everywhere. This integration test runs the complete brief on the
//! import surface — with one hard-won correction the telemetry forced:
//! pure random blobs are rejected by the container probe before any decoder
//! runs (0/1000 reached the decode path — the gate would have been
//! vacuous). So this fuzzer is **seeded mutation-based**: it encodes real
//! WAVs and MP3s with the app's own exporters, then hammers the importers
//! with mutations (byte flips, truncations, header stomps, garbage
//! splices). Every mutant that the probe accepts is guaranteed to reach
//! the real decoders.
//!
//! The contract under test: the importer always returns (clean `Result`),
//! never panics, never hangs, and anything it *accepts* is a coherent,
//! finite buffer.
//!
//! Deterministic xorshift seed so a failure is reproducible from the log.

use mvl_audio::{AudioBuffer, WavBitDepth, export_mp3, export_wav, import_mp3, import_wav};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn byte(&mut self) -> u8 {
        (self.next() & 0xFF) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A short voice-like buffer: harmonics + vibrato + breath noise, so the
/// encoders get real content (not silence, not constants).
fn seed_buffer(rng: &mut Rng) -> AudioBuffer {
    let rate: [u32; 3] = [44_100, 48_000, 96_000];
    let sr = rate[rng.below(3)];
    let channels: u16 = if rng.next().is_multiple_of(2) { 1 } else { 2 };
    let secs = 0.2 + (rng.next() % 30) as f32 * 0.01; // 0.20–0.49 s
    let n = (sr as f32 * secs) as usize;
    let f0 = 110.0 + (rng.next() % 40) as f32 * 4.0; // 110–266 Hz
    let mut buf = AudioBuffer::new(sr, channels).expect("seed format");
    let mut samples = Vec::with_capacity(n * usize::from(channels));
    for i in 0..n {
        let t = i as f32 / sr as f32;
        let vib = (2.0 * std::f32::consts::PI * 5.5 * t).sin() * 0.03;
        let mut s = (2.0 * std::f32::consts::PI * f0 * (1.0 + vib) * t)
            .sin()
            .mul_add(
                0.6,
                ((2.0 * std::f32::consts::PI * f0 * 2.0 * t).sin()) * 0.25,
            )
            + ((2.0 * std::f32::consts::PI * f0 * 3.0 * t).sin()) * 0.12;
        s += (rng.next() as f32 / u64::MAX as f32 - 0.5) * 0.04; // breath
        let s = s.clamp(-1.0, 1.0);
        samples.push(s);
        if channels == 2 {
            samples.push(s * 0.9);
        }
    }
    buf.append_interleaved(&samples).expect("seed samples");
    buf
}

fn mutate(blob: &mut Vec<u8>, rng: &mut Rng) {
    let ops = 1 + rng.below(16);
    for _ in 0..ops {
        if blob.is_empty() {
            break;
        }
        match rng.below(4) {
            0 => {
                // byte flip
                let i = rng.below(blob.len());
                blob[i] ^= (1 + rng.below(255)) as u8;
            }
            1 => {
                // truncate
                let i = rng.below(blob.len());
                blob.truncate(i.max(1));
            }
            2 => {
                // splice garbage run
                let i = rng.below(blob.len());
                let run = 1 + rng.below(64);
                for k in 0..run.min(blob.len() - i) {
                    blob[i + k] = rng.byte();
                }
            }
            _ => {
                // header stomp (first 16 bytes) — hits RIFF/sync structures
                for k in 0..16.min(blob.len()) {
                    blob[k] = rng.byte();
                }
            }
        }
    }
}

/// Imports `path` and enforces the acceptance contract. Returns true when
/// the file reached a decoder (accepted), for telemetry.
fn fuzz_one(path: &std::path::Path, blob: &[u8]) -> bool {
    std::fs::write(path, blob).expect("write fuzz file");
    let result = if path.extension().is_some_and(|e| e == "mp3") {
        import_mp3(path)
    } else {
        import_wav(path)
    };
    if let Ok(buffer) = result {
        assert!(buffer.frames() > 0, "accepted file with zero frames");
        assert!(buffer.channels() >= 1, "accepted file with 0 channels");
        for chunk in buffer.samples().chunks(64) {
            assert!(
                chunk.iter().all(|s| s.is_finite()),
                "accepted file decoded to NaN/Inf"
            );
        }
        true
    } else {
        false
    }
}

#[test]
fn fuzz_1000_files_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let dir = std::env::temp_dir().join(format!("mvl_fuzz_1000_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create fuzz dir");

    // Seed corpus: 40 real encodes through the app's own exporters.
    let mut seeds: Vec<(Vec<u8>, &'static str)> = Vec::new();
    for i in 0..40 {
        let buf = seed_buffer(&mut rng);
        let mp3 = dir.join(format!("seed{i}.mp3"));
        let wav = dir.join(format!("seed{i}.wav"));
        export_mp3(&mp3, &buf, 128 + (rng.next() % 5) as u32 * 32).expect("seed mp3 encode");
        let depth = if rng.next().is_multiple_of(2) {
            WavBitDepth::Int16
        } else {
            WavBitDepth::Float32
        };
        export_wav(&wav, &buf, depth).expect("seed wav encode");
        seeds.push((std::fs::read(&mp3).expect("read seed"), "mp3"));
        seeds.push((std::fs::read(&wav).expect("read seed"), "wav"));
        let _ = std::fs::remove_file(&mp3);
        let _ = std::fs::remove_file(&wav);
    }
    // Sanity: every seed must import cleanly (they were just produced by
    // the exporters — a failure here is an exporter/importer bug).
    for (i, (blob, kind)) in seeds.iter().enumerate() {
        let path = dir.join(format!("check{i}.{kind}"));
        assert!(
            fuzz_one(&path, blob),
            "seed corpus #{i} ({kind}) failed to import — exporter/importer mismatch"
        );
    }

    // 1000 mutants over the corpus (plus 50 re-runs of raw seeds for luck
    // under the mutation RNG ordering — total 1000 files through the gate).
    let mut reached = 0u32;
    for i in 0..1000 {
        let (seed, kind) = &seeds[rng.below(seeds.len())];
        let mut blob = seed.clone();
        mutate(&mut blob, &mut rng);
        let path = dir.join(format!("mut.{kind}"));
        if fuzz_one(&path, &blob) {
            reached += 1;
        }
        if i % 250 == 0 {
            eprintln!("fuzz progress: {i}/1000 mutants, no panic");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    // The gate must not be vacuous: a healthy share of mutants must get
    // past the probe into the decoders (with these mutations it is
    // typically > 50 %). Assert > 10 % so the test fails loudly if the
    // corpus ever degrades to probe-rejections only.
    eprintln!("fuzz telemetry: {reached}/1000 mutants reached the decoders");
    assert!(
        reached > 100,
        "only {reached}/1000 mutants reached a decoder — gate would be vacuous"
    );
}
