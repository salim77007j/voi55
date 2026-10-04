//! Waveform rendering engine — per-pixel peak/RMS columns with a decimated
//! min/max pyramid for instant redraw at any zoom depth (D13).
//!
//! Zoom domain: the view span is expressed in *frames* (f64), so sub-
//! millisecond windows ("sub-millisecond zoom", brief §A) work at any
//! sample rate. Columns finer than the pyramid's level-0 bin read the
//! samples directly (interpolated), columns coarser read pyramid bins.
//!
//! Voiced regions are tinted by pitch confidence and the F0 contour rides
//! on top, both from the shared pYIN track (D7) — the same analysis the
//! engines use, so what the user sees is what the engine acts on.

/// One drawn column of the waveform view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    /// Peak low / high over the column's frame span.
    pub min: f32,
    pub max: f32,
    /// RMS over the column's frame span (0 when the span is a single
    /// interpolated sample).
    pub rms: f32,
    /// Voicing presence 0..=1 (max voiced probability in the span).
    pub voiced: f32,
    /// Mean F0 over voiced frames in the span (0.0 = none).
    pub f0: f32,
}

/// Decimated min/max/power pyramid. Level 0 holds bins of [`Self::bin`]
/// frames; each level up halves the resolution (min of mins, max of maxes,
/// mean of powers).
pub struct WaveformPyramid {
    frames: usize,
    bin: usize,
    /// levels[0] is the finest. `levels[l].min[i]` covers frames
    /// `i·(bin<<l) .. (i+1)·(bin<<l)` (last bin may be short).
    levels: Vec<Level>,
}

struct Level {
    min: Vec<f32>,
    max: Vec<f32>,
    /// Mean square per bin (so levels average correctly).
    power: Vec<f32>,
}

impl WaveformPyramid {
    /// Builds the pyramid over a mono display signal.
    ///
    /// The level-0 bin adapts so the finest level stays ≤ ~2 M bins
    /// (≈ 25 MB) even for 10-minute 192 kHz captures; ≥ 16 frames
    /// otherwise.
    pub fn build(mono: &[f32], sample_rate: u32) -> Self {
        let _ = sample_rate;
        let frames = mono.len();
        let mut bin = 16usize;
        while frames / bin > 2_000_000 {
            bin *= 2;
        }
        let mut levels = Vec::new();
        let mut min: Vec<f32> = Vec::with_capacity(frames.div_ceil(bin));
        let mut max: Vec<f32> = Vec::with_capacity(frames.div_ceil(bin));
        let mut power: Vec<f32> = Vec::with_capacity(frames.div_ceil(bin));
        for chunk in mono.chunks(bin) {
            let (mut lo, mut hi, mut sq) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f64);
            for &s in chunk {
                lo = lo.min(s);
                hi = hi.max(s);
                sq += f64::from(s) * f64::from(s);
            }
            if chunk.is_empty() {
                lo = 0.0;
                hi = 0.0;
            }
            min.push(lo);
            max.push(hi);
            power.push((sq / chunk.len() as f64) as f32);
        }
        levels.push(Level { min, max, power });

        // Decimate until one bin covers everything.
        while levels.last().is_some_and(|l| l.min.len() > 1) {
            let prev = levels.last().expect("just checked");
            let half = prev.min.len().div_ceil(2);
            let mut mn = Vec::with_capacity(half);
            let mut mx = Vec::with_capacity(half);
            let mut pw = Vec::with_capacity(half);
            for i in 0..half {
                let j = i * 2;
                let take = 2.min(prev.min.len() - j);
                let lo = prev.min[j..j + take]
                    .iter()
                    .copied()
                    .fold(f32::INFINITY, f32::min);
                let hi = prev.max[j..j + take]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                let p = prev.power[j..j + take].iter().sum::<f32>() / take as f32;
                mn.push(lo);
                mx.push(hi);
                pw.push(p);
            }
            levels.push(Level {
                min: mn,
                max: mx,
                power: pw,
            });
        }

