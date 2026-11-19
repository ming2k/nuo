//! Pre-attach workspace trust interstitial surface.
//!
//! When the TUI attaches to a session whose project root carries
//! project-authored contributions that are awaiting a human decision —
//! `Quarantined`, `Changed`, or `Expired` per
//! [`WorkspaceSecuritySnapshot::domains_needing_review`] from the durable
//! `WorkspaceSecurityStore` — the PreAttach surface mounts before any chat
//! transcript, composer, or session chrome. It owns the entire terminal area
//! for the duration of the decision. While it is up there is no composer behind
//! it to fall through to, so it claims the keyboard — but a *decline* (Esc, or
//! trusting nothing) records the refusal and yields to the chat surface with
//! every affected domain unloaded. It deliberately does not quit: the gate also
//! opens for `Changed` content on a live session, and terminating the terminal
//! is not an answer to "someone changed AGENTS.md, do you still want it?".
//!
//! # Composition
//!
//! The surface is the whole screen, so its composition *is* the design. Four
//! rules govern it, and each replaced a specific defect:
//!
//! 1. **Sections are separated by area, never by a rule glyph.** The decision
//!    block is a raised band ([`Theme::raised`]) on the pure-black backdrop with
//!    the pointed-at row one step brighter ([`Theme::selected`]); the bands
//!    carry the grouping. A horizontal rule spends a row to say what a
//!    background step already says, and reads as a table where there is none.
//!
//! 2. **Everything on one line is one field.** The title is the title. The
//!    workspace path is *context*, so it gets its own line in the dimmest tone —
//!    never `Workspace trust · ~/projects/nuo`, where a `·` had to invent a
//!    relationship between two fields the reader must then decode.
//!
//! 3. **Status is a column, not punctuation.** The asset name is left-aligned
//!    and its [`TrustStatus`](view::trust_gate::TrustStatus) is right-aligned in
//!    its own tone ([`view::trust_gate::TrustStatus::tone`]). The earlier
//!    `"Project instructions  |  [Changed]"` spent five columns on `  |  [` so
//!    the eye had to parse a separator to find a word, and the brackets collided
//!    with the two other things brackets mean in this product (`[ROLE]` badges
//!    and `[N queued]` counts).
//!
//! 4. **The footer advertises only the keys that exist.** A one-row gate
//!    advertises `Enter` and `Esc`; `↑↓ move` and `Space toggle` appear only when
//!    there is more than one row to move between or toggle. The earlier footer
//!    spent half its width on four affordances regardless, which left the two
//!    that actually end the decision as the quietest things on screen.
//!
//! The trust question wording, the domain rows, and the answer-to-command
//! mapping remain owned by [`view::trust_gate`]; this module only owns the
//! surface. [`crate::shell::question_model::QuestionModel`] is reused as the
//! navigation/selection state machine, exactly as the Question sheet does for
//! AI-initiated `ask_user` — its MVU purity keeps the input path testable
//! without a terminal.
//!
//! [`WorkspaceSecuritySnapshot::domains_needing_review`]: protocol::WorkspaceSecuritySnapshot::domains_needing_review
use graticule::{Alignment, Block, Color, Frame, Line, Modifier, Paragraph, Rect, Style};
use protocol::WorkspaceSecuritySnapshot;
use ui_primitives::components::keycap::{AffordanceIntent, KeyAffordance, layout_affordances};
use ui_primitives::components::options::{ChoiceMarker, ChoiceStyle, push_wrapped_styled};
use ui_primitives::components::row::{ListRow, RowGroup, RowStyledAtom};
use ui_primitives::primitives::contrast_fg;
use view::trust_gate::{self, TrustDomainView};

use crate::shell::question_model::{QuestionAction, QuestionEffect, QuestionModel};
use crate::shell::render::Theme;

