//! Pre-view workspace trust gate.
//!
//! When the client attaches to a session whose project root carries
//! project-authored contributions (skills, MCP, hooks, rules) awaiting a human
//! decision — any `WorkspaceTrustState::needs_review` value from the durable
//! `WorkspaceSecurityStore`: never trusted (`Quarantined`), trusted content that
//! then changed on disk (`Changed`), or a lapsed lease (`Expired`) — the
//! server's attach-sync `HarnessState` already carries the security snapshot.
//! This module turns that snapshot into the *first* thing the user sees: a
//! blocking question dialog opened before the composer takes input, so the trust
//! decision happens up front instead of via a passive banner the user can (and
//! usually does) scroll past.
//!
//! `Changed` belongs here for the same reason `Quarantined` does: a trust
//! decision is bound to specific content, so content that moved after approval
//! is exactly as unauthorized as content that was never approved. Excluding it
//! (which this module used to do) demoted the highest-risk posture to a
//! transient toast.
//!
//! The dialog is deliberately not a new modal. It reuses the ask_user
//! question sheet (`QuestionModel`) by synthesizing
//! a [`UserQuestionRequest`] into the pending-question queue, so navigation,
//! rendering, scrolling, and multi-select semantics are the ones the user
//! already knows. Two seams keep the synthetic request honest:
//!
//! - **Identity.** The request id is the constant
//!   [`TRUST_GATE_REQUEST_ID`]. The reply path intercepts that id and turns it
//!   into the control-plane `AgentRequest::TrustWorkspace` decision — never a
//!   slash command typed into the transcript and never a `UserQuestionReply`
//!   (the server has no parked round waiting for one). Persistence and the
//!   atomic live reload stay owned by the one server path that handles them.
//! - **Lifecycle.** The gate is fed by `HarnessState` snapshots. It opens once
//!   per attach while anything is awaiting review, and closes as soon as a
//!   snapshot reports nothing left to decide — whether that is because the user
//!   trusted the content (`Trusted`) or refused it (`Denied`, which is equally
//!   settled). A stale dialog can never linger after the decision.
//!
//! Dismissing the dialog is a recorded refusal, not an exit: the offered
//! domains are refused for their current content, so they stay inactive and are
//! not asked about again until the bytes change. The session then continues on
//! the chat surface — the gate also opens for content that changed mid-session,
//! where quitting the terminal would be an absurd answer.
use nuo_protocol::{
    TrustDomain, UserQuestion, UserQuestionOption, UserQuestionRequest, WorkspaceSecuritySnapshot,
    WorkspaceTrustState,
};

/// Request id marking the synthesized trust-gate question. Recognized by the
/// reply path (`super::event_loop`) and never sent to the server as a
/// `UserQuestionReply`.
pub const TRUST_GATE_REQUEST_ID: &str = "__workspace_trust_gate__";

pub fn status_badge(state: WorkspaceTrustState) -> &'static str {
    match state {
        WorkspaceTrustState::Quarantined => "New",
        WorkspaceTrustState::Changed => "Changed",
        WorkspaceTrustState::Expired => "Expired",
        WorkspaceTrustState::Denied => "Denied",
        WorkspaceTrustState::Trusted => "Trusted",
        WorkspaceTrustState::Absent => "",
    }
}

/// Build the trust-gate question request for assets awaiting a decision, or
/// `None` when nothing needs gating (absent, trusted, or explicitly denied).
pub fn gate_request(snapshot: &WorkspaceSecuritySnapshot) -> Option<UserQuestionRequest> {
    let domains = snapshot.domains_needing_review();
    if domains.is_empty() {
        return None;
    }

    let options = domains
        .iter()
        .map(|d| {
            let state = snapshot.state(*d);
            let badge = status_badge(state);
            let label = if badge.is_empty() {
                domain_label(*d).to_string()
            } else {
                format!("{}  ·  [{badge}]", domain_label(*d))
            };
            UserQuestionOption {
                label,
                description: Some(domain_description(*d).to_string()),
            }
        })
        .collect();

    Some(UserQuestionRequest {
        id: TRUST_GATE_REQUEST_ID.to_string(),
        questions: vec![UserQuestion {
            header: None,
            question: gate_question(&domains, snapshot),
            options,
            multi_select: true,
        }],
        origin: Some("workspace trust".to_string()),
    })
}