        Self {
            frames,
            bin,
            levels,
        }
    }

    /// Frames per level-0 bin.
    pub fn bin(&self) -> usize {
        self.bin
    }

    /// Total frames the pyramid was built from.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

/// A view window over the signal, in frames (f64 for sub-sample zoom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub start_frame: f64,
    pub span_frames: f64,
}

impl View {
    /// Full-signal view.
    pub fn fitting(frames: usize) -> Self {
        Self {
            start_frame: 0.0,
            span_frames: frames.max(1) as f64,
        }
    }

    /// Minimum zoom-in span: 16 frames (≈ 0.33 ms at 48 kHz) keeps the
    /// view inside the direct-sample path below the pyramid.
    pub const MIN_SPAN_FRAMES: f64 = 16.0;

    /// Zooms by `factor` (> 1 = in) around a fractional anchor, clamped to
    /// the signal.
    pub fn zoom_at(&mut self, anchor_frac: f64, factor: f64, total_frames: usize) {
        let total = total_frames.max(1) as f64;
        let new_span =
            (self.span_frames / factor.max(f64::MIN_POSITIVE)).clamp(Self::MIN_SPAN_FRAMES, total);
        let anchor = self.start_frame + anchor_frac.clamp(0.0, 1.0) * self.span_frames;
        self.start_frame = (anchor - anchor_frac * new_span).clamp(0.0, total - new_span);
        self.span_frames = new_span;
    }

    /// Pans by a fraction of the current span (positive = later), clamped.
    pub fn pan(&mut self, delta_frac: f64, total_frames: usize) {
        let total = total_frames.max(1) as f64;
        let max_start = (total - self.span_frames).max(0.0);
        self.start_frame = (self.start_frame + delta_frac * self.span_frames).clamp(0.0, max_start);
    }
}

/// Aggregates the frame range `[f0, f1)` from the finest pyramid level
/// that covers it with at most a few bins.
fn aggregate(pyramid: &WaveformPyramid, mono: &[f32], f0: usize, f1: usize) -> (f32, f32, f32) {
    let span = f1 - f0;
    // Choose level: the level whose bin is ≤ span/2 (so ≤ ~3 bins per column).
    let mut level = 0usize;
    while level + 1 < pyramid.levels.len() && pyramid.bin << (level + 1) <= span / 2 {
        level += 1;
    }
    let bin = pyramid.bin << level;
    let lv = &pyramid.levels[level];
    let b0 = f0 / bin;
    let b1 = (f1 - 1) / bin; // inclusive
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut power = 0.0f64;
    let mut count = 0usize;
    for b in b0..=b1 {
        if let Some(&lo_b) = lv.min.get(b) {
            lo = lo.min(lo_b);
            hi = hi.max(lv.max[b]);
            power += f64::from(lv.power[b]);
            count += 1;
        }
    }
    if count == 0 {
        // Fall back to direct scan (short signals / out-of-range views).
        let seg = &mono[f0.min(mono.len())..f1.min(mono.len())];
        if seg.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut l = f32::INFINITY;
        let mut h = f32::NEG_INFINITY;
        let mut sq = 0.0f64;
        for &s in seg {
            l = l.min(s);
            h = h.max(s);
            sq += f64::from(s) * f64::from(s);
        }
        let rms = (sq / seg.len() as f64).sqrt() as f32;
        return (l, h, rms);
    }
    (lo, hi, (power / count as f64).sqrt() as f32)
}

/// Linear interpolation of `mono` at a fractional frame (edge-clamped).
fn sample_interp(mono: &[f32], pos: f64) -> f32 {
    if mono.is_empty() {
        return 0.0;
    }
    let clamped = pos.clamp(0.0, (mono.len() - 1) as f64);
    let i = clamped.floor() as usize;
    let frac = (clamped - i as f64) as f32;
    let a = mono[i];
    let b = mono.get(i + 1).copied().unwrap_or(a);
    a + (b - a) * frac
}