/// The widest the panel ever gets, in columns.
///
/// This is a **reading measure**, not a fraction of the screen. Past roughly 84
/// columns a line of prose stops getting easier to read and starts getting
/// harder — the eye has further to travel back to the next row — and this panel's
/// rows are a name with a two-word status pushed to opposite edges, so a wider
/// measure does not add legibility, it adds a gulf between the two fields. The
/// terminal's spare columns become margin, which is where a centred composition
/// wants them.
const PANEL_MAX_WIDTH: u16 = 84;

/// Columns kept clear between the panel and the terminal frame, so the centred
/// block never touches the edge.
const PANEL_H_MARGIN: u16 = 2;

/// The runtime state of the PreAttach surface.
///
/// Owns the trust-gate question's input state machine plus a flag
/// distinguishing a real review gate from a `NUO_FORCE_PRE_ATTACH=1` acceptance
/// fixture (which renders an extra banner so operators know they are in
/// acceptance mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreAttachState {
    /// The trust-gate question, modeled by [`QuestionModel`] for its
    /// navigation/selection logic. Always carries the constant
    /// [`trust_gate::TRUST_GATE_REQUEST_ID`] so the existing reply path can still
    /// recognize its answers.
    model: QuestionModel,
    /// `true` when this PreAttach was force-mounted by the `NUO_FORCE_PRE_ATTACH=1`
    /// acceptance env var (no real workspace awaiting review). The surface
    /// renders an extra banner naming the env var so operators know why they are
    /// seeing it.
    acceptance: bool,
    /// `true` once the user has submitted a decision to trust, providing
    /// immediate visual feedback and swallowing further keystrokes while the
    /// trusted snapshot round-trips.
    submitting: bool,
    /// The server's attestation outcome for the last submitted decision:
    /// `Some(message)` when the grant could not be persisted (e.g. the durable
    /// trust store is unavailable). Rendered on the surface so a failed
    /// confirmation is an actionable message instead of a silent loop of the
    /// same dialog, and reset with `submitting` so the user can retry or
    /// decline.
    attestation_error: Option<String>,
    /// The assets this gate offered — every one awaiting a decision when it
    /// mounted, with the status that put it there.
    ///
    /// Captured at mount rather than re-derived from a later snapshot that may
    /// have moved. A `Decline` refuses exactly this set.
    ///
    /// The invariant that makes the pairing in [`Self::apply`] and
    /// [`draw_pre_attach`] safe: this vector and the question's `options` are
    /// both produced by [`trust_gate::gate_request`] from
    /// [`trust_gate::domain_rows`], in that order, for the same snapshot, so
    /// `rows[i]` describes `options[i]` for every `i`. It is asserted by
    /// `every_offered_row_lines_up_with_its_option`.
    rows: Vec<TrustDomainView>,
    /// The workspace the decision applies to, tilde-shortened. Rendered only
    /// when the offered set needs it (see [`Self::shows_workspace`]).
    root: String,
}

/// The signal the listener task raises when a freshly-arrived `HarnessState`
/// snapshot demands a trust decision.
///
/// Drains to `None` once the per-frame sync has mounted [`PreAttachState`].
/// Carries the snapshot itself rather than a pre-built request so the loop can
/// decide — based on `trust_gate_dismissed` and the currently-mounted surface —
/// whether to mount, refresh, or ignore.
#[derive(Debug, Clone)]
pub struct PreAttachSignal {
    pub snapshot: WorkspaceSecuritySnapshot,
}

/// The outcome of a user decision on the PreAttach surface.
///
/// Pure data — the input path returns this and the caller (the event loop)
/// carries out the side effect. Mirrors the `QuestionEffect`-to-side-effect
/// boundary the Question sheet uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreAttachDecision {
    /// User chose to trust the workspace. Direct control-plane admission
    /// request — does not route through slash commands or pollute the transcript.
    Trust { domains: Vec<protocol::TrustDomain> },
    /// User refused the offered domains (`Esc`, or confirming an empty
    /// selection). The refusal is recorded durably, so the workspace stops
    /// asking about *this* content, and the session continues untrusted.
    ///
    /// A refusal used to mean `Quit`, which was defensible while the gate could
    /// only open on a never-trusted workspace: an untrusted workspace had no
    /// useful work to do, so there was no chat surface to fall through to. It is
    /// no longer defensible now that `Changed` gates too — that workspace is
    /// mid-session, with a transcript, a task list, and a user who only wanted
    /// to say "no" to a diff. Quitting the terminal is not an answer to that
    /// question, so the refusal now records itself and yields to the chat
    /// surface with every affected domain still unloaded.
    Decline { domains: Vec<protocol::TrustDomain> },
}

