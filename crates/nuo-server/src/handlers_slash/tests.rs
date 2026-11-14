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
    use nuo_protocol::TrustDomain;

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
    use nuo_protocol::TrustDomain;

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
            nuo_protocol::AgentIdentity::default(),
        ));
        let mcp = Arc::new(crate::mcp::McpRuntime::start_background(
            Default::default(),
            agent.dynamic_tool_sink(),
        ));
        let skills = nuo_harness::cognition::skills::SkillRegistry::empty();
        let shared_roots = nuo_protocol::SharedAdditionalRoots::empty();

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
            nuo_protocol::WorkspaceTrustState::Quarantined
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
            nuo_protocol::WorkspaceTrustState::Trusted
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
            nuo_protocol::WorkspaceTrustState::Quarantined
        );
        assert!(shared_roots.snapshot().is_empty());
    }
}

#[cfg(test)]
mod role_command_tests {
    use crate::startup::BuiltinCmd;
    use nuo_protocol::MainAgentRole;

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
    use nuo_protocol::CommandResult;

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

    use nuo_persistence::thread::ThreadSession;
    use nuo_protocol::{AgentResponse, CommandResult, Message, Role, RoundEvent};
    use tokio::sync::mpsc;

    use super::super::session_ops::fork_retired_aside;

    /// `[INV-DEPR-01]`: a submitted `/btw <prompt>` forks a branch at head.
    /// The branch shares facts and the current view switches to it.
    #[tokio::test]
    async fn btw_forks_a_branch_at_head() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![
                Message::new(Role::User, "hello"),
                Message::new(Role::Assistant, "hi"),
            ])
            .await
            .unwrap();
        let before = store.model_window().await;

        let (tx, mut rx) = mpsc::unbounded_channel();
        fork_retired_aside(&store, &tx, "/btw", "why is the sky blue").await;

        let mut texts: Vec<String> = Vec::new();
        while let Ok(resp) = rx.try_recv() {
            if let AgentResponse::Round {
                scope:
                    nuo_protocol::RoundScope::Session {
                        event:
                            RoundEvent::CommandResult {
                                result: CommandResult::Text(text),
                                ..
                            },
                    },
                ..
            } = resp
            {
                texts.push(text);
            }
        }

        assert_eq!(store.branches().await.unwrap().len(), 2);
        assert_eq!(store.model_window().await, before);
        assert!(
            texts.iter().any(|t| t.contains("retired")),
            "guidance must explain the retirement: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Forked branch")),
            "the command must announce the branch: {texts:?}"
        );
    }

    /// `/fork` writes a branch cursor only. The fact graph is unchanged and the
    /// active view moves to the new branch.
    #[tokio::test]
    async fn fork_command_writes_a_branch_cursor_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
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
        let facts_before = store.thread_aggregate().await.unwrap().graph.len();

        let (tx, mut rx) = mpsc::unbounded_channel();
        super::super::session_ops::fork_current_session(&store, &tx, "/fork", "2.0").await;
        while rx.try_recv().is_ok() {}

        let aggregate = store.thread_aggregate().await.unwrap();
        assert_eq!(aggregate.graph.len(), facts_before, "fork copied facts");
        assert_eq!(aggregate.state.branches.len(), 2);
        assert_ne!(
            aggregate.state.active_branch,
            nuo_protocol::BranchId::main(),
            "the fork must switch the view to the new cursor"
        );
    }

    /// A bare `/btw` still forks, but carries no opening message.
    #[tokio::test]
    async fn bare_btw_forks_without_prompt_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ThreadSession::for_path(dir.path().join("session.json")));
        store
            .replace_messages(vec![Message::new(Role::User, "hello")])
            .await
            .unwrap();

        let (tx, mut rx) = mpsc::unbounded_channel();
        fork_retired_aside(&store, &tx, "/btw", "   ").await;
        while rx.try_recv().is_ok() {}

        assert_eq!(store.branches().await.unwrap().len(), 2);
    }
}

/// ADR-0140 `[INV-CMD-05]`/`[INV-CMD-06]`: a reading is not a record.
///
/// The classification lives in the one vocabulary table, and the recorder funnel
/// decides residency from it — so these tests check the *mechanism*, not a
/// handler's discipline: the same helper call that records an act must record
/// nothing for a reading, and must still emit the reply in both cases.
mod command_effect_tests {
    use std::sync::Arc;

    use nuo_harness::orchestration::round_response;
    use nuo_persistence::thread::ThreadSession;
    use nuo_protocol::{AgentNotice, AgentResponse, CommandEffect, CommandResult, RoundEvent};
    use tokio::sync::mpsc;

