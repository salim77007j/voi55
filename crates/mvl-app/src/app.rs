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
use crate::DeviceItem;
use crate::dialogs;
use crate::i18n::{self, Lang};
use crate::session::Session;
use crate::waveform::{self, STUDIO_COLORS, View, WaveformColors, WaveformPyramid};
use mvl_audio::AudioBuffer;
use mvl_core::VERSION;
use mvl_core::engine::EngineParams;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
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
    /// The full rendered buffer, stored **mono**: the render is derived
    /// from the mono downmix anyway, the Player maps 1→N at output
    /// (map_channels), and mono halves the A/B memory — a 3-min 48 kHz
    /// preview costs 34.5 MB instead of the old stereo buffer + display
    /// downmix pair at 103.5 MB (Phase 5 RAM budget). Playback and the
    /// waveform/zoom view are unaffected.
    buffer: AudioBuffer,
    pyramid: WaveformPyramid,
    render_ms: f64,
    params: EngineParams,
}

/// Message from the render thread back to the UI thread.
enum RenderDone {
    Ok(Box<PreviewRender>),
    Failed(String),
}

/// Extracts a human-readable message from a panic payload (any type).
fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Root application object. `Rc`-held (Slint is single-threaded); use
/// [`App::as_weak`] inside callbacks.
pub struct App {
    window: AppWindow,
    session: RefCell<Option<Session>>,
    params: Cell<EngineParams>,
    render_timer: RefCell<slint::Timer>,
    poll_timer: RefCell<slint::Timer>,
    rendering: Arc<AtomicBool>,
    /// Parameters captured while a render was in flight (latest wins).
    pending: Arc<Mutex<Option<EngineParams>>>,
    render_rx: RefCell<Option<Receiver<RenderDone>>>,
    preview: RefCell<Option<PreviewRender>>,
    show_preview: Cell<bool>,
    base_status: RefCell<String>,
    /// Weak self handle for timers (UI thread only).
    self_weak: RefCell<Weak<App>>,
    /// Output transport (connected lazily — a machine may have no device).
    player: RefCell<Option<mvl_audio::Player>>,
    /// Running capture, if armed.
    recorder: RefCell<Option<mvl_audio::Recorder>>,
    record_started: RefCell<Option<Instant>>,
    /// Export panel state: 0=f32 WAV, 1=24-bit, 2=16-bit, 3=MP3.
    export_format: Cell<u8>,
    export_bitrate: Cell<u32>,
    /// True when parameters changed since the last render.
    dirty: Cell<bool>,
    /// Live streaming preview (Phase 6): started on the first play when a
    /// device is present and the session rate matches the device rate;
    /// slider changes restart it at the playhead. `None` on deviceless,
    /// rate-mismatched or neutral-parameter paths — the debounced offline
    /// render stays the honest fallback.
    live: RefCell<Option<mvl_audio::PreviewStream>>,
    /// UI language (4.5): drives strings, embedded font and layout
    /// direction; defaults to English (D1).
    lang: Cell<Lang>,
    /// User-picked input device (Phase 7.1). `None` = platform default
    /// (with the resolve chain as the safety net).
    selected_input: RefCell<Option<String>>,
    /// User-picked output device (Phase 7.1).
    selected_output: RefCell<Option<String>>,
}