impl PreAttachState {
    /// Build a PreAttach surface from a snapshot awaiting review. Returns `None`
    /// when nothing does (`Absent` / `Trusted` / `Denied`) — the
    /// [`trust_gate::gate_request`] rules are authoritative.
    pub fn from_snapshot(snapshot: &WorkspaceSecuritySnapshot) -> Option<Self> {
        let request = trust_gate::gate_request(snapshot)?;
        Some(Self::mount(request, snapshot, false))
    }

    /// Build an acceptance fixture: a synthesized snapshot with every domain
    /// awaiting review, so every domain row appears in the rendered prompt and
    /// operators can verify the visual design end-to-end without preparing a
    /// real workspace.
    ///
    /// Synthesis is deterministic — a snapshot with every domain awaiting review
    /// must produce a gate request — so the `expect` states an invariant rather
    /// than handling a case.
    #[allow(clippy::expect_used)]
    pub fn acceptance_fixture() -> Self {
        let mut snapshot = WorkspaceSecuritySnapshot::new("/tmp/nuo-acceptance");
        snapshot.mcp = protocol::WorkspaceTrustState::Quarantined;
        snapshot.skills = protocol::WorkspaceTrustState::Quarantined;
        snapshot.hooks = protocol::WorkspaceTrustState::Quarantined;
        snapshot.instructions = protocol::WorkspaceTrustState::Quarantined;
        snapshot.ex_workspace = protocol::WorkspaceTrustState::Quarantined;
        snapshot.user_assets = protocol::WorkspaceTrustState::Quarantined;
        let request = trust_gate::gate_request(&snapshot)
            .expect("a snapshot with every domain awaiting review must produce a gate request");
        Self::mount(request, &snapshot, true)
    }

    fn mount(
        request: tool::UserQuestionRequest,
        snapshot: &WorkspaceSecuritySnapshot,
        acceptance: bool,
    ) -> Self {
        let rows = trust_gate::domain_rows(snapshot);
        // A closed question: the answer is a subset of the offered assets, and
        // "none of these, here is my own answer" is not one. See
        // [`QuestionModel::open_closed`].
        let mut model = QuestionModel::open_closed(request);
        // Every offered asset starts trusted: the gate asks "may this load?",
        // and a user who presses `Enter` without reading should get the content
        // the workspace was authored with, not a silently empty session.
        model.select_all(0);
        Self {
            model,
            acceptance,
            submitting: false,
            attestation_error: None,
            rows,
            root: ui_primitives::components::path::tilde_shorten(std::path::Path::new(
                &snapshot.root,
            )),
        }
    }

    /// `true` when this surface was force-mounted by `NUO_FORCE_PRE_ATTACH=1`.
    /// The renderer paints an extra banner so operators can tell acceptance mode
    /// from a real review gate at a glance.
    pub fn acceptance(&self) -> bool {
        self.acceptance
    }

    /// `true` when a trust decision has been submitted and is pending unmount.
    pub fn submitting(&self) -> bool {
        self.submitting
    }

    /// The server's attestation outcome for the last submitted decision, if it
    /// failed. `Some` also implies `!submitting`: the round-trip resolved with a
    /// failure, so the surface is interactive again and renders the failure
    /// where the submitting state was.
    pub fn attestation_error(&self) -> Option<&str> {
        self.attestation_error.as_deref()
    }

    /// Record a failed trust-grant round-trip. Resets `submitting` so the
    /// surface resumes the decision instead of spinning, and keeps the question
    /// mounted — the user retries or declines with full knowledge of why the
    /// last confirmation did not take effect.
    pub fn note_attestation_error(&mut self, message: String) {
        self.submitting = false;
        self.attestation_error = Some(message);
    }

