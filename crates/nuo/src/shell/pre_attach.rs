//! Pre-attach workspace trust interstitial surface.
//!
//! When the TUI attaches to a session whose project root carries
//! project-authored contributions that are awaiting a human decision —
//! `Quarantined`, `Changed`, or `Expired` per
//! [`WorkspaceSecuritySnapshot::domains_needing_review`] from the durable
//! `WorkspaceSecurityStore` — the PreAttach surface mounts before any
//! chat transcript, composer, or session chrome. Visually it is a
//! full-screen black background with a centered trust prompt and an
//! option list navigated by `↑`/`↓`, selected by `Enter`/`Space`, and
//! declined by `Esc`. Selection is shown as a background highlight, not a
//! cursor marker.
//!
//! The surface owns the entire terminal area for the duration of the
//! trust decision. While it is up there is no composer behind it to fall
//! through to, so it claims the keyboard — but a *decline* (Esc, or
//! trusting nothing) records the refusal and yields to the chat surface
//! with every affected domain unloaded. It deliberately does not quit:
//! the gate also opens for `Changed` content on a live session, and
//! terminating the terminal is not an answer to "someone changed
//! AGENTS.md, do you still want it?".
//!
//! The trust question wording, domain listing, and answer-to-command
//! mapping remain owned by [`nuo_view::trust_gate`]; this module only
//! owns the surface placement. [`crate::shell::question_model::QuestionModel`]
//! is reused as the navigation/selection state machine, exactly as
//! the Question sheet does for AI-initiated `ask_user` — its MVU
//! purity keeps the input path testable without a terminal.
use nuo_protocol::{WorkspaceSecuritySnapshot, WorkspaceTrustState};
use nuotc::{Alignment, Block, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};

use crate::shell::question_model::{QuestionAction, QuestionEffect, QuestionModel};
use crate::shell::render::Theme;
use nuo_ui_primitives::components::options::push_wrapped_styled;
use nuo_ui_primitives::primitives::contrast_fg;
use nuo_view::trust_gate;

/// Width budget for the centered trust panel, as a percentage of the terminal
/// width. Clamped so the panel stays readable on very narrow terminals and does
/// not sprawl on very wide ones.
///
/// The divisor is a percentage (`60` → 60%), not a "sixth": the constant used to
/// be `6` while this comment claimed "roughly 60%", which produced a panel 16% of
/// the terminal wide. At 80 columns that is 13 cells — narrow enough that the
/// option descriptions and the key-hint footer were clipped mid-word.
const PANEL_WIDTH_PERCENT: u16 = 60;
/// Floor chosen so a full 60-column sentence still fits on an 80-column
/// terminal: the panel is inset from the screen, so a smaller floor forces
/// mid-word breaks in the security prompt itself.
const PANEL_MIN_WIDTH: u16 = 56;
const PANEL_MAX_WIDTH: u16 = 96;

/// Visual marker prefix on the highlighted option row. The user
/// requested "background highlight" only, so the marker
/// is intentionally absent — the inverted background alone carries
/// the selection affordance.
const INDENT: &str = "  ";

/// The runtime state of the PreAttach surface.
///
/// Owns the trust-gate question's input state machine plus a flag
/// distinguishing a real review gate from a
/// `NUO_FORCE_PRE_ATTACH=1` acceptance fixture (which renders an
/// extra banner so operators know they are in acceptance mode and
/// which domains they are verifying).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreAttachState {
    /// The trust-gate question, modeled by [`QuestionModel`] for its
    /// navigation/selection logic. Always carries the constant
    /// [`trust_gate::TRUST_GATE_REQUEST_ID`] so the existing reply
    /// path can still recognize its answers if it ever sees them.
    model: QuestionModel,
    /// `true` when this PreAttach was force-mounted by the
    /// `NUO_FORCE_PRE_ATTACH=1` acceptance env var (no real
    /// workspace awaiting review). The surface renders an extra banner
    /// naming the env var so operators know why they are seeing it.
    acceptance: bool,
    /// `true` once the user has submitted a decision to trust, providing
    /// immediate visual feedback and swallowing further keystrokes while the
    /// trusted snapshot round-trips.
    submitting: bool,
    /// The server's attestation outcome for the last submitted decision
    /// `Some(message)` when the grant could not be persisted
    /// (e.g. the durable trust store is unavailable). Rendered on the
    /// surface so a failed confirmation is an actionable message instead of
    /// a silent loop of the same dialog, and reset with `submitting` so the
    /// user can retry or decline.
    attestation_error: Option<String>,
    /// The domains this gate offered — every one awaiting a decision when it
    /// mounted. A `Decline` refuses exactly this set, so it is captured at mount
    /// rather than re-derived from a later snapshot that may have moved.
    domains: Vec<nuo_protocol::TrustDomain>,
}

