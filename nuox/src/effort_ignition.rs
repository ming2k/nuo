//! The "effort ignition" celebration, ported from codex's TUI
//! (`codex-rs/tui/src/bottom_pane/effort_ignition*.rs`): when the model's
//! top reasoning tier — Kimi K3's `max`, codex's `ultra` — is selected, the
//! input box and its hint bar light up for ~1.3s.
//!
//! Three coordinated pieces, all driven by one wall-clock epoch
//! ([`crate::app::App::effort_ignition_epoch`]) so the animation cadence
//! is immune to the event loop's irregular wakeups:
//!
//! 1. [`paint_ignition_bands`] tints the composer panel's *background* with
//!    two right-sweeping warm wave crests, then lands a `✦` spark on the
//!    panel's top-right corner. Background-only painting means the user's
//!    draft text is never touched.
//! 2. [`max_label_spans`] renders the `M A X` label converging from
//!    edge-wide gaps to a tight center row on the hint bar (codex's
//!    "letters start wide at the edges and converge to the center"), with a
//!    staggered fade-in: center letters first, edge letters trailing.
//! 3. [`ignition_prompt_color`] gives the composer prompt a 150ms "charge"
//!    from the brand color toward a fire gradient while the wave is live. The
//!    prompt glyph itself never changes — it stays `›`; only its color
//!    carries the ignition, and it returns to the ordinary palette once the
//!    animation ends.
//!
//! All frame content derives from elapsed milliseconds (pure functions), so
//! tests assert exact phases instead of racing a ticker.

use nuotc::{Color, Frame, Modifier, Rect, Span, Style};

use super::Theme;

/// Total ignition duration. Matches codex's Ultra Wave (1300ms).
pub(crate) const IGNITION_TOTAL_MS: u128 = 1300;

/// Duration of the `M A X` label takeover on the hint bar.
const LABEL_TOTAL_MS: u128 = 1100;

/// The prompt's brand→fire "charge" ramp on ignition (codex: 150ms).
const CHARGE_MS: u128 = 150;

/// Half-width of one wave crest in columns (codex `WAVE_HALF_WIDTH`).
const WAVE_HALF_WIDTH: f32 = 9.0;

/// Two sweeps, staggered like codex's Ultra wave bands `(launch, travel,
/// strength)` — the second chase crest is what makes Ultra read richer than
/// a single pass.
const WAVE_BANDS: &[(f32, f32, f32)] = &[(0.10, 0.70, 1.0), (0.35, 0.55, 1.0)];

/// The spark fires only after the wave has landed (codex `SPARK_START`).
const SPARK_START_MS: u128 = 900;

/// `elapsed >= total` ⇒ the ignition is over and the epoch can be dropped.
pub(crate) fn ignition_finished(elapsed_ms: u128) -> bool {
    elapsed_ms >= IGNITION_TOTAL_MS
}