impl App {
    /// Creates the window, installs the default (English) strings and
    /// wires all callbacks.
    ///
    /// # Errors
    /// Fails when the Slint platform cannot create the window adapter.
    pub fn new() -> Result<Rc<Self>, slint::PlatformError> {
        let window = AppWindow::new()?;

        let app = Rc::new(Self {
            window,
            session: RefCell::new(None),
            params: Cell::new(EngineParams::default()),
            render_timer: RefCell::new(slint::Timer::default()),
            poll_timer: RefCell::new(slint::Timer::default()),
            rendering: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(Mutex::new(None)),
            render_rx: RefCell::new(None),
            preview: RefCell::new(None),
            show_preview: Cell::new(false),
            base_status: RefCell::new(String::new()),
            self_weak: RefCell::new(Weak::new()),
            player: RefCell::new(None),
            recorder: RefCell::new(None),
            record_started: RefCell::new(None),
            export_format: Cell::new(0),
            export_bitrate: Cell::new(mvl_audio::DEFAULT_MP3_BITRATE),
            dirty: Cell::new(false),
            live: RefCell::new(None),
            lang: Cell::new(Lang::En),
            selected_input: RefCell::new(None),
            selected_output: RefCell::new(None),
        });
        *app.self_weak.borrow_mut() = Rc::downgrade(&app);

        // Install the English table (D1 default): strings, font, direction.
        app.apply_language(Lang::En);

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

        // Import / export / transport callbacks (4.4, No-Fake-UI).
        let weak = Rc::downgrade(&app);
        app.window.on_import_clicked(move || {
            if let Some(app) = weak.upgrade() {
                app.import_clicked();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_export_clicked(move || {
            if let Some(app) = weak.upgrade() {
                let open = !app.window.get_export_open();
                app.window.set_export_open(open);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_export_format_changed(move |i| {
            if let Some(app) = weak.upgrade() {
                app.export_format.set(i.clamp(0, 3) as u8);
                app.window.set_export_format(i.clamp(0, 3));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_export_bitrate_changed(move |b| {
            if let Some(app) = weak.upgrade() {
                app.export_bitrate.set(b as u32);
                app.window.set_export_bitrate(b);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_export_confirm(move || {
            if let Some(app) = weak.upgrade() {
                app.export_confirm();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_play_pause(move || {
            if let Some(app) = weak.upgrade() {
                app.play_pause();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_transport_stop(move || {
            if let Some(app) = weak.upgrade() {
                app.live_rewind();
                if let Some(p) = app.player.borrow_mut().as_ref() {
                    let _ = p.stop();
                }
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_transport_rewind(move || {
            if let Some(app) = weak.upgrade() {
                app.live_rewind();
                if let Some(p) = app.player.borrow_mut().as_ref() {
                    let _ = p.stop();
                }
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_record_toggle(move || {
            if let Some(app) = weak.upgrade() {
                app.record_toggle();
            }
        });

        // Language chip (4.5): toggles EN LTR ↔ AR RTL at runtime.
        let weak = Rc::downgrade(&app);
        app.window.on_toggle_language(move || {
            if let Some(app) = weak.upgrade() {
                let next = match app.lang.get() {
                    Lang::En => Lang::Ar,
                    Lang::Ar => Lang::En,
                };
                app.set_language(next);
            }
        });

        // Devices dialog (Phase 7.1): open/refresh enumerate through the
        // same mvl-audio paths the engine uses; picking re-targets the
        // next capture/playback connection; Test output plays a real tone
        // through the (re)connected player.
        let weak = Rc::downgrade(&app);
        app.window.on_devices_toggle(move || {
            if let Some(app) = weak.upgrade() {
                let open = !app.window.get_devices_open();
                app.window.set_devices_open(open);
                if open {
                    app.refresh_devices();
                }
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_devices_refresh(move || {
            if let Some(app) = weak.upgrade() {
                app.refresh_devices();
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_devices_close(move || {
            if let Some(app) = weak.upgrade() {
                app.window.set_devices_open(false);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_device_picked(move |name, kind| {
            if let Some(app) = weak.upgrade() {
                app.device_picked(name.as_ref(), kind);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_test_output(move || {
            if let Some(app) = weak.upgrade() {
                app.test_output();
            }
        });

        // UI poller: transport state, playhead and time readout.
        let weak = app.self_weak.borrow().clone();
        app.poll_timer.borrow_mut().start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(40),
            move || {
                let Some(app) = weak.upgrade() else { return };
                app.poll_transport();
            },
        );

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

    /// Project frame count (tests/evidence).
    pub fn frames(&self) -> usize {
        self.session
            .borrow()
            .as_ref()
            .map_or(0, |s| s.buffer.frames())
    }

    /// Current status line (tests/evidence).
    pub fn status_line(&self) -> String {
        self.window.get_status_core().to_string()
    }

    // ── i18n (4.5) ──────────────────────────────────────────────

    /// The active UI language (tests/evidence).
    pub fn lang(&self) -> Lang {
        self.lang.get()
    }

    /// Switches the UI language at runtime: every user-visible string,
    /// the embedded font family and the layout direction follow the
    /// table. Safe with a project loaded — composed lines rebuild from
    /// the session; transient status extras are dropped (they reappear
    /// with the next action, already in the new language).
    pub fn set_language(&self, lang: Lang) {
        if self.lang.get() == lang {
            return;
        }
        self.lang.set(lang);
        self.apply_language(lang);
    }

    /// Installs every string/font/direction property for `lang` and
    /// recomposes derived state. `toggle-language` and [`App::set_language`]
    /// both land here.
    fn apply_language(&self, lang: Lang) {
        let t = i18n::table(lang);
        self.window.set_ui_font(t.ui_font.into());
        self.window.set_rtl(t.rtl);
        // Brand name stays Latin in both languages (D12).
        self.window.set_t_title("Micro-Vocal Lab".into());
        self.window.set_t_version(format!("v{VERSION}").into());
        self.window.set_t_waveform_empty(t.waveform_empty.into());
        self.window.set_t_pitch(t.pitch.into());
        self.window.set_t_air(t.air.into());
        self.window.set_t_formant(t.formant.into());
        self.window.set_t_record(t.record.into());
        self.window.set_t_export_title(t.export_title.into());
        self.window.set_t_export_go(t.export_go.into());
        self.window.set_t_bitrate(t.bitrate.into());
        self.window.set_t_language(t.language_label.into());
        self.window.set_t_devices(t.devices_title.into());
        self.window.set_t_devices_in(t.devices_in.into());
        self.window.set_t_devices_out(t.devices_out.into());
        self.window.set_t_devices_test(t.devices_test.into());
        self.window.set_t_devices_refresh(t.devices_refresh.into());
        self.window.set_t_devices_none(t.devices_none.into());
        self.window.set_t_devices_close(t.devices_close.into());
        self.recompose_base_status();
        self.compose_status(None);
        self.refresh();
    }

    /// The string table for the active language.
    fn t(&self) -> i18n::StrTable {
        i18n::table(self.lang.get())
    }

    // ── Devices dialog (Phase 7.1) ────────────────────────────

    /// Re-enumerates inputs and outputs into the dialog lists, marking
    /// the device the engine will actually use (explicit pick, else the
    /// resolved default) as active. Honest: what you see is what the
    /// next connection targets.
    /// Re-enumerates and repopulates the device lists (also the headless
    /// evidence path — see main.rs --devices).
    pub fn refresh_devices(&self) {
        let active_input = self.effective_input_name();
        let active_output = self.effective_output_name();

        let inputs: Vec<DeviceItem> = mvl_audio::list_input_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|d| {
                let active = active_input.as_deref() == Some(d.name.as_str());
                DeviceItem {
                    name: d.name.into(),
                    active,
                }
            })
            .collect();
        let outputs: Vec<DeviceItem> = mvl_audio::list_output_devices()
            .unwrap_or_default()
            .into_iter()
            .map(|d| {
                let active = active_output.as_deref() == Some(d.name.as_str());
                DeviceItem {
                    name: d.name.into(),
                    active,
                }
            })
            .collect();

        self.window
            .set_input_items(ModelRc::new(VecModel::from(inputs)));
        self.window
            .set_output_items(ModelRc::new(VecModel::from(outputs)));
    }

    /// The device name the next capture will use (explicit pick, else the
    /// resolved fallback-chain default; `None` when nothing resolves).
    fn effective_input_name(&self) -> Option<String> {
        if let Some(name) = self.selected_input.borrow().as_ref() {
            return Some(name.clone());
        }
        mvl_audio::resolve_input_device(None)
            .ok()
            .map(|(d, _)| d.to_string())
    }

    /// The device name the next playback connection will use.
    fn effective_output_name(&self) -> Option<String> {
        if let Some(name) = self.selected_output.borrow().as_ref() {
            return Some(name.clone());
        }
        mvl_audio::resolve_output_device(None)
            .ok()
            .map(|(d, _)| d.to_string())
    }

    /// Applies a device pick: stores it, retargets the next connection,
    /// and (for outputs) re-homes the live player immediately so the
    /// change is audible without restarting the app.
    fn device_picked(&self, name: &str, kind: i32) {
        let fallback = self.t().device_fallback;
        match kind {
            0 => {
                *self.selected_input.borrow_mut() = Some(name.to_string());
                // A running capture keeps its old device (honest: noted in
                // the status line); the next arm uses the new one.
                let fell_back = mvl_audio::resolve_input_device(Some(name))
                    .map(|(_, fell)| fell)
                    .unwrap_or(true);
                self.status(
                    self.t().status_input_selected,
                    &[
                        ("name", self.num(name.to_string())),
                        (
                            "fallback",
                            if fell_back {
                                fallback.to_string()
                            } else {
                                String::new()
                            },
                        ),
                    ],
                );
            }
            _ => {
                *self.selected_output.borrow_mut() = Some(name.to_string());
                // Re-home the player now (drop + reconnect on the new
                // device; transport resets — disclosed by the status).
                *self.player.borrow_mut() = None;
                let fell_back = mvl_audio::resolve_output_device(Some(name))
                    .map(|(_, fell)| fell)
                    .unwrap_or(true);
                self.status(
                    self.t().status_output_selected,
                    &[
                        ("name", self.num(name.to_string())),
                        (
                            "fallback",
                            if fell_back {
                                fallback.to_string()
                            } else {
                                String::new()
                            },
                        ),
                    ],
                );
                self.refresh_devices();
            }
        }
    }

    /// Test output: connects (or re-homes) the player on the selected
    /// device and plays a real 440 Hz / 0.3 s tone. No fake meters — the
    /// user either hears it or gets the error in the status line.
    fn test_output(&self) {
        let name = self.selected_output.borrow().clone();
        let connect = mvl_audio::Player::connect_named(name.as_deref());
        match connect {
            Ok(player) => {
                let (rate, _ch) = player.output_format();
                let tone = AudioBuffer::sine(0.3, rate, 1, 440.0)
                    .unwrap_or_else(|_| AudioBuffer::new(rate, 1).expect("static config"));
                *self.player.borrow_mut() = Some(player);
                let active = self.effective_output_name().unwrap_or_default();
                if let Some(p) = self.player.borrow_mut().as_mut() {
                    let result = p.play(std::sync::Arc::new(tone));
                    match result {
                        Ok(()) => {
                            self.window.set_test_playing(true);
                            self.status(self.t().status_test_played, &[("name", self.num(active))]);
                            // The flag clears in poll_transport when the
                            // transport leaves Playing (state follows the
                            // real player, never a timer).
                        }
                        Err(e) => self.status(
                            self.t().status_playback_failed,
                            &[("error", self.num(e.to_string()))],
                        ),
                    }
                }
            }
            Err(e) => self.status(
                self.t().status_no_playback,
                &[("error", self.num(e.to_string()))],
            ),
        }
    }

    /// Wraps a technical/numeric fragment in LTR marks when the UI runs
    /// RTL so bidi never reorders it inside Arabic text (no-op in English).
    fn num(&self, s: String) -> String {
        match self.lang.get() {
            Lang::En => s,
            Lang::Ar => i18n::isolate_ltr(&s),
        }
    }

    /// Formats a status template and shows it as the transient part of
    /// the status line.
    fn status(&self, template: &str, values: &[(&str, String)]) {
        self.compose_status(Some(&i18n::tpl(template, values)));
    }

    /// The "loaded project" base status line in the active language.
    fn loaded_line(&self) -> String {
        let t = self.t();
        let borrowed = self.session.borrow();
        let Some(s) = borrowed.as_ref() else {
            return String::new();
        };
        if let Some(err) = &s.track_error {
            return i18n::tpl(t.status_track_failed, &[("error", self.num(err.clone()))]);
        }
        let (f0, voiced) = match &s.track {
            Some(track) => (track.median_f0(), track.voiced_ratio()),
            None => (0.0, 0.0),
        };
        i18n::tpl(
            t.status_loaded,
            &[
                ("name", s.name.clone()),
                ("frames", self.num(s.buffer.frames().to_string())),
                ("sr", self.num(s.buffer.sample_rate().to_string())),
                ("f0", self.num(format!("{f0:.1}"))),
                ("voiced", self.num(format!("{:.0}", voiced * 100.0))),
            ],
        )
    }

    /// Rebuilds the base status line from the current state + language
    /// (project loaded → loaded/track-failed line, else the ready line).
    fn recompose_base_status(&self) {
        let base = match self.session.borrow().as_ref() {
            Some(_) => self.loaded_line(),
            None => i18n::tpl(self.t().status_ready, &[("version", format!("v{VERSION}"))]),
        };
        *self.base_status.borrow_mut() = base;
    }

    // ── Parameter application ────────────────────────────────────────

    fn apply_pitch(&self, v: f64) {
        self.dirty.set(true);
        let mut p = self.params.get();
        p.set_pitch_semitones(v);
        self.params.set(p);
        self.sync_param_ui();
        self.schedule_render();
        self.live_update();
    }

    fn apply_air(&self, v: f64) {
        self.dirty.set(true);
        let mut p = self.params.get();
        p.set_air_db(v);
        self.params.set(p);
        self.sync_param_ui();
        self.schedule_render();
        self.live_update();
    }

    fn apply_formant(&self, v: f64) {
        self.dirty.set(true);
        let mut p = self.params.get();
        p.set_formant_mm(v);
        self.params.set(p);
        self.sync_param_ui();
        self.schedule_render();
        self.live_update();
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
        // The old live stream renders the *old* session — stop it first.
        self.live_stop();
        let session = Session::load(buffer, name.to_string(), true);
        self.session.replace(Some(session));

        // New project: invalidate the previous render, keep parameters.
        *self.preview.borrow_mut() = None;
        self.show_preview.set(false);
        self.window.set_preview_mode(false);
        self.window.set_can_preview(false);

        self.window.set_has_audio(true);
        self.window.set_has_project(true);
        self.recompose_base_status();
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
                1, // preview is stored mono (see PreviewRender)
                &self.params.get(),
                session.track.as_deref(),
            );
            let render_ms = t0.elapsed().as_secs_f64() * 1000.0;
            rendered.ok().map(|buffer| (buffer, render_ms))
        };
        match outcome {
            Some((buffer, render_ms)) => {
                let sample_rate = buffer.sample_rate();
                let pyramid = WaveformPyramid::build(buffer.samples(), sample_rate);
                *self.preview.borrow_mut() = Some(PreviewRender {
                    buffer,
                    pyramid,
                    render_ms,
                    params: self.params.get(),
                });
                self.window.set_can_preview(true);
                let stages = self.stages_label(&self.params.get());
                self.status(
                    self.t().status_rendered,
                    &[
                        ("ms", self.num(format!("{render_ms:.0}"))),
                        ("stages", stages),
                    ],
                );
            }
            None => self.status(
                self.t().status_render_failed,
                &[("error", "unknown".into())],
            ),
        }
        self.dirty.set(false);
        self.refresh();
    }

    /// Debounced render scheduling (GUI path).
    fn schedule_render(&self) {
        if self.session.borrow().is_none() {
            return;
        }
        self.compose_status(Some(self.t().status_rendering));
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
        // The old preview describes the *previous* parameters; drop it
        // before the render starts so the two never coexist in memory
        // (Phase 5 RAM: two previews would double the A/B cost on long
        // sessions). The A/B view falls back to the original until the
        // new render lands — the status bar already shows the render.
        if self.preview.borrow_mut().take().is_some() {
            self.show_preview.set(false);
            self.window.set_preview_mode(false);
            self.window.set_can_preview(false);
            self.refresh();
        }
        let session_borrow = self.session.borrow();
        let Some(session) = session_borrow.as_ref() else {
            self.rendering.store(false, Ordering::SeqCst);
            return;
        };
        let mono = Arc::clone(&session.mono);
        let track = session.track.clone();
        let sample_rate = session.buffer.sample_rate();
        // Preview stores mono (see PreviewRender): the Player maps 1→N at
        // output, and export still renders the original channel layout.
        let params = self.params.get();
        let rendering = Arc::clone(&self.rendering);

        let (tx, rx) = std::sync::mpsc::channel();
        *self.render_rx.borrow_mut() = Some(rx);
        self.start_render_poller();

        std::thread::spawn(move || {
            // Phase 7 ISSUE 3: a panicking render must never take the app
            // down — convert the panic into the normal Failed path.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let t0 = Instant::now();
                let rendered = mvl_audio::engine::render_mono_to_buffer(
                    &mono,
                    sample_rate,
                    1, // preview is stored mono (see PreviewRender)
                    &params,
                    track.as_deref(),
                );
                let render_ms = t0.elapsed().as_secs_f64() * 1000.0;
                (rendered, render_ms)
            }));
            rendering.store(false, Ordering::SeqCst);
            let done = match result {
                Ok((Ok(buffer), render_ms)) => {
                    let pyramid = WaveformPyramid::build(buffer.samples(), buffer.sample_rate());
                    RenderDone::Ok(Box::new(PreviewRender {
                        buffer,
                        pyramid,
                        render_ms,
                        params,
                    }))
                }
                Ok((Err(e), _)) => RenderDone::Failed(e.to_string()),
                Err(panic) => {
                    RenderDone::Failed(format!("render thread panicked: {}", panic_message(&panic)))
                }
            };
            let _ = tx.send(done);
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
                        let label = app.stages_label(&pr.params);
                        *app.preview.borrow_mut() = Some(*pr);
                        app.window.set_can_preview(true);
                        app.status(
                            app.t().status_rendered,
                            &[("ms", app.num(format!("{ms:.0}"))), ("stages", label)],
                        );
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
                        app.status(app.t().status_render_failed, &[("error", app.num(e))]);
                    }
                }
            },
        );
    }

    // ── Import / export / transport (4.4) ────────────────────────────

    fn import_clicked(&self) {
        match dialogs::pick_audio_file() {
            Ok(Some(path)) => match dialogs::load_any(&path) {
                Ok(buffer) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    self.load_audio(buffer, &name);
                }
                Err(e) => self.status(
                    self.t().status_import_failed,
                    &[("error", self.num(e.to_string()))],
                ),
            },
            Ok(None) => {}
            Err(e) => self.status(
                self.t().status_dialog_failed,
                &[("error", self.num(e.to_string()))],
            ),
        }
    }

    /// Renders the current parameter state and writes it to the chosen
    /// path (non-destructive: the project buffer is never touched).
    /// Exports the current render (public for the evidence test path).
    pub fn export_confirm(&self) {
        let mp3 = self.export_format.get() == 3;
        let default_name = {
            let session = self.session.borrow();
            let name = session
                .as_ref()
                .map_or("project", |s| s.name.as_str())
                .replace([' ', '/'], "-");
            format!("{name}-rendered.{}", if mp3 { "mp3" } else { "wav" })
        };
        let path = match dialogs::pick_save_path(&default_name, mp3) {
            Ok(Some(p)) => p,
            Ok(None) => return,
            Err(e) => {
                self.status(
                    self.t().status_dialog_failed,
                    &[("error", self.num(e.to_string()))],
                );
                self.window.set_export_open(false);
                return;
            }
        };
        self.window.set_export_open(false);

        // Always render fresh: export must reflect the current sliders.
        let rendered = {
            let session_borrow = self.session.borrow();
            let Some(session) = session_borrow.as_ref() else {
                return;
            };
            mvl_audio::engine::render_mono_to_buffer(
                &session.mono,
                session.buffer.sample_rate(),
                session.buffer.channels(),
                &self.params.get(),
                session.track.as_deref(),
            )
        };
        let rendered = match rendered {
            Ok(b) => b,
            Err(e) => {
                self.status(
                    self.t().status_render_failed,
                    &[("error", self.num(e.to_string()))],
                );
                return;
            }
        };
        let t0 = Instant::now();
        let written = if mp3 {
            mvl_audio::export_mp3(&path, &rendered, self.export_bitrate.get())
        } else {
            let depth = match self.export_format.get() {
                1 => mvl_audio::WavBitDepth::Int24,
                2 => mvl_audio::WavBitDepth::Int16,
                _ => mvl_audio::WavBitDepth::Float32,
            };
            mvl_audio::export_wav(&path, &rendered, depth)
        };
        match written {
            Ok(()) => self.status(
                self.t().status_exported,
                &[
                    ("path", self.num(path.display().to_string())),
                    (
                        "ms",
                        self.num(format!("{:.0}", t0.elapsed().as_secs_f64() * 1000.0)),
                    ),
                ],
            ),
            Err(e) => self.status(
                self.t().status_export_failed,
                &[("error", self.num(e.to_string()))],
            ),
        }
    }

    /// The buffer playback should use: the preview render when one is
    /// current, the original project buffer otherwise.
    fn playback_source(&self) -> Option<Arc<AudioBuffer>> {
        if !self.dirty.get()
            && let Some(pr) = self.preview.borrow().as_ref()
        {
            return Some(Arc::new(pr.buffer.clone()));
        }
        let session = self.session.borrow();
        session.as_ref().map(|s| Arc::new(s.buffer.clone()))
    }

    // ── Live streaming preview (Phase 6) ─────────────────────────────

    /// Starts the live preview when it can run: device present, session
    /// rate == device rate, pYIN track available, non-neutral parameters.
    /// Any miss keeps the offline preview path (honest fallback — a missing
    /// device is a normal situation, not an error to surface here).
    fn live_start(&self) -> bool {
        if self.live.borrow().is_some() {
            return true;
        }
        let session_borrow = self.session.borrow();
        let Some(session) = session_borrow.as_ref() else {
            return false;
        };
        let Some(track) = session.track.as_ref() else {
            return false;
        };
        if self.params.get().is_neutral() {
            return false;
        }
        let rate = session.buffer.sample_rate();
        let Some(device_rate) = mvl_audio::devices::default_output_rate() else {
            return false;
        };
        if device_rate != rate {
            return false;
        }
        let capacity = ((rate / 10) as usize).max(960); // 100 ms FIFO
        match mvl_audio::PreviewStream::start(
            Arc::clone(&session.mono),
            rate,
            Arc::clone(track),
            &self.params.get(),
            self.playhead_sample(),
            capacity,
        ) {
            Ok(stream) => {
                *self.live.borrow_mut() = Some(stream);
                true
            }
            Err(_) => false,
        }
    }

    /// Slider change while live: restart the chain at the playhead (the
    /// change is audible within ~one FIFO depth, crossfaded). Neutral
    /// parameters stop the live path — the original buffer plays.
    fn live_update(&self) {
        if self.params.get().is_neutral() {
            self.live_stop();
            return;
        }
        let active = self.live.borrow().is_some() || self.live_start();
        if !active {
            return;
        }
        if let Some(live) = self.live.borrow().as_ref() {
            live.restart(&self.params.get(), self.playhead_sample());
            // The latency readout is the *last* measured restart (the
            // current one is measured asynchronously in the worker).
            let us = live
                .stats()
                .last_restart_us
                .map(|v| self.num(format!("{v}")))
                .unwrap_or_else(|| self.num("—".into()));
            self.status(self.t().status_live, &[("us", us)]);
        }
    }

    /// Transport stop/rewind with a live source: rewind the stream cursor
    /// to the beginning (hard jump; the worker re-renders from 0).
    fn live_rewind(&self) {
        if let Some(live) = self.live.borrow().as_ref() {
            live.restart(&self.params.get(), 0);
        }
    }

    /// Stops and drops the live preview (new session, neutral parameters).
    fn live_stop(&self) {
        if let Some(live) = self.live.borrow_mut().take() {
            live.stop();
        }
    }

    /// Current playback position in session samples (the FIFO read cursor
    /// via the player; 0 when stopped or without a player).
    fn playhead_sample(&self) -> usize {
        let len = self.session.borrow().as_ref().map_or(0, |s| s.mono.len());
        let guard = self.player.borrow();
        let Some(player) = guard.as_ref() else {
            return 0;
        };
        let (rate, _) = player.output_format();
        let s = (player.position_secs() * f64::from(rate)) as usize;
        s.min(len)
    }

    fn play_pause(&self) {
        // Lazily connect; honest failure without a device. The user's
        // device pick (if any) steers the connection; the resolve chain
        // inside connect_named covers a vanished device.
        if self.player.borrow().is_none() {
            let out_name = self.selected_output.borrow().clone();
            let connect = mvl_audio::Player::connect_named(out_name.as_deref());
            if let Err(e) = connect.map(|p| {
                *self.player.borrow_mut() = Some(p);
            }) {
                self.status(
                    self.t().status_no_playback,
                    &[("error", self.num(e.to_string()))],
                );
                return;
            }
        }
        let transport = self
            .player
            .borrow()
            .as_ref()
            .map_or(mvl_audio::Transport::Stopped, |p| p.transport());
        let result = match transport {
            mvl_audio::Transport::Playing => self
                .player
                .borrow()
                .as_ref()
                .map(|p| p.pause())
                .unwrap_or(Ok(())),
            mvl_audio::Transport::Paused => self
                .player
                .borrow()
                .as_ref()
                .map(|p| p.resume())
                .unwrap_or(Ok(())),
            mvl_audio::Transport::Stopped => {
                // Offline path: render synchronously so playback always
                // matches the sliders. The live path (Phase 6) re-renders
                // continuously — no stall there.
                if self.dirty.get() && !self.live_start() {
                    self.render_now();
                }
                if self.live.borrow().is_some() {
                    // Stream from the live preview FIFO (mono, session
                    // rate — the Player validates the rate match).
                    let fifo = self.live.borrow().as_ref().map(|l| l.fifo().clone());
                    let rate = self
                        .session
                        .borrow()
                        .as_ref()
                        .map(|s| s.buffer.sample_rate());
                    match (fifo, rate) {
                        (Some(fifo), Some(rate)) => self
                            .player
                            .borrow_mut()
                            .as_mut()
                            .map_or(Ok(()), |p| p.play_stream(fifo, rate)),
                        _ => Ok(()),
                    }
                } else {
                    match self.playback_source() {
                        Some(buffer) => self
                            .player
                            .borrow_mut()
                            .as_mut()
                            .map_or(Ok(()), |p| p.play(buffer)),
                        None => Ok(()),
                    }
                }
            }
        };
        if let Err(e) = result {
            self.status(
                self.t().status_playback_failed,
                &[("error", self.num(e.to_string()))],
            );
        }
    }

    fn record_toggle(&self) {
        if self.recorder.borrow().is_some() {
            let recorder = self.recorder.borrow_mut().take();
            if let Some(recorder) = recorder {
                let overflow = recorder.dropped_overflow();
                match recorder.stop() {
                    Ok(buffer) => {
                        let secs = buffer.duration_secs();
                        let sr = buffer.sample_rate();
                        let ch = buffer.channels();
                        self.load_audio(buffer, "recording");
                        self.status(
                            self.t().status_recorded,
                            &[
                                ("secs", self.num(format!("{secs:.1}"))),
                                ("rate", self.num(sr.to_string())),
                                ("ch", self.num(ch.to_string())),
                                (
                                    "overflow",
                                    if overflow {
                                        i18n::overflow(self.lang.get()).to_string()
                                    } else {
                                        String::new()
                                    },
                                ),
                            ],
                        );
                    }
                    Err(e) => self.status(
                        self.t().status_capture_failed,
                        &[("error", self.num(e.to_string()))],
                    ),
                }
            }
            return;
        }
        // Arming: stop playback first (single capture stream policy).
        if let Some(p) = self.player.borrow_mut().as_ref() {
            let _ = p.stop();
        }
        let input_name = self.selected_input.borrow().clone();
        match mvl_audio::Recorder::start_named(input_name.as_deref()) {
            Ok(recorder) => {
                let info = recorder.info().clone();
                let matched = info.matched_preferred_rate;
                *self.record_started.borrow_mut() = Some(Instant::now());
                *self.recorder.borrow_mut() = Some(recorder);
                self.status(
                    self.t().status_recording,
                    &[
                        ("rate", self.num(info.sample_rate.to_string())),
                        (
                            "cap",
                            if matched {
                                String::new()
                            } else {
                                i18n::record_cap(self.lang.get()).to_string()
                            },
                        ),
                    ],
                );
            }
            Err(e) => {
                self.status(
                    self.t().status_no_record,
                    &[("error", self.num(e.to_string()))],
                );
            }
        }
    }

    /// 40 ms UI poller: syncs transport state, playhead and readout.
    fn poll_transport(&self) {
        let recording = self.recorder.borrow().is_some();
        self.window.set_recording(recording);
        if recording {
            if let Some(start) = *self.record_started.borrow() {
                let secs = start.elapsed().as_secs_f64();
                // The ● REC label follows the table; the clock is an LTR
                // island in both languages.
                self.window.set_time_readout(
                    format!("\u{25cf} {} {}", self.t().record, self.num(fmt_time(secs))).into(),
                );
            }
            return;
        }
        // Test-output flag follows the real transport (never a timer):
        // as soon as the player is not Playing, the affordance resets.
        if self.window.get_test_playing() {
            let still_playing = self
                .player
                .borrow()
                .as_ref()
                .is_some_and(|p| p.transport() == mvl_audio::Transport::Playing);
            if !still_playing {
                self.window.set_test_playing(false);
            }
        }
        let player = self.player.borrow();
        let Some(player) = player.as_ref() else {
            return;
        };
        let transport = player.transport();
        self.window
            .set_playing(transport == mvl_audio::Transport::Playing);
        let duration = self
            .session
            .borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration_secs());
        let pos = player.position_secs().min(duration);
        let readout = self.num(format!("{} / {}", fmt_time(pos), fmt_time(duration)));
        self.window.set_time_readout(readout.into());
        if transport == mvl_audio::Transport::Playing {
            self.set_playhead(pos);
        }
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

        let t = self.t();
        let name = s.name.clone();

        // A/B: the preview shows the rendered pyramid without the input
        // analysis overlay (tint/trace describe the *input*, not the
        // render).
        let (pyramid, mono, track, colors, label) = if show_preview {
            match preview.as_ref() {
                Some(pr) => (
                    &pr.pyramid,
                    pr.buffer.samples(),
                    None,
                    PREVIEW_COLORS,
                    i18n::tpl(t.track_preview, &[("name", name)]),
                ),
                None => (
                    &s.pyramid,
                    s.mono.as_slice(),
                    s.track.as_deref(),
                    STUDIO_COLORS,
                    i18n::tpl(t.track_original, &[("name", name)]),
                ),
            }
        } else {
            (
                &s.pyramid,
                s.mono.as_slice(),
                s.track.as_deref(),
                STUDIO_COLORS,
                i18n::tpl(t.track_original, &[("name", name)]),
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
        // The span itself is a Latin engineering fragment; the localized
        // prefix + LTR isolation keep it stable in RTL.
        let span = fmt_span(b - a);
        self.window
            .set_zoom_label(format!("{} {}", t.view_label, self.num(span)).into());
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

impl App {
    /// Human-readable stage list for the render report (localized;
    /// empty → the neutral label).
    fn stages_label(&self, p: &EngineParams) -> String {
        let active = stages_of(p);
        let applied: Vec<&str> = i18n::stages(self.lang.get())
            .iter()
            .zip(active.iter())
            .filter(|(_, on)| **on)
            .map(|(name, _)| *name)
            .collect();
        if applied.is_empty() {
            self.t().neutral.into()
        } else {
            applied.join(" → ")
        }
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

/// Formats seconds as `m:ss.mmm` (time readout).
fn fmt_time(secs: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    let m = total_ms / 60_000;
    let s = (total_ms % 60_000) / 1000;
    let ms = total_ms % 1000;
    format!("{m}:{s:02}.{ms:03}")
}

/// Formats a view span for the zoom readout (bare engineering fragment;
/// the localized "view"/"عرض" prefix is added by the caller).
fn fmt_span(secs: f64) -> String {
    if secs >= 1.0 {
        format!("{secs:.2} s")
    } else if secs >= 0.001 {
        format!("{:.1} ms", secs * 1000.0)
    } else {
        format!("{:.0} µs", secs * 1_000_000.0)
    }
}
