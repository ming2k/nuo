//! The flat Models picker modal (selecting the active model/instance).
use nuotc::{
    Frame, Rect, {Line, Span}, {Modifier, Style},
};

use unicode_width::UnicodeWidthStr;

use super::common::{
    draw_picker_search_row, match_set, place_picker_search_cursor, render_pricing_detail_rows,
    search_empty_body, split_search_bottom,
};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::columns::{Column, ColumnSpec, Grid, MAX_JUSTIFIED_GAP, column};
use nuo_ui_primitives::components::detail::DetailLayout;
use nuo_ui_primitives::components::options::{ChoiceTone, choice_style};
use nuo_ui_primitives::components::overflow::{
    MarqueeCadence, Overflow, TextCell, WidthAware, marquee_offset, window_max_offset,
};
use nuo_ui_primitives::components::row::{GROUP_GAP, GUTTER, ListRow, RowGroup};
use nuo_ui_primitives::primitives::{
    BodyRenderOptions, FixedModalSpec, FooterHint, FooterHintWithBand, SCROLL_EDGE_MARGIN,
    keyvocab, modal_area, modal_frame, modal_header, render_body, render_centered_body,
    render_modal_footer_with_extra,
};
use nuo_view::components::selectable_body::{SelectableRow, render_selectable_body};
use nuo_view::model::layout::LayoutMap;
use nuo_view::model::selection::SelectionState;
use nuo_view::providers::{ModelBodyLine, RankedModel, models_body_lines};

/// Properties for rendering the Models modal.
pub struct ModelsModalProps<'a> {
    pub models: &'a [RankedModel],
    pub modal_index: usize,
    pub query: &'a str,
    pub cursor_position: usize,
    pub scroll: &'a mut usize,
    pub follow_selection: bool,
    pub search: bool,
    /// Whether this surface currently owns the terminal cursor
    /// (`App::caret_owner == CaretOwner::Overlay`). The picker
    /// never places the physical cursor on its own authority — the frame-level
    /// arbiter decides that, and this flag is its verdict threaded down.
    pub show_caret: bool,
    pub refreshing: bool,
    pub spinner_phase: usize,
    pub info_detail: bool,
    pub info_scroll: &'a mut usize,
    /// Set by the draw: whether the focused row is currently revealing a cell
    /// that does not fit. The dialog stores it so the event loop can keep its
    /// animation heartbeat alive exactly while something is moving, and let it
    /// sleep the moment nothing is.
    pub reveal_active: &'a mut bool,
}