    use crate::handlers_slash::record::{
        record_ack, record_command, record_error, record_invocation, record_surface_only,
    };
    use crate::startup::{BuiltinCmd, effect_for};

    fn store(dir: &tempfile::TempDir) -> Arc<ThreadSession> {
        Arc::new(ThreadSession::for_path(dir.path().join("session.json")))
    }

    /// Every `CommandResult` a fake reading emits, so the test cannot pass by
    /// emitting a shape the funnel happens to ignore.
    fn drain(rx: &mut mpsc::UnboundedReceiver<AgentResponse>) -> Vec<RoundEvent> {
        let mut out = Vec::new();
        while let Ok(resp) = rx.try_recv() {
            // A round envelope carries other events too; only command results
            // matter here, and only `CommandResult` frames are collected.
            if let AgentResponse::Round { scope, .. } = resp
                && matches!(scope.event(), RoundEvent::CommandResult { .. })
            {
                out.push(scope.into_event());
            }
        }
        out
    }

    /// `[INV-CMD-05]`: every built-in declares a class, and the resolver agrees
    /// with the declaration. The macro makes omission a compile error, so this
    /// pins the *resolution* — the part a wildcard arm could silently break.
    #[test]
    fn every_builtin_declares_an_effect() {
        for (name, _summary) in BuiltinCmd::ALL {
            let cmd = BuiltinCmd::from_slash(name).unwrap_or_else(|| {
                panic!("{name} is in BuiltinCmd::ALL but does not resolve through from_slash")
            });
            let declared = cmd.effect();
            assert_eq!(
                effect_for(&[name]),
                declared,
                "{name}: effect_for must resolve the declared bare effect"
            );
            assert_eq!(
                cmd.effect_for(None),
                declared,
                "{name}: an argument-less invocation takes the bare effect"
            );
            // Every declared verb must resolve to its own declaration.
            for (verb, _summary, verb_effect) in cmd.spec().subcommands {
                assert_eq!(
                    effect_for(&[name, verb]),
                    *verb_effect,
                    "{name} {verb}: the declared verb effect must win"
                );
            }
        }
    }

    /// The classification itself, pinned. Changing a command's class is a design
    /// decision (ADR-0140 §2), so it must be a visible diff here rather than a
    /// quiet edit in the table.
    #[test]
    fn effect_table_is_pinned() {
        // Acts: durable state the thread must remember.
        for name in [
            "/models",
            "/connections",
            "/tools",
            "/mcp",
            "/compact",
            "/new",
            "/unattended",
            "/confinement",
            "/fork",
            "/undo",
            "/init",
            "/trust",
            "/untrust",
            "/retry",
        ] {
            assert_eq!(
                effect_for(&[name]),
                CommandEffect::Mutation,
                "{name} must be an act"
            );
        }
        // Readings: the state of the world at the moment of asking.
        for name in [
            "/permissions",
            "/search",
            "/tree",
            "/diff",
            "/stats",
            "/trace",
            "/usage",
            "/quotas",
            "/jobs",
            "/skills",
            "/export",
            "/help",
            "/exit",
        ] {
            assert_eq!(
                effect_for(&[name]),
                CommandEffect::Reading,
                "{name} must be a reading"
            );
        }
        // The two fields exist because these differ.
        assert_eq!(
            effect_for(&["/role"]),
            CommandEffect::Reading,
            "bare /role lists the vocabulary"
        );
        assert_eq!(
            effect_for(&["/role", "ops"]),
            CommandEffect::Mutation,
            "a role id switches identity — and ids are dynamic, so no verb table enumerates them"
        );
        assert_eq!(
            effect_for(&["/threads"]),
            CommandEffect::Reading,
            "bare /threads opens the picker"
        );
        assert_eq!(
            effect_for(&["/threads", "0195"]),
            CommandEffect::Mutation,
            "opening a thread switches it"
        );
        // A verb mutates inside a reading command, and reads inside an act.
        assert_eq!(
            effect_for(&["/jobs", "kill"]),
            CommandEffect::Mutation,
            "/jobs kill is an act inside a reading command"
        );
        assert_eq!(
            effect_for(&["/jobs", "logs"]),
            CommandEffect::Reading,
            "/jobs logs is a read"
        );
        assert_eq!(
            effect_for(&["/trust", "status"]),
            CommandEffect::Reading,
            "/trust status is a read inside an act"
        );
        assert_eq!(
            effect_for(&["/permissions", "clear"]),
            CommandEffect::Mutation,
            "/permissions clear is an act inside a reading command"
        );
        // An undeclared argument takes the handler's `_` arm semantics: `/jobs`
        // lists jobs whatever the argument, so it is a reading.
        assert_eq!(
            effect_for(&["/jobs", "whatever"]),
            CommandEffect::Reading,
            "an undeclared /jobs argument lists jobs"
        );
    }