/// Resolve an RGB triple for any palette color (named colors approximate to
/// their xterm values, `Reset` to a muted mid-gray). The palette is RGB-first
/// so this is effectively free in production themes.
fn rgb_of(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        other => {
            let v = other.luminance();
            (v as u8, v as u8, v as u8)
        }
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// `Color::blend` with raw triples: `t = 0` → `base`, `t = 1` → `overlay`.
fn blend_rgb(overlay: (u8, u8, u8), base: (u8, u8, u8), t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::Rgb(
        lerp_u8(base.0, overlay.0, t),
        lerp_u8(base.1, overlay.1, t),
        lerp_u8(base.2, overlay.2, t),
    )
}

/// Cubic ease-in-out (codex's `ease_in_out`): the crest accelerates off the
/// left edge and decelerates into the right, which is what makes the sweep
/// feel physical rather than linear.
fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Cosine bell profile: 1 at the crest center, 0 at `distance = 1`.
fn crest(distance: f32) -> f32 {
    if distance >= 1.0 {
        0.0
    } else {
        0.5 * (1.0 + (std::f32::consts::PI * distance).cos())
    }
}

/// Fire gradient along the sweep: ember red at the leading edge, warm amber
/// in the middle, pale gold at the trailing edge. Kimi's `max` tier takes a
/// warm ramp (codex's `Max` gold-orange family); a violet spark closes it.
fn fire_rgb(position: f32) -> (u8, u8, u8) {
    let t = position.clamp(0.0, 1.0);
    let (a, b, c) = ((255, 120, 60), (255, 178, 66), (255, 214, 120));
    if t < 0.5 {
        let u = t * 2.0;
        (
            lerp_u8(a.0, b.0, u),
            lerp_u8(a.1, b.1, u),
            lerp_u8(a.2, b.2, u),
        )
    } else {
        let u = t * 2.0 - 1.0;
        (
            lerp_u8(b.0, c.0, u),
            lerp_u8(b.1, c.1, u),
            lerp_u8(b.2, c.2, u),
        )
    }
}

/// The violet accent the spark and the charged prompt share (codex's Ultra
/// accent family — purple/pink reads as "top tier" against the warm sweep).
const SPARK_RGB: (u8, u8, u8) = (216, 180, 254);

/// Where a column's fire gradient position comes from: `column / width`.
fn column_rgb(column: usize, width: usize) -> (u8, u8, u8) {
    fire_rgb(column as f32 / width.max(1) as f32)
}

/// Sample one wave band at `column`. Returns the crest intensity in
/// `[0, 1]`; the hue is positional (see [`column_rgb`]), so the band only
/// modulates intensity.
fn wave_band_intensity(
    elapsed_ms: u128,
    band: (f32, f32, f32),
    column: usize,
    width: usize,
) -> f32 {
    let (launch, travel, strength) = band;
    let total = IGNITION_TOTAL_MS as f32;
    let progress = ((elapsed_ms as f32 / total) - launch) / travel;
    if !(0.0..=1.0).contains(&progress) {
        return 0.0;
    }
    let width_f = width as f32;
    let center = ease_in_out(progress) * (width_f + 2.0 * WAVE_HALF_WIDTH) - WAVE_HALF_WIDTH;
    let distance = (column as f32 - center).abs() / WAVE_HALF_WIDTH;
    crest(distance) * strength
}

/// Combined intensity of all bands at `column` (max wins, not additive —
/// overlapping crests brighten but never overshoot).
fn ignition_intensity(elapsed_ms: u128, column: usize, width: usize) -> f32 {
    WAVE_BANDS
        .iter()
        .map(|&band| wave_band_intensity(elapsed_ms, band, column, width))
        .fold(0.0_f32, f32::max)
}

/// The spark glyph for the tail of the ignition, codex's
/// `· → ✦ → ✧` sequence at 100ms per frame. `None` outside the window.
pub(crate) fn spark_glyph(elapsed_ms: u128) -> Option<&'static str> {
    if elapsed_ms < SPARK_START_MS {
        return None;
    }
    match (elapsed_ms - SPARK_START_MS) / 100 {
        0 => Some("·"),
        1 => Some("✦"),
        2 => Some("✧"),
        _ => None,
    }
}