/// Voicing/F0 aggregation for a frame span from the pYIN track.
fn track_span(
    track: Option<&mvl_core::pyin::PyinResult>,
    f0_frame: usize,
    f1_frame: usize,
) -> (f32, f32) {
    let Some(track) = track else {
        return (0.0, 0.0);
    };
    if track.is_empty() {
        return (0.0, 0.0);
    }
    let hop = track.hop.max(1) as f64;
    let first = (f0_frame as f64 / hop).floor() as usize;
    let last = ((f1_frame as f64) / hop).ceil() as usize;
    let mut voiced = 0.0f32;
    let mut f0_sum = 0.0f64;
    let mut f0_n = 0usize;
    for i in first..=last.min(track.len() - 1) {
        voiced = voiced.max(track.voiced_prob[i]);
        let f = track.f0[i];
        if track.voiced[i] && f > 0.0 {
            f0_sum += f64::from(f);
            f0_n += 1;
        }
    }
    (
        voiced,
        if f0_n == 0 {
            0.0
        } else {
            (f0_sum / f0_n as f64) as f32
        },
    )
}

/// Extracts `width_px` columns for the view.
pub fn columns(
    pyramid: &WaveformPyramid,
    mono: &[f32],
    track: Option<&mvl_core::pyin::PyinResult>,
    view: &View,
    width_px: usize,
) -> Vec<Column> {
    let mut out = Vec::with_capacity(width_px.max(1));
    if width_px == 0 || mono.is_empty() {
        return out;
    }
    let total = pyramid.frames.max(1) as f64;
    let start = view.start_frame.clamp(0.0, total);
    let span = view.span_frames.clamp(1.0, total);
    for x in 0..width_px {
        let f0 = start + x as f64 * span / width_px as f64;
        let f1 = start + (x + 1) as f64 * span / width_px as f64;
        let i0 = f0.floor() as usize;
        let i1 = (f1.ceil() as usize).max(i0 + 1);
        let (min, max, rms) = if span / width_px as f64 >= 2.0 {
            aggregate(
                pyramid,
                mono,
                i0.min(mono.len()),
                i1.min(mono.len()).max(i0 + 1),
            )
        } else {
            // Direct path (deep zoom): min/max over the real sample range
            // so adjacent columns connect; below one sample per column,
            // interpolate.
            if i1 - i0 >= 2 {
                let a = i0.min(mono.len());
                let b = (i1).min(mono.len()).max(a + 1);
                let seg = &mono[a..b];
                let lo = seg.iter().copied().fold(f32::INFINITY, f32::min);
                let hi = seg.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let sq: f64 = seg.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
                (lo, hi, (sq / seg.len() as f64).sqrt() as f32)
            } else {
                let v = sample_interp(mono, (f0 + f1) * 0.5);
                (v, v, 0.0)
            }
        };
        let (voiced, f0hz) = track_span(track, i0, i1);
        out.push(Column {
            min,
            max,
            rms,
            voiced,
            f0: f0hz,
        });
    }
    out
}

/// Palette entries used by the rasterizer (straight, non-premultiplied).
pub struct WaveformColors {
    /// Peak envelope (D13: accent/pitch).
    pub peak: [u8; 4],
    /// RMS core.
    pub core: [u8; 4],
    /// Voiced-region tint (D13: accent/air).
    pub voiced: [u8; 4],
    /// F0 trace (accent/pitch).
    pub trace: [u8; 4],
    /// Center hairline (border/subtle).
    pub center: [u8; 4],
}

/// Studio Graphite defaults (D13) with alpha channels tuned for overlay.
pub const STUDIO_COLORS: WaveformColors = WaveformColors {
    peak: [0x5A, 0xA7, 0xFF, 0x64],
    core: [0xE7, 0xEB, 0xF0, 0xD9],
    voiced: [0x46, 0xD6, 0xA5, 0x1C],
    trace: [0x5A, 0xA7, 0xFF, 0xE6],
    center: [0x2C, 0x34, 0x40, 0xFF],
};