/// Draw the **Models** flat model picker modal (`/models`).
pub fn draw_models_modal(
    frame: &mut Frame,
    layout_map: &mut LayoutMap,
    props: ModelsModalProps<'_>,
    theme: &Theme,
    selection: &SelectionState,
) -> nuotc::Rect {
    let ModelsModalProps {
        models,
        modal_index,
        query,
        cursor_position,
        scroll,
        follow_selection,
        search,
        show_caret,
        refreshing,
        spinner_phase,
        info_detail,
        info_scroll,
        reveal_active,
    } = props;
    // Cleared up front: the detail sub-view and the empty states never reveal
    // anything, so only the list path below may set it.
    *reveal_active = false;
    let area = modal_area(frame, FixedModalSpec::MODELS);
    let f = modal_frame(frame, area, theme, true, true);

    let header_rect = f.header;

    if info_detail && let Some(rm) = models.get(modal_index.min(models.len().saturating_sub(1))) {
        let display = rm.display_label();
        let breadcrumbs = nuo_ui_primitives::primitives::breadcrumb_parts("Models", display);
        nuo_ui_primitives::elevation::modal_header_parts(frame, header_rect, &breadcrumbs, theme);

        let rows = model_detail_body(rm, f.body.width as usize, theme);

        let refresh_label = if refreshing {
            "refreshing…"
        } else {
            "refresh"
        };
        let mut detail_hints: Vec<FooterHint> = Vec::with_capacity(5);
        if rm.usable {
            detail_hints.push(FooterHint::primary(
                crate::shell::keymap::Key::ENTER.display(),
                "activate",
            ));
            detail_hints.push(FooterHint::secondary("e", "edit"));
        }
        detail_hints.push(FooterHint::secondary("r", refresh_label));
        detail_hints.push(FooterHint::always(
            crate::shell::keymap::Key::ESC.display(),
            "list",
        ));

        render_selectable_body(
            frame,
            f.body,
            &rows,
            info_scroll,
            None,
            theme,
            selection,
            layout_map,
        );

        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, &detail_hints, &[], theme);
        }
        return area;
    }

    let refresh_label = if refreshing {
        "refreshing…"
    } else {
        "refresh"
    };
    let browse_hints: [FooterHint; 9] = [
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::secondary("/", "search"),
        FooterHint::primary(crate::shell::keymap::Key::ENTER.display(), "activate"),
        FooterHint::secondary("i", "details"),
        FooterHint::secondary("e", "edit"),
        FooterHint::secondary("*", "favorite"),
        FooterHint::secondary("x", "block"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
    ];
    let search_hints: [FooterHint; 4] = [
        FooterHint::secondary("type", "filter"),
        FooterHint::navigation(keyvocab::ARROWS_UD, "navigate"),
        FooterHint::primary(crate::shell::keymap::Key::ENTER.display(), "activate"),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), "clear search"),
    ];
    let empty_hints: [FooterHint; 3] = [
        FooterHint::primary("a", "add connection"),
        FooterHint::secondary("r", refresh_label),
        FooterHint::always(crate::shell::keymap::Key::ESC.display(), "close"),
    ];
    let (hints, extra): (&[FooterHint], &[FooterHintWithBand]) = if search {
        (&search_hints, &[])
    } else if models.is_empty() {
        (&empty_hints, &[])
    } else {
        (&browse_hints, &[])
    };

    if refreshing {
        let spin = theme.glyphs.spinner_frame(spinner_phase);
        let header = [
            nuo_ui_primitives::elevation::HeaderPart::title("Models"),
            nuo_ui_primitives::elevation::HeaderPart::Text {
                text: "  ",
                accent: false,
            },
            nuo_ui_primitives::elevation::HeaderPart::Text {
                text: spin,
                accent: true,
            },
            nuo_ui_primitives::elevation::HeaderPart::Text {
                text: " refreshing…",
                accent: false,
            },
        ];
        nuo_ui_primitives::elevation::modal_header_parts(frame, header_rect, &header, theme);
    } else if search && !query.is_empty() {
        let title = format!("Models ({})", models.len());
        modal_header(frame, header_rect, &title, theme);
    } else {
        modal_header(frame, header_rect, "Models", theme);
    }

    let (body_rect, search_rect) = split_search_bottom(f.body, search);
    if let Some(search_rect) = search_rect {
        if search_rect.y > f.body.y {
            let sep_rect = Rect {
                x: search_rect.x,
                y: search_rect.y - 1,
                width: search_rect.width,
                height: 1,
            };
            let sep_line = "─".repeat(sep_rect.width as usize);
            frame.render_widget(
                &mut nuotc::Paragraph::new(Line::from(Span::styled(
                    sep_line,
                    Style::default().fg(theme.muted()),
                ))),
                sep_rect,
            );
        }
        draw_picker_search_row(frame, search_rect, query, cursor_position, theme);
    }

    if models.is_empty() && !search {
        let body = if refreshing {
            let spin = theme.glyphs.spinner_frame(spinner_phase);
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(format!("{spin} "), Style::default().fg(theme.primary)),
                    Span::styled("Refreshing models…", Style::default().fg(theme.muted())),
                ]),
            ]
        } else {
            models_empty_body(theme)
        };
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        return area;
    }

    if models.is_empty() && search {
        let body = search_empty_body(theme);
        render_centered_body(frame, body_rect, body);
        if let Some(fo) = f.footer {
            render_modal_footer_with_extra(frame, fo, hints, extra, theme);
        }
        if show_caret && let Some(sr) = search_rect {
            place_picker_search_cursor(frame, sr, query, cursor_position);
        }
        return area;
    }

    let (body, row_line, revealing) = model_list_body_at(
        models,
        modal_index,
        theme,
        body_rect.width as usize,
        spinner_phase as u64 * REVEAL_PHASE_MS,
    );
    *reveal_active = revealing;

    let follow = if follow_selection {
        row_line.get(modal_index).copied()
    } else {
        None
    };

    render_body(
        frame,
        body_rect,
        body,
        scroll,
        BodyRenderOptions::new(follow, SCROLL_EDGE_MARGIN, false),
        theme,
    );

    if let Some(fo) = f.footer {
        render_modal_footer_with_extra(frame, fo, hints, extra, theme);
    }

    if show_caret
        && search
        && let Some(sr) = search_rect
    {
        place_picker_search_cursor(frame, sr, query, cursor_position);
    }
    area
}

