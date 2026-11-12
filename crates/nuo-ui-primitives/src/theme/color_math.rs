//! Color arithmetic, interpolation, distance metrics, and adjustments.
use nuotc::Color;

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
