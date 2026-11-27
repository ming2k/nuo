//! Pre-view workspace trust gate.
//!
//! When the client attaches to a session whose project root carries
//! project-authored contributions (skills, MCP, hooks, rules) awaiting a human
//! decision — any `WorkspaceTrustState::needs_review` value from the durable
//! `WorkspaceSecurityStore`: never trusted (`Quarantined`), trusted content that
//! then changed on disk (`Changed`), or a lapsed lease (`Expired`) — the
//! server's attach-sync `HarnessState` already carries the security snapshot.
//! This module turns that snapshot into the *first* thing the user sees: a
//! blocking decision opened before the composer takes input, so the trust
//! decision happens up front instead of via a passive banner the user can (and
//! usually does) scroll past.
//!
//! `Changed` belongs here for the same reason `Quarantined` does: a trust
//! decision is bound to specific content, so content that moved after approval
//! is exactly as unauthorized as content that was never approved. Excluding it
//! (which this module used to do) demoted the highest-risk posture to a
//! transient toast.
//!
//! # Two seams, two different jobs
//!
//! The surface reuses the `ask_user` question model for its navigation and
//! selection state machine, so `Enter`/`Space`/`Esc` semantics and multi-select
//! toggling are the ones the user already knows. That reuse stops at *state*:
//!
//! - **The painted row is built from domain data** ([`domain_rows`]), never from
//!   a `UserQuestionOption`'s `label`. A row carries its own status as a
//!   [`TrustStatus`] value, which the surface renders as a right-aligned column.
//!   Folding the status into the label string (`"Project instructions  |
//!   [Changed]"`) made a display string the only place the fact existed, so the
//!   fact could not be toned, aligned, or dropped under width pressure — and it
//!   had to be *parsed back out* to answer the question
//!   ([`answer_to_domains`]), which made presentation syntax load-bearing.
//! - **The answer maps through the offered set.** [`answer_to_domains`] takes
//!   the domains the surface actually offered and resolves each returned label
//!   against [`TrustDomain::label`]. A label that names a domain nobody offered
//!   maps to nothing, so a stale or hand-built answer can never grant authority
//!   the gate did not ask about.
//!
//! # Identity
//!
//! The request id is the constant [`TRUST_GATE_REQUEST_ID`]. The reply path
//! intercepts that id and turns it into the control-plane
//! `AgentRequest::TrustWorkspace` decision — never a slash command typed into
//! the transcript and never a `UserQuestionReply` (the server has no parked
//! round waiting for one). Persistence and the atomic live reload stay owned by
//! the one server path that handles them.
//!
//! # Lifecycle
//!
//! The gate is fed by `HarnessState` snapshots. It opens once per attach while
//! anything is awaiting review, and closes as soon as a snapshot reports nothing
//! left to decide — whether that is because the user trusted the content
//! (`Trusted`) or refused it (`Denied`, which is equally settled). A stale
//! dialog can never linger after the decision.
//!
//! Dismissing is a recorded refusal, not an exit: the offered domains are
//! refused for their current content, so they stay inactive and are not asked
//! about again until the bytes change. The session then continues on the chat
//! surface — the gate also opens for content that changed mid-session, where
//! quitting the terminal would be an absurd answer.
use graticule_canvas::Color;
use nuo_protocol::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};
use nuo_tool::{UserQuestion, UserQuestionOption, UserQuestionRequest};
use nuo_ui_primitives::Theme;

/// Request id marking the synthesized trust-gate question. Recognized by the
/// reply path (`super::event_loop`) and never sent to the server as a
/// `UserQuestionReply`.
pub const TRUST_GATE_REQUEST_ID: &str = "__workspace_trust_gate__";

/// The gate's title line.
///
/// Not `[workspace trust]`: brackets are the badge slot (`[ROLE]`) and the count
/// slot (`[2 queued]`), so a bracketed title reads as either. A title is prose
/// and says what the surface is.
pub const GATE_TITLE: &str = "Workspace trust";

/// The consequence line, painted beneath the rows in a secondary tone.
///
/// It states the two facts that make a *partial* selection safe to consider —
/// refused content stays off, and it is not re-asked until the bytes move — and
/// nothing else. `Apply`/`Review`/`Authorize` need no sentence: those are what
/// the rows and the confirm key already are.
///
/// Kept out of the question body so the surface can paint it in its own tone
/// rather than the question body's: it answers a different question ("what
/// happens if I leave this unchecked?") than the headline does.
pub const GATE_CONSEQUENCE: &str =
    "Unselected content stays off and is not asked about again until it changes.";

/// Why one asset is awaiting a decision.
///
/// A closed, lowercase, unbracketed vocabulary — the project's state-token rule
/// (`docs/reference/tui/visual-language.md` §3). Written as a word rather than a
/// `[Badge]` because a bracketed token in this position is indistinguishable
/// from the `[ROLE]` badge and the `[N queued]` count, and because the surface
/// needs to tone the word by severity — which a sentence stuffed into a label
/// string cannot do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustStatus {
    /// Content exists that was never decided.
    New,
    /// A decision was made for *different* bytes.
    Changed,
    /// The lease lapsed; routine re-attestation.
    Expired,
}

impl TrustStatus {
    /// The status a reviewable state maps to, or `None` for a settled state.
    ///
    /// Total over [`WorkspaceTrustState`] on purpose: a new state cannot compile
    /// until its gate status is chosen deliberately.
    pub fn of(state: WorkspaceTrustState) -> Option<Self> {
        match state {
            WorkspaceTrustState::Quarantined => Some(Self::New),
            WorkspaceTrustState::Changed => Some(Self::Changed),
            WorkspaceTrustState::Expired => Some(Self::Expired),
            WorkspaceTrustState::Absent
            | WorkspaceTrustState::Trusted
            | WorkspaceTrustState::Denied => None,
        }
    }

