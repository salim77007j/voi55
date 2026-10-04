//! Application bootstrap and UI glue.
//!
//! [`App`] owns the `AppWindow`, the session (loaded project, view,
//! waveform pyramid) and the engine parameter state, and implements every
//! UI callback. Both the GUI binary and the headless screenshot mode
//! construct the app through [`App::new`], so the evidence pipeline
//! exercises the exact same code path a desktop user runs.
//!
//! Preview pipeline (No-Fake-UI): every slider writes [`EngineParams`]
//! (the same clamping contract the DSP reads), debounces 250 ms, then
//! renders on a background thread through
//! `mvl_audio::engine::render_mono_to_buffer` with the session's cached
//! pYIN track — one analysis, many renders (D11). Completion flows back
//! over an mpsc channel that a UI-thread timer polls (Slint work stays on
//! the Slint thread; the render closure only carries `Send` data). The
//! finished render lands as an A/B-comparable preview in the waveform
//! view.

use crate::AppWindow;
use crate::session::Session;
use crate::waveform::{self, STUDIO_COLORS, View, WaveformColors, WaveformPyramid};
use mvl_audio::AudioBuffer;
use mvl_core::VERSION;
use mvl_core::engine::EngineParams;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, SharedString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::rc::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One completed preview render (A/B view source). `Send` by construction
/// (plain data), so a background thread can produce it.
struct PreviewRender {
    pyramid: WaveformPyramid,
    /// Rendered mono (deep-zoom display source).
    mono: Arc<Vec<f32>>,
    render_ms: f64,
    params: EngineParams,
}

/// Message from the render thread back to the UI thread.
enum RenderDone {
    Ok(Box<PreviewRender>),
    Failed(String),
}

/// Root application object. `Rc`-held (Slint is single-threaded); use
/// [`App::as_weak`] inside callbacks.
pub struct App {
    window: AppWindow,
    session: RefCell<Option<Session>>,
    params: Cell<EngineParams>,
    render_timer: RefCell<slint::Timer>,
    rendering: Arc<AtomicBool>,
    /// Parameters captured while a render was in flight (latest wins).
    pending: Arc<Mutex<Option<EngineParams>>>,
    render_rx: RefCell<Option<Receiver<RenderDone>>>,
    preview: RefCell<Option<PreviewRender>>,
    show_preview: Cell<bool>,
    base_status: RefCell<String>,
    /// Weak self handle for timers (UI thread only).
    self_weak: RefCell<Weak<App>>,
}

