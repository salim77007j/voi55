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
use crate::RulerTick;
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

use rustfft::FftPlanner;

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
    /// Drag selection on the waveform, in seconds `(start, end)` —
    /// absolute project time (Phase 7.2 View menu / zoom-to-selection).
    selection: RefCell<Option<(f64, f64)>>,
    /// Master output volume 0..=1.5 (1.0 unity). Applied to the real
    /// output callback; persisted across player reconnects.
    volume: Cell<f32>,
    /// Meter smoothing state (decay + peak hold), UI-thread only.
    meter_l: Cell<f32>,
    meter_r: Cell<f32>,
    peak_l: Cell<f32>,
    peak_r: Cell<f32>,
    peak_hold_l: Cell<u32>,
    peak_hold_r: Cell<u32>,
    /// Status-bar resource refresh throttle (RSS read ~1 Hz).
    slow_poll: Cell<u32>,
    /// Spectrum analyzer state (7.2e), UI-thread only.
    spectrum: RefCell<SpectrumState>,
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
            selection: RefCell::new(None),
            volume: Cell::new(1.0),
            meter_l: Cell::new(0.0),
            meter_r: Cell::new(0.0),
            peak_l: Cell::new(0.0),
            peak_r: Cell::new(0.0),
            peak_hold_l: Cell::new(0),
            peak_hold_r: Cell::new(0),
            slow_poll: Cell::new(0),
            spectrum: RefCell::new(SpectrumState::new()),
        });
        *app.self_weak.borrow_mut() = Rc::downgrade(&app);

        // LCD time display starts at zero (real idle state, no player).
        app.window.set_time_cur("00:00:00.000".into());
        app.window.set_time_total("00:00:00.000".into());

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

        // Phase 7.2: menus, transport forward-seek, master volume,
        // drag selection and the About dialog. Every entry maps to a
        // command an existing control also exposes (No-Fake-UI).
        let weak = Rc::downgrade(&app);
        app.window.on_menu_action(move |id| {
            if let Some(app) = weak.upgrade() {
                app.menu_action(id);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_transport_forward(move || {
            if let Some(app) = weak.upgrade() {
                app.seek_relative(5.0);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_volume_changed(move |v| {
            if let Some(app) = weak.upgrade() {
                app.set_volume(v);
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_wave_select(move |a, b| {
            if let Some(app) = weak.upgrade() {
                app.select_range(f64::from(a), f64::from(b));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_wave_zoom_range(move |a, b| {
            if let Some(app) = weak.upgrade() {
                app.zoom_to_selection(f64::from(a), f64::from(b));
            }
        });
        let weak = Rc::downgrade(&app);
        app.window.on_about_close(move || {
            if let Some(app) = weak.upgrade() {
                app.window.set_about_open(false);
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
        // Professional UI (7.2): menus, toolbar, info strip.
        self.window.set_t_menu_file(t.menu_file.into());
        self.window.set_t_menu_view(t.menu_view.into());
        self.window.set_t_menu_audio(t.menu_audio.into());
        self.window.set_t_menu_help(t.menu_help.into());
        self.window.set_t_mi_import(t.mi_import.into());
        self.window.set_t_mi_export(t.mi_export.into());
        self.window.set_t_mi_devices(t.mi_devices.into());
        self.window.set_t_mi_zoom_in(t.mi_zoom_in.into());
        self.window.set_t_mi_zoom_out(t.mi_zoom_out.into());
        self.window.set_t_mi_fit(t.mi_fit.into());
        self.window.set_t_mi_preview(t.mi_preview.into());
        self.window.set_t_mi_zoom_sel(t.mi_zoom_sel.into());
        self.window.set_t_mi_test_output(t.mi_test_output.into());
        self.window.set_t_mi_about(t.mi_about.into());
        self.window.set_t_toolbar_record(t.toolbar_record.into());
        self.window.set_t_toolbar_import(t.toolbar_import.into());
        self.window.set_t_toolbar_export(t.toolbar_export.into());
        self.window.set_t_panel_track(t.panel_track.into());
        self.window.set_t_panel_format(t.panel_format.into());
        self.window.set_t_panel_analysis(t.panel_analysis.into());
        self.window.set_t_cap_name(t.cap_name.into());
        self.window.set_t_cap_rate(t.cap_rate.into());
        self.window.set_t_cap_frames(t.cap_frames.into());
        self.window.set_t_cap_domain(t.cap_domain.into());
        self.window.set_t_cap_channels(t.cap_channels.into());
        self.window.set_t_cap_f0(t.cap_f0.into());
        self.window.set_t_cap_voiced(t.cap_voiced.into());
        self.window.set_t_spectrum(t.spectrum.into());
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
        // New project invalidates the old drag selection.
        *self.selection.borrow_mut() = None;
        self.window.set_sel_start_frac(-1.0);
        self.window.set_sel_end_frac(-1.0);
        self.sync_info_strip();
        self.sync_status_segments();
        self.recompose_base_status();
        self.sync_param_ui();
        self.sync_time_display(0.0);
        self.compose_status(None);
        self.refresh();
    }

    /// Pushes the loaded session's real values into the info strip
    /// (No-Fake-UI): identity, format, pYIN analysis summary. Missing
    /// values render as em-dashes in the strip.
    fn sync_info_strip(&self) {
        let guard = self.session.borrow();
        let Some(s) = guard.as_ref() else {
            self.window.set_info_name("".into());
            self.window.set_info_rate("".into());
            self.window.set_info_frames("".into());
            self.window.set_info_domain("".into());
            self.window.set_info_channels("".into());
            self.window.set_info_f0("".into());
            self.window.set_info_voiced("".into());
            return;
        };
        let (f0, voiced) = match &s.track {
            Some(track) => (
                self.num(format!("{:.1} Hz", track.median_f0())),
                self.num(format!("{:.0} %", track.voiced_ratio() * 100.0)),
            ),
            // Analysis skipped/failed: honest dash, not a fake number.
            _ => ("\u{2014}".into(), "\u{2014}".into()),
        };
        self.window.set_info_name(s.name.clone().into());
        self.window
            .set_info_rate(self.num(format!("{} Hz", s.buffer.sample_rate())).into());
        self.window
            .set_info_frames(self.num(s.buffer.frames().to_string()).into());
        self.window.set_info_domain("f32".into());
        self.window
            .set_info_channels(self.num(format!("{} ch", s.buffer.channels())).into());
        self.window.set_info_f0(f0.into());
        self.window.set_info_voiced(voiced.into());
    }

    /// Refreshes the status-bar segments from real state: file name,
    /// buffer format, resolved device routing (explicit pick or the
    /// resolve-chain default the engine will actually use).
    fn sync_status_segments(&self) {
        let guard = self.session.borrow();
        if let Some(s) = guard.as_ref() {
            self.window.set_seg_file(s.name.clone().into());
            self.window.set_seg_format(
                self.num(format!(
                    "{} Hz \u{b7} f32 \u{b7} {} ch",
                    s.buffer.sample_rate(),
                    s.buffer.channels()
                ))
                .into(),
            );
        } else {
            self.window.set_seg_file("".into());
            self.window.set_seg_format("".into());
        }
        drop(guard);
        let din = self.effective_input_name().unwrap_or_default();
        let dout = self.effective_output_name().unwrap_or_default();
        self.window.set_seg_in(din.into());
        self.window.set_seg_out(dout.into());
    }

    /// Menu dispatch: File 0-9, View 10-19, Audio 20-29, Help 30-39.
    fn menu_action(&self, id: i32) {
        match id {
            0 => self.import_clicked(),
            1 => {
                let open = !self.window.get_export_open();
                self.window.set_export_open(open);
            }
            2 | 21 => {
                let open = !self.window.get_devices_open();
                self.window.set_devices_open(open);
                if open {
                    self.refresh_devices();
                }
            }
            10 => self.zoom(1.6, 0.5),
            11 => self.zoom(1.0 / 1.6, 0.5),
            12 => self.fit(),
            13 => self.toggle_preview(),
            14 => {
                // Zoom to the live selection (real view change).
                let sel = *self.selection.borrow();
                if let Some((a, b)) = sel {
                    let mut borrowed = self.session.borrow_mut();
                    if let Some(session) = borrowed.as_mut() {
                        session.set_view_secs(a.min(b), a.max(b));
                    }
                    drop(borrowed);
                    self.refresh();
                }
            }
            20 => self.test_output(),
            30 => self.window.set_about_open(true),
            _ => {}
        }
    }

    /// Seeks to an absolute position (seconds). The streaming path
    /// restarts the preview worker at the target; the buffered path
    /// jumps the player cursor. Silent no-op without a project.
    fn seek_to(&self, secs: f64) {
        let target = {
            let guard = self.session.borrow();
            let Some(s) = guard.as_ref() else {
                return;
            };
            secs.clamp(0.0, s.duration_secs())
        };
        let live_active = self.live.borrow().is_some();
        let frame = {
            let guard = self.player.borrow();
            let Some(player) = guard.as_ref() else {
                return;
            };
            let (rate, _) = player.output_format();
            (target * f64::from(rate)) as usize
        };
        if live_active {
            // Streaming source: the worker re-renders from the target;
            // the FIFO cursor (the player's position) jumps with it.
            if let Some(live) = self.live.borrow().as_ref() {
                live.restart(&self.params.get(), frame);
            }
        } else {
            let guard = self.player.borrow();
            if let Some(player) = guard.as_ref() {
                let _ = player.seek_to_frame(frame);
            }
        }
        self.set_playhead(target);
        self.sync_time_display(target);
    }

    /// Forward seek relative to the current position (the FF transport
    /// button). The to-start button keeps its existing stop+rewind.
    fn seek_relative(&self, delta: f64) {
        let pos = {
            let guard = self.player.borrow();
            let Some(player) = guard.as_ref() else {
                return;
            };
            if player.transport() == mvl_audio::Transport::Stopped {
                return;
            }
            player.position_secs()
        };
        self.seek_to(pos + delta);
    }

    /// Applies the master volume to the real output callback and keeps
    /// the persisted value across reconnects.
    fn set_volume(&self, v: f32) {
        let v = v.clamp(0.0, 1.5);
        self.volume.set(v);
        self.window.set_volume(v);
        if let Some(p) = self.player.borrow().as_ref() {
            p.set_volume(v);
        }
    }

    /// Drag selection: converts view fractions to absolute seconds and
    /// publishes both the overlay and the selection readout.
    fn select_range(&self, a: f64, b: f64) {
        let guard = self.session.borrow();
        let Some(s) = guard.as_ref() else {
            return;
        };
        let (start, end) = s.view_secs();
        let span = end - start;
        let sa = start + a.clamp(0.0, 1.0) * span;
        let sb = start + b.clamp(0.0, 1.0) * span;
        drop(guard);
        *self.selection.borrow_mut() = Some((sa.min(sb), sa.max(sb)));
        self.window
            .set_sel_start_frac(a.min(b).clamp(0.0, 1.0) as f32);
        self.window
            .set_sel_end_frac(a.max(b).clamp(0.0, 1.0) as f32);
        let dur = (sa.max(sb) - sa.min(sb)).abs();
        self.status(
            self.t().status_selected,
            &[("span", self.num(fmt_span(dur)))],
        );
    }

    /// View-menu zoom-to-selection from raw fractions (same conversion
    /// as [`Self::select_range`] but jumps the view window).
    fn zoom_to_selection(&self, a: f64, b: f64) {
        let guard = self.session.borrow();
        let Some(s) = guard.as_ref() else {
            return;
        };
        let (start, end) = s.view_secs();
        let span = end - start;
        let sa = start + a.clamp(0.0, 1.0) * span;
        let sb = start + b.clamp(0.0, 1.0) * span;
        drop(guard);
        if (sa - sb).abs() < 1e-9 {
            return;
        }
        let mut borrowed = self.session.borrow_mut();
        if let Some(session) = borrowed.as_mut() {
            session.set_view_secs(sa.min(sb), sa.max(sb));
        }
        drop(borrowed);
        self.refresh();
    }

    /// Rebuilds the ruler tick model from the real view window (nice
    /// steps of 1/2/5 decades; the same model drives the well grid).
    fn sync_ruler(&self) {
        let guard = self.session.borrow();
        let Some(s) = guard.as_ref() else {
            self.window.set_ruler_ticks(ModelRc::default());
            self.window.set_grid_ticks(ModelRc::default());
            return;
        };
        let (start, end) = s.view_secs();
        drop(guard);
        let span = end - start;
        if span <= 0.0 {
            self.window.set_ruler_ticks(ModelRc::default());
            self.window.set_grid_ticks(ModelRc::default());
            return;
        }
        // Nice step: 1-2-5 decades aiming for ~8 ticks.
        let rough = span / 8.0;
        let mag = 10f64.powf(rough.log10().floor());
        let step = if rough / mag >= 5.0 {
            5.0 * mag
        } else if rough / mag >= 2.0 {
            2.0 * mag
        } else {
            mag
        };
        let model = VecModel::default();
        let mut t = (start / step).ceil() * step;
        while t <= end + 1e-12 {
            let frac = ((t - start) / span).clamp(0.0, 1.0) as f32;
            // Sub-millisecond steps need µs-precision labels or
            // consecutive ticks collide on the same rounded string.
            let label = if step < 0.001 {
                format!("{t:.4} s")
            } else {
                fmt_time(t)
            };
            model.push(RulerTick {
                frac,
                label: label.into(),
            });
            t += step;
        }
        let model = ModelRc::new(model);
        self.window.set_ruler_ticks(model.clone());
        self.window.set_grid_ticks(model);
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
                // The LCD shows the real capture clock (LTR digits in
                // both languages); the armed state shows in the toolbar
                // and transport button.
                self.window.set_time_cur(fmt_time(secs).into());
                self.window.set_time_total("\u{2014}".into());
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
        self.sync_time_display(pos);
        if transport == mvl_audio::Transport::Playing {
            self.set_playhead(pos);
        }
        self.poll_meters();
    }

    /// Drains the real level tap (record input while armed, output
    /// otherwise), applies meter ballistics (fast fall, 2 s peak hold,
    /// slow peak decay) and refreshes the resource segment at ~1 Hz.
    /// The values shown are the audio threads' own measurements — the
    /// tap is drained exactly once per tick (drain clears the level
    /// cells; the duty figure is a load-only EMA read).
    fn poll_meters(&self) {
        let source = self
            .recorder
            .borrow()
            .as_ref()
            .map(|rec| rec.meter().drain())
            .or_else(|| self.player.borrow().as_ref().map(|p| p.meter().drain()))
            .unwrap_or((0.0, 0.0, 0));
        let (raw_l, raw_r, duty_ppm) = source;
        // Ballistics per channel: level falls 20 %/tick toward the new
        // peak; the peak-hold marker waits ~2 s (50 ticks) then decays.
        let smooth = |raw: f32, level: &Cell<f32>, peak: &Cell<f32>, hold: &Cell<u32>| {
            let target = raw.max(level.get() * 0.80);
            level.set(target);
            if target >= peak.get() {
                peak.set(target);
                hold.set(50);
            } else if hold.get() > 0 {
                hold.set(hold.get() - 1);
            } else {
                peak.set(peak.get() * 0.97);
            }
        };
        smooth(raw_l, &self.meter_l, &self.peak_l, &self.peak_hold_l);
        smooth(raw_r, &self.meter_r, &self.peak_r, &self.peak_hold_r);
        self.window.set_meter_l(self.meter_l.get());
        self.window.set_meter_r(self.meter_r.get());
        self.window.set_peak_l(self.peak_l.get());
        self.window.set_peak_r(self.peak_r.get());

        // Resource segment at ~1 Hz (25 × 40 ms). Engineering fragments
        // (MB / %) stay Latin in both languages; duty ppm → % via /10⁴.
        let tick = (self.slow_poll.get() + 1) % 25;
        self.slow_poll.set(tick);
        if tick == 0 {
            let duty_pct = f64::from(duty_ppm) / 10_000.0;
            let resources = match crate::sysmetrics::resident_bytes() {
                Some(bytes) => self.num(format!(
                    "RSS {} MB \u{b7} DSP {duty_pct:.1} %",
                    bytes / (1024 * 1024)
                )),
                None => self.num(format!("DSP {duty_pct:.1} %")),
            };
            self.window.set_seg_resources(resources.into());
        }
        self.poll_spectrum();
    }

    /// Spectrum analyzer tick (7.2e): drains the output tap's newest
    /// window, FFTs it (Hann + forward), maps to 48 log-frequency bands
    /// (40 Hz–16 kHz), applies the peak-hold/decay ballistics and
    /// renders the frame the panel displays. Decays toward silence when
    /// playback stops (real signal → real decay, no frozen fake frame).
    fn poll_spectrum(&self) {
        let playing = self.window.get_playing();
        let mut st = self.spectrum.borrow_mut();
        let rate = {
            let guard = self.player.borrow();
            guard.as_ref().map_or(48_000, |p| p.output_format().0)
        };

        // 1. Gather magnitudes (0..1 per band) from the raw window.
        let mut mags = [0.0f32; SPEC_BANDS];
        if playing {
            let mut window = vec![0.0f32; SPEC_WINDOW];
            let has_audio = {
                let guard = self.player.borrow();
                match guard.as_ref() {
                    Some(p) => p.spectrum().drain_window(&mut window),
                    None => false,
                }
            };
            if has_audio {
                let hann = st.hann.clone();
                mags = spectrum::band_magnitudes(&window, rate, &hann, &mut st.planner);
            }
        }

        // 2. dB-mapped bars + peak hold/decay (ballistics regardless of
        //    playback state: the display decays honestly to silence).
        for (b, mag) in mags.iter().enumerate() {
            let db = 20.0 * mag.max(1e-9).log10();
            let norm = ((db - SPEC_DB_FLOOR) / -SPEC_DB_FLOOR).clamp(0.0, 1.0);
            let target = norm.max(st.bars[b] * 0.82);
            st.bars[b] = target;
            if target >= st.peaks[b] {
                st.peaks[b] = target;
            } else {
                st.peaks[b] = (st.peaks[b] * 0.985).max(target);
            }
        }

        // 3. Render at the panel's physical pixel size.
        let scale = f64::from(self.window.window().scale_factor());
        let w = (f64::from(self.window.get_spec_width()) * scale)
            .round()
            .max(1.0) as u32;
        let h = (f64::from(self.window.get_spec_height()) * scale)
            .round()
            .max(1.0) as u32;
        let image = Image::from_rgba8(render_spectrum(&st.bars, &st.peaks, w, h));
        self.window.set_spectrum_image(image);
    }

    /// Pushes the split LCD readout (current / total, mono digits, LTR
    /// islands in both languages).
    fn sync_time_display(&self, pos: f64) {
        let duration = self
            .session
            .borrow()
            .as_ref()
            .map_or(0.0, |s| s.duration_secs());
        self.window.set_time_cur(fmt_time(pos).into());
        self.window.set_time_total(fmt_time(duration).into());
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
            drop(session);
            self.sync_ruler();
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
        drop(session);
        self.sync_ruler();
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

/// Analyzer state (Phase 7.2e): the FFT window, the per-band peak
/// holds and the latest rendered frame. UI-thread only (the raw signal
/// crosses threads through `mvl_audio::SpectrumTap`).
use spectrum::{BANDS as SPEC_BANDS, WINDOW as SPEC_WINDOW};

const SPEC_DB_FLOOR: f32 = -64.0;

struct SpectrumState {
    /// Cached Hann window (α = 0.5), precomputed once.
    hann: Vec<f32>,
    planner: FftPlanner<f32>,
    /// Peak-hold magnitudes (normalized 0..1) per band, decaying.
    peaks: [f32; SPEC_BANDS],
    /// Last rendered frame (band bar heights 0..1), for idle decay.
    bars: [f32; SPEC_BANDS],
}

impl SpectrumState {
    fn new() -> Self {
        let hann = spectrum::hann_window(SPEC_WINDOW);
        Self {
            hann,
            planner: FftPlanner::new(),
            peaks: [0.0; SPEC_BANDS],
            bars: [0.0; SPEC_BANDS],
        }
    }
}

/// Renders the analyzer frame into an RGBA8 buffer (transparent
/// background; the panel well shows through). Bars are the live band
/// magnitudes in the pitch accent; the peak holds ride above them in
/// white (task book: peak hold + decay).
fn render_spectrum(
    bars: &[f32; SPEC_BANDS],
    peaks: &[f32; SPEC_BANDS],
    w: u32,
    h: u32,
) -> SharedPixelBuffer<Rgba8Pixel> {
    let bw = w.max(1) as usize;
    let bh = h.max(1) as usize;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(w.max(1), h.max(1));
    let band_w = bw / SPEC_BANDS;
    if band_w == 0 {
        return buf;
    }
    let bytes = buf.make_mut_bytes();
    let stride = bw * 4;
    // Opaque write over the transparent background (bars + peak ticks).
    let put = |x: usize, y: usize, c: [u8; 3], bytes: &mut [u8]| {
        let off = y * stride + x * 4;
        bytes[off] = c[0];
        bytes[off + 1] = c[1];
        bytes[off + 2] = c[2];
        bytes[off + 3] = 255;
    };
    for (b, (&bar, &peak)) in bars.iter().zip(peaks.iter()).enumerate() {
        let x0 = b * band_w;
        let x1 = (x0 + band_w).min(bw).max(x0 + 1);
        let bar_h = (bar * (bh as f32)).round() as usize;
        let y_top = bh.saturating_sub(bar_h).min(bh.saturating_sub(1));
        // Bar: pitch accent (the pitch engine's trace color family).
        for y in y_top..bh {
            for x in x0..x1 {
                put(x, y, [0x00, 0xB4, 0xD8], bytes);
            }
        }
        // Peak-hold tick (white), clamped inside the well.
        let py = bh
            .saturating_sub((peak * (bh as f32)).round() as usize)
            .min(bh - 1);
        for x in x0..x1 {
            put(x, py, [0xE0, 0xE0, 0xE0], bytes);
        }
    }
    buf
}

/// Spectrum analysis core (7.2e): Hann window + forward FFT + mapping
/// onto `SPEC_BANDS` log-spaced bands (40 Hz..min(16 kHz, Nyquist)).
/// Band magnitude = max bin in the band (spectral peaks, like an RTA).
/// Pure and unit-tested; `poll_spectrum` drives it from the live tap.
mod spectrum {
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;
    use std::f32::consts::TAU;

    pub const WINDOW: usize = 2048;
    pub const BANDS: usize = 48;
    const F_LO: f32 = 40.0;
    const F_HI: f32 = 16_000.0;

    pub fn hann_window(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                0.5 - 0.5 * (TAU * t).cos()
            })
            .collect()
    }

    pub fn band_magnitudes(
        window: &[f32],
        rate: u32,
        hann: &[f32],
        planner: &mut FftPlanner<f32>,
    ) -> [f32; BANDS] {
        debug_assert_eq!(window.len(), WINDOW);
        let mut bins: Vec<Complex<f32>> = window
            .iter()
            .zip(hann)
            .map(|(&s, &w)| Complex::new(s * w, 0.0))
            .collect();
        planner.plan_fft_forward(WINDOW).process(&mut bins);
        let norm = 2.0 / WINDOW as f32;
        let df = rate as f32 / WINDOW as f32;
        let f_hi = F_HI.min(rate as f32 / 2.0);
        let ratio = (f_hi / F_LO).ln();
        let mut mags = [0.0f32; BANDS];
        for (b, mag) in mags.iter_mut().enumerate() {
            let lo = F_LO * (ratio * b as f32 / BANDS as f32).exp();
            let hi = F_LO * (ratio * (b + 1) as f32 / BANDS as f32).exp();
            let i0 = ((lo / df).ceil() as usize).clamp(1, WINDOW / 2 - 1);
            let i1 = ((hi / df).ceil() as usize).clamp(i0 + 1, WINDOW / 2);
            *mag = bins[i0..i1]
                .iter()
                .map(|c| c.norm() * norm)
                .fold(0.0f32, f32::max);
        }
        mags
    }
}

/// Preview-mode waveform colors: the A/B view drops the voiced tint and
/// F0 trace (they describe the *input* analysis, not the render) and uses
/// the formant accent to signal "this is the processed side".
const PREVIEW_COLORS: WaveformColors = WaveformColors {
    peak_low: [0xB3, 0x88, 0xFF, 0x64],
    peak_high: [0x7C, 0x4D, 0xFF, 0x64],
    core: [0xE0, 0xE0, 0xE0, 0xD9],
    voiced: [0x00, 0x00, 0x00, 0x00],
    trace: [0x00, 0x00, 0x00, 0x00],
    center: [0x3C, 0x3C, 0x3C, 0xFF],
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

#[cfg(test)]
mod spectrum_tests {
    use super::*;

    #[test]
    fn sine_energy_lands_in_its_band() {
        let rate = 48_000u32;
        let hann = spectrum::hann_window(SPEC_WINDOW);
        let f0 = 440.0f32;
        let window: Vec<f32> = (0..SPEC_WINDOW)
            .map(|i| (std::f32::consts::TAU * f0 * i as f32 / rate as f32).sin() * 0.8)
            .collect();
        let mut planner = FftPlanner::new();
        let mags = spectrum::band_magnitudes(&window, rate, &hann, &mut planner);
        // Band edges: 40·(400)^(b/48)..40·(400)^((b+1)/48) — band 19
        // covers ≈ 429–485 Hz, so 440 Hz must peak there.
        let (bi, _) = mags
            .iter()
            .enumerate()
            .fold(
                (0usize, 0.0f32),
                |(bi, best), (i, &m)| {
                    if m > best { (i, m) } else { (bi, best) }
                },
            );
        assert!((18..=21).contains(&bi), "peak band {bi} for 440 Hz");
        // Energy well above the peak band is negligible (no leakage).
        for (i, m) in mags.iter().enumerate() {
            if i > bi + 4 {
                assert!(*m < mags[bi] * 0.05, "band {i} leaked {m}");
            }
        }
    }

    #[test]
    fn silence_maps_to_silent_bars() {
        let rate = 48_000u32;
        let hann = spectrum::hann_window(SPEC_WINDOW);
        let window = vec![0.0f32; SPEC_WINDOW];
        let mut planner = FftPlanner::new();
        let mags = spectrum::band_magnitudes(&window, rate, &hann, &mut planner);
        assert!(mags.iter().all(|&m| m < 1e-6));
    }

    #[test]
    fn renderer_draws_bars_where_the_data_is() {
        let mut bars = [0.0f32; SPEC_BANDS];
        let mut peaks = [0.0f32; SPEC_BANDS];
        bars[10] = 0.5; // half-height bar in band 10
        peaks[10] = 0.5;
        let buf = render_spectrum(&bars, &peaks, 480, 100);
        let bytes = buf.as_bytes();
        let stride = 480 * 4;
        // Band 10 spans x = 100..110; the bar covers the bottom half —
        // a pixel mid-bar must be the pitch accent.
        let off = 60 * stride + 105 * 4; // y=60 (mid-bar), x=105
        assert_eq!(&bytes[off..off + 3], &[0x00, 0xB4, 0xD8]);
        // The peak tick sits at y ≈ 50 (light gray).
        let off2 = 50 * stride + 105 * 4;
        assert_eq!(&bytes[off2..off2 + 3], &[0xE0, 0xE0, 0xE0]);
        // Empty band 30 stays transparent.
        let off3 = 90 * stride + 305 * 4;
        assert_eq!(bytes[off3 + 3], 0);
    }
}
