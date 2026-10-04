//! Headless screenshot mode — renders the real UI through Slint's software
//! renderer into a PNG, with no display server required.
//!
//! This is the evidence pipeline (D14: every "works" claim needs a
//! screenshot): the pixels saved by `--screenshot` come from the actual
//! `AppWindow` component with the actual engine state applied, exactly as a
//! desktop user would see it. It also serves as the CI smoke test
//! (`tests/screenshot.rs` uses the same code path).
//!
//! The platform uses a deterministic virtual clock that advances 16 ms per
//! animation tick, so a fixed number of spins runs every entry animation to
//! completion — screenshots are byte-reproducible for identical inputs.

use slint::ComponentHandle;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use crate::i18n::Lang;

thread_local! {
    /// The installed headless platform. Slint installs a platform once per
    /// thread; subsequent renders swap in a fresh window instead.
    static PLATFORM: RefCell<Option<Rc<HeadlessPlatform>>> = const { RefCell::new(None) };
}

/// A platform that renders into a pixel buffer with a virtual clock.
struct HeadlessPlatform {
    window: RefCell<Rc<MinimalSoftwareWindow>>,
    /// Virtual milliseconds; advances 16 ms per `duration_since_start` call
    /// (one animation tick per spin of the render loop).
    clock_ms: Cell<u64>,
}

impl HeadlessPlatform {
    /// Makes `window` the adapter handed to the next `AppWindow::new()`.
    fn install_window(&self, window: Rc<MinimalSoftwareWindow>) {
        *self.window.borrow_mut() = window;
    }
}

/// `Box<dyn Platform>` owner that delegates to the shared inner platform
/// (the thread-local keeps an `Rc` so later renders can swap windows).
struct SharedHeadless(Rc<HeadlessPlatform>);

impl Platform for SharedHeadless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.window.borrow().clone())
    }

    fn duration_since_start(&self) -> std::time::Duration {
        let next = self.0.clock_ms.get() + 16;
        self.0.clock_ms.set(next);
        std::time::Duration::from_millis(next)
    }
}

/// Parsed `--screenshot` invocation.
#[derive(Debug)]
pub struct ScreenshotArgs {
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    /// `--demo synth` — built-in demo vocal.
    pub demo: Option<String>,
    /// `--open PATH` — load an audio file.
    pub open: Option<PathBuf>,
    /// `--playhead SECS` — static playhead position.
    pub playhead: Option<f64>,
    /// `--window SECS` — visible time span (zoom state).
    pub window_secs: Option<f64>,
    /// `--pitch ST` / `--air DB` / `--formant MM` — engine parameters.
    pub pitch: Option<f64>,
    pub air: Option<f64>,
    pub formant: Option<f64>,
    /// `--preview` — show the A/B preview view instead of the original.
    pub preview: bool,
    /// `--lang en|ar` — UI language (ar renders the RTL mirrored layout).
    pub lang: Lang,
    /// `--export-panel` — open the export panel (the same property the
    /// export button toggles).
    pub export_panel: bool,
}

/// Parses `--screenshot --out PATH [flags]`.
///
/// Unknown flags are rejected so typos fail loudly instead of silently
/// rendering a wrong evidence state.
///
/// # Errors
/// Fails on a missing `--out`, malformed values, or unknown flags.
pub fn parse_args(args: &[String]) -> Result<ScreenshotArgs, String> {
    let mut out = None;
    let mut width = 1280u32;
    let mut height = 800u32;
    let mut demo = None;
    let mut open = None;
    let mut playhead = None;
    let mut window_secs = None;
    let mut pitch = None;
    let mut air = None;
    let mut formant = None;
    let mut preview = false;
    let mut lang = Lang::En;
    let mut export_panel = false;
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        match a {
            "--screenshot" => {} // mode selector, already consumed
            "--preview" => preview = true,
            "--export-panel" => export_panel = true,
            "--lang" => {
                lang = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--lang needs en|ar")?
            }
            "--out" => out = it.next().map(PathBuf::from),
            "--width" => {
                width = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--width needs a number")?
            }
            "--height" => {
                height = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--height needs a number")?
            }
            "--demo" => demo = it.next().map(String::from),
            "--open" => open = it.next().map(PathBuf::from),
            "--playhead" => {
                playhead = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--playhead needs seconds")?,
                )
            }
            "--window" => {
                window_secs = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--window needs seconds")?,
                )
            }
            "--pitch" => {
                pitch = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--pitch needs semitones")?,
                )
            }
            "--air" => {
                air = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--air needs dB")?,
                )
            }
            "--formant" => {
                formant = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--formant needs millimetres")?,
                )
            }
            other => return Err(format!("unknown screenshot flag: {other}")),
        }
    }
    Ok(ScreenshotArgs {
        out: out.ok_or("--screenshot requires --out PATH")?,
        width,
        height,
        demo,
        open,
        playhead,
        window_secs,
        pitch,
        air,
        formant,
        preview,
        lang,
        export_panel,
    })
}

/// Renders the app headlessly and writes a PNG.
///
/// # Errors
/// Fails on platform setup, PNG encoding, or I/O — surfaced to `main`.
pub fn render_to_png(
    args: &ScreenshotArgs,
    populate: impl FnOnce(&crate::app::App) -> Result<(), String>,
) -> Result<(), String> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    PLATFORM.with(|slot| -> Result<(), String> {
        let mut installed = slot.borrow_mut();
        match installed.as_ref() {
            Some(p) => p.install_window(Rc::clone(&window)),
            None => {
                let inner = Rc::new(HeadlessPlatform {
                    window: RefCell::new(Rc::clone(&window)),
                    clock_ms: Cell::new(0),
                });
                slint::platform::set_platform(Box::new(SharedHeadless(Rc::clone(&inner))))
                    .map_err(|e| format!("platform setup failed: {e}"))?;
                *installed = Some(inner);
            }
        }
        Ok(())
    })?;

    let app = crate::app::App::new().map_err(|e| e.to_string())?;
    window.set_size(slint::PhysicalSize::new(args.width, args.height));
    populate(&app)?;
    app.window().show().map_err(|e| e.to_string())?;

    // Spin the animation clock: 40 ticks × 16 ms = 640 virtual ms, enough
    // for the longest 160 ms token animation to finish from any start.
    for _ in 0..40 {
        slint::platform::update_timers_and_animations();
    }
    // Layout has settled: re-render the waveform at the final viewport.
    app.refresh();
    for _ in 0..4 {
        slint::platform::update_timers_and_animations();
    }

    let (w, h) = (args.width as usize, args.height as usize);
    let mut pixels = vec![PremultipliedRgbaColor::default(); w * h];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, w);
    });

    save_png(&args.out, &pixels, args.width, args.height)?;
    app.window().hide().map_err(|e| e.to_string())?;
    Ok(())
}

/// Un-premultiplies and encodes the pixel buffer as PNG (RGBA8).
/// Creates parent directories when missing.
fn save_png(
    path: &std::path::Path,
    pixels: &[PremultipliedRgbaColor],
    width: u32,
    height: u32,
) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for p in pixels {
        if p.alpha == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let a = u16::from(p.alpha);
            rgba.push((u16::from(p.red) * 255 / a) as u8);
            rgba.push((u16::from(p.green) * 255 / a) as u8);
            rgba.push((u16::from(p.blue) * 255 / a) as u8);
            rgba.push(p.alpha);
        }
    }

    let file = std::fs::File::create(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| format!("PNG header failed: {e}"))?;
    writer
        .write_image_data(&rgba)
        .map_err(|e| format!("PNG write failed: {e}"))?;
    Ok(())
}
