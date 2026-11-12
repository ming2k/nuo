use super::*;

#[cfg(test)]
mod attach_refusal_tests {
    //! ADR-0123 `[INV-ATTACH-01]`: the server must draw the taxonomy the client
    //! branches on, and draw it from evidence rather than from prose.

    use super::*;

    /// An id with neither a hosted entry nor a durable binding is *gone*: the
    /// client must retire the tab naming it.
    #[tokio::test]
    async fn unknown_session_is_refused_as_gone() {
        let registry = ThreadRegistry::prehost_only();
        match registry
            .resolve_with_declaration(
                AttachAction::Attach(Some("no-such-session-anywhere".into())),
                std::path::Path::new("/tmp"),
                true,
            )
            .await
        {
            ResolveOutcome::Refused { code, message } => {
                assert_eq!(
                    code,
                    Some(nuo_protocol::ERR_SESSION_GONE),
                    "a session with no durable binding is provably gone"
                );
                assert!(message.contains("no-such-session-anywhere"));
            }
            other => panic!("expected a typed refusal, got {}", describe(&other)),
        }
    }

    /// A host that cannot create sessions refuses without inventing a recovery:
    /// `None` is the contract for "no specific taxonomy".
    #[tokio::test]
    async fn missing_host_refuses_without_taxonomy() {
        let registry = ThreadRegistry::prehost_only();
        match registry
            .resolve_with_declaration(AttachAction::New(None), std::path::Path::new("/tmp"), true)
            .await
        {
            ResolveOutcome::Refused { code, .. } => assert_eq!(
                code, None,
                "an unhostable server must not authorize a client-side recovery"
            ),
            other => panic!("expected a typed refusal, got {}", describe(&other)),
        }
    }

    /// The two recoveries must never collapse into one another: a resumable
    /// session that fails to assemble is not a reason to discard its tab.
    #[test]
    fn gone_and_unresumable_are_distinct_codes() {
        assert_ne!(
            nuo_protocol::ERR_SESSION_GONE,
            nuo_protocol::ERR_SESSION_UNRESUMABLE
        );
        let gone = ResolveOutcome::refused(nuo_protocol::ERR_SESSION_GONE, "gone");
        let unresumable = ResolveOutcome::refused(nuo_protocol::ERR_SESSION_UNRESUMABLE, "boom");
        let code = |o: &ResolveOutcome| match o {
            ResolveOutcome::Refused { code, .. } => *code,
            _ => None,
        };
        assert_eq!(code(&gone), Some(nuo_protocol::ERR_SESSION_GONE));
        assert_eq!(
            code(&unresumable),
            Some(nuo_protocol::ERR_SESSION_UNRESUMABLE)
        );
    }

    /// `[INV-ATTACH-03]` (ADR-0133): the pre-Welcome claim may do bounded local
    /// identity work only. This is a source-level contract in the same spirit as
    /// the terminal's first-paint test: the compiler cannot see "no network
    /// dependency was introduced into this function", but the text can.
    #[test]
    fn claim_path_is_network_free() {
        let source = include_str!("resolution.rs");
        let start = source
            .find("pub(super) async fn claim(")
            .expect("claim exists");
        let rest = &source[start..];
        let end = rest.find("async fn build(").expect("build follows claim");
        let claim = &rest[..end];
        for forbidden in [
            "assemble_with_store",
            "refresh_oauth",
            "SkillRegistry",
            "fire_session_start",
            "full_transcript",
        ] {
            assert!(
                !claim.contains(forbidden),
                "claim must not perform `{forbidden}` before Welcome"
            );
        }
        assert!(
            claim.contains("spawn_blocking"),
            "the bounded store claim must run on the blocking pool"
        );
        assert_eq!(
            claim.matches("SessionStore::for_workspace").count(),
            1,
            "claim owns exactly one store open (no transcript load)"
        );
    }

    fn describe(outcome: &ResolveOutcome) -> &'static str {
        match outcome {
            ResolveOutcome::Welcome(_) => "Welcome",
            ResolveOutcome::Pick { .. } => "Pick",
            ResolveOutcome::Refused { .. } => "Refused",
        }
    }
}

#[cfg(test)]
mod acp_fabric_tests {
    use super::*;

    #[tokio::test]
    async fn session_mailbox_registers_and_teardown_unregisters() {
        let registry = ThreadRegistry::prehost_only();

        assert!(registry.hypervisor().await.is_none());

        registry.register_session_mailbox("s-test").await;
        assert!(registry.hypervisor().await.is_some());

        let target_addr = acp::AgentAddress::parse("agent://local/session/s-test").unwrap();
        assert!(registry.fabric().manifest_of(&target_addr).await.is_some());

        registry.drop_session_mailbox("s-test").await;
        assert!(registry.fabric().manifest_of(&target_addr).await.is_none());
    }

    #[tokio::test]
    async fn instruction_routing_is_fail_closed_after_teardown() {
        let registry = ThreadRegistry::prehost_only();
        registry.register_session_mailbox("s-live").await;

        let from = acp::AgentAddress::parse("agent://local/hypervisor").unwrap();
        let to = acp::AgentAddress::parse("agent://local/session/s-live").unwrap();
        let envelope = acp::AgentEnvelope::new(
            from.clone(),
            to.clone(),
            acp::MessageIntent::steer("align the retry loop", acp::SteerAction::Note),
        );

        assert!(registry.fabric().dispatch(envelope).await.is_ok());

        registry.drop_session_mailbox("s-live").await;
        let envelope = acp::AgentEnvelope::new(
            from,
            to,
            acp::MessageIntent::steer("nobody home", acp::SteerAction::Note),
        );
        assert!(registry.fabric().dispatch(envelope).await.is_err());
    }

    #[test]
    fn resumed_developer_manifest_strips_legacy_recall_memory() {
        let mut manifest = nuo_protocol::SessionRoleManifest::developer();
        manifest.tools.push("recall_memory".to_string());
        assert!(manifest.tools.contains(&"recall_memory".to_string()));

        let mut admitted_tools = manifest.tools.clone();
        if manifest.role_id == "developer" {
            admitted_tools.retain(|t| t != "recall_memory");
        }
        let selection = nuo_protocol::ToolSelection::from_allowlist(&admitted_tools);
        assert!(!selection.scope.admits("recall_memory"));
    }
}
