//! Shared helpers and liveness calculations for chrome components.

use nuotc::Color;
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::render::Theme;

/// Number of distinct luminance steps in one breathing cycle. At the 100ms
/// spinner tick this is ~1.2s per cycle — calm, not frantic.
pub const SPINNER_PHASES: usize = 12;

/// The activity indicator glyph: delegates to the active typography glyph set.
#[allow(dead_code)]
pub fn spinner_glyph(glyphs: &nuotc::GlyphSet) -> &'static str {
    glyphs.dot
}

/// Cosine luminance sweep between `bg` (dim, at phase 0) and `base` (bright,
/// at mid-cycle).
pub fn breathing_color(phase: usize, base: Color, bg: Color) -> Color {
    let (br, bgc, bb) = rgb_of(bg);
    let (fr, fgc, fb) = rgb_of(base);
    let n = SPINNER_PHASES as f32;
    let t = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (phase % SPINNER_PHASES) as f32 / n).cos();
    Color::Rgb(lerp_u8(br, fr, t), lerp_u8(bgc, fgc, t), lerp_u8(bb, fb, t))
}

/// Which mechanism drives the activity dot this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Liveness {
    Breathing,
    Gated,
}

/// Classify the frame's dot mechanism. Gate wins over everything.
pub fn classify_liveness(awaiting_permission: bool) -> Liveness {
    if awaiting_permission {
        Liveness::Gated
    } else {
        Liveness::Breathing
    }
}

pub fn dot_color(liveness: Liveness, spinner_phase: usize, theme: &Theme) -> Color {
    match liveness {
        Liveness::Gated => theme.warning,
        Liveness::Breathing => breathing_color(spinner_phase, theme.brand(), theme.surface()),
    }
}

pub(crate) fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (119, 125, 117),
    }
}

pub(crate) fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Format duration progressively: `45s`, `2m 15s`, `1h 05m`.
pub fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s:02}s")
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h {m:02}m")
    }
}

/// Abbreviate an absolute path to its native `~`-relative form.
pub fn tilde_home(path: &std::path::Path) -> String {
    crate::components::path::tilde_shorten(path)
}

/// Truncate `s` to at most `max` display cells, appending `…` when cut.
pub(crate) fn truncate_for_bar(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else if max == 1 {
        "…".to_string()
    } else {
        let mut used = 0usize;
        let mut head = String::new();
        for ch in s.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + width > max - 1 {
                break;
            }
            head.push(ch);
            used += width;
        }
        format!("{head}…")
    }
}