/// The millisecond cost of one animation phase, shared by every time-driven
/// surface (spinner, reveal). `spinner_phase` counts 100 ms ticks, so a phase
/// index converts to elapsed milliseconds by this constant — the reveal cadence
/// then lives in [`MarqueeCadence`] alone instead of being re-derived here.
const REVEAL_PHASE_MS: u64 = 100;

/// The floor below which the model-name column stops being readable: enough for
/// `Claude Sonnet` plus an ellipsis, so the family is always identifiable.
const NAME_MIN_WIDTH: usize = 16;
/// The name's relative maximum width. The name is the row's identity, but it is
/// not the whole row: capping it is what keeps the connection column — the one
/// answering *whose quota am I spending?* — legible beside it.
const NAME_MAX_SHARE: usize = 34;
/// The wire id's floor. Below this `deepseek-flash` degenerates into `deep…`,
/// which reads as a *different* model rather than as an abbreviation, so the
/// column drops instead of truncating that far.
const ID_MIN_WIDTH: usize = 12;
/// The wire id's relative maximum width. An id is a technical handle, not prose:
/// it earns less of the row than the name it annotates.
const ID_MAX_SHARE: usize = 18;
/// Capability tags (`vis`, `high`) are at most ten columns and are cosmetic.
const SPECS_MAX_SHARE: usize = 10;
/// The connection's floor: enough for `anthropic`, `openai-work`, `deepseek`.
/// This column is never sacrificed to fund a verbose neighbour.
const CONNECTION_MIN_WIDTH: usize = 10;
/// The connection's relative maximum width.
const CONNECTION_MAX_SHARE: usize = 24;

/// The status word a row carries, or `""` when it is simply usable.
fn row_status(rm: &RankedModel) -> &str {
    if !rm.usable {
        rm.display_status().unwrap_or("disabled")
    } else if rm.availability_overridden {
        "overridden"
    } else if rm.availability_stale {
        "stale"
    } else {
        ""
    }
}

/// The reasoning-effort tag a row carries, or `""`.
fn model_effort_tag(rm: &RankedModel) -> &str {
    if !rm.usable {
        return "";
    }
    match (rm.thinking, rm.effort.as_deref()) {
        (Some(true), Some(effort)) => effort,
        (Some(true), None) => "on",
        (Some(false), _) if !rm.effort_levels.is_empty() => "off",
        (None, Some(effort)) => effort,
        _ => "",
    }
}