impl App {
    /// Creates the window, installs the English default strings and wires
    /// all callbacks.
    ///
    /// # Errors
    /// Fails when the Slint platform cannot create the window adapter.
    pub fn new() -> Result<Rc<Self>, slint::PlatformError> {
        let window = AppWindow::new()?;

        // English defaults (i18n table with Arabic arrives in sub-item 4.5).
        window.set_t_title("Micro-Vocal Lab".into());
        window.set_t_version(format!("v{VERSION}").into());
        window.set_t_waveform_empty(
            "No audio loaded — import a WAV / MP3, record, or run with --demo synth".into(),
        );
        window.set_t_pitch("Pitch".into());
        window.set_t_air("Air & Breath".into());
        window.set_t_formant("Formant".into());
        window.set_status_core(
            format!(
                "Engine {VERSION} ready · pitch ±12 st / 1 ¢ · air ±dB / 0.1 dB · formant 130–190 mm"
            )
            .into(),
        );

        let app = Rc::new(Self {
            window,
            session: RefCell::new(None),
            params: Cell::new(EngineParams::default()),
            render_timer: RefCell::new(slint::Timer::default()),
            rendering: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(Mutex::new(None)),
            render_rx: RefCell::new(None),
            preview: RefCell::new(None),
            show_preview: Cell::new(false),
            base_status: RefCell::new(String::new()),
            self_weak: RefCell::new(Weak::new()),
        });
        *app.self_weak.borrow_mut() = Rc::downgrade(&app);

        // Waveform callbacks.
        let weak = Rc::downgrade(&app);
        app.window.on_wave_zoom(move |factor, anchor| {
            if let Some(app) = weak.upgrade() {
                app.zoom(f64::from(factor), f64::from(anchor));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_wave_pan(move |delta| {
            if let Some(app) = weak.upgrade() {
                app.pan(f64::from(delta));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_wave_fit(move || {
            if let Some(app) = weak.upgrade() {
                app.fit();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_wave_viewport_changed(move || {
            if let Some(app) = weak.upgrade() {
                app.refresh();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_toggle_preview(move || {
            if let Some(app) = weak.upgrade() {
                app.toggle_preview();
            }
        });

        // Parameter callbacks — clamp/snap in EngineParams, echo the
        // canonical value back, schedule a debounced render.
        let weak = Rc::downgrade(&app);
        app.window.on_pitch_changed(move |v| {
            if let Some(app) = weak.upgrade() {
                app.apply_pitch(f64::from(v));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_air_changed(move |v| {
            if let Some(app) = weak.upgrade() {
                app.apply_air(f64::from(v));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_formant_changed(move |v| {
            if let Some(app) = weak.upgrade() {
                app.apply_formant(f64::from(v));
            }
        });

        Ok(app)
    }

    /// Weak handle for callbacks.
    pub fn as_weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    /// The root window.
    pub fn window(&self) -> &AppWindow {
        &self.window
    }

    /// Current engine parameters (export, tests, evidence).
    pub fn params(&self) -> EngineParams {
        self.params.get()
    }

    // ── Parameter application ────────────────────────────────────────

    fn apply_pitch(&self, v: f64) {
        let mut p = self.params.get();
        p.set_pitch_semitones(v);
        self.params.set(p);
        self.sync_param_ui();
        self.schedule_render();
    }

    fn apply_air(&self, v: f64) {
        let mut p = self.params.get();
        p.set_air_db(v);
        self.params.set(p);
        self.sync_param_ui();
        self.schedule_render();
    }

    fn apply_formant(&self, v: f64) {
        let mut p = self.params.get();
        p.set_formant_mm(v);
        self.params.set(p);
        self.sync_param_ui();
        self.schedule_render();
    }

    /// Sets the three engine parameters, echoes the UI and renders
    /// synchronously. Evidence/tests path (`--pitch/--air/--formant`).
    pub fn set_params_and_render(&self, pitch: f64, air: f64, formant: f64) {
        let mut p = EngineParams::default();
        p.set_pitch_semitones(pitch);
        p.set_air_db(air);
        p.set_formant_mm(formant);
        self.params.set(p);
        self.sync_param_ui();
        if self.session.borrow().is_some() {
            self.render_now();
        }
    }

    /// Loads a project buffer (analyzing the pYIN track) and shows it.
    pub fn load_audio(&self, buffer: AudioBuffer, name: &str) {
        let session = Session::load(buffer, name.to_string(), true);
        let sr = session.buffer.sample_rate();
        let ch = session.buffer.channels();
        let frames = session.buffer.frames();
        let (f0, voiced) = match &session.track {
            Some(t) => (t.median_f0(), t.voiced_ratio()),
            None => (0.0, 0.0),
        };
        let track_error = session.track_error.clone();
        self.session.replace(Some(session));

        // New project: invalidate the previous render, keep parameters.
        *self.preview.borrow_mut() = None;
        self.show_preview.set(false);
        self.window.set_preview_mode(false);
        self.window.set_can_preview(false);

        self.window.set_has_audio(true);
        self.window.set_has_project(true);
        self.window
            .set_track_label(SharedString::from(format!("{name} — {sr} Hz · {ch} ch")));
        *self.base_status.borrow_mut() = format!(
            "{name} · {frames} frames @ {sr} Hz · median F0 {f0:.1} Hz · voiced {:.0} %",
            voiced * 100.0
        );
        if let Some(err) = track_error {
            *self.base_status.borrow_mut() =
                format!("pitch analysis failed ({err}) — waveform shown without F0 overlay");
        }
        self.sync_param_ui();
        self.compose_status(None);
        self.refresh();
    }

    /// Renders synchronously on the calling thread and installs the
    /// preview. Used by the screenshot/evidence path (no event loop) and
    /// by tests; the GUI uses the debounced background thread instead.
    pub fn render_now(&self) {
        let outcome = {
            let session_borrow = self.session.borrow();
            let Some(session) = session_borrow.as_ref() else {
                return;
            };
            let t0 = Instant::now();
            let rendered = mvl_audio::engine::render_mono_to_buffer(
                &session.mono,
                session.buffer.sample_rate(),
                session.buffer.channels(),
                &self.params.get(),
                session.track.as_deref(),
            );
            let render_ms = t0.elapsed().as_secs_f64() * 1000.0;
            rendered.ok().map(|buffer| {
                (
                    Arc::new(mvl_audio::engine::downmix_mono(&buffer)),
                    buffer.sample_rate(),
                    render_ms,
                )
            })
        };
        match outcome {
            Some((mono, sample_rate, render_ms)) => {
                let pyramid = WaveformPyramid::build(&mono, sample_rate);
                *self.preview.borrow_mut() = Some(PreviewRender {
                    pyramid,
                    mono,
                    render_ms,
                    params: self.params.get(),
                });
                self.window.set_can_preview(true);
                self.compose_status(Some(&format!(
                    "preview rendered in {render_ms:.0} ms ({})",
                    stages_label(&self.params.get())
                )));
            }
            None => self.compose_status(Some("render failed")),
        }
        self.refresh();
    }

    /// Debounced render scheduling (GUI path).
    fn schedule_render(&self) {
        if self.session.borrow().is_none() {
            return;
        }
        self.compose_status(Some("rendering…"));
        let weak = self.self_weak.borrow().clone();
        self.render_timer.borrow_mut().start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(250),
            move || {
                if let Some(app) = weak.upgrade() {
                    app.spawn_render();
                }
            },
        );
    }

    /// Starts a background render with the current parameters (or queues
    /// them if one is already running).
    fn spawn_render(&self) {
        if self.rendering.swap(true, Ordering::SeqCst) {
            *self.pending.lock().expect("pending lock") = Some(self.params.get());
            return;
        }
        let session_borrow = self.session.borrow();
        let Some(session) = session_borrow.as_ref() else {
            self.rendering.store(false, Ordering::SeqCst);
            return;
        };
        let mono = Arc::clone(&session.mono);
        let track = session.track.clone();
        let sample_rate = session.buffer.sample_rate();
        let channels = session.buffer.channels();
        let params = self.params.get();
        let rendering = Arc::clone(&self.rendering);

        let (tx, rx) = std::sync::mpsc::channel();
        *self.render_rx.borrow_mut() = Some(rx);
        self.start_render_poller();

        std::thread::spawn(move || {
            let t0 = Instant::now();
            let rendered = mvl_audio::engine::render_mono_to_buffer(
                &mono,
                sample_rate,
                channels,
                &params,
                track.as_deref(),
            );
            let render_ms = t0.elapsed().as_secs_f64() * 1000.0;
            rendering.store(false, Ordering::SeqCst);
            let _ = tx.send(match rendered {
                Ok(buffer) => {
                    let m = Arc::new(mvl_audio::engine::downmix_mono(&buffer));
                    RenderDone::Ok(Box::new(PreviewRender {
                        pyramid: WaveformPyramid::build(&m, buffer.sample_rate()),
                        mono: m,
                        render_ms,
                        params,
                    }))
                }
                Err(e) => RenderDone::Failed(e.to_string()),
            });
        });
    }

    /// Polls the render-completion channel on the UI thread (a repeating
    /// timer that stops itself once the render landed).
    fn start_render_poller(&self) {
        let weak = self.self_weak.borrow().clone();
        self.render_timer.borrow_mut().start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(40),
            move || {
                let Some(app) = weak.upgrade() else { return };
                let msg = {
                    let rx = app.render_rx.borrow();
                    match rx.as_ref() {
                        Some(rx) => match rx.try_recv() {
                            Ok(msg) => Some(msg),
                            Err(TryRecvError::Empty) => None,
                            Err(TryRecvError::Disconnected) => {
                                Some(RenderDone::Failed("render thread died".into()))
                            }
                        },
                        None => None,
                    }
                };
                let Some(msg) = msg else { return };
                *app.render_rx.borrow_mut() = None;
                app.render_timer.borrow_mut().stop();
                match msg {
                    RenderDone::Ok(pr) => {
                        let ms = pr.render_ms;
                        let label = stages_label(&pr.params);
                        *app.preview.borrow_mut() = Some(*pr);
                        app.window.set_can_preview(true);
                        app.compose_status(Some(&format!(
                            "preview rendered in {ms:.0} ms ({label})"
                        )));
                        // A queued parameter change re-renders immediately.
                        if let Some(next) = app.pending.lock().expect("pending lock").take() {
                            app.params.set(next);
                            app.sync_param_ui();
                            app.spawn_render();
                        } else if app.show_preview.get() {
                            app.refresh();
                        }
                    }
                    RenderDone::Failed(e) => {
                        app.compose_status(Some(&format!("render failed: {e}")));
                    }
                }
            },
        );
    }

    /// A/B view switch (waveform chip and `--preview`).
    pub fn toggle_preview(&self) {
        self.show_preview.set(!self.show_preview.get());
        self.window.set_preview_mode(self.show_preview.get());
        self.refresh();
    }

    /// Sets the playhead position in seconds (negative hides it).
    pub fn set_playhead(&self, secs: f64) {
        let frac = self.session.borrow().as_ref().and_then(|s| {
            let sr = f64::from(s.buffer.sample_rate());
            let pos = secs * sr - s.view.start_frame;
            (s.view.span_frames > 0.0).then(|| (pos / s.view.span_frames) as f32)
        });
        self.window.set_playhead_frac(frac.unwrap_or(-1.0));
    }

    /// Sets the visible window to `secs` centered in the track (evidence
    /// zoom state).
    pub fn set_view_window_secs(&self, secs: f64) {
        let mut borrowed = self.session.borrow_mut();
        let Some(session) = borrowed.as_mut() else {
            return;
        };
        let total = session.duration_secs();
        let start = ((total - secs) / 2.0).max(0.0);
        session.set_view_secs(start, start + secs);
        drop(borrowed);
        self.refresh();
    }

    /// Pushes current parameter values + readouts into the window.
    fn sync_param_ui(&self) {
        let p = self.params.get();
        self.window.set_pitch_value(p.pitch_semitones() as f32);
        self.window.set_air_value(p.air_db() as f32);
        self.window.set_formant_value(p.formant_mm() as f32);
        self.window
            .set_pitch_readout(fmt_pitch(p.pitch_semitones()).into());
        self.window.set_air_readout(fmt_air(p.air_db()).into());
        self.window
            .set_formant_readout(fmt_formant(p.formant_mm()).into());
    }

    fn compose_status(&self, extra: Option<&str>) {
        let mut s = self.base_status.borrow().clone();
        if let Some(extra) = extra {
            if !s.is_empty() {
                s.push_str(" · ");
            }
            s.push_str(extra);
        }
        self.window.set_status_core(s.into());
    }

    /// Re-renders the waveform image from the current state (input or
    /// preview) at the current viewport size.
    pub fn refresh(&self) {
        let show_preview = self.show_preview.get();
        let preview = self.preview.borrow();
        let mut session = self.session.borrow_mut();

        let Some(s) = session.as_mut() else {
            self.window.set_wave_image(Image::default());
            return;
        };

        // A/B: the preview shows the rendered pyramid without the input
        // analysis overlay (tint/trace describe the *input*, not the
        // render).
        let (pyramid, mono, track, colors, label) = if show_preview {
            match preview.as_ref() {
                Some(pr) => (
                    &pr.pyramid,
                    &pr.mono,
                    None,
                    PREVIEW_COLORS,
                    format!("{} — preview (rendered)", s.name),
                ),
                None => (
                    &s.pyramid,
                    &s.mono,
                    s.track.as_deref(),
                    STUDIO_COLORS,
                    format!("{} — original", s.name),
                ),
            }
        } else {
            (
                &s.pyramid,
                &s.mono,
                s.track.as_deref(),
                STUDIO_COLORS,
                format!("{} — original", s.name),
            )
        };

        let scale = f64::from(self.window.window().scale_factor());
        let w = (f64::from(self.window.get_view_width()) * scale)
            .round()
            .max(1.0) as usize;
        let h = (f64::from(self.window.get_view_height()) * scale)
            .round()
            .max(1.0) as usize;

        let cols = waveform::columns(pyramid, mono, track, &s.view, w);
        let buf = waveform::draw(&cols, w, h, &colors);
        let image = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &buf, w as u32, h as u32,
        ));
        self.window.set_wave_image(image);
        self.window.set_track_label(label.into());

        let (a, b) = s.view_secs();
        self.window
            .set_zoom_label(SharedString::from(fmt_span(b - a)));
    }

    fn zoom(&self, factor: f64, anchor: f64) {
        let mut borrowed = self.session.borrow_mut();
        let Some(session) = borrowed.as_mut() else {
            return;
        };
        session
            .view
            .zoom_at(anchor, factor, session.buffer.frames());
        drop(borrowed);
        self.refresh();
    }

    fn pan(&self, delta: f64) {
        let mut borrowed = self.session.borrow_mut();
        let Some(session) = borrowed.as_mut() else {
            return;
        };
        session.view.pan(delta, session.buffer.frames());
        drop(borrowed);
        self.refresh();
    }

    fn fit(&self) {
        let mut borrowed = self.session.borrow_mut();
        let Some(session) = borrowed.as_mut() else {
            return;
        };
        session.view = View::fitting(session.buffer.frames());
        drop(borrowed);
        self.refresh();
    }
}

/// Preview-mode waveform colors: the A/B view drops the voiced tint and
/// F0 trace (they describe the *input* analysis, not the render) and uses
/// the formant accent to signal "this is the processed side".
const PREVIEW_COLORS: WaveformColors = WaveformColors {
    peak: [0xB7, 0x8C, 0xFF, 0x64],
    core: [0xE7, 0xEB, 0xF0, 0xD9],
    voiced: [0x00, 0x00, 0x00, 0x00],
    trace: [0x00, 0x00, 0x00, 0x00],
    center: [0x2C, 0x34, 0x40, 0xFF],
};

fn stages_of(p: &EngineParams) -> [bool; 3] {
    [
        p.pitch_semitones().abs() >= 1e-9,
        (p.formant_mm() - mvl_core::engine::REFERENCE_VTL_MM).abs() >= 1e-9,
        p.air_db().abs() >= 1e-9,
    ]
}

fn stages_label(p: &EngineParams) -> String {
    let s = stages_of(p);
    let names = ["pitch", "formant", "air"];
    let applied: Vec<&str> = names
        .iter()
        .zip(s.iter())
        .filter(|(_, on)| **on)
        .map(|(n, _)| *n)
        .collect();
    if applied.is_empty() {
        "neutral".into()
    } else {
        applied.join(" → ")
    }
}

fn fmt_pitch(st: f64) -> String {
    format!("{st:+.2} st")
}

fn fmt_air(db: f64) -> String {
    format!("{db:+.1} dB")
}

fn fmt_formant(mm: f64) -> String {
    format!("{mm:.0} mm")
}

/// Formats a view span for the zoom readout.
fn fmt_span(secs: f64) -> String {
    if secs >= 1.0 {
        format!("view {secs:.2} s")
    } else if secs >= 0.001 {
        format!("view {:.1} ms", secs * 1000.0)
    } else {
        format!("view {:.0} µs", secs * 1_000_000.0)
    }
}