    /// The trust-gate [`QuestionModel`] backing this surface. The renderer reads
    /// its current question, options, and highlight straight from this; the
    /// input path mutates it through [`PreAttachState::apply`].
    pub fn model(&self) -> &QuestionModel {
        &self.model
    }

    /// The offered assets, in display order. Paired index-for-index with
    /// `model().request().questions[0].options`; see [`Self::rows`].
    pub fn rows(&self) -> &[TrustDomainView] {
        &self.rows
    }

    /// Whether the workspace path is worth a line.
    ///
    /// A project-scoped decision happens inside the directory the user just
    /// opened, so the path is zero information and costs a line. It becomes
    /// load-bearing exactly when the offered set crosses the project boundary —
    /// a user-global asset is on the list, and "is this my project or my whole
    /// machine?" is the question that decides it.
    pub fn shows_workspace(&self) -> bool {
        !self.root.is_empty()
            && self
                .rows
                .iter()
                .any(|row| matches!(row.domain, protocol::TrustDomain::UserAssets))
    }

    /// The domains this gate offered, in display order.
    pub fn offered_domains(&self) -> Vec<protocol::TrustDomain> {
        self.rows.iter().map(|row| row.domain).collect()
    }

    /// Apply a navigation/selection action. Returns `Some(decision)` when the
    /// action resolves the surface (a trust choice or a dismissal); `None` when
    /// the action only moved the highlight and the surface should keep running.
    ///
    /// Mirrors the [`QuestionModel::update`] take-and-replace MVU pattern:
    /// `QuestionModel::update` consumes `self`, so this swaps the inner model
    /// out, runs the update, and writes the result back.
    pub fn apply(&mut self, action: QuestionAction) -> Option<PreAttachDecision> {
        if self.submitting {
            // A submitted decision is already in flight; the server's republished
            // snapshot is the authority on what it did. Cancelling here used to
            // emit a refusal of *every* offered domain — including the ones just
            // submitted — so the outcome depended on whether the grant or the
            // refusal reached the store first. That is a race with a security
            // answer, so the surface simply stays inert until the snapshot
            // resolves it (`attestation_error` re-opens the decision on failure).
            return None;
        }
        // Clone-aside pattern: take the model, update, write back.
        let placeholder = self.model.clone();
        let model = std::mem::replace(&mut self.model, placeholder);
        let (new_model, effects) = model.update(action);
        self.model = new_model;

        for effect in effects {
            match effect {
                QuestionEffect::Reply { answers, .. } => {
                    let offered = self.offered_domains();
                    let selected = trust_gate::answer_to_domains(&answers, &offered);
                    // Anything the user left unselected is a refusal, and it is
                    // named here rather than inferred later: the domains that
                    // were *offered* are the ones to refuse, not merely the ones
                    // still pending after a partial grant.
                    let declined = offered
                        .iter()
                        .copied()
                        .filter(|domain| !selected.contains(domain))
                        .collect();
                    self.submitting = true;
                    if selected.is_empty() {
                        // Nothing selected: this is a refusal, and dispatching a
                        // grant for zero domains would ask the server to do
                        // nothing and leave the gate to re-open.
                        return Some(PreAttachDecision::Decline { domains: declined });
                    }
                    return Some(PreAttachDecision::Trust { domains: selected });
                }
                QuestionEffect::Cancelled { .. } => {
                    return Some(PreAttachDecision::Decline {
                        domains: self.offered_domains(),
                    });
                }
                // `Closed` always follows `Reply` or `Cancelled` in the same
                // effect batch (see `QuestionModel::update`), so the decision has
                // already been emitted and we simply stop iterating. Reaching
                // this arm first would indicate a QuestionModel bug — fall
                // through to "no decision" defensively.
                QuestionEffect::Closed { .. } => continue,
            }
        }
        None
    }
}