/// Build the **Models** flat model list body via the shared
/// [`nuo_ui_primitives::components::row::ListRow`] standard, **sectioned into
/// three labeled groups** — Favorites, Recent, All models.
///
/// # Column budget (ADR-0152 `[INV-ROW-GRID-01]`)
///
/// The five fields are resolved once per frame by
/// [`nuo_ui_primitives::components::columns`], *before* a single cell is fitted.
/// Each field declares what its data actually needs, the floor below which it
/// stops being readable, and its own relative maximum width; the solver then
/// reconciles the sum against the body and emits every column's start by one
/// left-to-right accumulation. No column's position is derived from a
/// neighbour's demand, which is what used to let a long model name push the
/// connection column off the row.
///
/// | field | overflow | floor | max share | why |
/// | :--- | :--- | ---: | ---: | :--- |
/// | model name | `End` | 16 | 34% | prose: the head carries the meaning |
/// | wire id | `Middle` | 12 | 18% | `claude…0219`: both ends discriminate |
/// | specs | `End` | 0 | 10% | `vis` + effort, ten columns at most |
/// | connection | `End` | 10 | 24% | *whose quota am I spending?* |
/// | status | `End` | 0 | 14% | right-pinned badge, reserved not placed |
///
/// # Reveal (`[INV-TUI-MARQUEE-01]`)
///
/// The focused row may slide any cell that overflows, so a truncated tail is
/// reachable without widening the terminal. Every other row stays static: a
/// table in which every row drifts is unreadable and burns CPU proportional to
/// the row count. Motion marks focus.
/// [`model_list_body`] at a specific point in the reveal animation, also
/// reporting whether the focused row is currently revealing anything.
///
/// The report is what lets the runtime gate its animation heartbeat: an idle
/// list must not repaint at 10 fps merely because reveal *exists*.
pub(crate) fn model_list_body_at(
    models: &[RankedModel],
    modal_index: usize,
    theme: &Theme,
    body_width: usize,
    elapsed_ms: u64,
) -> (Vec<Line<'static>>, Vec<usize>, bool) {
    if models.is_empty() {
        return (Vec::new(), Vec::new(), false);
    }
    let (geometry, row_line) = models_body_lines(models);
    let mut body: Vec<Line> = Vec::with_capacity(geometry.len() + 3);

    let spacer = || {
        Line::from(Span::styled(
            " ".repeat(body_width.max(1)),
            Style::default().bg(theme.panel()),
        ))
    };

    let layout = ModelColumns::measure(models, body_width);
    let revealing = models
        .get(modal_index)
        .is_some_and(|rm| layout.overflows(rm));

    for line in geometry {
        match line {
            ModelBodyLine::Section(section) => {
                if !body.is_empty() {
                    body.push(spacer());
                }
                body.push(Line::from(Span::styled(
                    format!(" {}", section.label()),
                    Style::default().fg(theme.muted()),
                )));
            }
            ModelBodyLine::Row(row) => {
                let rm = &models[row];
                let is_selected = row == modal_index;
                body.push(layout.row(rm, is_selected, theme, elapsed_ms));
            }
        }
    }
    (body, row_line, revealing)
}

/// Column indices into [`ModelColumns::columns`].
const COL_NAME: usize = 0;
const COL_ID: usize = 1;
const COL_SPECS: usize = 2;
const COL_CONNECTION: usize = 3;

/// The resolved column geometry of the Models list.
///
/// Built once per frame so every row shares one budget: a column present in row
/// 3 must start at the same offset in row 30, or the table stops reading as a
/// table.
struct ModelColumns {
    columns: Vec<Column>,
    /// Whether the trailing status badge is funded. The solver decides, because
    /// it is the only thing that knows whether honouring the badge would starve a
    /// column it kept — and painting a badge the solver did not reserve overflows
    /// the row.
    status_badge_funded: bool,
    body_width: usize,
    /// Whether any row draws a `vis` tag, which decides whether the specs
    /// column reserves its three columns at all.
    any_vision: bool,
    /// Whether any row draws an effort tag.
    any_effort: bool,
    /// Whether any row carries a name distinct from its wire id — i.e. whether
    /// the id column has anything to show.
    has_distinct_id: bool,
    /// The atomic width of the capability-tag block. The column is all-or-
    /// nothing: painting part of it would misalign the effort down the column.
    specs_demand: usize,
}