/// Tint `area`'s existing background cells in place with the ignition waves.
///
/// This is the codex `Canvas::tint` idea adapted to muta's retained grid:
/// the composer has already painted its panel this frame, so we walk the
/// back grid and *re-color* each cell's `bg` by blending its current value
/// toward the fire hue — text glyphs and foregrounds are never touched, so
/// the user's draft rides the glow untouched. Blank cells additionally take
/// a faint foreground ember so the tint reads on terminals whose block
/// elements dither.
///
/// Returns `true` when anything was painted (empty areas return `false`, so
/// callers can treat "nothing visible" as "animation over").
pub(crate) fn paint_ignition_bands(
    frame: &mut Frame,
    area: Rect,
    hint_row: Option<u16>,
    elapsed_ms: u128,
) -> bool {
    if area.width == 0 || area.height == 0 || ignition_finished(elapsed_ms) {
        return false;
    }
    let width = area.width as usize;
    let grid = frame.buffer_mut();
    let mut painted = false;
    for row in 0..area.height {
        let y = area.y + row;
        for col in 0..width {
            let intensity = ignition_intensity(elapsed_ms, col, width);
            if intensity < 0.02 {
                continue;
            }
            let x = area.x + col as u16;
            let Some(cell) = grid.get(x, y).cloned() else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let hue = column_rgb(col, width);
            let base = rgb_of(cell.bg());
            // Cap the alpha the way codex does (0.6) so the panel's own hue
            // stays legible under the glow.
            let alpha = (intensity * 0.55).min(0.5);
            let tinted = blend_rgb(hue, base, alpha);
            // Count a paint only when the blend visibly changes the cell —
            // a sub-1/255 rounding of a low-intensity tail is not a frame.
            if tinted == cell.bg() {
                continue;
            }
            let mut next = cell;
            next.set_bg(tinted);
            grid.set(x, y, next);
            painted = true;
        }
    }

    // The spark lands on the composer's top edge, right-of-center — the
    // landing site codex uses (`width - 2`), violet so it cuts through the
    // warm field. Written only onto a blank cell so it never clobbers text.
    if let Some(glyph) = spark_glyph(elapsed_ms) {
        let x = area.x + area.width.saturating_sub(2);
        let y = area.y;
        if let Some(cell) = grid.get(x, y).cloned()
            && cell.symbol().trim().is_empty()
        {
            let spark_fg = blend_rgb(SPARK_RGB, rgb_of(cell.bg()), 0.92);
            let mut next = cell;
            next.set_symbol(glyph);
            next.set_fg(spark_fg);
            next.style.add |= Modifier::BOLD;
            grid.set(x, y, next);
        }
    }

    // The hint bar keeps showing its content; it only borrows the wave tint
    // so the whole footer block ignites as one surface.
    if let Some(y) = hint_row {
        for col in 0..width {
            let intensity = ignition_intensity(elapsed_ms, col, width) * 0.7;
            if intensity < 0.02 {
                continue;
            }
            let x = area.x + col as u16;
            if let Some(cell) = grid.get(x, y).cloned()
                && !cell.is_wide_continuation()
            {
                let hue = column_rgb(col, width);
                let alpha = (intensity * 0.45).min(0.4);
                let tinted = blend_rgb(hue, rgb_of(cell.bg()), alpha);
                let mut next = cell;
                next.set_bg(tinted);
                grid.set(x, y, next);
            }
        }
    }

    painted
}

