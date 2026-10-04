//! Bridge between the I/O layer (`AudioBuffer`) and the DSP engine
//! (`mvl-core::pipeline`). This is the seam the UI will drive in Phase 4:
//! a project buffer goes in, a fully rendered buffer comes out.
//!
//! The engine is mono; multi-channel input is downmixed (equal power is
//! not needed for voice stems — a plain average preserves the level of a
//! centered vocal) and the result is copied back to every channel, so a
//! stereo render stays phase-coherent.

use crate::buffer::AudioBuffer;
use crate::error::{AudioError, Result};
use mvl_core::engine::EngineParams;

/// Renders `buf` offline with the Micro-Vocal Lab engine (D11 order:
/// pitch → formant → air).
///
/// The sample rate and channel count are preserved; the returned buffer
/// has the same frame count (PSOLA preserves duration exactly).
///
/// # Errors
/// Fails when the engine reports an analysis/FFT error.
pub fn render_offline(buf: &AudioBuffer, params: &EngineParams) -> Result<AudioBuffer> {
    let channels = usize::from(buf.channels());
    if channels == 0 {
        return Err(AudioError::UnsupportedConfig(
            "buffer has no channels".into(),
        ));
    }

    // Downmix to mono.
    let frames = buf.frames();
    let mut mono = Vec::with_capacity(frames);
    if channels == 1 {
        mono.extend_from_slice(buf.samples());
    } else {
        let scale = 1.0 / channels as f32;
        for f in 0..frames {
            let start = f * channels;
            let sum: f32 = buf.samples()[start..start + channels].iter().sum();
            mono.push(sum * scale);
        }
    }

    let (rendered, _track, _report) = mvl_core::pipeline::render(&mono, buf.sample_rate(), params)?;

    // Map back to the original channel layout.
    let mut out_samples = Vec::with_capacity(rendered.len() * channels);
    for s in &rendered {
        out_samples.extend(std::iter::repeat_n(*s, channels));
    }
    AudioBuffer::from_interleaved(buf.sample_rate(), buf.channels(), out_samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvl_core::synth::{self, Rng, VowelSpec};

    fn stereo_vowel(sr: u32) -> AudioBuffer {
        let track_f0 = synth::f0_track_const(170.0, sr as usize / 2);
        let mut rng = Rng::new(81);
        let mono = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
        let mut interleaved = Vec::with_capacity(mono.len() * 2);
        for s in &mono {
            interleaved.push(*s);
            interleaved.push(*s);
        }
        AudioBuffer::from_interleaved(sr, 2, interleaved).expect("stereo buffer")
    }

    #[test]
    fn neutral_render_preserves_the_buffer() {
        let sr = 48_000u32;
        let buf = stereo_vowel(sr);
        let out = render_offline(&buf, &EngineParams::default()).expect("render");
        assert_eq!(out.sample_rate(), sr);
        assert_eq!(out.channels(), 2);
        assert_eq!(out.frames(), buf.frames());
        assert!(out.samples().iter().zip(buf.samples()).all(|(a, b)| a == b));
    }

    #[test]
    fn shifted_render_moves_both_channels_together() {
        let sr = 48_000u32;
        let buf = stereo_vowel(sr);
        let mut params = EngineParams::default();
        params.set_pitch_semitones(2.0);
        let out = render_offline(&buf, &params).expect("render");
        assert_eq!(out.frames(), buf.frames(), "duration must be preserved");

        // Stereo stays phase-coherent: L == R after a mono engine.
        for f in 0..out.frames() {
            let frame = out.frame(f).expect("frame");
            assert_eq!(frame[0], frame[1], "channels diverged at frame {f}");
        }

        // The pitch actually moved (ceps comb at the target).
        let mono_out: Vec<f32> = out.samples().iter().step_by(2).copied().collect();
        let target = 170.0f64 * 2.0f64.powf(2.0 / 12.0);
        let s_target = mvl_core::measure::ceps_strength(&mono_out, sr, target);
        let s_off =
            mvl_core::measure::ceps_strength(&mono_out, sr, target * 2.0f64.powf(3.5 / 12.0));
        assert!(s_target > 2.0 * s_off, "no comb at {target:.1} Hz");
    }
}
