//! Pre-attach workspace trust interstitial surface. See ADR-0175.
//!
//! When the TUI attaches to a session whose project root carries
//! never-trusted project-authored contributions (skills, MCP, hooks,
//! rules) — `WorkspaceTrustState::Quarantined` from the durable
//! `WorkspaceSecurityStore` — the PreAttach surface mounts before any
//! chat transcript, composer, or session chrome. Visually it is a
//! full-screen black background with a centered trust prompt and an
//! option list navigated by `↑`/`↓`, selected by `Enter`, and escaped
//! by `Esc`. Selection is shown as a background highlight, not a
//! cursor marker.
//!
//! The surface owns the entire terminal area for the duration of the
//! trust decision. There is no parent surface to drop back to and no
//! composer to fall through to: dismissing the surface quits the TUI,
//! because an untrusted workspace has no useful work to do per
//! ADR-0140 §3 (every model round and direct-shell command fails
//! preflight).
//!
//! The trust question wording, domain listing, and answer-to-command
//! mapping remain owned by [`crate::trust_gate`]; this module only
//! owns the surface placement. [`crate::question_model::QuestionModel`]
//! is reused as the navigation/selection state machine, exactly as
//! the Question sheet does for AI-initiated `ask_user` — its MVU
//! purity keeps the input path testable without a terminal.

use nuo_wire::{WorkspaceSecuritySnapshot, WorkspaceTrustState};
use nuotc::{Alignment, Block, Color, Frame, Line, Modifier, Paragraph, Rect, Span, Style};

use crate::components::options::push_wrapped_styled;
use crate::primitives::contrast_fg;
use crate::question_model::{QuestionAction, QuestionEffect, QuestionModel};
use crate::render::Theme;
use crate::trust_gate;

/// Width budget for the centered trust panel, as a fraction of the
/// terminal width. Clamped so the panel stays readable both on very
/// narrow and very wide terminals.
const PANEL_WIDTH_FRACTION: u16 = 6; // /10
const PANEL_MIN_WIDTH: u16 = 48;
const PANEL_MAX_WIDTH: u16 = 96;

/// Visual marker prefix on the highlighted option row. The user
/// requested "background highlight" only (ADR-0175 §1), so the marker
/// is intentionally absent — the inverted background alone carries
/// the selection affordance.
const INDENT: &str = "  ";

/// The runtime state of the PreAttach surface.
///
/// Owns the trust-gate question's input state machine plus a flag
/// distinguishing a real first-contact gate from a
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
    /// quarantined workspace). The surface renders an extra banner
    /// naming the env var so operators know why they are seeing it.
    acceptance: bool,
    /// `true` once the user has submitted a decision to trust, providing
    /// immediate visual feedback and swallowing further keystrokes while the
    /// trusted snapshot round-trips.
    submitting: bool,
    /// The quarantined domains present in this workspace.
    domains: Vec<nuo_wire::TrustDomain>,
}

/// The signal the listener task raises when a freshly-arrived
/// `HarnessState` snapshot demands a first-contact trust decision.
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
        domains: Vec<nuo_wire::TrustDomain>,
    },
    /// User chose to keep the workspace untrusted (`Keep quarantined`
    /// option or `Esc`). The TUI should quit — there is no chat
    /// surface to fall through to under ADR-0175 §4.
    Quit,
}

impl PreAttachState {
    /// Build a PreAttach surface from a quarantined snapshot. Returns
    /// `None` when the snapshot does not actually demand a gate
    /// (`Absent` / `Trusted` / `Changed`) — the
    /// [`trust_gate::gate_request`] rules are authoritative.
    pub fn from_snapshot(snapshot: &WorkspaceSecuritySnapshot) -> Option<Self> {
        let request = trust_gate::gate_request(snapshot)?;
        let domains = trust_gate::quarantined_domains(snapshot);
        let mut model = QuestionModel::open(request);
        model.select_all(0);
        Some(Self {
            model,
            acceptance: false,
            submitting: false,
            domains,
        })
    }

