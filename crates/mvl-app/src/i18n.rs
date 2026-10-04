//! i18n (D1/D12): English LTR default, Arabic RTL at runtime.
//!
//! Explicit string tables (rather than gettext) keep every user-visible
//! string in one place, testable without translation tooling, and allow
//! switching language + layout direction live. Western digits are kept for
//! engineering readouts in both directions (D12); LTR marks (U+200E) wrap
//! numeric/composed readouts inside Arabic text so bidi never mangles
//! them.
//!
//! Honest scope note: status-bar *technical* fragments (file names, unit
//! symbols `st`/`dB`/`mm`, Hz) stay in Latin engineering notation in both
//! languages; all fixed UI labels, commands and warnings are translated.

use std::str::FromStr;

/// The LTR mark, used to protect numeric readouts inside RTL text.
pub const LRM: &str = "\u{200E}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Ar,
}

impl FromStr for Lang {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "en" | "en-us" | "en_US" => Ok(Self::En),
            "ar" | "ar-eg" | "ar-sa" | "ar_EG" | "ar_SA" => Ok(Self::Ar),
            other => Err(format!("unknown language {other:?} (en | ar)")),
        }
    }
}

/// All user-visible strings for one language.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrTable {
    pub ui_font: &'static str,
    pub rtl: bool,
    pub waveform_empty: &'static str,
    pub pitch: &'static str,
    pub air: &'static str,
    pub formant: &'static str,
    pub record: &'static str,
    pub export_title: &'static str,
    pub export_go: &'static str,
    pub bitrate: &'static str,
    /// Language chip shows the *other* language as the affordance.
    pub language_label: &'static str,
    pub status_ready: &'static str,
    pub status_loaded: &'static str,
    pub status_track_failed: &'static str,
    pub status_rendered: &'static str,
    pub status_rendering: &'static str,
    pub status_render_failed: &'static str,
    pub status_no_playback: &'static str,
    pub status_playback_failed: &'static str,
    /// Live streaming preview engaged (Phase 6). `{us}` is the last
    /// measured restart latency in µs (empty on the first restart).
    pub status_live: &'static str,
    pub status_no_record: &'static str,
    pub status_recording: &'static str,
    pub status_recorded: &'static str,
    pub status_capture_failed: &'static str,
    pub status_exported: &'static str,
    pub status_export_failed: &'static str,
    pub status_import_failed: &'static str,
    pub status_dialog_failed: &'static str,
    /// Waveform corner label for the un-processed project view.
    pub track_original: &'static str,
    /// Waveform corner label for the A/B rendered view.
    pub track_preview: &'static str,
    /// Zoom readout prefix (`view 2.50 s`). The span itself is a Latin
    /// engineering fragment in both languages.
    pub view_label: &'static str,
    /// Render-report label when no stage deviates from neutral.
    pub neutral: &'static str,
    // ── Devices dialog (Phase 7.1) ──────────────────────────────
    pub devices_title: &'static str,
    pub devices_in: &'static str,
    pub devices_out: &'static str,
    pub devices_test: &'static str,
    pub devices_refresh: &'static str,
    pub devices_none: &'static str,
    pub devices_close: &'static str,
    /// Status: input device now `{name}`.
    pub status_input_selected: &'static str,
    /// Status: output device now `{name}`.
    pub status_output_selected: &'static str,
    /// Status suffix when the requested device was gone and a fallback
    /// device was used instead.
    pub device_fallback: &'static str,
    /// Status: the output test tone played.
    pub status_test_played: &'static str,
}

pub const EN: StrTable = StrTable {
    ui_font: "Inter",
    rtl: false,
    waveform_empty: "No audio loaded — import a WAV / MP3, record, or run with --demo synth",
    pitch: "Pitch",
    air: "Air & Breath",
    formant: "Formant",
    record: "REC",
    export_title: "Export the current render",
    export_go: "Choose location & export",
    bitrate: "Bitrate",
    // Affordance: the language the chip switches TO.
    language_label: "عربي",
    status_ready: "Engine {version} ready · pitch ±12 st / 1 ¢ · air ±dB / 0.1 dB · formant 130–190 mm",
    status_loaded: "{name} · {frames} frames @ {sr} Hz · median F0 {f0} Hz · voiced {voiced}%",
    status_track_failed: "pitch analysis failed ({error}) — waveform shown without F0 overlay",
    status_rendered: "preview rendered in {ms} ms ({stages})",
    status_rendering: "rendering…",
    status_render_failed: "render failed: {error}",
    status_live: "live preview · restart {us}",
    status_no_playback: "playback unavailable ({error}) — export still works",
    status_playback_failed: "playback failed: {error}",
    status_no_record: "recording unavailable ({error})",
    status_recording: "recording @ {rate} Hz{cap}",
    status_recorded: "recorded {secs} s @ {rate} Hz · {ch} ch{overflow}",
    status_capture_failed: "capture failed: {error}",
    status_exported: "exported {path} ({ms} ms)",
    status_export_failed: "export failed: {error}",
    status_import_failed: "import failed: {error}",
    status_dialog_failed: "dialog failed: {error}",
    track_original: "{name} — original",
    track_preview: "{name} — preview (rendered)",
    view_label: "view",
    neutral: "neutral",
    devices_title: "Audio Devices",
    devices_in: "Input (microphone)",
    devices_out: "Output (playback)",
    devices_test: "Test output",
    devices_refresh: "Refresh",
    devices_none: "No devices found",
    devices_close: "Close",
    status_input_selected: "input device → {name}{fallback}",
    status_output_selected: "output device → {name}{fallback}",
    device_fallback: " (fell back — requested device unavailable)",
    status_test_played: "output test tone played on {name}",
};

