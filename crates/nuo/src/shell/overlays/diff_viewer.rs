//! Fullscreen diff viewer scene (`SceneKind::Diff`).
//!
//! Displays file changes accumulated in the current session (from `edit_text`,
//! `write_file`, etc.) with hunk/line diff highlighting, file-by-file
//! navigation (`n`/`p`), and per-file reviewed toggles (`r`).

use graticule::{
    Constraint, Direction, Frame, Layout, Line, Modifier, Paragraph, Rect, Span, Style,
};
use unicode_width::UnicodeWidthStr;

use crate::shell::render::{Theme, draw_scene_bar, draw_tab_bar};
use nuo_ui_primitives::primitives::viewport_rect;
use nuo_view::disclosure::renderers::base::truncate_to_width;
use nuo_view::text_layout::padded_tail;
use nuo_view::view_header::{TabBarNavRects, TabBarProps, ViewHints};

/// Kind of a single line in a unified diff view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Header,
    Context,
    Add,
    Remove,
}

/// One rendered line in a file's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffViewerLine {
    pub kind: DiffLineKind,
    pub text: String,
}

/// One changed file in the diff viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileEntry {
    pub path: String,
    pub op_label: &'static str,
    pub additions: usize,
    pub deletions: usize,
    pub reviewed: bool,
    pub lines: Vec<DiffViewerLine>,
}

/// State owned by the fullscreen diff viewer scene.
#[derive(Debug, Clone, Default)]
pub struct DiffViewerState {
    pub files: Vec<DiffFileEntry>,
    pub selected_file: usize,
    pub scroll: usize,
}

impl DiffViewerState {
    pub fn current_file(&self) -> Option<&DiffFileEntry> {
        self.files.get(self.selected_file)
    }

    pub fn select_next_file(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.selected_file = (self.selected_file + 1) % self.files.len();
        self.scroll = 0;
    }

    pub fn select_prev_file(&mut self) {
        if self.files.is_empty() {
            return;
        }
        self.selected_file = if self.selected_file == 0 {
            self.files.len() - 1
        } else {
            self.selected_file - 1
        };
        self.scroll = 0;
    }

    pub fn toggle_reviewed(&mut self) {
        if let Some(file) = self.files.get_mut(self.selected_file) {
            file.reviewed = !file.reviewed;
        }
    }

    pub fn reviewed_count(&self) -> usize {
        self.files.iter().filter(|f| f.reviewed).count()
    }
}

/// Build unified diff lines from `old` and `new` strings.
pub fn build_diff_lines(old: &str, new: &str) -> (Vec<DiffViewerLine>, usize, usize) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_end = old_lines.len().saturating_sub(suffix);
    let new_end = new_lines.len().saturating_sub(suffix);

    let mut out = Vec::new();
    let ctx_before = prefix.saturating_sub(3);
    let ctx_after = (old_end + 3).min(old_lines.len());

    out.push(DiffViewerLine {
        kind: DiffLineKind::Header,
        text: format!(
            "@@ -{},{} +{},{} @@",
            ctx_before + 1,
            old_end.saturating_sub(ctx_before),
            ctx_before + 1,
            new_end.saturating_sub(ctx_before)
        ),
    });

    for line in &old_lines[ctx_before..prefix] {
        out.push(DiffViewerLine {
            kind: DiffLineKind::Context,
            text: format!(" {line}"),
        });
    }

    let mut deletions = 0usize;
    for line in &old_lines[prefix..old_end] {
        deletions += 1;
        out.push(DiffViewerLine {
            kind: DiffLineKind::Remove,
            text: format!("-{line}"),
        });
    }

    let mut additions = 0usize;
    for line in &new_lines[prefix..new_end] {
        additions += 1;
        out.push(DiffViewerLine {
            kind: DiffLineKind::Add,
            text: format!("+{line}"),
        });
    }

    for line in &old_lines[old_end..ctx_after] {
        out.push(DiffViewerLine {
            kind: DiffLineKind::Context,
            text: format!(" {line}"),
        });
    }

    (out, additions, deletions)
}

/// Rects reported by [`draw_diff_scene`].
#[derive(Debug, Clone, Default)]
pub struct DiffSceneRects {
    pub area: Rect,
    pub body: Rect,
    pub tab_bar_rect: Option<Rect>,
    pub tab_rects: Vec<(usize, Rect)>,
    pub tab_close_rects: Vec<(usize, Rect)>,
    pub tab_add_rect: Option<Rect>,
    pub tab_reveal_active: bool,
    pub tab_shimmer_active: bool,
    pub tab_prev_rect: Option<Rect>,
    pub tab_next_rect: Option<Rect>,
    pub tab_overflow_rect: Option<Rect>,
    pub tab_notices_rect: Option<Rect>,
    pub tab_help_rect: Option<Rect>,
}

/// Props for [`draw_diff_scene`].
pub struct DiffSceneProps<'a> {
    pub state: Option<&'a mut DiffViewerState>,
    pub tab_bar: Option<TabBarProps<'a>>,
    /// Client/server lifecycle warning, surfaced in Tier 2 `SceneBar` when degraded.
    pub lifecycle_status: Option<&'a str>,
}