/// Build the centered `M A X` label for the hint bar's identity cluster
/// during the ignition's label phase.
///
/// Port of codex's `tier_label_line`: the letters always occupy the full
/// `width` (centered); `assemble` 0→1 collapses the inter-letter gaps from
/// "edges wide, center tight" (gap weights `|2i+1 − n| + 1` ⇒ `[4,2,1,3]`
/// for 4 gaps) down to single spaces. Letters fade in with a 22% stagger
/// from center outward, so the middle of the word lights first.
pub(crate) fn max_label_spans(
    width: usize,
    progress: f32,
    bg: Color,
    _theme: &Theme,
) -> Vec<Span<'static>> {
    const LETTERS: [char; 3] = ['M', 'A', 'X'];
    let base = rgb_of(bg);
    let fill = || Span::styled(" ".repeat(width), Style::default().bg(bg));
    if width == 0 {
        return vec![fill()];
    }

    // Ease-out cubic: fast initial convergence that settles gently.
    let assemble = 1.0 - (1.0 - progress.clamp(0.0, 1.0)).powi(3);
    let opacity = (progress / 0.55).clamp(0.0, 1.0);

    let letters = LETTERS.len();
    let gaps = letters - 1;
    let compact_width = letters + gaps; // "M A X"
    if width <= compact_width {
        // Too narrow to spread: render the tight label centered, tinted by
        // opacity so the fade still reads.
        let color = blend_rgb(SPARK_RGB, base, opacity);
        let label = Span::styled(
            "MAX",
            Style::default()
                .fg(color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        );
        let pad = (width - 3) / 2;
        return vec![
            Span::styled(" ".repeat(pad), Style::default().bg(bg)),
            label,
            Span::styled(
                " ".repeat(width.saturating_sub(pad + 3)),
                Style::default().bg(bg),
            ),
        ];
    }

    // Gap weights: edge gaps shrink fastest, so the letters appear to fall
    // inward from both sides and meet in the middle. The raw spread `f32` is
    // distributed by largest remainder so every gap's share rounds fairly —
    // integer division would hand the truncation residue to the last gap.
    let max_extra = width - compact_width;
    let spread = max_extra as f32 * (1.0 - assemble);
    let weights: Vec<usize> = (0..gaps).map(|i| (2 * i + 1).abs_diff(gaps) + 1).collect();
    let weight_total: usize = weights.iter().sum();
    let total = spread.round() as usize;
    let exact: Vec<f32> = weights
        .iter()
        .map(|&w| spread * w as f32 / weight_total.max(1) as f32)
        .collect();
    let mut shares: Vec<usize> = exact.iter().map(|e| e.floor() as usize).collect();
    let remaining = total.saturating_sub(shares.iter().sum());
    let mut by_frac: Vec<usize> = (0..gaps).collect();
    by_frac.sort_by(|&a, &b| {
        exact[b]
            .fract()
            .partial_cmp(&exact[a].fract())
            .unwrap_or(std::cmp::Ordering::Equal)
            // Tie-break toward the edge gap so symmetric words stay
            // symmetric-ish while the edge still wins the odd cell.
            .then(a.cmp(&b))
    });
    for &i in by_frac.iter().cycle().take(remaining) {
        shares[i] += 1;
    }
    let gap_widths: Vec<usize> = shares.iter().map(|&s| 1 + s).collect();
    let label_width = letters + gap_widths.iter().sum::<usize>();
    let left_pad = width.saturating_sub(label_width) / 2;

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(letters + gaps + 2);
    spans.push(Span::styled(" ".repeat(left_pad), Style::default().bg(bg)));
    for (i, ch) in LETTERS.iter().enumerate() {
        // Center letters fade in first; edge letters trail by up to 22%.
        let edge = (2 * i).abs_diff(letters - 1) as f32 / (letters - 1).max(1) as f32;
        let stagger = 0.22 * edge;
        let letter_opacity = ((opacity - stagger) / (1.0 - stagger)).clamp(0.0, 1.0);
        // Letters pick up the fire hue by position once fully in, so the
        // assembled word carries the same ember→gold ramp as the waves.
        let hue = fire_rgb(i as f32 / (letters - 1).max(1) as f32);
        let color = blend_rgb(hue, base, letter_opacity);
        let mut style = Style::default().fg(color).bg(bg);
        if letter_opacity >= 0.7 {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(ch.to_string(), style));
        if i < gaps {
            spans.push(Span::styled(
                " ".repeat(gap_widths[i]),
                Style::default().bg(bg),
            ));
        }
    }
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used)),
        Style::default().bg(bg),
    ));
    spans
}

/// Whether the hint bar's identity cluster is currently showing the `M A X`
/// takeover label instead of the model/effort/instance segments.
pub(crate) fn label_active(elapsed_ms: u128) -> bool {
    elapsed_ms < LABEL_TOTAL_MS
}