/// The signal the listener task raises when a freshly-arrived
/// `HarnessState` snapshot demands a trust decision.
///
/// Drains to `None` once the per-frame sync has mounted
/// [`PreAttachState`]. Carries the snapshot itself rather than a
/// pre-built request so the loop can decide — based on
/// `trust_gate_dismissed` and the currently-mounted surface — whether
/// to mount, refresh, or ignore.
#[derive(Debug, Clone)]
pub struct PreAttachSignal {
    pub snapshot: WorkspaceSecuritySnapshot,
}

/// The outcome of a user decision on the PreAttach surface.
///
/// Pure data — the input path returns this and the caller (the event
/// loop) carries out the side effect. Mirrors the
/// `QuestionEffect`-to-side-effect boundary the Question sheet uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreAttachDecision {
    /// User chose to trust the workspace. Direct control-plane admission
    /// request — does not route through slash commands or pollute the transcript.
    Trust {
        domains: Vec<nuo_protocol::TrustDomain>,
    },
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
    Decline {
        domains: Vec<nuo_protocol::TrustDomain>,
    },
}

impl PreAttachState {
    /// Build a PreAttach surface from a snapshot awaiting review. Returns
    /// `None` when nothing does (`Absent` / `Trusted` / `Denied`) — the
    /// [`trust_gate::gate_request`] rules are authoritative.
    pub fn from_snapshot(snapshot: &WorkspaceSecuritySnapshot) -> Option<Self> {
        let request = trust_gate::gate_request(snapshot)?;
        let domains = snapshot.domains_needing_review();
        let mut model = QuestionModel::open(request);
        model.select_all(0);
        Some(Self {
            model,
            acceptance: false,
            submitting: false,
            attestation_error: None,
            domains,
        })
    }

    /// Build an acceptance fixture: a synthesized snapshot with every domain
    /// awaiting review, so every domain row appears in the rendered prompt and
    /// operators can verify the visual design end-to-end without preparing a
    /// real workspace.
    #[allow(clippy::expect_used)] // Synthesis is deterministic: all-six-awaiting-review MUST produce a gate request.
    pub fn acceptance_fixture() -> Self {
        let snapshot = WorkspaceSecuritySnapshot {
            root: "/tmp/nuo-acceptance".to_string(),
            mcp: WorkspaceTrustState::Quarantined,
            skills: WorkspaceTrustState::Quarantined,
            hooks: WorkspaceTrustState::Quarantined,
            instructions: WorkspaceTrustState::Quarantined,
            ex_workspace: WorkspaceTrustState::Quarantined,
            user_assets: WorkspaceTrustState::Quarantined,
        };
        let request = trust_gate::gate_request(&snapshot)
            .expect("a snapshot with every domain awaiting review must produce a gate request");
        let domains = snapshot.domains_needing_review();
        let mut model = QuestionModel::open(request);
        model.select_all(0);
        Self {
            model,
            acceptance: true,
            submitting: false,
            attestation_error: None,
            domains,
        }
    }

    /// `true` when this surface was force-mounted by
    /// `NUO_FORCE_PRE_ATTACH=1`. The renderer paints an extra
    /// banner so operators can tell acceptance mode from a real
    /// real review gate at a glance.
    pub fn acceptance(&self) -> bool {
        self.acceptance
    }

    /// `true` when a trust decision has been submitted and is pending unmount.
    pub fn submitting(&self) -> bool {
        self.submitting
    }

    /// The server's attestation outcome for the last submitted decision, if
    /// it failed. `Some` also implies `!submitting`: the
    /// round-trip resolved with a failure, so the surface is interactive
    /// again and renders the failure where the submitting spinner was.
    pub fn attestation_error(&self) -> Option<&str> {
        self.attestation_error.as_deref()
    }