/// Render the fullscreen diff viewer scene.
pub fn draw_diff_scene(
    frame: &mut Frame,
    props: DiffSceneProps<'_>,
    theme: &Theme,
) -> DiffSceneRects {
    // Fullscreen surface: clear and paint the app backdrop directly so Diff
    // never relies on an underlying transcript pass.
    frame.render_widget(&mut graticule::Clear::default(), frame.area());
    frame.render_widget(
        &mut graticule::Block::default().style(Style::default().bg(theme.app_bg)),
        frame.area(),
    );
    let area = viewport_rect(frame);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
    let (head, _rule, file_bar, body, footer) =
        (chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]);

    let tab_bar_rect = props.tab_bar.as_ref().map(|_| Rect { height: 1, ..head });
    let tab_rects = match props.tab_bar {
        Some(tab_props) => draw_tab_bar(frame, Rect { height: 1, ..head }, &tab_props, theme),
        None => TabBarNavRects::default(),
    };

    let (context, file_header) = match props.state.as_deref() {
        Some(state) if !state.files.is_empty() => {
            let file = &state.files[state.selected_file.min(state.files.len() - 1)];
            let reviewed_mark = if file.reviewed { "[reviewed] " } else { "" };
            let ctx = format!(
                "{}/{} files ({} reviewed)",
                state.selected_file + 1,
                state.files.len(),
                state.reviewed_count()
            );
            let hdr = format!(
                " {reviewed_mark}{} {}  +{} -{}",
                file.op_label, file.path, file.additions, file.deletions
            );
            (ctx, hdr)
        }
        _ => (
            "0 files".to_string(),
            " No file diffs recorded in this session".to_string(),
        ),
    };

    let (scene_context, scene_warn) = match props.lifecycle_status {
        Some(status) => (status, true),
        None => (context.as_str(), false),
    };
    draw_scene_bar(
        frame,
        Rect {
            y: head.y.saturating_add(1),
            height: 1,
            ..head
        },
        &ViewHints {
            kind: crate::shell::surfaces::SceneKind::Diff,
            role: None,
            context: Some(scene_context),
            context_warn: scene_warn,
            breadcrumbs: None,
            can_back: false,
            can_forward: false,
        },
        None,
        theme,
    );

    let panel_bg = theme.panel();
    let hdr_trunc = truncate_to_width(&file_header, file_bar.width as usize);
    frame.render_widget(
        &mut Paragraph::new(Line::from(vec![
            Span::styled(
                hdr_trunc.to_string(),
                Style::default()
                    .bg(panel_bg)
                    .fg(theme.fg())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                padded_tail(file_bar.width as usize, hdr_trunc.width()),
                Style::default().bg(panel_bg),
            ),
        ])),
        file_bar,
    );

    let bg = theme.code_surface();
    let body_h = body.height as usize;

    if let Some(state) = props.state {
        if let Some(file) = state.files.get(state.selected_file).cloned() {
            let total = file.lines.len();
            let max_scroll = total.saturating_sub(body_h);
            state.scroll = state.scroll.min(max_scroll);

            for row_idx in 0..body_h {
                let rect = Rect::new(body.x, body.y + row_idx as u16, body.width, 1);
                let line_idx = state.scroll + row_idx;
                if let Some(line) = file.lines.get(line_idx) {
                    let (fg, line_bg) = match line.kind {
                        DiffLineKind::Header => (theme.affordance_fg, bg),
                        DiffLineKind::Context => (theme.code_text(), bg),
                        DiffLineKind::Add => (theme.code_text(), theme.diff_add_bg),
                        DiffLineKind::Remove => (theme.code_text(), theme.diff_del_bg),
                    };
                    let trunc = truncate_to_width(&line.text, body.width as usize);
                    frame.render_widget(
                        &mut Paragraph::new(Line::from(vec![
                            Span::styled(trunc.to_string(), Style::default().fg(fg).bg(line_bg)),
                            Span::styled(
                                padded_tail(body.width as usize, trunc.width()),
                                Style::default().bg(line_bg),
                            ),
                        ])),
                        rect,
                    );
                } else {
                    frame.render_widget(
                        &mut Paragraph::new(Line::from(Span::styled(
                            padded_tail(body.width as usize, 0),
                            Style::default().bg(bg),
                        ))),
                        rect,
                    );
                }
            }
        }
    } else {
        for row_idx in 0..body_h {
            let rect = Rect::new(body.x, body.y + row_idx as u16, body.width, 1);
            frame.render_widget(
                &mut Paragraph::new(Line::from(Span::styled(
                    padded_tail(body.width as usize, 0),
                    Style::default().bg(bg),
                ))),
                rect,
            );
        }
    }

    let footer_text = " j/k scroll   n/p next/prev file   r toggle reviewed   Esc/q back";
    let footer_trunc = truncate_to_width(footer_text, footer.width as usize);
    frame.render_widget(
        &mut Paragraph::new(Line::from(vec![
            Span::styled(
                footer_trunc.to_string(),
                Style::default().fg(theme.dim()).bg(panel_bg),
            ),
            Span::styled(
                padded_tail(footer.width as usize, footer_trunc.width()),
                Style::default().bg(panel_bg),
            ),
        ])),
        footer,
    );

    DiffSceneRects {
        area,
        body,
        tab_bar_rect,
        tab_rects: tab_rects.tab_rects,
        tab_close_rects: tab_rects.close_rects,
        tab_add_rect: tab_rects.add_rect,
        tab_reveal_active: tab_rects.reveal_active,
        tab_shimmer_active: tab_rects.shimmer_active,
        tab_prev_rect: tab_rects.prev_rect,
        tab_next_rect: tab_rects.next_rect,
        tab_overflow_rect: tab_rects.overview_rect,
        tab_notices_rect: tab_rects.notices_rect,
        tab_help_rect: tab_rects.help_rect,
    }
}