/// The composer prompt color while the ignition is live. During the 150ms
/// charge the `›` prompt blends from the theme brand color to the fire gold,
/// then rides the wave's warm accent until the animation ends. The prompt
/// glyph itself never changes — it stays `›`; only its color carries the
/// ignition. Returns `None` once the ignition is over so the composer falls
/// back to its ordinary palette.
pub(crate) fn ignition_prompt_color(igniting_ms: Option<u128>, theme: &Theme) -> Option<Color> {
    let ms = igniting_ms?;
    if ignition_finished(ms) {
        return None;
    }
    let brand = theme.brand();
    let color = if ms < CHARGE_MS {
        let charge = ms as f32 / CHARGE_MS as f32;
        blend_rgb(SPARK_RGB, rgb_of(brand), charge * 0.86)
    } else {
        blend_rgb(fire_rgb(0.55), rgb_of(brand), 0.86)
    };
    Some(color)
}

/// Place the `M A X` label into the hint bar's right cluster: pure label,
/// or `None` when the phase is over (the normal cluster renders then).
pub(crate) fn label_cluster(
    width: usize,
    elapsed_ms: u128,
    bg: Color,
    theme: &Theme,
) -> Option<Vec<Span<'static>>> {
    if !label_active(elapsed_ms) {
        return None;
    }
    Some(max_label_spans(
        width,
        elapsed_ms as f32 / LABEL_TOTAL_MS as f32,
        bg,
        theme,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuotc::{Cell, TestTerminal};
    fn theme() -> Theme {
        Theme::default()
    }

    // Phase / timing contract

    #[test]
    fn ignition_finishes_at_total_duration() {
        assert!(!ignition_finished(IGNITION_TOTAL_MS - 1));
        assert!(ignition_finished(IGNITION_TOTAL_MS));
        assert!(ignition_finished(IGNITION_TOTAL_MS + 500));
    }

    #[test]
    fn spark_only_fires_after_landing() {
        // Codex's exact spark contract: · → ✦ → ✧ at 100ms frames from
        // SPARK_START, then nothing.
        assert_eq!(spark_glyph(SPARK_START_MS - 50), None);
        assert_eq!(spark_glyph(SPARK_START_MS), Some("·"));
        assert_eq!(spark_glyph(SPARK_START_MS + 100), Some("✦"));
        assert_eq!(spark_glyph(SPARK_START_MS + 200), Some("✧"));
        assert_eq!(spark_glyph(SPARK_START_MS + 300), None);
    }

    #[test]
    fn label_phase_ends_before_the_waves_do() {
        assert!(label_active(LABEL_TOTAL_MS - 1));
        assert!(!label_active(LABEL_TOTAL_MS));
        assert!(!ignition_finished(LABEL_TOTAL_MS)); // waves still running
    }

    // Wave sampling

    #[test]
    fn wave_sweeps_left_to_right() {
        // Early frame: crest near the left edge; later frame: crest near the
        // right. The midpoint column is never simultaneously cresting in both.
        let w = 60;
        let early: Vec<f32> = (0..w).map(|c| ignition_intensity(250, c, w)).collect();
        let late: Vec<f32> = (0..w).map(|c| ignition_intensity(1050, c, w)).collect();
        let early_peak = early
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        let late_peak = late
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert!(
            early_peak < w / 3,
            "early crest should be left third, got {early_peak}"
        );
        assert!(
            late_peak > w * 2 / 3,
            "late crest should be right third, got {late_peak}"
        );
    }

    #[test]
    fn wave_is_silent_before_launch_and_after_total() {
        let w = 60;
        let pre: f32 = (0..w).map(|c| ignition_intensity(0, c, w)).sum();
        let post: f32 = (0..w)
            .map(|c| ignition_intensity(IGNITION_TOTAL_MS + 1, c, w))
            .sum();
        assert_eq!(pre, 0.0, "nothing painted before the first band launches");
        assert_eq!(post, 0.0, "nothing painted after the animation ends");
    }

    #[test]
    fn second_band_chases_the_first() {
        // Two bands ⇒ two simultaneous crests. Sample band-by-band: the two
        // crest centers must sit in different columns mid-animation, with
        // the chase crest trailing the leader.
        let w = 80;
        let ms = 620;
        let peak_of = |band: (f32, f32, f32)| {
            (0..w)
                .map(|c| wave_band_intensity(ms, band, c, w))
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap()
        };
        let lead = peak_of(WAVE_BANDS[0]);
        let chase = peak_of(WAVE_BANDS[1]);
        assert!(
            chase.abs_diff(lead) >= w / 4,
            "two distinct crests at {ms}ms: lead col {lead}, chase col {chase}"
        );
        assert!(chase < lead, "the second band chases the first rightward");
        // And the combined profile crests on the leader's column.
        let combined: Vec<f32> = (0..w).map(|c| ignition_intensity(ms, c, w)).collect();
        let max = combined.iter().cloned().fold(0.0_f32, f32::max);
        assert!(max > 0.5, "crest intensity is substantial: {max}");
    }

    // Band painting over the grid

    /// Render a uniform panel, run the band painter at `ms`, and return the
    /// grid rows for inspection.
    fn paint_panel(width: u16, height: u16, ms: u128, draft: &str) -> Vec<Vec<Cell>> {
        let mut terminal = TestTerminal::new(width, height);
        terminal.draw(|f| {
            // Fill the panel like the composer does: bg + the draft text.
            let bg = Color::Rgb(30, 30, 36);
            for y in 0..height {
                f.put(0, y, Style::default().bg(bg), &" ".repeat(width as usize));
            }
            if !draft.is_empty() {
                f.put(2, 1, Style::default().fg(Color::White).bg(bg), draft);
            }
            let area = Rect::new(0, 0, width, height);
            paint_ignition_bands(f, area, None, ms);
        });
        terminal
            .buffer()
            .rows()
            .into_iter()
            .map(<[_]>::to_vec)
            .collect()
    }

    #[test]
    fn band_tint_moves_across_the_panel() {
        let tint_columns = |rows: &[Vec<Cell>]| -> Vec<usize> {
            (0..rows[0].len())
                .filter(|&x| {
                    let bg = rows[0][x].bg();
                    bg != Color::Rgb(30, 30, 36)
                })
                .collect()
        };
        let early = tint_columns(&paint_panel(44, 3, 300, ""));
        let late = tint_columns(&paint_panel(44, 3, 1000, ""));
        assert!(!early.is_empty(), "mid-animation must tint some columns");
        assert!(!late.is_empty(), "the chase band tints late frames too");
        assert!(
            early.iter().max().unwrap() < late.iter().min().unwrap(),
            "the tinted region must move rightward: early {early:?} late {late:?}"
        );
    }

    #[test]
    fn band_tint_never_touches_text_or_foregrounds() {
        let draft = "keep my draft exactly as typed";
        let rows = paint_panel(44, 3, 500, draft);
        let line: String = rows[1].iter().map(|c| c.symbol()).collect();
        assert!(
            line.contains(draft),
            "draft row must survive the wave: {line:?}"
        );
        for (i, ch) in draft.chars().enumerate() {
            let cell = &rows[1][2 + i];
            assert_eq!(
                cell.fg(),
                Color::White,
                "text foreground must be untouched at {ch}"
            );
        }
    }

    #[test]
    fn spark_lands_on_blank_cell_near_top_right() {
        let rows = paint_panel(44, 3, SPARK_START_MS + 100, "");
        let top: String = rows[0].iter().map(|c| c.symbol()).collect();
        assert!(
            top.trim_end().ends_with('✦'),
            "spark ✦ at 100ms past landing: {top:?}"
        );
        let x = 44 - 2;
        assert_eq!(rows[0][x].symbol(), "✦");
        assert!(
            rows[0][x].style().add.contains(Modifier::BOLD),
            "spark is bold"
        );
    }

    #[test]
    fn spark_never_clobbers_text() {
        // Draft that reaches into the spark's landing cell (width-2).
        let mut terminal = TestTerminal::new(10, 2);
        terminal.draw(|f| {
            let bg = Color::Rgb(30, 30, 36);
            for y in 0..2 {
                f.put(0, y, Style::default().bg(bg), "          ");
            }
            f.put(6, 0, Style::default().fg(Color::White).bg(bg), "TEXT");
            paint_ignition_bands(f, Rect::new(0, 0, 10, 2), None, SPARK_START_MS + 100);
        });
        let rows = terminal.buffer().rows();
        let top: String = rows[0].iter().map(|c| c.symbol()).collect();
        assert!(top.contains("TEXT"), "text survives: {top:?}");
        assert!(
            !top.contains('✦'),
            "spark must not overwrite the text cell: {top:?}"
        );
    }

    #[test]
    fn paint_returns_false_when_finished_or_empty() {
        let mut terminal = TestTerminal::new(10, 2);
        terminal.draw(|f| {
            assert!(!paint_ignition_bands(
                f,
                Rect::new(0, 0, 10, 2),
                None,
                IGNITION_TOTAL_MS
            ));
            assert!(!paint_ignition_bands(f, Rect::new(0, 0, 0, 0), None, 100));
            assert!(paint_ignition_bands(f, Rect::new(0, 0, 10, 2), None, 550));
        });
    }

    // MAX label

    fn label_text(width: usize, progress: f32) -> String {
        max_label_spans(width, progress, Color::Rgb(20, 20, 26), &theme())
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    fn label_gaps(width: usize, progress: f32) -> Vec<usize> {
        // Measure the actual gap runs between M/A/X.
        let text = label_text(width, progress);
        let mut gaps = Vec::new();
        let mut run = 0usize;
        let mut seen_letter = false;
        for ch in text.chars() {
            match ch {
                ' ' if seen_letter => run += 1,
                'M' | 'A' | 'X' => {
                    if seen_letter {
                        gaps.push(run);
                    }
                    run = 0;
                    seen_letter = true;
                }
                _ => {}
            }
        }
        gaps
    }

    #[test]
    fn label_is_always_full_width_and_centered() {
        for progress in [0.0, 0.3, 0.7, 1.0] {
            let text = label_text(32, progress);
            assert_eq!(
                text.chars().count(),
                32,
                "label spans must fill the width at {progress}: {text:?}"
            );
            let left = text.chars().take_while(|&c| c == ' ').count();
            let right = text.chars().rev().take_while(|&c| c == ' ').count();
            assert!(
                left.abs_diff(right) <= 1,
                "label stays centered at {progress}: left {left} right {right} — {text:?}"
            );
        }
    }

    #[test]
    fn letters_start_wide_at_the_edges_and_converge_to_the_center() {
        // Codex's gap-weight contract: gap[0] > gap[1] — the outer gap
        // carries more of the spread, so the letters fall inward.
        let early = label_gaps(32, 0.05);
        assert_eq!(early.len(), 2, "M A X has two gaps: {early:?}");
        assert!(
            early[0] > early[1],
            "edge gap wider than center gap early: {early:?}"
        );
        let tight = label_gaps(32, 1.0);
        assert_eq!(tight, vec![1, 1], "assembled label is single-spaced");
        assert_eq!(label_text(32, 1.0).trim(), "M A X");
    }

    #[test]
    fn label_converges_monotonically() {
        let widths: Vec<usize> = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0]
            .iter()
            .map(|&p| label_gaps(40, p).iter().sum())
            .collect();
        for pair in widths.windows(2) {
            assert!(pair[1] <= pair[0], "gaps shrink monotonically: {widths:?}");
        }
    }

    #[test]
    fn center_letter_fades_in_first() {
        // At an early phase the edge letters are still in their 22% stagger
        // lag: the center A must be at least as bright as the edge M/X.
        let spans = max_label_spans(32, 0.2, Color::Rgb(20, 20, 26), &theme());
        let brightness = |ch: char| {
            spans
                .iter()
                .find(|s| s.content.as_ref() == ch.to_string())
                .map(|s| {
                    let (r, g, b) = rgb_of(s.style.fg);
                    r as u32 + g as u32 + b as u32
                })
                .unwrap_or(0)
        };
        let center = brightness('A');
        let edge = brightness('M').max(brightness('X'));
        assert!(
            center >= edge,
            "center letter leads the fade-in: center {center} edge {edge}"
        );
    }

    #[test]
    fn label_cluster_only_during_the_label_phase() {
        let theme = theme();
        assert!(label_cluster(20, 0, Color::Rgb(20, 20, 26), &theme).is_some());
        assert!(label_cluster(20, LABEL_TOTAL_MS - 1, Color::Rgb(20, 20, 26), &theme).is_some());
        assert!(label_cluster(20, LABEL_TOTAL_MS, Color::Rgb(20, 20, 26), &theme).is_none());
    }

    #[test]
    fn tiny_widths_fall_back_to_a_tight_label() {
        let text = label_text(4, 0.5);
        assert!(text.contains("MAX"), "tiny width renders MAX: {text:?}");
        assert_eq!(text.chars().count(), 4);
    }

    // Prompt accent

    #[test]
    fn prompt_tint_charges_then_fades_back_to_default() {
        let theme = theme();
        // While the wave is live the `›` prompt takes a fire accent that
        // visibly ramps during the 150ms charge.
        let c0 = ignition_prompt_color(Some(0), &theme).expect("live at t=0");
        let c1 = ignition_prompt_color(Some(CHARGE_MS - 1), &theme).expect("mid-charge");
        let c_peak = ignition_prompt_color(Some(CHARGE_MS), &theme).expect("post-charge");
        assert_ne!(c0, c1, "the charge visibly ramps");
        assert_ne!(c0, c_peak, "charge start vs peak differ");
        // Once the ignition ends the prompt falls back to the ordinary
        // palette (no standing glyph/color change).
        assert!(ignition_prompt_color(Some(IGNITION_TOTAL_MS), &theme).is_none());
        assert!(ignition_prompt_color(None, &theme).is_none());
    }

    // Frame gallery (visual regression anchor)

    #[test]
    fn ignition_frame_gallery() {
        let bg = Color::Rgb(30, 30, 36);
        let mut out = String::new();
        for ms in [0u128, 250, 500, 750, 950, 1050, 1150, 1290] {
            let mut terminal = TestTerminal::new(44, 3);
            terminal.draw(|f| {
                for y in 0..3 {
                    f.put(0, y, Style::default().bg(bg), &" ".repeat(44));
                }
                f.put(
                    2,
                    1,
                    Style::default().fg(Color::White).bg(bg),
                    "› keep my draft exactly as typed",
                );
                paint_ignition_bands(f, Rect::new(0, 0, 44, 3), None, ms);
            });
            let rows = terminal.buffer().rows();
            out.push_str(&format!("{ms:>5}ms\n"));
            for row in &rows {
                let line: String = row
                    .iter()
                    .map(|c| {
                        if !c.symbol().trim().is_empty() {
                            return c.symbol().to_string();
                        }
                        match c.bg() {
                            b if b == bg => "·".to_string(),
                            Color::Rgb(r, g, b) => {
                                // Heat map: brighter tint → hotter glyph.
                                let heat = (r as u32 + g as u32 + b as u32) / 3;
                                match heat {
                                    0..=40 => "·".to_string(),
                                    41..=70 => "░".to_string(),
                                    72..=110 => "▒".to_string(),
                                    _ => "▓".to_string(),
                                }
                            }
                            _ => "░".to_string(),
                        }
                    })
                    .collect();
                out.push_str(&format!("│{line}│\n"));
            }
        }
        insta::assert_snapshot!(out);
    }
}
