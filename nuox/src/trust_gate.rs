//! Pre-view workspace trust gate.
//!
//! When the client attaches to a session whose project root carries
//! project-authored contributions (skills, MCP, hooks, rules) that were
//! **never** trusted — `WorkspaceTrustState::Quarantined` from the durable
//! `WorkspaceSecurityStore` — the daemon's attach-sync `HarnessState`
//! already carries the security snapshot. This module turns that snapshot
//! into the *first* thing the user sees: a blocking question dialog opened
//! before the composer takes input, so the trust decision happens up front
//! instead of via a passive banner the user can (and usually does) scroll
//! past.
//!
//! The dialog is deliberately not a new modal. It reuses the ask_user
//! question sheet ([`crate::question_model::QuestionModel`]) by synthesizing
//! a [`UserQuestionRequest`] into the pending-question queue, so navigation,
//! rendering, scrolling, and multi-select semantics are the ones the user
//! already knows. Two seams keep the synthetic request honest:
//!
//! - **Identity.** The request id is the constant
//!   [`TRUST_GATE_REQUEST_ID`]. The reply path intercepts that id and maps
//!   the answer to the canonical `/trust …` slash command — never a bespoke
//!   wire message — so persistence *and* the atomic live reload stay owned
//!   by the one code path that already handles them. A synthetic request is
//!   never forwarded as `UserQuestionReply` (the daemon has no parked round
//!   waiting for it).
//! - **Lifecycle.** The gate is fed by `HarnessState` snapshots. It opens
//!   once per quarantined attach and closes as soon as a snapshot reports
//!   the workspace trusted (the `/trust` handler republishes the snapshot),
//!   so a stale dialog can never linger after the decision.
//!
//! Escaping the dialog is an explicit "keep quarantined" — the same outcome
//! as picking the option — which mirrors how the old banner left the
//! workspace untrusted, just with a decision the user actually made.

use nuo_contracts::{
    TrustDomain, UserQuestion, UserQuestionOption, UserQuestionRequest, WorkspaceSecuritySnapshot,
    WorkspaceTrustState,
};

/// Request id marking the synthesized trust-gate question. Recognized by the
/// reply path (`super::event_loop`) and never sent to the daemon as a
/// `UserQuestionReply`.
pub const TRUST_GATE_REQUEST_ID: &str = "__workspace_trust_gate__";

/// Which quarantined domains the snapshot advertises, in display order.
/// Only `Absent` domains are skipped: there is nothing to decide about a
/// domain the workspace does not use.
pub fn quarantined_domains(snapshot: &WorkspaceSecuritySnapshot) -> Vec<TrustDomain> {
    [
        (TrustDomain::Mcp, snapshot.mcp),
        (TrustDomain::Skills, snapshot.skills),
        (TrustDomain::Hooks, snapshot.hooks),
        (TrustDomain::Instructions, snapshot.instructions),
        (TrustDomain::ExWorkspace, snapshot.ex_workspace),
        (TrustDomain::UserAssets, snapshot.user_assets),
    ]
    .into_iter()
    .filter(|(_, state)| {
        matches!(
            *state,
            WorkspaceTrustState::Quarantined
                | WorkspaceTrustState::Changed
                | WorkspaceTrustState::Expired
        )
    })
    .map(|(domain, _)| domain)
    .collect()
}

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

/// Build the trust-gate question request for unverified file assets, or
/// `None` when nothing needs gating (trusted, absent, or explicitly denied).
pub fn gate_request(snapshot: &WorkspaceSecuritySnapshot) -> Option<UserQuestionRequest> {
    let domains = quarantined_domains(snapshot);
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
            question: "Select unverified file assets to authorize:\n(Unselected assets will remain quarantined and invisible during this session)".to_string(),
            options,
            multi_select: true,
        }],
        origin: Some("file asset trust".to_string()),
    })
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

pub fn domain_label(domain: TrustDomain) -> &'static str {
    match domain {
        TrustDomain::UserAssets => "~/.config/nuo/config.toml",
        TrustDomain::ExWorkspace => ".nuo/config.toml",
        TrustDomain::Mcp => ".nuo/mcp.json",
        TrustDomain::Skills => ".nuo/skills/",
        TrustDomain::Hooks => ".nuo/hooks/",
        TrustDomain::Instructions => "AGENTS.md",
    }
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
        // Explicitly denied domains do not gate again (ADR-0253 zero-nagging).
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
