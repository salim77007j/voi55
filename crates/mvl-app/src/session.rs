//! Session state — the loaded project, its view, and everything the UI
//! glue needs between the audio layer and the Slint window.
//!
//! One session at a time (a single vocal take). The pYIN track is computed
//! once on load and shared by the display, the sliders' re-renders (4.3)
//! and the export (4.4) — one analysis, many renders.

use crate::waveform::{View, WaveformPyramid};
use mvl_audio::AudioBuffer;

/// A loaded project.
pub struct Session {
    pub name: String,
    pub buffer: AudioBuffer,
    /// Mono display/analysis domain (engine input).
    pub mono: Vec<f32>,
    pub pyramid: WaveformPyramid,
    /// Shared pYIN track (`None` when analysis was skipped or failed —
    /// the failure is surfaced, not hidden).
    pub track: Option<mvl_core::pyin::PyinResult>,
    pub track_error: Option<String>,
    pub view: View,
}

impl Session {
    /// Loads a project, building the pyramid and (unless `analyze` is
    /// false) the pYIN track. Analysis errors are recorded, never fatal —
    /// the waveform still displays.
    pub fn load(buffer: AudioBuffer, name: impl Into<String>, analyze: bool) -> Self {
        let mono = mvl_audio::engine::downmix_mono(&buffer);
        let pyramid = WaveformPyramid::build(&mono, buffer.sample_rate());
        let (track, track_error) = if analyze {
            match mvl_core::pyin::pyin(&mono, buffer.sample_rate()) {
                Ok(t) => (Some(t), None),
                Err(e) => (None, Some(e.to_string())),
            }
        } else {
            (None, None)
        };
        Self {
            name: name.into(),
            view: View::fitting(buffer.frames()),
            pyramid,
            buffer,
            mono,
            track,
            track_error,
        }
    }

    /// Duration in seconds.
    pub fn duration_secs(&self) -> f64 {
        self.buffer.duration_secs()
    }

    /// The view window in seconds (start, end).
    pub fn view_secs(&self) -> (f64, f64) {
        let sr = f64::from(self.buffer.sample_rate());
        (
            self.view.start_frame / sr,
            (self.view.start_frame + self.view.span_frames) / sr,
        )
    }

    /// Sets the view window from seconds (clamped to the file).
    pub fn set_view_secs(&mut self, start: f64, end: f64) {
        let sr = f64::from(self.buffer.sample_rate());
        let total = self.buffer.frames() as f64;
        let s = (start.min(end) * sr).clamp(0.0, total);
        let e = (end.max(start) * sr).clamp(0.0, total);
        self.view.start_frame = s;
        self.view.span_frames = (e - s).max(View::MIN_SPAN_FRAMES.min(total));
    }
}

/// Synthesizes the built-in demo vocal (vibrato vowel, 2 s at 48 kHz) for
/// screenshots and self-checks when no file is available.
///
/// # Errors
/// Propagates buffer-construction failures (never expected for fixed
/// parameters).
pub fn demo_vocal() -> Result<AudioBuffer, mvl_audio::AudioError> {
    let sr = 48_000u32;
    let n = sr as usize * 2;
    // ±45 cents @ 5 Hz: realistic vibrato that stays inside the pYIN
    // confidence envelope disclosed in Phase 3 (deep fast vibrato beyond
    // ±3 % F0 smear leaks into the unvoiced path).
    let f0_track = mvl_core::synth::vibrato_f0_track(160.0, 45.0, 5.0, n, sr);
    let mut rng = mvl_core::synth::Rng::new(42);
    let vowel = mvl_core::synth::vowel(
        &f0_track,
        sr,
        &mvl_core::synth::VowelSpec::default(),
        &mut rng,
    );
    AudioBuffer::from_interleaved(sr, 1, vowel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_builds_pyramid_and_track() {
        let buf = demo_vocal().expect("demo");
        let s = Session::load(buf, "demo", true);
        assert_eq!(s.pyramid.frames(), 96_000);
        assert!(s.track.is_some(), "vowel must analyze");
        assert_eq!(s.duration_secs(), 2.0);
        let (a, b) = s.view_secs();
        assert!((a - 0.0).abs() < 1e-9 && (b - 2.0).abs() < 1e-9);
    }

    #[test]
    fn view_secs_round_trip_and_clamp() {
        let buf = demo_vocal().expect("demo");
        let mut s = Session::load(buf, "demo", false);
        s.set_view_secs(0.5, 1.0);
        let (a, b) = s.view_secs();
        assert!((a - 0.5).abs() < 1e-6 && (b - 1.0).abs() < 1e-6);
        // Reversed / out-of-range inputs clamp sanely.
        s.set_view_secs(5.0, -1.0);
        let (a, b) = s.view_secs();
        assert!((a - 0.0).abs() < 1e-6 && (b - 2.0).abs() < 1e-6);
        // Deep zoom below MIN_SPAN holds at the floor.
        s.set_view_secs(1.0, 1.0001);
        let (a, b) = s.view_secs();
        let span = (b - a) * 48_000.0;
        assert!(span >= View::MIN_SPAN_FRAMES - 1.0, "span {span}");
    }
}
