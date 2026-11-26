//! Color arithmetic, interpolation, distance metrics, and adjustments.
use graticule_canvas::Color;

/// have no defined RGB to compute with.
///
/// Every derivation that has to *measure* a palette — the pane headroom clamp, the
/// marker's neutrality, the swatch-staging band — goes through this rather than
/// [`rgb`]'s placeholder greys, because a `Reset` surface has no channels and
/// pretending it does silently fabricates headroom that is not there.
pub(crate) fn rgb_opt(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

pub(crate) fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        _ => (128, 128, 128),
    }
}

pub fn mix(a: Color, b: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let channel =
        |left: u8, right: u8| (left as f32 + (right as f32 - left as f32) * amount).round() as u8;
    Color::Rgb(channel(ar, br), channel(ag, bg), channel(ab, bb))
}

/// Weighted RGB distance between two colors (the classic "redmean" metric):
/// cheap, and perceptually closer than plain Euclidean RGB, which matters when
/// deciding whether a highlight band would visibly separate from a palette
/// chip or fuse into it. `Color::Reset` (the terminal default) has no defined
/// RGB, so it is treated as maximally distant — a chip we cannot measure is
/// never the reason a band gets rejected.
pub(crate) fn color_distance(a: Color, b: Color) -> f32 {
    if a == Color::Reset || b == Color::Reset {
        return f32::MAX;
    }
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let (dr, dg, db) = (
        ar as f32 - br as f32,
        ag as f32 - bg as f32,
        ab as f32 - bb as f32,
    );
    let rmean = (ar as f32 + br as f32) / 2.0;
    let weight_r = 2.0 + rmean / 256.0;
    let weight_g = 4.0;
    let weight_b = 2.0 + (255.0 - rmean) / 256.0;
    (weight_r * dr * dr + weight_g * dg * dg + weight_b * db * db).sqrt()
}

/// The smallest [`color_distance`] between `band` and every `swatch`. Used to
/// choose the highlight that leaves the advertised palette chips most legible.
pub(crate) fn min_swatch_distance(band: Color, swatches: &[Color]) -> f32 {
    swatches
        .iter()
        .map(|swatch| color_distance(band, *swatch))
        .fold(f32::MAX, f32::min)
}

/// Pull `color` toward the neutral grey of the same mean channel value by
/// `amount` (0.0 = unchanged, 1.0 = fully grey).
///
/// The transcript's keyboard cues must read as *marks*, never as states: every
/// tone a renderer picks for a row carries meaning (disclosure luminance,
/// lifecycle hue), so a cue that borrowed even a little chroma would start to
/// look like one of them. Stripping the chroma makes the marker legible on
/// light and dark palettes alike without claiming a semantic hue.
pub(crate) fn desaturate(color: Color, amount: f32) -> Color {
    let (r, g, b) = rgb(color);
    let grey = (r as f32 + g as f32 + b as f32) / 3.0;
    let amount = amount.clamp(0.0, 1.0);
    let channel = |c: u8| (c as f32 + (grey - c as f32) * amount).round() as u8;
    Color::Rgb(channel(r), channel(g), channel(b))
}