pub const AR: StrTable = StrTable {
    ui_font: "IBM Plex Sans Arabic",
    rtl: true,
    waveform_empty: "لا يوجد صوت محمَّل — استورد ملف WAV / MP3 أو سجِّل أو شغِّل --demo synth",
    pitch: "طبقة الصوت",
    air: "الهواء والنَفَس",
    formant: "الفورمانت",
    record: "تسجيل",
    export_title: "تصدير المعالجة الحالية",
    export_go: "اختر المكان وصدِّر",
    bitrate: "معدل البت",
    // Affordance: the language the chip switches TO.
    language_label: "EN",
    status_ready: "المحرّك {version} جاهز · طبقة ±12 نصف بُعد / 1 سنت · هواء ±dB / 0.1 dB · فورمانت 130–190 مم",
    status_loaded: "{name} · {frames} إطار @ {sr} Hz · الوسط F0 {f0} Hz · مُصوَّت {voiced}%",
    status_track_failed: "فشل تحليل الطبقة ({error}) — تُعرض الموجة دون طبقة F0",
    status_rendered: "المعاينة جاهزة خلال {ms} ms ({stages})",
    status_rendering: "جارٍ المعالجة…",
    status_render_failed: "فشلت المعالجة: {error}",
    status_live: "معاينة حيّة · إعادة التشغيل {us}",
    status_no_playback: "التشغيل غير متاح ({error}) — التصدير يعمل",
    status_playback_failed: "فشل التشغيل: {error}",
    status_no_record: "التسجيل غير متاح ({error})",
    status_recording: "جارٍ التسجيل @ {rate} Hz{cap}",
    status_recorded: "تم تسجيل {secs} ثانية @ {rate} Hz · {ch} قناة{overflow}",
    status_capture_failed: "فشل الالتقاط: {error}",
    status_exported: "تم التصدير إلى {path} ({ms} ms)",
    status_export_failed: "فشل التصدير: {error}",
    status_import_failed: "فشل الاستيراد: {error}",
    status_dialog_failed: "فشل مربع الحوار: {error}",
    track_original: "{name} — الأصلي",
    track_preview: "{name} — معاينة (معالَجة)",
    view_label: "عرض",
    neutral: "محايد",
    devices_title: "الأجهزة الصوتية",
    devices_in: "المدخل (الميكروفون)",
    devices_out: "المخرج (التشغيل)",
    devices_test: "اختبار المخرج",
    devices_refresh: "تحديث",
    devices_none: "لا توجد أجهزة",
    devices_close: "إغلاق",
    status_input_selected: "جهاز المدخل ← {name}{fallback}",
    status_output_selected: "جهاز المخرج ← {name}{fallback}",
    device_fallback: " (تحوّل تلقائي — الجهاز المطلوب غير متاح)",
    status_test_played: "تم تشغيل نغمة الاختبار على {name}",
};

pub const AR_RECORD_CAP: &str = " (الجهاز محدود — 192 kHz غير متاح)";
pub const AR_OVERFLOW: &str = " · فائض: سُقط جزء من المدخل";
pub const EN_RECORD_CAP: &str = " (device capped — 192 kHz unavailable)";
pub const EN_OVERFLOW: &str = " · OVERFLOW: some input was dropped";

/// Stage names for the render report.
pub fn stages(lang: Lang) -> [&'static str; 3] {
    match lang {
        Lang::En => ["pitch", "formant", "air"],
        Lang::Ar => ["الطبقة", "الفورمانت", "الهواء"],
    }
}

/// Recording-status suffix shown when the device refused 192 kHz.
pub fn record_cap(lang: Lang) -> &'static str {
    match lang {
        Lang::En => EN_RECORD_CAP,
        Lang::Ar => AR_RECORD_CAP,
    }
}