    /// `[INV-CMD-06]`: a reading leaves the ledger empty and still replies.
    ///
    /// `/help`, `/usage`, `/stats`, `/trace`, `/jobs`, `/jobs logs`, `/diff`,
    /// `/search`, `/skills list`, `/skills status`, `/trust status`,
    /// `/permissions`, `/role`, `/tree`, `/threads`, and `/export` all reach the
    /// funnel through one of the helpers exercised here; the point is that the
    /// *helper* refuses, so no handler can opt in.
    #[tokio::test]
    async fn readings_leave_the_ledger_empty() {
        let cases: &[(&str, &str)] = &[
            ("help", ""),
            ("usage", ""),
            ("stats", ""),
            ("trace", ""),
            ("quotas", ""),
            ("jobs", ""),
            ("jobs", "logs job_1"),
            ("diff", ""),
            ("search", "parser bug"),
            ("skills", "list"),
            ("skills", "status"),
            ("trust", "status"),
            ("permissions", ""),
            ("role", ""),
            ("tree", ""),
            ("threads", ""),
            ("export", ""),
        ];
        for (name, args) in cases {
            let dir = tempfile::tempdir().unwrap();
            let session = store(&dir);
            let (tx, mut rx) = mpsc::unbounded_channel();

            record_command(
                &session,
                &tx,
                name,
                args,
                CommandResult::Text("a snapshot".to_string()),
            )
            .await;

            assert!(
                session.commands().await.is_empty(),
                "/{name} {args} is a reading and must not reach the ledger"
            );
            let events = drain(&mut rx);
            assert_eq!(
                events.len(),
                1,
                "/{name} {args} must still reply on the wire — ephemeral is not silent"
            );
            match &events[0] {
                RoundEvent::CommandResult { effect, .. } => assert_eq!(
                    *effect,
                    CommandEffect::Reading,
                    "/{name} {args}: the reply must declare the reading class"
                ),
                other => panic!("expected a CommandResult, got {other:?}"),
            }
        }
    }