pub(crate) fn luminance(color: Color) -> f32 {
    let (r, g, b) = rgb(color);
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

/// IEC 61966-2-1 sRGB relative luminance in `[0.0, 1.0]` for WCAG 2.1 contrast math.
pub fn srgb_relative_luminance(color: Color) -> f32 {
    let linearize = |channel: u8| -> f32 {
        let c = channel as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = rgb(color);
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

/// WCAG 2.1 contrast ratio in `[1.0, 21.0]` between two colors (ADR-0258).
pub fn wcag_contrast_ratio(a: Color, b: Color) -> f32 {
    let la = srgb_relative_luminance(a);
    let lb = srgb_relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Convert a color to `(hue_degrees [0..360), saturation [0..1], lightness [0..1])`.
pub fn rgb_to_hsl(color: Color) -> (f32, f32, f32) {
    let (r, g, b) = rgb(color);
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let mut h = if (max - rf).abs() <= f32::EPSILON {
        ((gf - bf) / d + if gf < bf { 6.0 } else { 0.0 }) * 60.0
    } else if (max - gf).abs() <= f32::EPSILON {
        ((bf - rf) / d + 2.0) * 60.0
    } else {
        ((rf - gf) / d + 4.0) * 60.0
    };
    h = h.rem_euclid(360.0);
    (h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0))
}

/// HSL saturation in `[0.0, 1.0]`.
pub fn hsl_saturation(color: Color) -> f32 {
    rgb_to_hsl(color).1
}

/// Convert `(hue_degrees, saturation [0..1], lightness [0..1])` to `Color::Rgb`.
pub fn hsl_to_rgb(hue_deg: f32, saturation: f32, lightness: f32) -> Color {
    let h = hue_deg.rem_euclid(360.0) / 360.0;
    let s = saturation.clamp(0.0, 1.0);
    let l = lightness.clamp(0.0, 1.0);
    if s <= f32::EPSILON {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return Color::Rgb(v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let hue_to_rgb = |mut t: f32| -> f32 {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    let r = (hue_to_rgb(h + 1.0 / 3.0) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    let g = (hue_to_rgb(h) * 255.0).round().clamp(0.0, 255.0) as u8;
    let b = (hue_to_rgb(h - 1.0 / 3.0) * 255.0)
        .round()
        .clamp(0.0, 255.0) as u8;
    Color::Rgb(r, g, b)
}

/// Derive a palette-native, chroma-bounded, WCAG-AA legible in-flight (`running`)
/// color when a custom palette omits an explicit `running` override (ADR-0258
/// `[INV-TUI-COLOR-03]`).
///
/// The algorithm respects the palette's own saturation discipline, stays within
/// the calm cool/indigo/violet/teal sector (away from warning amber and error
/// red), enforces WCAG 2.1 contrast (`>= 4.5:1` across `background`, `panel`,
/// and `raised` whenever the canvas permits), and maximizes minimum perceptual
/// (`redmean`) separation from structural and outcome tokens.
#[allow(clippy::too_many_arguments)]
pub(crate) fn derive_running_color(
    background: Color,
    panel: Color,
    raised: Color,
    muted: Color,
    accent: Color,
    info: Color,
    success: Color,
    warning: Color,
    error: Color,
) -> Color {
    let light = luminance(background) > 150.0;
    let palette_sats = [accent, success, warning, error].map(hsl_saturation);
    let mean_sat = palette_sats.iter().sum::<f32>() / palette_sats.len() as f32;
    let max_sat = palette_sats.iter().copied().fold(0.0_f32, f32::max);
    // Stay inside the palette's chroma discipline while keeping enough tint to
    // separate cleanly from neutral `muted` text.
    let target_sat = mean_sat.clamp(0.28, max_sat.clamp(0.30, 0.56));
    let base_lightness = if light { 0.42 } else { 0.70 };
    let peers = [info, accent, success, warning, error, muted];
    let surfaces = [background, panel, raised];

    // Candidate hues prioritize the calm active sector (slate-blue / indigo /
    // twilight violet / mineral teal), avoiding warning amber and error red.
    const CANDIDATE_HUES: [f32; 13] = [
        235.0, 255.0, 220.0, 270.0, 200.0, 285.0, 190.0, 300.0, 245.0, 210.0, 265.0, 180.0, 310.0,
    ];

    let min_surface_contrast = |c: Color| -> f32 {
        surfaces
            .iter()
            .map(|&bg| wcag_contrast_ratio(c, bg))
            .fold(f32::MAX, f32::min)
    };

    let mut best_color = hsl_to_rgb(235.0, target_sat, base_lightness);
    let mut best_score = f32::NEG_INFINITY;

    for hue in CANDIDATE_HUES {
        let mut l = base_lightness;
        let mut candidate = hsl_to_rgb(hue, target_sat, l);
        // Nudge lightness toward the contrasting pole until WCAG AA (4.5:1) is
        // met across `background`, `panel`, and `raised`.
        for _ in 0..20 {
            if min_surface_contrast(candidate) >= 4.5 {
                break;
            }
            l = if light {
                (l - 0.025).max(0.14)
            } else {
                (l + 0.025).min(0.90)
            };
            candidate = hsl_to_rgb(hue, target_sat, l);
        }
        let min_dist = peers
            .iter()
            .map(|peer| color_distance(candidate, *peer))
            .fold(f32::MAX, f32::min);
        let contrast = min_surface_contrast(candidate);
        // Heavily reward achieving WCAG AA across all three surfaces; among
        // passing candidates, pick maximum separation from all peers.
        let score = if contrast >= 4.5 {
            1000.0 + min_dist
        } else {
            contrast * 100.0 + min_dist
        };
        if score > best_score {
            best_score = score;
            best_color = candidate;
        }
    }

    best_color
}
