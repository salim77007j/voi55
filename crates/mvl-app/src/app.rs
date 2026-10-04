//! Application bootstrap and UI glue.
//!
//! [`App`] owns the `AppWindow` plus the session (loaded project, view,
//! waveform pyramid) and implements every UI callback. Both the GUI binary
//! and the headless screenshot mode construct the app through
//! [`App::new`], so the evidence pipeline exercises the exact same code
//! path a desktop user runs.

use crate::AppWindow;
use crate::session::Session;
use crate::waveform::{self, STUDIO_COLORS};
use mvl_audio::AudioBuffer;
use mvl_core::VERSION;
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, SharedString};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// Root application object. `Rc`-held (Slint is single-threaded); use
/// [`App::as_weak`] inside callbacks.
pub struct App {
    window: AppWindow,
    session: RefCell<Option<Session>>,
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
        window.set_status_core(format!("Engine {VERSION} ready · pitch ±12 st / 1 ¢ · air ±dB / 0.1 dB · formant 130–190 mm").into());

        let app = Rc::new(Self {
            window,
            session: RefCell::new(None),
        });

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

        self.window.set_has_audio(true);
        self.window
            .set_track_label(SharedString::from(format!("{name} — {sr} Hz · {} ch", ch)));
        self.window.set_status_core(SharedString::from(format!(
            "{name} · {frames} frames @ {sr} Hz · median F0 {f0:.1} Hz · voiced {:.0} %",
            voiced * 100.0
        )));
        if let Some(err) = track_error {
            self.window.set_status_core(
                format!("pitch analysis failed ({err}) — waveform shown without F0 overlay").into(),
            );
        }
        self.refresh();
    }

    /// Re-renders the waveform image from the current session state at the
    /// current viewport size.
    pub fn refresh(&self) {
        let mut borrowed = self.session.borrow_mut();
        let Some(session) = borrowed.as_mut() else {
            self.window.set_wave_image(Image::default());
            return;
        };
        let scale = f64::from(self.window.window().scale_factor());
        let w = (f64::from(self.window.get_view_width()) * scale)
            .round()
            .max(1.0) as usize;
        let h = (f64::from(self.window.get_view_height()) * scale)
            .round()
            .max(1.0) as usize;

        let cols = waveform::columns(
            &session.pyramid,
            &session.mono,
            session.track.as_ref(),
            &session.view,
            w,
        );
        let buf = waveform::draw(&cols, w, h, &STUDIO_COLORS);
        let image = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &buf, w as u32, h as u32,
        ));
        self.window.set_wave_image(image);

        let (a, b) = session.view_secs();
        self.window
            .set_zoom_label(SharedString::from(fmt_span(b - a)));
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
        session.view = crate::waveform::View::fitting(session.buffer.frames());
        drop(borrowed);
        self.refresh();
    }
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