impl ModelColumns {
    fn measure(models: &[RankedModel], body_width: usize) -> Self {
        let any_vision = models.iter().any(|m| m.vision == Some(true));
        let max_effort = models
            .iter()
            .map(|m| model_effort_tag(m).width())
            .max()
            .unwrap_or(0);
        let any_effort = max_effort > 0;
        let has_distinct_id = models.iter().any(RankedModel::has_distinct_name);

        let name_demand = models
            .iter()
            .map(|m| m.display_label().width())
            .max()
            .unwrap_or(0);
        let id_demand = models
            .iter()
            .filter(|m| m.has_distinct_name())
            .map(|m| m.model.width())
            .max()
            .unwrap_or(0);
        let specs_demand = match (any_vision, any_effort) {
            (true, true) => 3 + 1 + max_effort,
            (true, false) => 3,
            (false, true) => max_effort,
            (false, false) => 0,
        };
        let conn_demand = models
            .iter()
            .map(|m| m.provider_label.width())
            .max()
            .unwrap_or(0);
        let status_demand = models
            .iter()
            .map(|m| row_status(m).width())
            .max()
            .unwrap_or(0);

        // The trailing badge is *reserved*, never placed: it is pinned to the
        // right edge, so funding it from the pool the columns draw on would make
        // it a competitor instead of an anchor.
        let status_reserve = if status_demand > 0 {
            status_demand + GROUP_GAP
        } else {
            0
        };

        let specs = [
            // The name absorbs slack first: it is the row's identity, and a wide
            // terminal is exactly where a long name becomes readable.
            ColumnSpec::new(name_demand, NAME_MIN_WIDTH, NAME_MAX_SHARE),
            // The wire id is the first thing to drop: a fragment of the only
            // typeable handle is worse than its absence, and the id stays
            // reachable through the focused row's reveal and the detail view.
            ColumnSpec::new(id_demand, ID_MIN_WIDTH, ID_MAX_SHARE)
                .droppable()
                .priority(1),
            // Capability tags are an *atomic* block: a fixed-slot composite of
            // `vis` plus effort, whose blanks keep the effort aligned down the
            // column. A partially funded block would misalign every row, so its
            // floor is its whole demand and it drops as a unit instead.
            ColumnSpec::new(specs_demand, specs_demand, SPECS_MAX_SHARE)
                .droppable()
                .priority(3),
            // The connection answers "whose quota am I spending?" — it is never
            // sacrificed to fund a verbose model name.
            ColumnSpec::new(conn_demand, CONNECTION_MIN_WIDTH, CONNECTION_MAX_SHARE),
        ];

        let solved = Grid::new(body_width)
            .gutter(GUTTER)
            .gap(GROUP_GAP)
            .max_gap(MAX_JUSTIFIED_GAP)
            .reserve_tail(status_reserve)
            .justify(true)
            .layout(&specs);
        let columns = solved.columns;

        Self {
            columns,
            status_badge_funded: solved.reserve,
            body_width,
            any_vision,
            any_effort,
            has_distinct_id,
            specs_demand,
        }
    }

    fn column(&self, index: usize) -> Option<Column> {
        column(&self.columns, index)
    }

    /// Whether any cell of this row is wider than the column it must fit.
    ///
    /// Only the focused row acts on this — it is the row the reader is looking
    /// at — but the modal reports it so the runtime knows whether to keep its
    /// animation heartbeat alive.
    fn overflows(&self, rm: &RankedModel) -> bool {
        let mut any = false;
        if let Some(c) = self.column(COL_NAME) {
            any |= rm.display_label().width() > c.width;
        }
        if rm.has_distinct_name()
            && self.has_distinct_id
            && let Some(c) = self.column(COL_ID)
        {
            any |= rm.model.width() > c.width;
        }
        if let Some(c) = self.column(COL_CONNECTION) {
            any |= rm.provider_label.width() > c.width;
        }
        any
    }