    /// Build an acceptance fixture: a synthesized snapshot with every
    /// domain `Quarantined`, so every domain row appears in the
    /// rendered prompt and operators can verify the visual design
    /// end-to-end without preparing a real quarantined workspace.
    #[allow(clippy::expect_used)] // Synthesis is deterministic: all-five-Quarantined MUST produce a gate request.
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
        let request = trust_gate::gate_request(&snapshot).expect(
            "synthesized quarantined snapshot with all five domains must produce a gate request",
        );
        let domains = trust_gate::quarantined_domains(&snapshot);
        let mut model = QuestionModel::open(request);
        model.select_all(0);
        Self {
            model,
            acceptance: true,
            submitting: false,
            domains,
        }
    }

    /// `true` when this surface was force-mounted by
    /// `NUO_FORCE_PRE_ATTACH=1`. The renderer paints an extra
    /// banner so operators can tell acceptance mode from a real
    /// first-contact gate at a glance.
    pub fn acceptance(&self) -> bool {
        self.acceptance
    }

    /// `true` when a trust decision has been submitted and is pending unmount.
    pub fn submitting(&self) -> bool {
        self.submitting
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
            if matches!(action, QuestionAction::Cancel) {
                return Some(PreAttachDecision::Quit);
            }
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
                    self.submitting = true;
                    return Some(PreAttachDecision::Trust { domains: selected });
                }
                QuestionEffect::Cancelled { .. } => return Some(PreAttachDecision::Quit),
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
/// marker — matching ADR-0175 §1.
pub fn draw_pre_attach(f: &mut Frame, state: &PreAttachState, theme: &Theme) {
    let area = f.area();

    // Full-screen pure black background — the user-requested "全部黑屏"
    // visual. Distinct from `theme.surface()` (the chat surface) so
    // the operator cannot mistake PreAttach for a live chat.
    f.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
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
            "Acceptance mode (NUO_FORCE_PRE_ATTACH=1) — selecting Trust persists against the real workspace.",
            Style::default()
                .fg(theme.warn())
                .add_modifier(Modifier::BOLD),
            body_width,
        );
        lines.push(Line::default());
    }

    // Header (origin badge)
    lines.push(Line::from(vec![Span::styled(
        "[file asset trust]",
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
            "Attesting file assets...",
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
            "Granting 30-day lease and entering session...",
            Style::default().fg(theme.muted()),
            body_width,
        );
        lines.push(Line::default());
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
            "Press [Esc] to cancel and keep quarantined.",
            Style::default().fg(theme.muted()),
            body_width,
        );
        f.render_widget(
            Paragraph::new(lines)
                .alignment(Alignment::Left)
                .style(Style::default().bg(Color::Black)),
            panel,
        );
        return;
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
        push_wrapped_styled(
            &mut lines,
            INDENT,
            INDENT,
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

    // Footer key hints
    let hint_text = if qmodel.active_multi_select() {
        "↑/↓ navigate   Space toggle   Enter confirm   Esc quit"
    } else {
        "↑/↓ navigate   Enter select   Esc quit"
    };
    let hint = Span::styled(hint_text, Style::default().fg(theme.muted()));
    lines.push(Line::from(hint));

    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .style(Style::default().bg(Color::Black)),
        panel,
    );
}

/// Compute a centered panel rectangle inside `area`: roughly 60%
/// width × auto-height (the paragraph lays out its own rows), pinned
/// to the vertical center of the terminal.
fn centered_panel(area: Rect) -> Rect {
    let tenth = area.width.max(1) / PANEL_WIDTH_FRACTION.max(1);
    let width = tenth
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
    use nuo_wire::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};

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
    fn from_snapshot_returns_none_when_not_quarantined() {
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
    fn navigate_then_submit_trust_all_emits_command() {
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
        // Subsequent navigation or submit actions while submitting are ignored.
        assert_eq!(state.apply(QuestionAction::Down), None);
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
    fn esc_emits_quit() {
        let mut state = PreAttachState::from_snapshot(&quarantined_snapshot()).unwrap();
        let decision = state.apply(QuestionAction::Cancel);
        assert_eq!(decision, Some(PreAttachDecision::Quit));
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