/// Render the PreAttach interstitial.
pub fn draw_pre_attach(f: &mut Frame, state: &PreAttachState, theme: &Theme) {
    let area = f.area();

    // Full-screen pure black background — the user-requested "全部黑屏" visual.
    // Distinct from `theme.surface()` (the chat surface) so the operator cannot
    // mistake PreAttach for a live chat.
    f.render_widget(
        &mut Block::default().style(Style::default().bg(theme.backdrop)),
        area,
    );

    let width = panel_width(area);
    let body_width = width as usize;

    // Sections, in paint order. Context is built behind a flag so a short
    // terminal drops it before it drops the decision: the rows, their reasons,
    // and the keys that answer them survive every geometry.
    let mut sections = Sections {
        context: context_lines(state, theme, body_width),
        title: title_lines(state, theme, body_width),
        headline: headline_lines(state, theme, body_width),
        rows: row_lines(state, theme, body_width),
        consequence: consequence_lines(theme, body_width),
        footer: footer_lines(state, theme, body_width),
    };
    let mut lines = sections.join();
    if lines.len() as u16 > area.height {
        sections.context.clear();
        lines = sections.join();
    }
    if lines.len() as u16 > area.height {
        sections.consequence.clear();
        lines = sections.join();
    }

    let panel = centered_panel(area, width, lines.len() as u16);
    f.render_widget(
        &mut Paragraph::new(lines)
            .alignment(Alignment::Left)
            .style(Style::default().bg(theme.backdrop)),
        panel,
    );
}

/// The panel's sections, in paint order.
struct Sections {
    /// Context above the title: acceptance mode, a failed attestation, the
    /// workspace path. Never the decision.
    context: Vec<Line<'static>>,
    /// The surface's name (and the in-flight notice).
    title: Vec<Line<'static>>,
    /// Why the gate is open.
    headline: Vec<Line<'static>>,
    /// The decision band.
    rows: Vec<Line<'static>>,
    /// What an unticked row means.
    consequence: Vec<Line<'static>>,
    /// The keys that answer the question.
    footer: Vec<Line<'static>>,
}

impl Sections {
    /// Concatenate with exactly one blank row between adjacent sections.
    ///
    /// A blank row rather than a rule: the sections are already distinct
    /// surfaces and tones, so a line glyph would spend a row to restate a
    /// boundary the background step already draws — and would read as a table
    /// header where there is no table. An empty section contributes nothing,
    /// including no separator.
    fn join(&self) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        for section in [
            &self.context,
            &self.title,
            &self.headline,
            &self.rows,
            &self.consequence,
            &self.footer,
        ] {
            if section.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.extend(section.iter().cloned());
        }
        out
    }
}

