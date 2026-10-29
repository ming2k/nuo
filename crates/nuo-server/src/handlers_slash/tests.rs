use super::security_ops::{TrustRoute, parse_trust_domain, trust_route};
use super::session_route::{
    SessionRoute, parse_confinement_arg, parse_unattended_arg, session_route,
};

#[cfg(test)]
mod session_route_tests {
    use super::{SessionRoute, session_route};

    fn parts(cmd: &str) -> Vec<&str> {
        cmd.split_whitespace().collect()
    }

    #[test]
    fn canonical_sessions_forms() {
        assert_eq!(
            session_route("threads", &parts("/threads")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("threads", &parts("/threads abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        assert_eq!(
            session_route("sessions", &parts("/sessions")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("sessions", &parts("/sessions abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
    }

    #[test]
    fn legacy_resume_keeps_its_id_slot() {
        assert_eq!(
            session_route("resume", &parts("/resume")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("resume", &parts("/resume abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
    }

    #[test]
    fn legacy_session_subcommands_translate() {
        assert_eq!(
            session_route("session", &parts("/session open abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        assert_eq!(
            session_route("session", &parts("/session resume abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        assert_eq!(
            session_route("session", &parts("/session")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session open")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session list")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session new")),
            Ok(SessionRoute::New)
        );
        assert_eq!(
            session_route("session", &parts("/session fork")),
            Ok(SessionRoute::Fork)
        );
        assert_eq!(
            session_route("session", &parts("/session status")),
            Ok(SessionRoute::Status)
        );
    }

    #[test]
    fn unknown_legacy_subcommand_is_an_error() {
        let err = session_route("session", &parts("/session frobnicate")).unwrap_err();
        assert!(
            err.contains("/session is retired"),
            "error should steer away from the retired command: {err}"
        );
    }

    #[test]
    fn fork_target_parsing_granular() {
        use super::super::session_ops::parse_fork_target;
        assert_eq!(
            parse_fork_target("").unwrap(),
            nuo_persistence::ForkTarget::Head
        );
        assert_eq!(
            parse_fork_target("  ").unwrap(),
            nuo_persistence::ForkTarget::Head
        );
        assert_eq!(
            parse_fork_target("2").unwrap(),
            nuo_persistence::ForkTarget::Round(2)
        );
        assert_eq!(
            parse_fork_target("r3").unwrap(),
            nuo_persistence::ForkTarget::Round(3)
        );
        assert_eq!(
            parse_fork_target("R4").unwrap(),
            nuo_persistence::ForkTarget::Round(4)
        );
        assert_eq!(
            parse_fork_target("2.1").unwrap(),
            nuo_persistence::ForkTarget::Turn { round: 2, turn: 1 }
        );
        assert_eq!(
            parse_fork_target("r2.0").unwrap(),
            nuo_persistence::ForkTarget::Turn { round: 2, turn: 0 }
        );
        assert!(parse_fork_target("xyz").is_err());
    }
}

#[cfg(test)]
mod trust_route_tests {
    use super::{TrustRoute, trust_route};
    use nuo_wire::TrustDomain;

    fn parts(command: &str) -> Vec<&str> {
        command.split_whitespace().collect()
    }

    #[test]
    fn canonical_trust_grammar_is_closed() {
        assert_eq!(
            trust_route("trust", &parts("/trust")),
            Ok(TrustRoute::GrantAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust all")),
            Ok(TrustRoute::GrantAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust mcp")),
            Ok(TrustRoute::Grant(TrustDomain::Mcp))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust skills")),
            Ok(TrustRoute::Grant(TrustDomain::Skills))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust hooks")),
            Ok(TrustRoute::Grant(TrustDomain::Hooks))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust instructions")),
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust agents")),
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust rules")),
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust ex-workspace")),
            Ok(TrustRoute::Grant(TrustDomain::ExWorkspace))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust externals")),
            Ok(TrustRoute::Grant(TrustDomain::ExWorkspace))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust status")),
            Ok(TrustRoute::Status)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust revoke")),
            Ok(TrustRoute::Revoke)
        );
    }

    #[test]
    fn deny_is_a_real_subcommand_with_the_grant_grammar() {
        // The exit the attach banner used to promise but did not have. `deny`
        // records a refusal, which is the *only* way to stop the escalation
        // without revoking every grant (that is `/untrust`).
        assert_eq!(
            trust_route("trust", &parts("/trust deny")),
            Ok(TrustRoute::DenyAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust deny all")),
            Ok(TrustRoute::DenyAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust deny instructions")),
            Ok(TrustRoute::Deny(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust deny AGENTS.md")),
            Ok(TrustRoute::Deny(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust deny user-assets")),
            Ok(TrustRoute::Deny(TrustDomain::UserAssets))
        );
        // The same domain vocabulary as a grant, so a user cannot learn one
        // spelling for yes and a different one for no.
        for domain in [
            "mcp",
            "skills",
            "hooks",
            "instructions",
            "ex-workspace",
            "user-assets",
        ] {
            let granted = trust_route("trust", &parts(&format!("/trust {domain}"))).unwrap();
            let denied = trust_route("trust", &parts(&format!("/trust deny {domain}"))).unwrap();
            let granted_domain = match granted {
                TrustRoute::Grant(domain) => domain,
                other => panic!("expected a grant for {domain}: {other:?}"),
            };
            assert_eq!(
                denied,
                TrustRoute::Deny(granted_domain),
                "deny must accept the same domain names as grant ({domain})"
            );
        }
    }

    #[test]
    fn deny_rejects_an_unknown_domain() {
        let err = trust_route("trust", &parts("/trust deny frobnicate")).unwrap_err();
        assert!(
            err.contains("Unknown trust domain"),
            "an unknown deny target must name the valid vocabulary: {err}"
        );
    }

    #[test]
    fn untrust_takes_no_arguments() {
        assert_eq!(
            trust_route("untrust", &parts("/untrust")),
            Ok(TrustRoute::Revoke)
        );
        assert!(trust_route("untrust", &parts("/untrust all")).is_err());
        assert!(trust_route("untrust", &parts("/untrust mcp")).is_err());
        // The rejection points at the refusal verb instead of leaving the user
        // to guess that `/untrust mcp` means something else entirely.
        let err = trust_route("untrust", &parts("/untrust mcp")).unwrap_err();
        assert!(
            err.contains("/trust deny"),
            "the error must name the per-domain exit: {err}"
        );
    }

    #[test]
    fn unknown_trust_subcommand_is_an_error() {
        let err = trust_route("trust", &parts("/trust frobnicate")).unwrap_err();
        assert!(
            err.contains("Unknown /trust subcommand 'frobnicate'"),
            "unexpected error message: {err}"
        );
    }
}

#[cfg(test)]
mod unattended_arg_tests {
    use super::parse_unattended_arg;

    #[test]
    fn empty_arg_is_toggle() {
        assert_eq!(parse_unattended_arg(""), Ok(None));
    }

    #[test]
    fn truthy_forms() {
        for s in ["on", "true", "1", "unattended", "auto", "delegate", "yolo"] {
            assert_eq!(parse_unattended_arg(s), Ok(Some(true)), "failed on {s:?}");
        }
    }

    #[test]
    fn falsy_forms() {
        for s in ["off", "false", "0", "disable", "disabled", "attended"] {
            assert_eq!(parse_unattended_arg(s), Ok(Some(false)), "failed on {s:?}");
        }
    }

    #[test]
    fn unknown_forms_error() {
        assert!(parse_unattended_arg("yes").is_err());
        assert!(parse_unattended_arg("no").is_err());
        assert!(parse_unattended_arg("random").is_err());
    }
}

#[cfg(test)]
mod confinement_arg_tests {
    use super::parse_confinement_arg;

    #[test]
    fn empty_arg_is_toggle() {
        assert_eq!(parse_confinement_arg(""), Ok(None));
        assert_eq!(parse_confinement_arg("   "), Ok(None));
    }

    #[test]
    fn enable_forms() {
        for s in [
            "on", "true", "1", "enable", "enabled", "confine", "confined", "jail",
        ] {
            assert_eq!(parse_confinement_arg(s), Ok(Some(true)), "failed on {s:?}");
        }
    }

    #[test]
    fn disable_forms() {
        for s in [
            "off",
            "false",
            "0",
            "disable",
            "disabled",
            "unconfine",
            "unconfined",
            "escape",
        ] {
            assert_eq!(parse_confinement_arg(s), Ok(Some(false)), "failed on {s:?}");
        }
    }

    #[test]
    fn unknown_forms_error() {
        assert!(parse_confinement_arg("yes").is_err());
        assert!(parse_confinement_arg("no").is_err());
        assert!(parse_confinement_arg("sandbox").is_err());
    }
}

#[cfg(test)]
mod trust_domain_tests {
    use super::parse_trust_domain;
    use nuo_wire::TrustDomain;

    #[test]
    fn known_domains_parse() {
        assert_eq!(parse_trust_domain("mcp"), Ok(TrustDomain::Mcp));
        assert_eq!(parse_trust_domain("skills"), Ok(TrustDomain::Skills));
        assert_eq!(parse_trust_domain("hooks"), Ok(TrustDomain::Hooks));
        assert_eq!(
            parse_trust_domain("instructions"),
            Ok(TrustDomain::Instructions)
        );
        assert_eq!(parse_trust_domain("agents"), Ok(TrustDomain::Instructions));
        assert_eq!(parse_trust_domain("rules"), Ok(TrustDomain::Instructions));
        assert_eq!(
            parse_trust_domain("AGENTS.md"),
            Ok(TrustDomain::Instructions)
        );
        assert_eq!(
            parse_trust_domain("agents.md"),
            Ok(TrustDomain::Instructions)
        );
        assert_eq!(parse_trust_domain(".nuo/mcp.json"), Ok(TrustDomain::Mcp));
        assert_eq!(
            parse_trust_domain("ex-workspace"),
            Ok(TrustDomain::ExWorkspace)
        );
        assert_eq!(
            parse_trust_domain("externals"),
            Ok(TrustDomain::ExWorkspace)
        );
    }

    #[test]
    fn unknown_domain_is_an_error() {
        let err = parse_trust_domain("unknown").unwrap_err();
        assert!(
            err.contains("Unknown trust domain `unknown`"),
            "unexpected error message: {err}"
        );
    }

    #[tokio::test]
    async fn reload_trusted_assets_updates_roots_on_trust() {
        use crate::handlers_slash::security_ops;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let external = tmp.path().join("extra");
        std::fs::create_dir_all(root.join(".nuo")).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(
            root.join(".nuo/config.toml"),
            "[workspace]\nadditional_roots = [\"../extra\"]\n",
        )
        .unwrap();

        let sec_file = tmp.path().join("state/security.json");
        let store =
            nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(sec_file);
        let agent = Arc::new(nuo_harness::Agent::new(
            Arc::new(nuo_harness::NoProvider),
            vec![],
            nuo_wire::AgentIdentity::default(),
        ));
        let mcp = Arc::new(crate::mcp::McpRuntime::start_background(
            Default::default(),
            agent.dynamic_tool_sink(),
        ));
        let skills = nuo_harness::cognition::skills::SkillRegistry::empty();
        let shared_roots = nuo_wire::SharedAdditionalRoots::empty();

        // Initially untrusted: roots should remain quarantined and empty
        let report = security_ops::reload_trusted_assets(
            &agent,
            &mcp,
            &store,
            &root,
            &skills,
            &shared_roots,
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.ex_workspace,
            nuo_wire::WorkspaceTrustState::Quarantined
        );
        assert!(shared_roots.snapshot().is_empty());

        // Trust ex-workspace domain: additional roots are dynamically resolved and stored in shared_roots
        store.trust_domain(&root, TrustDomain::ExWorkspace).unwrap();
        let report = security_ops::reload_trusted_assets(
            &agent,
            &mcp,
            &store,
            &root,
            &skills,
            &shared_roots,
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.ex_workspace,
            nuo_wire::WorkspaceTrustState::Trusted
        );
        let canonical_extra = std::fs::canonicalize(&external).unwrap();
        assert_eq!(shared_roots.snapshot(), vec![canonical_extra]);

        // Revoke trust: roots quarantined and shared_roots cleared again
        store.revoke_workspace(&root).unwrap();
        let report = security_ops::reload_trusted_assets(
            &agent,
            &mcp,
            &store,
            &root,
            &skills,
            &shared_roots,
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.ex_workspace,
            nuo_wire::WorkspaceTrustState::Quarantined
        );
        assert!(shared_roots.snapshot().is_empty());
    }
}

#[cfg(test)]
mod role_command_tests {
    use crate::startup::BuiltinCmd;
    use nuo_wire::MainAgentRole;

    #[test]
    fn role_command_resolves_to_builtin_role() {
        assert_eq!(BuiltinCmd::from_slash("/role"), Some(BuiltinCmd::Role));
    }

    #[test]
    fn telemetry_commands_resolve_to_builtin_cmds() {
        assert_eq!(BuiltinCmd::from_slash("/stats"), Some(BuiltinCmd::Stats));
        assert_eq!(BuiltinCmd::from_slash("/trace"), Some(BuiltinCmd::Trace));
    }

    #[test]
    fn builtin_roles_include_developer_philosophist_and_ops() {
        assert_eq!(MainAgentRole::ALL.len(), 3);
        assert_eq!(MainAgentRole::ALL[0], MainAgentRole::Developer);
        assert_eq!(MainAgentRole::ALL[1], MainAgentRole::Philosophist);
        assert_eq!(MainAgentRole::ALL[2], MainAgentRole::Ops);
        assert_eq!(MainAgentRole::ALL[0].as_str(), "developer");
        assert_eq!(MainAgentRole::ALL[1].as_str(), "philosophist");
        assert_eq!(MainAgentRole::ALL[2].as_str(), "ops");
    }
}

#[cfg(test)]
mod unknown_command_tests {
    use nuo_wire::CommandResult;

    /// A typo must come back as a **typed, machine-readable** result that names
    /// the intended command: the client deliberately routes every `/`-prefixed
    /// draft to the dispatcher instead of downgrading it to a prompt, so this is
    /// what a typo produces, and the client uses the payload to hand the draft
    /// back to the composer.
    #[test]
    fn typo_names_the_command_that_was_meant() {
        let result = CommandResult::UnknownCommand {
            name: "/modles".to_string(),
            suggestion: crate::startup::suggest_command("/modles").map(str::to_string),
        };
        assert!(
            matches!(
                &result,
                CommandResult::UnknownCommand { suggestion: Some(s), .. } if s == "/models"
            ),
            "{result:?}"
        );
        let text = result.to_text();
        assert!(text.starts_with("Unknown command: /modles."), "{text}");
        assert!(text.contains("Did you mean /models?"), "{text}");
    }

    /// Prose that merely starts with a slash gets the escape hatch, never a
    /// bogus suggestion.
    #[test]
    fn unrelated_slash_text_gets_the_escape_hatch() {
        let result = CommandResult::UnknownCommand {
            name: "/etc/hosts".to_string(),
            suggestion: crate::startup::suggest_command("/etc/hosts").map(str::to_string),
        };
        assert!(
            matches!(
                &result,
                CommandResult::UnknownCommand {
                    suggestion: None,
                    ..
                }
            ),
            "{result:?}"
        );
        let text = result.to_text();
        assert!(!text.contains("Did you mean"), "{text}");
        assert!(
            text.contains("prefix the input with a space"),
            "the sent-as-text escape must be discoverable: {text}"
        );
    }
}

mod retired_aside_redirect_tests {
    use std::sync::Arc;

    use nuo_persistence::session::SessionStore;
    use nuo_wire::{AgentResponse, CommandResult, Message, Role, RoundEvent};
    use tokio::sync::mpsc;

    use super::super::session_ops::fork_retired_aside;

    /// `[INV-DEPR-01]`: a submitted `/btw <prompt>` forks at head and carries
    /// the message into the new branch, leaving the parent untouched.
    #[tokio::test]
    async fn btw_forks_at_head_and_carries_the_prompt_into_the_branch() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![
                Message::new(Role::User, "hello"),
                Message::new(Role::Assistant, "hi"),
            ])
            .await
            .unwrap();
        let parent_id = store.id().await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        fork_retired_aside(&store, &tx, "/btw", "why is the sky blue").await;

        let mut forked = None;
        let mut texts: Vec<String> = Vec::new();
        while let Ok(resp) = rx.try_recv() {
            match resp {
                AgentResponse::SessionForked {
                    child_id, prompt, ..
                } => forked = Some((child_id, prompt)),
                AgentResponse::Round {
                    event:
                        RoundEvent::CommandResult {
                            result: CommandResult::Text(text),
                            ..
                        },
                    ..
                } => texts.push(text),
                _ => {}
            }
        }

        let (child_id, prompt) = forked.expect("a retired /btw must fork");
        assert_eq!(prompt.as_deref(), Some("why is the sky blue"));
        assert_ne!(child_id, parent_id, "the child is a new sovereign thread");
        // Sovereign fork [INV-FORK-04]: the parent pointer is never repointed.
        assert_eq!(store.id().await, parent_id);

        assert!(
            texts.iter().any(|t| t.contains("retired")),
            "guidance must explain the retirement: {texts:?}"
        );
    }

    /// `[INV-LINEAGE-01]`: the announcement carries the same fork point the
    /// child persisted, and the picker's projection surfaces it. One fact, one
    /// writer, every reader.
    #[tokio::test]
    async fn fork_announcement_and_overview_carry_the_persisted_fork_point() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![
                Message::new(Role::User, "r1 user"),
                Message::new(Role::Assistant, "r1 answer"),
                Message::new(Role::User, "r2 user"),
                Message::new(Role::Assistant, "r2 answer"),
            ])
            .await
            .unwrap();
        store.set_round_counter(2).await.unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        super::super::session_ops::fork_current_session(&store, &tx, "/fork", "2.0").await;

        let mut announced = None;
        while let Ok(resp) = rx.try_recv() {
            if let AgentResponse::SessionForked {
                child_id,
                fork_point,
                ..
            } = resp
            {
                announced = Some((child_id, fork_point));
            }
        }
        let (child_id, fork_point) = announced.expect("a /fork must announce the branch");
        assert_eq!(fork_point, Some(nuo_wire::ForkPoint::at_turn(2, 0)));

        // The picker's own projection reads the same value off the store — it
        // does not re-derive it from the announcement.
        let overview = crate::session_view::build_sessions_overview(&store).await;
        let child = overview
            .iter()
            .find(|row| row.id == child_id)
            .expect("the branch appears in the overview");
        assert_eq!(child.fork_point, Some(nuo_wire::ForkPoint::at_turn(2, 0)));
        assert_eq!(child.fork_kind, nuo_wire::SessionForkKind::Fork);
        assert_eq!(child.parent_id.as_deref(), Some(store.id().await.as_str()));
        // A fork no longer invents a title to carry its provenance.
        assert_eq!(child.title, None, "provenance is not smuggled into a title");
    }

    /// A bare `/btw` still forks, but carries no opening message.
    #[tokio::test]
    async fn bare_btw_forks_without_a_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SessionStore::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![Message::new(Role::User, "hello")])
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        fork_retired_aside(&store, &tx, "/btw", "   ").await;

        let mut prompt_seen = None;
        while let Ok(resp) = rx.try_recv() {
            if let AgentResponse::SessionForked { prompt, .. } = resp {
                prompt_seen = Some(prompt);
            }
        }
        assert_eq!(prompt_seen, Some(None), "a bare /btw carries no prompt");
    }
}