    /// The ack path used to persist *silently*: four commands were invisible to
    /// any client that could not intercept them locally. Both halves are pinned
    /// here — a reading ack records nothing, an act ack records and emits.
    #[tokio::test]
    async fn ack_emits_and_records_only_for_an_act() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);
        let (tx, mut rx) = mpsc::unbounded_channel();

        // `/stats` is a reading.
        record_ack(&session, &tx, "stats", "", "stats live in the overlay").await;
        assert!(
            session.commands().await.is_empty(),
            "a reading ack is not a record"
        );
        assert_eq!(drain(&mut rx).len(), 1, "a reading ack must still reply");

        // `/unattended on` is an act.
        record_ack(&session, &tx, "unattended", "on", "Unattended mode ON").await;
        let records = session.commands().await;
        assert_eq!(records.len(), 1, "an act ack is a record");
        assert_eq!(records[0].name, "unattended");
        assert_eq!(drain(&mut rx).len(), 1, "an act ack still replies");
    }

    /// `[INV-CMD-06]`: acts still record — the gate must not become a filter that
    /// silently drops the audit trail.
    #[tokio::test]
    async fn acts_still_record() {
        let cases: &[(&str, &str)] = &[
            ("unattended", "on"),
            ("confinement", "off"),
            ("permissions", "clear"),
            ("compact", ""),
            ("fork", ""),
            ("init", ""),
            ("undo", ""),
            ("retry", ""),
            ("trust", "deny"),
            ("new", ""),
        ];
        for (name, args) in cases {
            let dir = tempfile::tempdir().unwrap();
            let session = store(&dir);
            let (tx, mut rx) = mpsc::unbounded_channel();

            record_command(
                &session,
                &tx,
                name,
                args,
                CommandResult::Text("done".to_string()),
            )
            .await;

            let records = session.commands().await;
            assert_eq!(
                records.len(),
                1,
                "/{name} {args} is an act and must be recorded"
            );
            let events = drain(&mut rx);
            match &events[0] {
                RoundEvent::CommandResult { effect, .. } => assert_eq!(
                    *effect,
                    CommandEffect::Mutation,
                    "/{name} {args}: the reply must declare the act class"
                ),
                other => panic!("expected a CommandResult, got {other:?}"),
            }
        }
    }

    /// `[INV-CMD-06]`: a rejected command leaves no scar — of either class.
    ///
    /// Nothing ran and nothing reached the model (ADR-0119 §3), and the draft is
    /// handed back to the composer, so the ledger owes nothing. This holds
    /// regardless of the declared class, which is why it is the funnel's
    /// condition rather than a handler's.
    #[tokio::test]
    async fn a_rejection_is_never_durable() {
        for (name, args) in [("models", "nope"), ("help", "nope")] {
            let dir = tempfile::tempdir().unwrap();
            let session = store(&dir);
            let (tx, mut rx) = mpsc::unbounded_channel();

            record_command(
                &session,
                &tx,
                name,
                args,
                CommandResult::UnknownCommand {
                    name: format!("/{name}"),
                    suggestion: None,
                },
            )
            .await;

            assert!(
                session.commands().await.is_empty(),
                "a rejected /{name} must not leave a record"
            );
            assert_eq!(drain(&mut rx).len(), 1, "the rejection still replies");
        }
    }

    /// A failed act is a record; a failed reading is not. `/undo` refusing is
    /// something the user attempted and must be able to re-read; `/diff` failing
    /// is an answer that could not be produced.
    #[tokio::test]
    async fn failure_follows_the_class() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);
        let (tx, mut rx) = mpsc::unbounded_channel();

        record_error(&session, &tx, "diff", "", "could not read the journal").await;
        assert!(
            session.commands().await.is_empty(),
            "a reading that could not be produced changed nothing"
        );
        drain(&mut rx);

        record_error(&session, &tx, "undo", "", "could not read the journal").await;
        let records = session.commands().await;
        assert_eq!(records.len(), 1, "an attempted act belongs in the record");
        assert_eq!(
            records[0].status,
            nuo_protocol::CommandStatus::Error,
            "the refusal is recorded as a failure"
        );
    }

    /// An invocation-only reply for a reading (`/exit` is the live case) writes
    /// nothing; for an act (`/compact`) it writes the invocation.
    #[tokio::test]
    async fn invocation_only_follows_the_class() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);

        record_invocation(&session, "exit", "").await;
        assert!(
            session.commands().await.is_empty(),
            "/exit is a reading: quitting writes nothing"
        );

        record_invocation(&session, "compact", "").await;
        assert_eq!(
            session.commands().await.len(),
            1,
            "/compact is an act whose success is its own state event"
        );
    }

    /// A surface-owned command answers and records nothing: the client performs
    /// the act, this process never touched the state, and a record for it would
    /// be a lie in the audit.
    #[tokio::test]
    async fn a_surface_only_command_answers_and_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);
        let (tx, mut rx) = mpsc::unbounded_channel();

        record_surface_only(
            &session,
            &tx,
            "models",
            "",
            "The model picker is a client surface",
        )
        .await;

        assert!(session.commands().await.is_empty());
        assert_eq!(
            drain(&mut rx).len(),
            1,
            "a surface command must not be silently dropped"
        );
    }

    /// A name outside the built-in vocabulary is an act: extension code and
    /// project command prompts may have mutated anything, and over-recording is
    /// the safe direction.
    #[test]
    fn unknown_vocabulary_is_an_act() {
        assert_eq!(effect_for(&["/pursue", "ship it"]), CommandEffect::Mutation);
        assert_eq!(effect_for(&["/btw", "q"]), CommandEffect::Mutation);
    }

    /// The notice-bus defect this ADR fixes: a `CommandResult` frame must never
    /// be mistaken for an `AgentNotice` (the old ack path emitted the latter and
    /// persisted the former, so a non-TUI client saw neither).
    #[tokio::test]
    async fn a_reply_is_a_command_result_not_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);
        let (tx, mut rx) = mpsc::unbounded_channel();

        record_ack(&session, &tx, "usage", "", "usage lives in the overlay").await;

        // The only frames are the round envelope carrying a CommandResult.
        let mut saw_notice = false;
        while let Ok(resp) = rx.try_recv() {
            if let AgentResponse::Round { scope, .. } = resp
                && matches!(scope.event(), RoundEvent::Notice(_))
            {
                saw_notice = true;
            }
        }
        assert!(!saw_notice, "a command reply is not a notice");
        let _ = round_response(
            "s",
            RoundEvent::Notice(AgentNotice::new(
                nuo_protocol::NoticeKind::CommandAck,
                nuo_protocol::NoticeSeverity::Info,
                "x",
                nuo_protocol::NoticeSource::Harness,
            )),
        );
    }
}
