//! Micro-Vocal Lab — application library.
//!
//! Everything the binary needs lives here so the headless screenshot mode,
//! the GUI binary and the integration tests share one code path:
//!
//! - [`app`] — component bootstrap and string installation.
//! - [`headless`] — software-renderer platform + PNG evidence pipeline.
//! - [`generated`] — the Slint-generated bindings (re-exported at root).
//!
//! The session/audio state machine joins in sub-items 4.2+.

pub mod app;
pub mod dialogs;
pub mod headless;
pub mod i18n;
pub mod session;
pub mod waveform;

mod generated {
    // The generated bindings are managed by SixtyFPS; keep our crate-level
    // lint intent (no unsafe of our own) while allowing theirs in that one
    // module.
    #![allow(unsafe_code)]
    slint::include_modules!();
}

pub use generated::*;