    /// Record a failed trust-grant round-trip. Resets
    /// `submitting` so the surface resumes the decision instead of spinning,
    /// and keeps the question mounted — the user retries or declines with full
    /// knowledge of why the last confirmation did not take effect.
    pub fn note_attestation_error(&mut self, message: String) {
        self.submitting = false;
        self.attestation_error = Some(message);
    }

    /// The trust-gate [`QuestionModel`] backing this surface. The
    /// renderer reads its current question, options, and highlight
    /// straight from this; the input path mutates it through
    /// [`PreAttachState::apply`].
    pub fn model(&self) -> &QuestionModel {
        &self.model
    }

    /// Apply a navigation/selection action. Returns `Some(decision)`
    /// when the action resolves the surface (a trust choice or a
    /// dismissal); `None` when the action only moved the highlight
    /// and the surface should keep running.
    ///
    /// Mirrors the [`QuestionModel::update`] take-and-replace MVU
    /// pattern: `QuestionModel::update` consumes `self`, so this
    /// swaps the inner model out, runs the update, and writes the
    /// result back.
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
                    let selected = trust_gate::answer_to_domains(&answers);
                    // Anything the user left unselected is a refusal, and it is
                    // named here rather than inferred later: the domains that
                    // were *offered* are the ones to refuse, not merely the ones
                    // still pending after a partial grant.
                    let declined = self
                        .domains
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
                        domains: self.domains.clone(),
                    });
                }
                // `Closed` always follows `Reply` or `Cancelled` in
                // the same effect batch (see `QuestionModel::update`),
                // so the decision has already been emitted and we
                // simply stop iterating. Reaching this arm first
                // would indicate a QuestionModel bug — fall through
                // to "no decision" defensively.
                QuestionEffect::Closed { .. } => continue,
            }
        }
        None
    }
}