/// Context above the title: the acceptance banner, a failed attestation, and
/// the workspace path when it disambiguates. Every one of these is *context*,
/// never the decision.
fn context_lines(state: &PreAttachState, theme: &Theme, body_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if state.acceptance() {
        push_wrapped_styled(
            &mut lines,
            "",
            "",
            "Acceptance mode (NUO_FORCE_PRE_ATTACH=1) — this decision persists against the real workspace.",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
        lines.push(Line::default());
    }

    if let Some(error) = state.attestation_error() {
        push_wrapped_styled(
            &mut lines,
            "",
            "",
            "Trust was not applied",
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
        push_wrapped_styled(
            &mut lines,
            "",
            "",
            error,
            Style::default().fg(theme.err()),
            body_width,
        );
    }

    if state.shows_workspace() {
        push_wrapped_styled(
            &mut lines,
            "",
            "",
            &state.root,
            Style::default().fg(theme.dim()),
            body_width,
        );
    }

    lines
}

/// The surface's name. Not bracketed — brackets are this product's badge and
/// count slots.
fn title_lines(state: &PreAttachState, theme: &Theme, body_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    push_wrapped_styled(
        &mut lines,
        "",
        "",
        trust_gate::GATE_TITLE,
        Style::default()
            .fg(theme.brand())
            .add_modifier(Modifier::BOLD),
        body_width,
    );
    // While a decision is in flight the panel keeps its identity and states the
    // one thing that is true: the wait. Three sentences said this before.
    if state.submitting() {
        push_wrapped_styled(
            &mut lines,
            "",
            "",
            "Applying trust settings…",
            Style::default()
                .fg(theme.running())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
    }
    lines
}

/// Why the gate is open — one line per reason class present, and nothing else.
fn headline_lines(state: &PreAttachState, theme: &Theme, body_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let Some(question) = state.model().request().questions.first() else {
        return lines;
    };
    push_wrapped_styled(
        &mut lines,
        "",
        "",
        &question.question,
        Style::default().fg(theme.fg()),
        body_width,
    );
    lines
}

/// The decision band: one row per offered asset, name left, status right.
fn row_lines(state: &PreAttachState, theme: &Theme, body_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    // A submitted decision is in flight: the rows are no longer answerable, so
    // they are not painted at all rather than painted and ignored.
    if state.submitting() {
        return lines;
    }
    let Some(question) = state.model().request().questions.first() else {
        return lines;
    };
    if body_width == 0 {
        return lines;
    }

    for (row, point) in state.rows().iter().zip(row_points(state, question)) {
        lines.push(render_row(
            row,
            point,
            question.multi_select,
            theme,
            body_width,
        ));
    }
    lines
}

/// Whether a row is pointed at, ticked, and where it sits in the band.
#[derive(Clone, Copy)]
struct RowPoint {
    highlighted: bool,
    checked: bool,
}

/// Resolve each offered row's cursor and tick state, index-paired with the
/// rendered rows.
fn row_points<'a>(
    state: &'a PreAttachState,
    question: &'a tool::UserQuestion,
) -> impl Iterator<Item = RowPoint> + 'a {
    let highlighted = state.model().highlight();
    let selected = state
        .model()
        .selected()
        .first()
        .map(Vec::as_slice)
        .unwrap_or_default();
    state.rows().iter().enumerate().map(move |(idx, row)| {
        // `rows[i]` describes `options[i]`: both come from
        // `trust_gate::domain_rows` for the same snapshot, in that order.
        debug_assert!(
            question
                .options
                .get(idx)
                .is_some_and(|option| option.label == row.label),
            "the offered rows and the question's options must stay index-paired"
        );
        RowPoint {
            highlighted: idx == highlighted,
            checked: selected.contains(&idx),
        }
    })
}

/// Paint one decision row: marker, name, and the status column.
///
/// A checkbox only appears when the gate offers a choice of assets. One asset is
/// a binary decision — the highlighted row *is* the answer — so a checkbox there
/// would be a control with nothing to control, exactly as a single-select
/// question sheet shows no radio marker.
fn render_row(
    row: &TrustDomainView,
    point: RowPoint,
    multi_select: bool,
    theme: &Theme,
    body_width: usize,
) -> Line<'static> {
    // The pointed-at row rides one luminance step above the resting band, so the
    // cursor and the selection are two signals: bright band = "you are here",
    // filled box = "this one is included".
    let bg = if point.highlighted {
        theme.selected()
    } else {
        theme.raised()
    };
    let marker = if multi_select {
        ChoiceMarker::Checkbox
    } else {
        ChoiceMarker::None
    };
    let fg = if point.highlighted {
        contrast_fg(bg)
    } else {
        theme.fg()
    };
    let style = ChoiceStyle {
        bg,
        fg,
        dim: if point.highlighted {
            contrast_fg(bg)
        } else {
            theme.muted()
        },
    };

    // On the pointed-at band the marker and the status join the row's own
    // contrast pair; the `ok`/`dim` tones are calibrated against the resting
    // band and would sink into a highlighted one.
    let marker_style = if point.highlighted {
        Style::default()
            .bg(bg)
            .fg(contrast_fg(bg))
            .add_modifier(Modifier::BOLD)
    } else if point.checked {
        Style::default()
            .bg(bg)
            .fg(theme.ok())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(bg).fg(theme.dim())
    };
    let label_style = if point.highlighted {
        Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(bg).fg(fg)
    };
    let status_style = Style::default().bg(bg).fg(if point.highlighted {
        style.dim
    } else {
        row.status.tone(theme)
    });

    // Marker and name are *one* field — the marker qualifies the name — so they
    // share a group and one column of air. Two fixed groups would also insert
    // `GROUP_GAP` (2) between them, reading as three fields where there are two.
    ListRow::new(style, body_width)
        .group(
            RowGroup::fixed()
                .styled(
                    RowStyledAtom {
                        text: style.marker_glyph(marker, point.checked).to_string(),
                        style: marker_style,
                    },
                    0,
                )
                .styled(
                    RowStyledAtom {
                        text: row.label.to_string(),
                        style: label_style,
                    },
                    if marker == ChoiceMarker::None { 0 } else { 1 },
                ),
        )
        .group(RowGroup::trailing().styled(
            RowStyledAtom {
                text: row.status.token().to_string(),
                style: status_style,
            },
            0,
        ))
        .finish()
}