/// The gate's question, which must say *which* kind of decision this is.
///
/// A first-contact prompt and a re-review prompt are different questions with
/// the same options. Telling a user "authorize these assets" when they already
/// did, and the bytes simply changed underneath them, hides the one fact that
/// makes the decision informed: what they approved is not what is on disk.
/// The affected domains are named so the user need not diff the workspace to
/// find out.
fn gate_question(domains: &[TrustDomain], snapshot: &WorkspaceSecuritySnapshot) -> String {
    let changed: Vec<&str> = domains
        .iter()
        .filter(|d| snapshot.state(**d) == WorkspaceTrustState::Changed)
        .map(|d| domain_label(*d))
        .collect();
    let expired: Vec<&str> = domains
        .iter()
        .filter(|d| snapshot.state(**d) == WorkspaceTrustState::Expired)
        .map(|d| domain_label(*d))
        .collect();
    let fresh: Vec<&str> = domains
        .iter()
        .filter(|d| snapshot.state(**d) == WorkspaceTrustState::Quarantined)
        .map(|d| domain_label(*d))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if !changed.is_empty() {
        lines.push(format!(
            "Previously trusted content changed on disk: {}.",
            changed.join(", ")
        ));
        lines.push(
            "The earlier approval was for different content, so it does not carry over. \
             Review the changes before authorizing them again."
                .to_string(),
        );
    }
    if !expired.is_empty() {
        lines.push(format!(
            "Attestation lease expired, so it needs routine re-approval: {}.",
            expired.join(", ")
        ));
    }
    if !fresh.is_empty() {
        lines.push(format!(
            "Project assets never reviewed in this workspace: {}.",
            fresh.join(", ")
        ));
    }
    lines.push(
        "Unselected assets stay inactive for this session and will not be asked about again \
         unless their content changes."
            .to_string(),
    );
    lines.join("\n")
}

/// Map a trust-gate dialog answer back to the selected trust domains.
pub fn answer_to_domains(answers: &[Vec<String>]) -> Vec<TrustDomain> {
    let selected_labels = answers.first().cloned().unwrap_or_default();
    let mut domains = Vec::new();
    for label in selected_labels {
        let path = label.split("  ·  ").next().unwrap_or(&label).trim();
        match path {
            "~/.config/nuo/config.toml" | "~/.nuo/config.toml" => {
                domains.push(TrustDomain::UserAssets)
            }
            ".nuo/config.toml" => domains.push(TrustDomain::ExWorkspace),
            ".nuo/mcp.json" => domains.push(TrustDomain::Mcp),
            ".nuo/skills/" => domains.push(TrustDomain::Skills),
            ".nuo/hooks/" => domains.push(TrustDomain::Hooks),
            "AGENTS.md" => domains.push(TrustDomain::Instructions),
            _ => {}
        }
    }
    domains
}

/// The path a user recognizes a domain by.
///
/// Delegates to [`TrustDomain::label`] so the gate, the attach banner (which the
/// server composes, without linking this crate), and `/trust status` cannot name
/// the same asset three different ways.
pub fn domain_label(domain: TrustDomain) -> &'static str {
    domain.label()
}