    /// The token the surface paints, right-aligned against the row's trailing
    /// edge.
    pub fn token(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Changed => "changed",
            Self::Expired => "expired",
        }
    }

    /// The tone this status is painted in.
    ///
    /// Severity rides the palette's own outcome tones, so a scheme retune moves
    /// it and nothing has to be re-thought here. The ladder is the point:
    /// `changed` means "the bytes you approved are not the bytes on disk" — the
    /// one status that should catch the eye — while `expired` is routine
    /// re-attestation and `new` is a plain first look, so neither should shout.
    /// Painting all three in one tone (or, worse, in the row's foreground) makes
    /// the strongest reason to ask look like the weakest.
    pub fn tone(self, theme: &Theme) -> Color {
        match self {
            Self::Changed => theme.warn(),
            Self::New => theme.muted(),
            Self::Expired => theme.dim(),
        }
    }
}

/// One asset awaiting a decision, as the surface paints it.
///
/// The row is the unit of *display*: a domain, the path a user recognizes it by,
/// and why it is being asked about. Answers travel as [`TrustDomain`]s only —
/// nothing downstream parses [`Self::label`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrustDomainView {
    /// What a decision on this row grants or refuses.
    pub domain: TrustDomain,
    /// The path (or, for instructions, the collective name) the user recognizes
    /// it by. Delegates to [`TrustDomain::label`] so the gate, the attach banner
    /// — which the server composes without linking this crate — and
    /// `/trust status` cannot name the same asset three ways.
    pub label: &'static str,
    /// Why it is being asked about now.
    pub status: TrustStatus,
}

/// Every offered asset, in `TrustDomain::ALL` order — the same enumeration the
/// server's banner and `/trust deny` consume, so the gate cannot list a set the
/// rest of the system disagrees with.
pub fn domain_rows(snapshot: &WorkspaceSecuritySnapshot) -> Vec<TrustDomainView> {
    snapshot
        .domains_needing_review()
        .into_iter()
        .filter_map(|domain| {
            let status = TrustStatus::of(snapshot.state(domain))?;
            Some(TrustDomainView {
                domain,
                label: domain.label(),
                status,
            })
        })
        .collect()
}

/// Build the trust-gate question request for assets awaiting a decision, or
/// `None` when nothing needs gating (absent, trusted, or explicitly denied).
///
/// The options exist so the question sheet's own state machine can carry the
/// selection; their `label` is the plain asset name and nothing else. The status
/// lives on [`TrustDomainView`], where the surface can render it as a column.
pub fn gate_request(snapshot: &WorkspaceSecuritySnapshot) -> Option<UserQuestionRequest> {
    let rows = domain_rows(snapshot);
    if rows.is_empty() {
        return None;
    }

    // One asset is a binary decision, not a selection: `Enter` trusts it and
    // `Esc` skips it, the highlighted row *is* the choice, and there is no
    // checkbox to read. Declaring it multi-select would leave `Space` as a live
    // toggle for a row the footer does not advertise — a key that silently
    // deselects, so the next `Enter` refuses what the user just meant to grant.
    let multi_select = rows.len() > 1;
    Some(UserQuestionRequest {
        id: TRUST_GATE_REQUEST_ID.to_string(),
        questions: vec![UserQuestion {
            header: None,
            question: gate_question(&rows),
            options: rows
                .iter()
                .map(|row| UserQuestionOption {
                    label: row.label.to_string(),
                    description: None,
                })
                .collect(),
            multi_select,
        }],
        origin: Some("workspace trust".to_string()),
    })
}

/// The gate's headline: *why is this being asked right now?*
///
/// One sentence per reason class present, and no more. The reasons are genuinely
/// different questions — "the bytes you approved are not the bytes on disk" is
/// not "you have never looked at this" — so they cannot be collapsed into one
/// generic sentence without hiding the fact that makes the decision informed.
/// In the common case exactly one class is present and the headline is one line.
///
/// The *assets* are not named here. They are named on the rows, with their own
/// status column, one line each; repeating them in prose was a second, weaker
/// copy of the same list (and the copy that had to break words to fit).
fn gate_question(rows: &[TrustDomainView]) -> String {
    let present = |status: TrustStatus| rows.iter().any(|row| row.status == status);
    let mut lines: Vec<&str> = Vec::new();
    if present(TrustStatus::Changed) {
        lines.push("Content changed on disk since you approved it.");
    }
    if present(TrustStatus::Expired) {
        lines.push("Content needs re-approval after its lease expired.");
    }
    if present(TrustStatus::New) {
        lines.push("Content in this workspace has not been reviewed.");
    }
    lines.join("\n")
}

/// Map a trust-gate answer back to the selected trust domains.
///
/// `offered` is the set the surface actually put in front of the user. Labels
/// resolve through [`TrustDomain::label`], the one naming authority; anything
/// that does not name an offered domain is dropped rather than guessed at, so an
/// answer can never grant or refuse authority the user was not shown.
///
/// The result is in `offered` order, not selection order: the decision is a set,
/// and a stable order keeps the request it becomes reproducible.
pub fn answer_to_domains(answers: &[Vec<String>], offered: &[TrustDomain]) -> Vec<TrustDomain> {
    let selected = answers.first().map(Vec::as_slice).unwrap_or_default();
    offered
        .iter()
        .copied()
        .filter(|domain| selected.iter().any(|label| label == domain.label()))
        .collect()
}

#[cfg(test)]
mod tests;
