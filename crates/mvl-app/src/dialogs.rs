//! File open/save plumbing.
//!
//! Native dialogs come from `rfd` (xdg-portal backend on Linux — no C
//! toolkit dependency; native APIs on Windows/macOS). For headless
//! automation, CI and evidence generation the environment variables
//! `MVL_OPEN_FILE` / `MVL_SAVE_FILE` bypass the dialog entirely — the
//! automation path drives the exact same downstream code a user's dialog
//! selection would.

use std::path::PathBuf;

/// Asks the user for an audio file to open (WAV/MP3).
///
/// Returns `Ok(None)` when the user cancels.
///
/// # Errors
/// Fails when the dialog backend itself fails.
pub fn pick_audio_file() -> Result<Option<PathBuf>, String> {
    if let Some(path) = std::env::var_os("MVL_OPEN_FILE") {
        return Ok(Some(PathBuf::from(path)));
    }
    let file = rfd::FileDialog::new()
        .add_filter("Audio (WAV / MP3)", &["wav", "wave", "mp3"])
        .add_filter("WAV", &["wav", "wave"])
        .add_filter("MP3", &["mp3"])
        .set_title("Import audio")
        .pick_file();
    Ok(file)
}

/// Asks the user where to save a rendered file.
///
/// Returns `Ok(None)` when the user cancels.
///
/// # Errors
/// Fails when the dialog backend itself fails.
pub fn pick_save_path(default_name: &str, mp3: bool) -> Result<Option<PathBuf>, String> {
    if let Some(path) = std::env::var_os("MVL_SAVE_FILE") {
        return Ok(Some(PathBuf::from(path)));
    }
    let filter_name = if mp3 { "MP3" } else { "WAV" };
    let filter_ext: &[&str] = if mp3 { &["mp3"] } else { &["wav"] };
    let dialog = rfd::FileDialog::new()
        .set_title("Export render")
        .set_file_name(default_name)
        .add_filter(filter_name, filter_ext);
    Ok(dialog.save_file())
}

/// Import dispatch by extension — the single entry point used by the UI
/// import button and the CLI (`--open`).
///
/// # Errors
/// Fails for unsupported extensions and when the decoder rejects the file.
pub fn load_any(path: &std::path::Path) -> Result<mvl_audio::AudioBuffer, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "wav" | "wave" => mvl_audio::import_wav(path).map_err(|e| e.to_string()),
        "mp3" => mvl_audio::import_mp3(path).map_err(|e| e.to_string()),
        other => Err(format!(
            "unsupported file type {other:?} — open .wav or .mp3"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes env-var mutation against other tests in this binary.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_override_bypasses_the_dialog() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        // SAFETY: single env writer (this test holds ENV_LOCK); no other
        // thread in this binary reads these variables concurrently.
        unsafe { std::env::set_var("MVL_OPEN_FILE", "/tmp/some-take.wav") };
        let picked = pick_audio_file().expect("pick");
        assert_eq!(picked, Some(PathBuf::from("/tmp/some-take.wav")));
        unsafe { std::env::remove_var("MVL_OPEN_FILE") };

        unsafe { std::env::set_var("MVL_SAVE_FILE", "/tmp/render.wav") };
        let saved = pick_save_path("x.wav", false).expect("save");
        assert_eq!(saved, Some(PathBuf::from("/tmp/render.wav")));
        unsafe { std::env::remove_var("MVL_SAVE_FILE") };
    }
}