/// Render the PreAttach interstitial: full-screen black background
/// with a centered trust prompt and option list. The highlighted
/// option uses a background-highlight affordance only — no cursor
/// marker — matching.
pub fn draw_pre_attach(f: &mut Frame, state: &PreAttachState, theme: &Theme) {
    let area = f.area();

    // Full-screen pure black background — the user-requested "全部黑屏"
    // visual. Distinct from `theme.surface()` (the chat surface) so
    // the operator cannot mistake PreAttach for a live chat.
    f.render_widget(
        &mut Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let panel = centered_panel(area);
    let body_width = panel.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Acceptance banner (only when force-mounted by env var)
    if state.acceptance() {
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Acceptance mode (NUO_FORCE_PRE_ATTACH=1) — a decision here persists against the real workspace.",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
        lines.push(Line::default());
    }

    // Header (origin badge)
    lines.push(Line::from(vec![Span::styled(
        "[workspace trust]",
        Style::default()
            .fg(theme.info())
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::default());

    if state.submitting() {
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Applying trust settings...",
            Style::default()
                .fg(theme.info())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
        lines.push(Line::default());
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Entering session...",
            Style::default().fg(theme.muted()),
            body_width,
        );
        lines.push(Line::default());
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Waiting for the server to record the decision...",
            Style::default().fg(theme.muted()),
            body_width,
        );
        f.render_widget(
            &mut Paragraph::new(lines)
                .alignment(Alignment::Left)
                .style(Style::default().bg(Color::Black)),
            panel,
        );
        return;
    }

    // Failed attestation of the last decision: render the
    // server's outcome instead of looping the dialog silently.
    if let Some(error) = state.attestation_error() {
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Trust could not be applied:",
            Style::default()
                .fg(theme.err())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
        lines.push(Line::default());
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            error,
            Style::default().fg(theme.err()),
            body_width,
        );
        lines.push(Line::default());
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Adjust the selection and press Enter to retry, or Esc to refuse and continue untrusted.",
            Style::default().fg(theme.muted()),
            body_width,
        );
        lines.push(Line::default());
        lines.push(Line::default());
    }

    // Question body
    let qmodel = state.model();
    let request = qmodel.request();
    let current = qmodel.current();
    let highlight = qmodel.highlight();
    if let Some(question) = request.questions.get(current) {
        if let Some(header) = &question.header {
            push_wrapped_styled(
                &mut lines,
                INDENT,
                INDENT,
                header,
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
                body_width,
            );
            lines.push(Line::default());
        }
        // No indent on the question body: the panel is already inset by black
        // margins, so an extra gutter only narrows the wrap width — which is
        // what pushed a 47-column phrase into a mid-word break
        // ("…different conten" / "t, so…") in the sentence a user must read
        // most carefully.
        push_wrapped_styled(
            &mut lines,
            "",
            "",
            &question.question,
            Style::default().fg(theme.fg()),
            body_width,
        );
        lines.push(Line::default());
        lines.push(Line::default());

        // Options
        let selected_bg = theme.selected();
        let selected_fg = contrast_fg(selected_bg);
        let is_multi = question.multi_select;
        for (idx, opt) in question.options.iter().enumerate() {
            let is_highlighted = idx == highlight;
            let (row_bg, row_fg, row_mod) = if is_highlighted {
                (selected_bg, selected_fg, Modifier::BOLD)
            } else {
                (Color::Reset, theme.fg(), Modifier::empty())
            };
            let prefix = if is_multi {
                let is_checked = qmodel
                    .selected()
                    .get(current)
                    .map(|sel| sel.contains(&idx))
                    .unwrap_or(false);
                if is_checked { "[x] " } else { "[ ] " }
            } else {
                ""
            };
            let display_label = format!("{prefix}{}", opt.label);
            // Highlight is conveyed purely by background fill across
            // the whole row — pad the label out to the panel width so
            // the inversion reads as a solid bar, not a fragment.
            let label_line = format_option_row(display_label.as_str(), body_width, INDENT.len());
            lines.push(Line::from(vec![Span::styled(
                label_line,
                Style::default().bg(row_bg).fg(row_fg).add_modifier(row_mod),
            )]));
            if let Some(description) = opt.description.as_deref() {
                let desc_fg = if is_highlighted {
                    selected_fg
                } else {
                    theme.muted()
                };
                push_wrapped_styled(
                    &mut lines,
                    &format!("{}{}", INDENT, INDENT),
                    &format!("{}{}", INDENT, INDENT),
                    description,
                    Style::default().fg(desc_fg),
                    body_width,
                );
            }
        }
    }

    lines.push(Line::default());
    lines.push(Line::default());

    // Footer key hints.
    //
    // Wrapped, not a bare `Line`: a `Line` longer than the panel is silently
    // clipped, which shipped a gate whose footer read "Enter confirm   Es" — the
    // exit hint never reached the screen. The gate owns the whole terminal, so a
    // clipped footer is a surface with no stated exit.
    //
    // The single-select wording is kept under the panel's narrowest usable width
    // so the exit hint stays on one line; wrapping it is the fallback, not the
    // normal case.
    let hint_text = if qmodel.active_multi_select() {
        "↑/↓ move   Space toggle   Enter confirm   Esc decline"
    } else {
        "↑/↓ move   Enter select   Esc decline"
    };
    push_wrapped_styled(
        &mut lines,
        "",
        "",
        hint_text,
        Style::default().fg(theme.muted()),
        body_width,
    );

    f.render_widget(
        &mut Paragraph::new(lines)
            .alignment(Alignment::Left)
            .style(Style::default().bg(Color::Black)),
        panel,
    );
}