    /// The overflow strategy for one cell of one row.
    ///
    /// `static_overflow` is the field's own semantic strategy, used whenever the
    /// row is not the focused one: a wire id degrades to `claude…0219` rather
    /// than losing its discriminating tail. The focused row instead *reveals*
    /// whatever does not fit, because motion is how the list says "this is the
    /// line you are reading".
    ///
    /// The marker's width comes from the theme (`…` is one column, ASCII `...` is
    /// three): the travel range must reserve exactly that many, or the window
    /// cannot reach the last columns of the string on a terminal that degrades
    /// the glyph.
    fn overflow(
        is_selected: bool,
        static_overflow: Overflow,
        total: usize,
        budget: usize,
        elapsed_ms: u64,
        marker_width: usize,
    ) -> Overflow {
        if is_selected && total > budget {
            Overflow::Window {
                offset: marquee_offset(
                    window_max_offset(total, budget, marker_width),
                    elapsed_ms,
                    MarqueeCadence::default(),
                ),
            }
        } else {
            static_overflow
        }
    }

    /// Paint one model row into the resolved geometry.
    fn row(
        &self,
        rm: &RankedModel,
        is_selected: bool,
        theme: &Theme,
        elapsed_ms: u64,
    ) -> Line<'static> {
        let marker_width = theme.glyphs.ellipsis.width();
        let disabled = !rm.usable;
        let style = choice_style(ChoiceTone::Filled, is_selected && !disabled, theme);
        let mut groups: Vec<RowGroup> = Vec::new();

        let label_fg = if disabled { theme.dim() } else { style.fg };
        let label_match_fg = if disabled {
            theme.dim()
        } else if is_selected {
            style.fg
        } else {
            theme.brand()
        };
        let label_modifier = if disabled {
            Modifier::empty()
        } else {
            Modifier::BOLD
        };

        // 1. Model name — the row's primary identity: the provider's own name
        // when it publishes one, the wire id otherwise, so the column is never
        // empty. Identity itself is unaffected by what is drawn — activation,
        // favorites, hiding and every configuration surface still key on
        // `rm.model` (docs/reference/tui/modals.md, "name-first, id-fallback").
        //
        // Highlighting indexes what is drawn, per column: a labelled row
        // highlights its name via `match_name`, an unlabelled one its id via
        // `match_id`. A row kept alive by the *other* field (or by the
        // connection name) renders unhighlighted, because its `match_*` is then
        // empty — never a mis-indexed `m`, whose positions belong to whichever
        // field matched first.
        let distinct_name = rm.has_distinct_name();
        let label_matched = match_set(if distinct_name {
            rm.match_name.as_ref()
        } else {
            rm.match_id.as_ref()
        });
        let name_col = self.column(COL_NAME);
        let name_budget = name_col.map_or(0, |c| c.width);
        let name_cell = TextCell::new(
            rm.display_label(),
            Self::overflow(
                is_selected,
                Overflow::End,
                rm.display_label().width(),
                name_budget,
                elapsed_ms,
                marker_width,
            ),
            Style::default()
                .bg(style.bg)
                .fg(label_fg)
                .add_modifier(label_modifier),
        )
        .ellipsis(theme.glyphs.ellipsis)
        .highlighted(
            (!label_matched.is_empty()).then_some(&label_matched),
            Style::default()
                .bg(style.bg)
                .fg(label_match_fg)
                .add_modifier(label_modifier),
        );
        groups.push(
            RowGroup::column(name_col.map_or(GUTTER, |c| c.start))
                .spans(name_cell.render_width(name_budget)),
        );

        // 2. Wire id — the secondary column, dimmed. Only drawn when a distinct
        // name leads: an unlabelled row already draws the id in column 1. The id
        // must never hide behind the name — it is the string that goes on the
        // wire and into `hidden_models`/favorites/route settings — but it drops
        // entirely when the budget cannot fund a readable width, since a
        // useless mid-word truncation of the only typeable handle is worse than
        // its absence.
        if distinct_name
            && self.has_distinct_id
            && let Some(id_col) = self.column(COL_ID)
        {
            let id_matched = match_set(rm.match_id.as_ref());
            let id_fg = if disabled { theme.dim() } else { style.dim };
            let id_match_fg = if disabled {
                theme.dim()
            } else if is_selected {
                style.fg
            } else {
                theme.brand()
            };
            let id_modifier = if disabled {
                Modifier::empty()
            } else {
                Modifier::BOLD
            };
            let id_cell = TextCell::new(
                rm.model.as_str(),
                Self::overflow(
                    is_selected,
                    Overflow::Middle,
                    rm.model.width(),
                    id_col.width,
                    elapsed_ms,
                    marker_width,
                ),
                Style::default().bg(style.bg).fg(id_fg),
            )
            .ellipsis(theme.glyphs.ellipsis)
            .highlighted(
                (!id_matched.is_empty()).then_some(&id_matched),
                Style::default()
                    .bg(style.bg)
                    .fg(id_match_fg)
                    .add_modifier(id_modifier),
            );
            groups.push(RowGroup::column(id_col.start).spans(id_cell.render_width(id_col.width)));
        }