#[inline]
fn blend(dst: &mut [u8], color: [u8; 4]) {
    // Straight-alpha over existing straight-alpha pixel.
    let da = dst[3] as u32;
    let sa = color[3] as u32;
    let out_a = sa + da * (255 - sa) / 255;
    for c in 0..3 {
        let dc = dst[c] as u32;
        let sc = color[c] as u32;
        // out_a == 0 only when both alphas are 0: the channel value is
        // arbitrary then; 0 keeps the buffer neutral.
        let num = sc * sa + dc * da * (255 - sa) / 255;
        let v = num.checked_div(out_a).unwrap_or(0);
        dst[c] = v.min(255) as u8;
    }
    dst[3] = out_a.min(255) as u8;
}

fn rect(buf: &mut [u8], w: usize, x: usize, y0: usize, y1: usize, color: [u8; 4]) {
    if x >= w || y0 >= y1 {
        return;
    }
    for y in y0..y1.min(buf.len() / (4 * w)) {
        let off = (y * w + x) * 4;
        blend(&mut buf[off..off + 4], color);
    }
}

/// Rasterizes the columns into an RGBA8 buffer (`width_px × height_px`,
/// straight alpha). F0 maps to a log scale 60 Hz..1 kHz, drawn along the
/// full height (top = high).
pub fn draw(
    columns: &[Column],
    width_px: usize,
    height_px: usize,
    colors: &WaveformColors,
) -> Vec<u8> {
    let mut buf = vec![0u8; width_px.max(1) * height_px.max(1) * 4];
    if width_px == 0 || height_px == 0 {
        return buf;
    }
    let w = width_px;
    let h = height_px;
    let mid = h / 2;

    // Voiced tint under everything.
    for (x, c) in columns.iter().enumerate().take(w) {
        if c.voiced > 0.0 {
            let mut tint = colors.voiced;
            tint[3] = ((tint[3] as u32) * (c.voiced as u32)).min(255) as u8;
            rect(&mut buf, w, x, 0, h, tint);
        }
    }

    // Peak + core columns.
    for (x, c) in columns.iter().enumerate().take(w) {
        let top = ((1.0 - c.max.clamp(-1.0, 1.0)) * mid as f32) as usize;
        let bot = ((1.0 - c.min.clamp(-1.0, 1.0)) * mid as f32) as usize;
        rect(
            &mut buf,
            w,
            x,
            top.min(bot),
            bot.max(top).max(top + 1),
            colors.peak,
        );
        if c.rms > 0.0 {
            let r = (c.rms * mid as f32).max(1.0) as usize;
            rect(
                &mut buf,
                w,
                x,
                mid.saturating_sub(r),
                (mid + r).min(h),
                colors.core,
            );
        }
    }

    // Center hairline above the envelope.
    for x in 0..w {
        rect(&mut buf, w, x, mid, mid + 1, colors.center);
    }

    // F0 trace: connect consecutive voiced columns on the log scale.
    const F_LOW: f64 = 60.0;
    const F_HIGH: f64 = 1000.0;
    let log_lo = F_LOW.ln();
    let log_hi = F_HIGH.ln();
    let mut prev: Option<(usize, usize)> = None;
    for (x, c) in columns.iter().enumerate().take(w) {
        if c.f0 <= 0.0 {
            prev = None;
            continue;
        }
        let t = f64::from(c.f0).clamp(F_LOW, F_HIGH).ln();
        let y = ((1.0 - (t - log_lo) / (log_hi - log_lo)) * (h - 1) as f64) as usize;
        let y = y.min(h - 1);
        if let Some((px, py)) = prev {
            // Line between (px, py) and (x, y); s starts at 1 because the
            // starting pixel was painted by the previous column already.
            let steps = (x - px).max(py.abs_diff(y)).max(1);
            for s in 1..=steps {
                let xi = px + (x - px) * s / steps;
                let yi = if y > py {
                    py + (y - py) * s / steps
                } else {
                    py - (py - y) * s / steps
                };
                rect(&mut buf, w, xi, yi, (yi + 2).min(h), colors.trace);
            }
        } else {
            rect(&mut buf, w, x, y, (y + 2).min(h), colors.trace);
        }
        prev = Some((x, y));
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvl_core::synth::{self, Rng, VowelSpec};

    fn sine(freq: f32, sr: u32, secs: f32) -> Vec<f32> {
        (0..(sr as f32 * secs) as usize)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin() * 0.8)
            .collect()
    }

    #[test]
    fn pyramid_decimates_min_max_exactly() {
        let sr = 48_000u32;
        let x = sine(220.0, sr, 0.1); // 4800 frames
        let pyr = WaveformPyramid::build(&x, sr);
        assert_eq!(pyr.frames(), 4800);
        assert_eq!(pyr.bin(), 16);
        // Level 0 bins match a direct scan of the first bin.
        let l0 = &pyr.levels[0];
        let direct_min = x[..16].iter().copied().fold(f32::INFINITY, f32::min);
        let direct_max = x[..16].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(l0.min[0], direct_min);
        assert_eq!(l0.max[0], direct_max);
        // Top level covers everything.
        let top = pyr.levels.last().expect("top");
        assert_eq!(top.min.len(), 1);
        assert!(*top.min.last().expect("min") <= direct_min);
        assert!(*top.max.last().expect("max") >= direct_max);
    }

    #[test]
    fn coarse_columns_match_direct_scan() {
        let sr = 48_000u32;
        let x = sine(110.0, sr, 0.2);
        let pyr = WaveformPyramid::build(&x, sr);
        let view = View {
            start_frame: 0.0,
            span_frames: 4800.0,
        };
        let cols = columns(&pyr, &x, None, &view, 40);
        assert_eq!(cols.len(), 40);
        // Column 0 spans frames 0..120: direct min/max must equal it.
        let dmin = x[..120].iter().copied().fold(f32::INFINITY, f32::min);
        let dmax = x[..120].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (cols[0].min - dmin).abs() < 1e-4,
            "{} vs {dmin}",
            cols[0].min
        );
        assert!(
            (cols[0].max - dmax).abs() < 1e-4,
            "{} vs {dmax}",
            cols[0].max
        );
        // RMS stays within the peak envelope (rms is non-negative and
        // bounded by the larger absolute extreme).
        for c in &cols {
            let peak_abs = c.max.abs().max(c.min.abs());
            assert!(
                c.rms <= peak_abs + 1e-3,
                "rms {} vs peak abs {peak_abs}",
                c.rms
            );
        }
    }

    #[test]
    fn deep_zoom_reads_interpolated_samples() {
        let sr = 48_000u32;
        let x = sine(220.0, sr, 0.05);
        let pyr = WaveformPyramid::build(&x, sr);
        // 16-frame span across 64 px: finer than a bin → interpolated.
        let view = View {
            start_frame: 100.0,
            span_frames: 16.0,
        };
        let cols = columns(&pyr, &x, None, &view, 64);
        assert_eq!(cols.len(), 64);
        // Every column value must lie between the direct neighbours.
        for (i, c) in cols.iter().enumerate() {
            let pos = 100.0 + (i as f64 + 0.5) * 16.0 / 64.0;
            let v = sample_interp(&x, pos);
            assert!((c.max - v).abs() < 1e-3);
        }
    }

    #[test]
    fn zoom_clamps_and_anchors() {
        let pyr = WaveformPyramid::build(&sine(220.0, 48_000, 0.1), 48_000);
        let total = pyr.frames();
        let mut v = View::fitting(total);
        v.zoom_at(0.5, 2.0, total);
        assert!((v.span_frames - total as f64 / 2.0).abs() < 1.0);
        // Anchor stays put.
        assert!((v.start_frame + v.span_frames * 0.5 - total as f64 * 0.5).abs() < 1.0);
        // Zoom out clamps to full.
        v.zoom_at(0.0, 0.001, total);
        assert_eq!(v.span_frames, total as f64);
        // Zoom in clamps to MIN_SPAN.
        let mut w = View::fitting(total);
        w.zoom_at(0.5, 1e9, total);
        assert_eq!(w.span_frames, View::MIN_SPAN_FRAMES);
        // Pan clamps.
        let mut p = View {
            start_frame: 0.0,
            span_frames: total as f64 / 2.0,
        };
        p.pan(10.0, total);
        assert_eq!(p.start_frame, total as f64 / 2.0);
        p.pan(-10.0, total);
        assert_eq!(p.start_frame, 0.0);
    }

    #[test]
    fn voiced_tint_and_trace_come_from_the_track() {
        let sr = 48_000u32;
        let track_f0 = synth::vibrato_f0_track(160.0, 30.0, 5.0, sr as usize / 10, sr);
        let mut rng = Rng::new(7);
        let vowel = synth::vowel(&track_f0, sr, &VowelSpec::default(), &mut rng);
        let track = mvl_core::pyin::pyin(&vowel, sr).expect("pyin");
        let pyr = WaveformPyramid::build(&vowel, sr);
        let view = View::fitting(vowel.len());
        let cols = columns(&pyr, &vowel, Some(&track), &view, 200);
        let voiced_cols = cols.iter().filter(|c| c.voiced > 0.5).count();
        assert!(voiced_cols > 100, "vowel should be mostly voiced");
        let f0_cols = cols.iter().filter(|c| c.f0 > 140.0 && c.f0 < 180.0).count();
        assert!(f0_cols > 50, "F0 trace should track ~160 Hz");
    }

    #[test]
    fn draw_layers_are_exact_and_ordered() {
        let w = 40usize;
        let h = 60usize;
        let mid = h / 2;
        // Silent columns carrying only the F0 trace (160 Hz).
        let trace_cols: Vec<Column> = (0..w)
            .map(|_| Column {
                min: 0.0,
                max: 0.0,
                rms: 0.0,
                voiced: 0.0,
                f0: 160.0,
            })
            .collect();
        let buf = draw(&trace_cols, w, h, &STUDIO_COLORS);
        let px = |b: &Vec<u8>, x: usize, y: usize| -> [u8; 4] {
            let o = (y * w + x) * 4;
            b[o..o + 4].try_into().expect("slice")
        };
        // Center hairline is the opaque border color.
        assert_eq!(px(&buf, 10, mid), STUDIO_COLORS.center);
        // The trace maps 160 Hz onto the log scale away from the center and
        // is painted exactly (nothing else was drawn there).
        let t = 160f64.ln();
        let log_lo = 60f64.ln();
        let log_hi = 1000f64.ln();
        let yt = ((1.0 - (t - log_lo) / (log_hi - log_lo)) * (h - 1) as f64) as usize;
        // First column paints the trace once (exact); later columns may
        // double-paint at the seam between dot and connector, so they only
        // assert coverage.
        assert_eq!(px(&buf, 0, yt.min(h - 2)), STUDIO_COLORS.trace);
        assert!(px(&buf, 10, yt.min(h - 2))[3] >= STUDIO_COLORS.trace[3]);

        // Now with a full-amplitude voiced column: the peak rect is painted
        // over the tint (composite), the tint alone shows above the peak.
        let loud: Vec<Column> = (0..w)
            .map(|_| Column {
                min: -0.5,
                max: 0.5,
                rms: 0.2,
                voiced: 1.0,
                f0: 0.0,
            })
            .collect();
        let buf2 = draw(&loud, w, h, &STUDIO_COLORS);
        // Top edge (outside peak, inside tint) is translucent but present.
        assert!(px(&buf2, 10, 2)[3] > 0);
        // Peak paints over the tint (composite, so assert coverage), and
        // the center hairline paints last (exact color).
        let top = ((1.0 - 0.5) * mid as f32) as usize;
        assert!(px(&buf2, 20, top.max(1))[3] > STUDIO_COLORS.voiced[3]);
        assert_eq!(px(&buf2, 20, mid), STUDIO_COLORS.center);
    }
}
