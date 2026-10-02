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
}

#[cfg(test)]
mod trust_route_tests {
    use super::{TrustRoute, trust_route};
    use nuo_contracts::TrustDomain;

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
    fn untrust_takes_no_arguments() {
        assert_eq!(
            trust_route("untrust", &parts("/untrust")),
            Ok(TrustRoute::Revoke)
        );
        assert!(trust_route("untrust", &parts("/untrust all")).is_err());
        assert!(trust_route("untrust", &parts("/untrust mcp")).is_err());
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
    use nuo_contracts::TrustDomain;

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
            nuo_contracts::AgentIdentity::default(),
        ));
        let mcp = Arc::new(crate::mcp::McpRuntime::start_background(
            Default::default(),
            agent.dynamic_tool_sink(),
        ));
        let skills = nuo_harness::skills::SkillRegistry::empty();
        let shared_roots = nuo_contracts::SharedAdditionalRoots::empty();

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
            nuo_contracts::WorkspaceTrustState::Quarantined
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
            nuo_contracts::WorkspaceTrustState::Trusted
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
            nuo_contracts::WorkspaceTrustState::Quarantined
        );
        assert!(shared_roots.snapshot().is_empty());
    }
}

#[cfg(test)]
mod role_command_tests {
    use crate::startup::BuiltinCmd;
    use nuo_contracts::MainAgentRole;

    #[test]
    fn role_command_resolves_to_builtin_role() {
        assert_eq!(BuiltinCmd::from_slash("/role"), Some(BuiltinCmd::Role));
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