pub fn domain_description(domain: TrustDomain) -> &'static str {
    match domain {
        TrustDomain::UserAssets => "User global configuration",
        TrustDomain::ExWorkspace => "Workspace configuration & additional roots",
        TrustDomain::Mcp => "Model Context Protocol servers",
        TrustDomain::Skills => "Project-local custom skills",
        TrustDomain::Hooks => "Lifecycle execution hooks",
        TrustDomain::Instructions => "Project rules & instructions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        mcp: WorkspaceTrustState,
        skills: WorkspaceTrustState,
        hooks: WorkspaceTrustState,
        instructions: WorkspaceTrustState,
    ) -> WorkspaceSecuritySnapshot {
        WorkspaceSecuritySnapshot {
            root: "/tmp/proj".to_string(),
            mcp,
            skills,
            hooks,
            instructions,
            ex_workspace: WorkspaceTrustState::Absent,
            user_assets: WorkspaceTrustState::Absent,
        }
    }

    #[test]
    fn absent_workspace_gates_nothing() {
        assert!(
            gate_request(&snapshot(
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent
            ))
            .is_none()
        );
    }

    #[test]
    fn trusted_and_denied_do_not_gate() {
        assert!(
            gate_request(&snapshot(
                WorkspaceTrustState::Trusted,
                WorkspaceTrustState::Trusted,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Trusted
            ))
            .is_none()
        );
        // Explicitly denied domains do not gate again (zero-nagging).
        assert!(
            gate_request(&snapshot(
                WorkspaceTrustState::Denied,
                WorkspaceTrustState::Trusted,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Denied
            ))
            .is_none()
        );
    }

    #[test]
    fn changed_domains_gate_exactly_like_never_trusted_domains() {
        // The regression this pins. `Changed` means "the content you approved
        // is not the content on disk" — the strongest reason to ask a human,
        // and previously the only one that did *not* gate. It escalated to a
        // transient banner instead, which re-fired on every attach and could be
        // scrolled past, so the highest-risk state had the weakest affordance.
        let changed = gate_request(&snapshot(
            WorkspaceTrustState::Changed,
            WorkspaceTrustState::Changed,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Changed,
        ))
        .expect("changed content must gate: the earlier approval was for other bytes");

        let quarantined = gate_request(&snapshot(
            WorkspaceTrustState::Quarantined,
            WorkspaceTrustState::Quarantined,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Quarantined,
        ))
        .expect("never-trusted content must gate");

        // Same gate, same domains — only the wording and the badge differ. The
        // badge is the point: a re-review must be distinguishable from a first
        // contact at a glance.
        let paths = |r: &UserQuestionRequest| {
            r.questions
                .first()
                .unwrap()
                .options
                .iter()
                .map(|o| {
                    o.label
                        .split("  ·  ")
                        .next()
                        .unwrap_or(&o.label)
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&changed), paths(&quarantined));
        assert!(
            changed.questions[0]
                .options
                .iter()
                .all(|o| o.label.contains("[Changed]")),
            "a re-review must be badged as changed, not as new: {:?}",
            paths(&changed)
        );
        assert!(
            quarantined.questions[0]
                .options
                .iter()
                .all(|o| o.label.contains("[New]")),
            "a first contact must be badged as new"
        );
    }

    #[test]
    fn expired_domains_gate_for_routine_re_attestation() {
        let req = gate_request(&snapshot(
            WorkspaceTrustState::Expired,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
        ))
        .expect("a lapsed lease needs re-attestation");
        assert!(req.questions[0].options[0].label.contains("[Expired]"));
    }

    #[test]
    fn the_gate_question_names_why_it_is_asking() {
        // A first-contact prompt and a re-review prompt have identical options
        // but are different questions. Saying "authorize these assets" when the
        // user already did — and the bytes changed underneath them — hides the
        // one fact that makes the decision informed.
        let changed = gate_request(&snapshot(
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Changed,
        ))
        .expect("changed instructions must gate");
        let question = &changed.questions[0].question;
        assert!(
            question.contains("changed on disk") && question.contains("AGENTS.md"),
            "a re-review must name what changed: {question}"
        );
        assert!(
            !question.contains("never reviewed"),
            "a re-review must not be described as a first contact: {question}"
        );

        let fresh = gate_request(&snapshot(
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Quarantined,
        ))
        .expect("never-trusted instructions must gate");
        let question = &fresh.questions[0].question;
        assert!(
            question.contains("never reviewed") && !question.contains("changed on disk"),
            "a first contact must not be described as a change: {question}"
        );
    }

    #[test]
    fn quarantined_opens_gate_listing_only_present_domains() {
        let req = gate_request(&snapshot(
            WorkspaceTrustState::Quarantined,
            WorkspaceTrustState::Quarantined,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Quarantined,
        ))
        .expect("quarantined workspace must gate");
        assert_eq!(req.id, TRUST_GATE_REQUEST_ID);
        let q = req.questions.first().unwrap();
        assert!(q.multi_select);
        let labels: Vec<&str> = q.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                ".nuo/mcp.json  ·  [New]",
                ".nuo/skills/  ·  [New]",
                "AGENTS.md  ·  [New]"
            ]
        );
    }

    #[test]
    fn answers_map_to_domains() {
        let domains = answer_to_domains(&[vec![
            ".nuo/mcp.json  ·  [New]".to_string(),
            "AGENTS.md  ·  [Changed]".to_string(),
        ]]);
        assert_eq!(domains, vec![TrustDomain::Mcp, TrustDomain::Instructions]);

        let empty = answer_to_domains(&[]);
        assert!(empty.is_empty());
    }

    #[test]
    fn user_assets_quarantine_triggers_gate_in_workspace_free() {
        let mut snap = WorkspaceSecuritySnapshot::new("workspace-free");
        snap.user_assets = WorkspaceTrustState::Quarantined;
        let req = gate_request(&snap)
            .expect("untrusted user assets must trigger gate even without workspace");
        let q = req.questions.first().unwrap();
        assert_eq!(q.options[0].label, "~/.config/nuo/config.toml  ·  [New]");
    }
}