/// Recording-status suffix shown when the capture ring overflowed.
pub fn overflow(lang: Lang) -> &'static str {
    match lang {
        Lang::En => EN_OVERFLOW,
        Lang::Ar => AR_OVERFLOW,
    }
}

/// Wraps a technical/numeric fragment in LTR marks so bidi never reorders
/// it inside Arabic text (a visible no-op inside LTR text).
pub fn isolate_ltr(s: &str) -> String {
    format!("{LRM}{s}{LRM}")
}

/// Table lookup.
pub fn table(lang: Lang) -> StrTable {
    match lang {
        Lang::En => EN,
        Lang::Ar => AR,
    }
}

/// Minimal `{key}` template fill (values are pre-formatted strings).
pub fn tpl(template: &str, values: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (key, value) in values {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_strings_present_in_both_languages() {
        for lang in [Lang::En, Lang::Ar] {
            let t = table(lang);
            assert!(!t.waveform_empty.is_empty());
            assert!(!t.pitch.is_empty());
            assert!(!t.air.is_empty());
            assert!(!t.formant.is_empty());
            assert!(!t.record.is_empty());
            assert!(!t.export_title.is_empty());
            assert!(!t.export_go.is_empty());
            assert!(!t.bitrate.is_empty());
            assert!(!t.language_label.is_empty());
            assert!(!t.status_ready.is_empty());
            assert!(t.status_track_failed.contains("{error}"));
            assert!(t.status_render_failed.contains("{error}"));
            assert!(t.status_dialog_failed.contains("{error}"));
            assert!(t.status_playback_failed.contains("{error}"));
            assert!(t.status_capture_failed.contains("{error}"));
            assert!(t.status_loaded.contains("{frames}"));
            assert!(t.track_original.contains("{name}"));
            assert!(t.track_preview.contains("{name}"));
            assert!(!t.view_label.is_empty());
            assert!(!t.neutral.is_empty());
            // Devices dialog (7.1): every label exists in both tables;
            // the selection templates name their device placeholder.
            assert!(!t.devices_title.is_empty());
            assert!(!t.devices_in.is_empty());
            assert!(!t.devices_out.is_empty());
            assert!(!t.devices_test.is_empty());
            assert!(!t.devices_refresh.is_empty());
            assert!(!t.devices_none.is_empty());
            assert!(!t.devices_close.is_empty());
            assert!(t.status_input_selected.contains("{name}"));
            assert!(t.status_output_selected.contains("{name}"));
            assert!(t.status_test_played.contains("{name}"));
            assert!(!t.device_fallback.is_empty());
        }
        // Compile-time-checked invariants (clippy wants const blocks for
        // constant assertions — they are constants on purpose).
        const { assert!(!EN.rtl, "EN must be LTR") };
        const { assert!(AR.rtl, "AR must be RTL") };
        assert_eq!(EN.ui_font, "Inter");
        assert_eq!(AR.ui_font, "IBM Plex Sans Arabic");
        // The chip affords the language it switches to.
        assert_eq!(EN.language_label, "عربي");
        assert_eq!(AR.language_label, "EN");
        // Engineering suffixes exist per language.
        assert!(record_cap(Lang::En).contains("192 kHz"));
        assert!(record_cap(Lang::Ar).contains("192 kHz"));
        assert!(overflow(Lang::En).contains("OVERFLOW"));
        assert!(!overflow(Lang::Ar).is_empty());
    }

    #[test]
    fn templates_fill_completely() {
        let filled = tpl(
            EN.status_loaded,
            &[
                ("name", "take-01".into()),
                ("frames", "48000".into()),
                ("sr", "48000".into()),
                ("f0", "220.0".into()),
                ("voiced", "87".into()),
            ],
        );
        assert!(!filled.contains('{'));
        assert!(filled.contains("take-01"));
        let ar = tpl(
            AR.status_recorded,
            &[
                ("secs", "1.2".into()),
                ("rate", "48000".into()),
                ("ch", "2".into()),
                ("overflow", String::new()),
            ],
        );
        assert!(!ar.contains('{'));
        assert!(ar.contains("48000"));
        // LTR isolation wraps the fragment on both sides.
        assert_eq!(isolate_ltr("2.50 s"), format!("{LRM}2.50 s{LRM}"));
    }

    #[test]
    fn lang_parse() {
        assert_eq!("en".parse::<Lang>().expect("en"), Lang::En);
        assert_eq!("AR".parse::<Lang>().expect("ar"), Lang::Ar);
        assert!("fr".parse::<Lang>().is_err());
    }
}
