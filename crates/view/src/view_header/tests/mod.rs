//! Header chrome tests, split by tier (ADR-0261 two-tier head band).

pub(crate) use super::*;

fn render_row1(width: u16, tab_bar: TabBarProps<'_>) -> String {
    let cells = render_row1_cells(width, tab_bar, &Theme::default());
    cells.iter().map(|c| c.symbol.as_str()).collect()
}

fn render_row1_cells(
    width: u16,
    tab_bar: TabBarProps<'_>,
    theme: &Theme,
) -> Vec<graticule_canvas::Cell> {
    let mut terminal = graticule_canvas::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_tab_bar(frame, frame.area(), &tab_bar, theme);
    });
    terminal.buffer().cells().to_vec()
}

fn render_row2(width: u16, thread_id: Option<&str>, hints: ViewHints<'_>) -> String {
    render_row2_with_context(width, thread_id, hints, None)
}

fn render_row2_with_context(
    width: u16,
    thread_id: Option<&str>,
    hints: ViewHints<'_>,
    context: Option<ThreadContext<'_>>,
) -> String {
    let cells = render_row2_cells(width, thread_id, hints, context, &Theme::default());
    cells.iter().map(|c| c.symbol.as_str()).collect()
}

fn render_row2_cells(
    width: u16,
    thread_id: Option<&str>,
    hints: ViewHints<'_>,
    context: Option<ThreadContext<'_>>,
    theme: &Theme,
) -> Vec<graticule_canvas::Cell> {
    let mut terminal = graticule_canvas::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_scene_bar(
            frame,
            frame.area(),
            thread_id,
            &hints,
            context.as_ref(),
            theme,
        );
    });
    terminal.buffer().cells().to_vec()
}

fn render_thread_row(
    width: u16,
    thread_id: Option<&str>,
    title: Option<&str>,
    context: ThreadContext<'_>,
) -> String {
    render_thread_row_with_theme(width, thread_id, title, context, &Theme::default())
}

fn render_thread_row_with_theme(
    width: u16,
    thread_id: Option<&str>,
    title: Option<&str>,
    context: ThreadContext<'_>,
    theme: &Theme,
) -> String {
    render_row2_cells(
        width,
        thread_id,
        ViewHints {
            kind: SceneKind::Thread,
            context: title,
            ..hints(SceneKind::Thread)
        },
        Some(context),
        theme,
    )
    .iter()
    .map(|c| c.symbol.as_str())
    .collect()
}

fn hints<'a>(kind: ViewKind) -> ViewHints<'a> {
    ViewHints::simple(kind)
}

#[test]
fn header_tiers_exhibit_distinct_micro_elevation_ladder() {
    let theme = Theme::default();
    assert_ne!(
        theme.tabbar_bg(),
        theme.scenebar_bg(),
        "tier 1 tabbar background must differ from tier 2 scenebar"
    );

    let (r1, g1, b1) = match theme.tabbar_bg() {
        graticule_canvas::Color::Rgb(r, g, b) => (r, g, b),
        _ => panic!("RGB expected"),
    };
    let (r2, g2, b2) = match theme.scenebar_bg() {
        graticule_canvas::Color::Rgb(r, g, b) => (r, g, b),
        _ => panic!("RGB expected"),
    };

    // Monotonic elevation ladder: SceneBar (focal plane) > TabBar (base)
    assert!(
        r2 > r1,
        "scenebar must be elevated above tabbar: r2={r2}, r1={r1}"
    );
    assert!(
        g2 > g1,
        "scenebar must be elevated above tabbar: g2={g2}, g1={g1}"
    );
    assert!(
        b2 > b1,
        "scenebar must be elevated above tabbar: b2={b2}, b1={b1}"
    );
    assert!(
        r2.saturating_sub(r1) >= 10,
        "scenebar and tabbar must have at least 10 units of perceptible contrast: r2={r2}, r1={r1}"
    );
}

mod scene_bar;
mod tab_bar;
mod thread_bar;