        // 3. Capability tags: vision (`vis`) + reasoning effort. Context size is
        // deliberately absent from the list (ADR-0129) — it belongs to the
        // detail view, where there is room to explain it.
        if let Some(specs_col) = self
            .column(COL_SPECS)
            .filter(|c| c.width >= self.specs_demand)
        {
            let mut specs_group = RowGroup::column(specs_col.start);
            if self.any_vision {
                let cap_fg = if disabled { theme.dim() } else { theme.ok() };
                let text = if rm.vision == Some(true) {
                    "vis"
                } else {
                    "   "
                };
                specs_group = specs_group.text(text, cap_fg, 0);
            }
            if self.any_effort {
                let effort_fg = if disabled { theme.dim() } else { theme.info() };
                let gap = usize::from(self.any_vision);
                specs_group = specs_group.text(model_effort_tag(rm), effort_fg, gap);
            }
            groups.push(specs_group);
        }

        // 4. Connection — whose quota this model spends.
        if let Some(conn_col) = self.column(COL_CONNECTION) {
            let conn_matched = match_set(rm.match_connection.as_ref());
            let conn_fg = if disabled { theme.dim() } else { style.dim };
            let conn_match_fg = if disabled {
                theme.dim()
            } else if is_selected {
                style.fg
            } else {
                theme.brand()
            };
            let conn_modifier = if disabled {
                Modifier::empty()
            } else {
                Modifier::BOLD
            };
            let conn_cell = TextCell::new(
                rm.provider_label.as_str(),
                Self::overflow(
                    is_selected,
                    Overflow::End,
                    rm.provider_label.width(),
                    conn_col.width,
                    elapsed_ms,
                    marker_width,
                ),
                Style::default().bg(style.bg).fg(conn_fg),
            )
            .ellipsis(theme.glyphs.ellipsis)
            .highlighted(
                (!conn_matched.is_empty()).then_some(&conn_matched),
                Style::default()
                    .bg(style.bg)
                    .fg(conn_match_fg)
                    .add_modifier(conn_modifier),
            );
            groups.push(
                RowGroup::column(conn_col.start).spans(conn_cell.render_width(conn_col.width)),
            );
        }

        // 5. Status — a right-pinned badge, empty for usable models. Trailing
        // anchoring keeps it out of the column budget entirely, so no left-hand
        // column can shift it and it can never shift one — but only while the
        // solver says the reserve is funded. On a body too narrow to hold both,
        // the badge yields to the row's identity rather than overflowing it.
        let status_text = row_status(rm);
        if self.status_badge_funded && !status_text.is_empty() {
            let status_fg = if disabled {
                theme.dim()
            } else if is_selected {
                style.fg
            } else {
                theme.ok()
            };
            groups.push(RowGroup::trailing().text(status_text, status_fg, 0));
        }

        let mut list_row = ListRow::new(style, self.body_width);
        for group in groups {
            list_row = list_row.group(group);
        }
        list_row.finish()
    }
}

/// The Models empty-state body: shown when no model exists (no provider configured
/// or no models returned).
pub(crate) fn models_empty_body(theme: &Theme) -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "No models available",
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Add a connection via ", Style::default().fg(theme.muted())),
            Span::styled(
                "/connections",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" (or press ", Style::default().fg(theme.muted())),
            Span::styled(
                "a",
                Style::default()
                    .fg(theme.info())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(")", Style::default().fg(theme.muted())),
        ]),
        Line::from(Span::styled(
            "Configured models will appear here",
            Style::default().fg(theme.muted()),
        )),
    ]
}