/// Compute a centered panel rectangle inside `area`: roughly 60%
/// width × auto-height (the paragraph lays out its own rows), pinned
/// to the vertical center of the terminal.
fn centered_panel(area: Rect) -> Rect {
    let width = area
        .width
        .saturating_mul(PANEL_WIDTH_PERCENT)
        .saturating_div(100)
        .clamp(PANEL_MIN_WIDTH, PANEL_MAX_WIDTH)
        .min(area.width.saturating_sub(2));
    let height = area.height.saturating_sub(4).clamp(12, 28);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Pad a label to the full body width so the highlighted-row
/// background reads as a solid bar. The indent reserves space for
/// the row's left gutter.
fn format_option_row(label: &str, body_width: usize, indent: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let avail = body_width.saturating_sub(indent);
    let label_width = label.width();
    let mut row = String::with_capacity(body_width);
    row.push_str(label);
    if label_width < avail {
        // Pad with spaces to fill the row; the caller's bg fills the
        // whole span.
        row.extend(std::iter::repeat_n(' ', avail.saturating_sub(label_width)));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

    fn quarantined_snapshot() -> WorkspaceSecuritySnapshot {
        WorkspaceSecuritySnapshot {
            root: "/tmp/proj".to_string(),
            mcp: WorkspaceTrustState::Quarantined,
            skills: WorkspaceTrustState::Quarantined,
            hooks: WorkspaceTrustState::Absent,
            instructions: WorkspaceTrustState::Quarantined,
            ex_workspace: WorkspaceTrustState::Absent,
            user_assets: WorkspaceTrustState::Absent,
        }
    }

    #[test]
    fn from_snapshot_returns_none_when_nothing_needs_review() {
        let trusted = WorkspaceSecuritySnapshot {
            root: "/tmp/proj".to_string(),
            mcp: WorkspaceTrustState::Trusted,
            skills: WorkspaceTrustState::Trusted,
            hooks: WorkspaceTrustState::Trusted,
            instructions: WorkspaceTrustState::Trusted,
            ex_workspace: WorkspaceTrustState::Trusted,
            user_assets: WorkspaceTrustState::Trusted,
        };
        assert!(PreAttachState::from_snapshot(&trusted).is_none());

        // A refusal is a decision: `Denied` is settled, so the gate must not
        // re-open and nag.
        let denied = WorkspaceSecuritySnapshot {
            root: "/tmp/proj".to_string(),
            mcp: WorkspaceTrustState::Denied,
            skills: WorkspaceTrustState::Trusted,
            hooks: WorkspaceTrustState::Absent,
            instructions: WorkspaceTrustState::Denied,
            ex_workspace: WorkspaceTrustState::Trusted,
            user_assets: WorkspaceTrustState::Absent,
        };
        assert!(PreAttachState::from_snapshot(&denied).is_none());
    }

    #[test]
    fn from_snapshot_mounts_for_changed_content() {
        // The regression this pins. `Changed` used to escalate through an
        // in-session banner instead of gating, so the one state meaning "the
        // content you approved is not the content on disk" got a scroll-past
        // toast that re-fired on every attach. It gates now, and the prompt says
        // what changed.
        let changed = WorkspaceSecuritySnapshot {
            root: "/tmp/proj".to_string(),
            mcp: WorkspaceTrustState::Trusted,
            skills: WorkspaceTrustState::Trusted,
            hooks: WorkspaceTrustState::Trusted,
            instructions: WorkspaceTrustState::Changed,
            ex_workspace: WorkspaceTrustState::Trusted,
            user_assets: WorkspaceTrustState::Trusted,
        };
        let state = PreAttachState::from_snapshot(&changed)
            .expect("changed instructions must mount the gate");
        let question = &state.model().request().questions[0];
        assert_eq!(
            question.options.len(),
            1,
            "only the changed domain is listed"
        );
        assert_eq!(question.options[0].label, "AGENTS.md  ·  [Changed]");
        assert!(
            question.question.contains("changed on disk"),
            "the prompt must say why it is asking: {}",
            question.question
        );
    }

    #[test]
    fn from_snapshot_mounts_for_quarantined() {
        let state = PreAttachState::from_snapshot(&quarantined_snapshot())
            .expect("quarantined snapshot must mount PreAttach");
        assert!(!state.acceptance());
        assert_eq!(
            state.model().request().id,
            trust_gate::TRUST_GATE_REQUEST_ID
        );
        // First option highlighted by default (single-select is live).
        assert_eq!(state.model().highlight(), 0);
    }

    #[test]
    fn acceptance_fixture_lists_every_domain() {
        let state = PreAttachState::acceptance_fixture();
        assert!(state.acceptance());
        let q = state
            .model()
            .request()
            .questions
            .first()
            .expect("trust gate has one question");
        assert!(q.multi_select);
        let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
        assert!(labels.iter().any(|l| l.starts_with(".nuo/mcp.json")));
        assert!(labels.iter().any(|l| l.starts_with(".nuo/skills/")));
        assert!(labels.iter().any(|l| l.starts_with(".nuo/hooks/")));
        assert!(labels.iter().any(|l| l.starts_with("AGENTS.md")));
        assert!(labels.iter().any(|l| l.starts_with(".nuo/config.toml")));
        assert!(
            labels
                .iter()
                .any(|l| l.starts_with("~/.config/nuo/config.toml"))
        );
    }

    #[test]
    fn navigate_then_submit_trust_all_emits_a_grant() {
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        // By default, all options start selected. Pressing enter trusts all of them.
        let decision = state.apply(QuestionAction::Submit);
        assert_eq!(
            decision,
            Some(PreAttachDecision::Trust {
                domains: vec![
                    TrustDomain::Mcp,
                    TrustDomain::Skills,
                    TrustDomain::Instructions,
                ],
            })
        );
        assert!(state.submitting());
        // While the decision is in flight the surface is inert — including for
        // Esc. A cancel here used to refuse every offered domain (the ones just
        // submitted included), so the outcome raced the in-flight grant.
        assert_eq!(state.apply(QuestionAction::Down), None);
        assert_eq!(state.apply(QuestionAction::Cancel), None);
        assert_eq!(state.apply(QuestionAction::Submit), None);
    }

    #[test]
    fn space_toggles_and_submits_partial_selection() {
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        // Option 0: .nuo/mcp.json, Option 1: .nuo/skills/, Option 2: AGENTS.md
        state.apply(QuestionAction::Down); // highlight -> 1 (.nuo/skills/)
        state.apply(QuestionAction::Toggle); // toggle off .nuo/skills/
        let decision = state.apply(QuestionAction::Submit);
        assert_eq!(
            decision,
            Some(PreAttachDecision::Trust {
                domains: vec![TrustDomain::Mcp, TrustDomain::Instructions,],
            })
        );
    }

    #[test]
    fn esc_declines_without_quitting() {
        // A refusal is a decision, not an exit. The gate also opens for
        // `Changed` content on a live session, where terminating the terminal
        // would be an absurd answer to "someone changed AGENTS.md".
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        let decision = state.apply(QuestionAction::Cancel);
        assert_eq!(
            decision,
            Some(PreAttachDecision::Decline {
                domains: vec![
                    TrustDomain::Mcp,
                    TrustDomain::Skills,
                    TrustDomain::Instructions,
                ],
            }),
            "declining must name every offered domain so the refusal can be recorded"
        );
    }

    #[test]
    fn a_partial_selection_declines_exactly_what_was_left_out() {
        // The domains to refuse are the ones *offered but unselected* — not
        // merely the ones still pending after a partial grant, which is how the
        // server-side pre-attach path had to infer it.
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        state.apply(QuestionAction::Down); // highlight -> 1 (.nuo/skills/)
        state.apply(QuestionAction::Toggle); // toggle off .nuo/skills/
        let decision = state.apply(QuestionAction::Submit);
        assert_eq!(
            decision,
            Some(PreAttachDecision::Trust {
                domains: vec![TrustDomain::Mcp, TrustDomain::Instructions],
            }),
        );
    }

    #[test]
    fn submitting_nothing_is_a_refusal_not_an_empty_grant() {
        // Dispatching a grant for zero domains would ask the server to do
        // nothing and leave the gate to re-open — the loop, with extra steps.
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        for _ in 0..3 {
            state.apply(QuestionAction::Toggle); // clear all three selections
            state.apply(QuestionAction::Down);
        }
        let decision = state.apply(QuestionAction::Submit);
        assert_eq!(
            decision,
            Some(PreAttachDecision::Decline {
                domains: vec![
                    TrustDomain::Mcp,
                    TrustDomain::Skills,
                    TrustDomain::Instructions,
                ],
            }),
        );
    }

    /// A failed attestation round-trip: the failure message is
    /// carried on the surface, `submitting` clears so the decision resumes,
    /// and the next submit re-dispatches the complete selection.
    #[test]
    fn attestation_failure_resumes_the_decision() {
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        assert_eq!(state.attestation_error(), None);
        let _ = state.apply(QuestionAction::Submit);
        assert!(state.submitting());

        state.note_attestation_error("cannot open sqlite db".to_string());
        assert!(!state.submitting());
        assert_eq!(state.attestation_error(), Some("cannot open sqlite db"));

        let retry = state.apply(QuestionAction::Submit);
        assert!(matches!(retry, Some(PreAttachDecision::Trust { .. })));
        assert!(state.submitting());
        assert_eq!(state.attestation_error(), Some("cannot open sqlite db"));
    }

    #[test]
    fn arrow_only_does_not_emit_decision() {
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        assert_eq!(state.apply(QuestionAction::Down), None);
        assert_eq!(state.apply(QuestionAction::Up), None);
        assert_eq!(state.model().highlight(), 0);
    }

    /// `TrustDomain` exists and is enumerable — sanity check the
    /// contract hasn't drifted out from under this module.
    #[test]
    fn trust_domain_all_is_still_enumerable() {
        assert!(!TrustDomain::ALL.is_empty());
    }
}