/// What an unticked row means. Stated once, in a secondary tone, beneath the
/// band — it answers a different question than the headline does.
fn consequence_lines(theme: &Theme, body_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    push_wrapped_styled(
        &mut lines,
        "",
        "",
        trust_gate::GATE_CONSEQUENCE,
        Style::default().fg(theme.muted()),
        body_width,
    );
    lines
}

/// The keys that answer the question — only the ones that exist.
fn footer_lines(state: &PreAttachState, theme: &Theme, body_width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if state.submitting() {
        return lines;
    }

    let count = state.rows().len();
    let mut affordances: Vec<KeyAffordance> = Vec::new();
    // Both verbs are two words. The panel is 56 columns at an 80-column
    // terminal, and a footer that spells out "continue untrusted" pushes the
    // navigation affordances of a multi-row gate into `F1 more` — a key legend
    // that hides the keys the surface actually has. What `Esc` leaves behind is
    // stated by the consequence line directly above, so the key only has to name
    // the act.
    affordances.push(KeyAffordance::with_intent_and_rank(
        "Enter",
        "trust",
        AffordanceIntent::Primary,
        80,
    ));
    affordances.push(KeyAffordance::with_intent_and_rank(
        "Esc",
        "skip",
        AffordanceIntent::Neutral,
        100,
    ));
    if count > 1 {
        affordances.push(KeyAffordance::with_intent_and_rank(
            ui_primitives::keyvocab::ARROWS_UD,
            "move",
            AffordanceIntent::Neutral,
            60,
        ));
        affordances.push(KeyAffordance::with_intent_and_rank(
            ui_primitives::keyvocab::SPACE,
            "toggle",
            AffordanceIntent::Neutral,
            50,
        ));
    }

    let layout = layout_affordances(&affordances, body_width, theme, Color::Reset, Some("more"));
    lines.push(Line::from(layout.spans));
    lines
}

/// The panel's width: a reading measure that yields to the screen.
///
/// The measure is the design constraint and the terminal is the hard one — the
/// *opposite* of a percentage of the viewport. A percentage made the panel narrow
/// on a narrow terminal (where the prompt most needs its columns) and wide on a
/// wide one (where the eye least wants the travel), so the same sentence wrapped
/// twice at 80 columns and once at 120 for no reason of legibility. Here the
/// panel claims its measure whenever the screen can fund it, and gives back
/// exactly what the frame's margin needs when it cannot.
pub(crate) fn panel_width(area: Rect) -> u16 {
    area.width
        .saturating_sub(PANEL_H_MARGIN)
        .min(PANEL_MAX_WIDTH)
}

/// Center a `width` × `height` panel inside `area`.
///
/// The height is the content's own: the panel is exactly as tall as what it
/// paints, then centered. Sizing to a fixed slab (`clamp(12, 28)`) pinned a
/// short prompt to the top of the screen and left the bottom two-thirds black,
/// which read as a rendering bug rather than as a decision surface.
fn centered_panel(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests;
