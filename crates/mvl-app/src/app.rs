//! Application bootstrap: creates the `AppWindow` and installs the initial
//! (English) strings. The audio/session state machine joins in 4.2+; this
//! module stays the single place that constructs the component so both the
//! GUI and the headless screenshot mode share one code path.

use crate::AppWindow;
use mvl_core::VERSION;

/// Creates the root window with the English default strings applied.
///
/// # Errors
/// Fails when the Slint platform cannot create the window adapter.
pub fn create() -> Result<AppWindow, slint::PlatformError> {
    let app = AppWindow::new()?;

    // English defaults (i18n table with Arabic arrives in sub-item 4.5).
    app.set_t_title("Micro-Vocal Lab".into());
    app.set_t_version(format!("v{VERSION}").into());
    app.set_t_waveform_empty("No audio loaded — the waveform view arrives with Phase 4.2".into());
    app.set_status_core(
        "Engine ready · pitch ±12 st / 1 ¢ · air ±dB / 0.1 dB · formant 130–190 mm".into(),
    );

    Ok(app)
}