/// The model detail sub-view body: renders identity, capabilities, pricing, and live configuration or availability reason.
pub(crate) fn model_detail_body(
    rm: &RankedModel,
    _body_width: usize,
    theme: &Theme,
) -> Vec<SelectableRow> {
    let mut layout = DetailLayout::new(theme);
    let value_style = Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD);

    // 1. Identity section
    layout = layout.section("Identity");
    if let Some(name) = &rm.name {
        layout = layout.kv_styled("Display Name", name, value_style);
    }
    layout = layout.kv_styled("Wire ID", &rm.model, value_style);
    layout = layout.kv(
        "Connection",
        format!("{} ({})", rm.provider_label, rm.provider_id),
    );

    // 2. Capabilities section
    layout = layout.section("Capabilities");
    let ctx_formatted = rm.format_context_window();
    let ctx_display = if ctx_formatted.is_empty() {
        format!("{} tokens", rm.context_window)
    } else {
        format!("{} tokens ({ctx_formatted})", rm.context_window)
    };
    layout = layout.kv("Context Window", ctx_display);
    if let Some(vision) = rm.vision {
        layout = layout.kv("Vision", if vision { "Supported" } else { "Text only" });
    }
    if !rm.effort_levels.is_empty() {
        layout = layout.kv("Reasoning Tiers", rm.effort_levels.join(", "));
    }

    // 3. Pricing section (if declared or recognized)
    if let Some(pricing_rows) =
        render_pricing_detail_rows(&rm.provider_id, &rm.model, None, "Pricing", theme)
    {
        layout = layout.raw_rows(pricing_rows);
    }

    // 4. Status or Configuration section
    if rm.usable {
        layout = layout.section("Configuration");
        // Show the server's resolved value, never a locally invented default:
        // `rm.effort` is `None` only when the route exposes no depth knob.
        if let Some(current_effort) = rm.effort.as_deref() {
            layout = layout.kv_styled("Reasoning Effort", current_effort, value_style);
        }
        if let Some(thinking) = rm.thinking {
            let check = if thinking { "on" } else { "off" };
            let style = if thinking {
                Style::default().fg(theme.ok()).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.dim())
            };
            layout = layout.kv_styled("Thinking", check, style);
        }
        if rm.effort.is_none() && rm.thinking.is_none() {
            layout = layout.kv("Mode", "Standard");
        }
    } else {
        layout = layout.section("Availability");
        let status_desc = rm.display_status().unwrap_or("Disabled");
        layout = layout.kv_styled(
            "Status",
            status_desc,
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
        );
        let reason_text = rm
            .reason
            .as_deref()
            .unwrap_or("Model is currently unavailable.");
        layout = layout.kv_muted("Reason", reason_text);
        if let Some(reset_ms) = rm.resets_at_ms {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if reset_ms > now_ms {
                let rem_secs = (reset_ms - now_ms) / 1000;
                let mins = rem_secs.div_ceil(60);
                let dt = chrono::DateTime::from_timestamp_millis(reset_ms as i64);
                let time_str = dt
                    .map(|d| d.format("%H:%M").to_string())
                    .unwrap_or_default();
                let reset_desc = if mins > 60 {
                    let hrs = mins / 60;
                    let m = mins % 60;
                    format!("{time_str} (in {hrs}h {m}m)")
                } else {
                    format!("{time_str} (in {mins}m)")
                };
                layout = layout.kv("Resets", reset_desc);
            }
        }
        if rm.availability_overridden {
            layout = layout.kv_styled(
                "Scope Override",
                "Allowed by user scope",
                Style::default().fg(theme.ok()),
            );
        }
        if rm.availability_stale {
            layout = layout.kv_styled(
                "Notice",
                "Verdict observed before catalog refresh failure; may be stale upstream.",
                Style::default().fg(theme.dim()),
            );
        }
    }

    layout.build()
}
