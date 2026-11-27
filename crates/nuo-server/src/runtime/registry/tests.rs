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
                ThreadTarget::Attach {
                    id: Some("no-such-session-anywhere".into()),
                },
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
            .resolve_with_declaration(
                ThreadTarget::New { options: None },
                std::path::Path::new("/tmp"),
                true,
            )
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
            claim.matches("ThreadSession::for_workspace").count(),
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

        let target_addr = nuo_acp::AgentAddress::parse("agent://local/session/s-test").unwrap();
        assert!(registry.fabric().manifest_of(&target_addr).await.is_some());

        registry.drop_session_mailbox("s-test").await;
        assert!(registry.fabric().manifest_of(&target_addr).await.is_none());
    }

    #[tokio::test]
    async fn instruction_routing_is_fail_closed_after_teardown() {
        let registry = ThreadRegistry::prehost_only();
        registry.register_session_mailbox("s-live").await;

        let from = nuo_acp::AgentAddress::parse("agent://local/hypervisor").unwrap();
        let to = nuo_acp::AgentAddress::parse("agent://local/session/s-live").unwrap();
        let envelope = nuo_acp::AgentEnvelope::new(
            from.clone(),
            to.clone(),
            nuo_acp::MessageIntent::steer("align the retry loop", nuo_acp::SteerAction::Note),
        );

        assert!(registry.fabric().dispatch(envelope).await.is_ok());

        registry.drop_session_mailbox("s-live").await;
        let envelope = nuo_acp::AgentEnvelope::new(
            from,
            to,
            nuo_acp::MessageIntent::steer("nobody home", nuo_acp::SteerAction::Note),
        );
        assert!(registry.fabric().dispatch(envelope).await.is_err());
    }

    #[test]
    fn resumed_developer_manifest_strips_legacy_recall_memory() {
        let mut manifest = nuo_protocol::ThreadRoleManifest::developer();
        manifest.tools.push("recall_memory".to_string());
        assert!(manifest.tools.contains(&"recall_memory".to_string()));

        let mut admitted_tools = manifest.tools.clone();
        if manifest.role_id == "developer" {
            admitted_tools.retain(|t| t != "recall_memory");
        }
        let selection = nuo_protocol::ToolSelection::from_allowlist(&admitted_tools);
        assert!(!selection.scope.admits("recall_memory"));
    }

    /// Regression test: a connection that subscribed while a thread was still
    /// `Building` holds `AttachHandle::Building(pending)` for the life of the
    /// subscription. Once promoted to a live `HostedThread`, subsequent calls
    /// to `handle.snapshot()` (on broadcast lag re-anchor or re-subscribe) MUST
    /// read live state from the `HostedThread` rather than replaying the frozen
    /// attach-time `latest_snapshot` — and once the `HostedThread` is dropped,
    /// `handle.snapshot()` MUST return `None` rather than a stale copy.
    #[tokio::test]
    async fn promoted_building_handle_returns_live_snapshot_and_live_session_id() {
        let dir = std::env::temp_dir().join(format!(
            "nuo-promoted-building-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let session = Arc::new(ThreadSession::for_path(dir.join("session.json")));
        let initial_id = session.id().await;

        let (events_tx, _) = broadcast::channel::<AgentResponse>(16);
        let (req_tx, _req_rx) = mpsc::channel::<AgentRequest>(16);
        let (phase_tx, _) = tokio::sync::watch::channel(nuo_protocol::ThreadPhase::Ready);
        let human_channel = Arc::new(nuo_protocol::human_request::HumanChannelAccountant::new());
        let sync_buffer = Arc::new(Mutex::new(AttachSyncBuffer::new()));
        let cancel = CancellationToken::new();

        let pending = Arc::new(PendingSession {
            id: initial_id.clone(),
            workspace_root: Some(dir.clone()),
            role: "developer".to_string(),
            events: events_tx.clone(),
            req_tx: req_tx.clone(),
            phase: Arc::new(phase_tx),
            cancel: cancel.clone(),
            done: Arc::new(tokio::sync::Semaphore::new(1)),
            human_channel: human_channel.clone(),
            sync_buffer: sync_buffer.clone(),
            latest_snapshot: Arc::new(Mutex::new(None)),
            promoted: Arc::new(std::sync::OnceLock::new()),
            started_at: std::time::Instant::now(),
        });

        let base_overview = ThreadOverview {
            id: initial_id.clone(),
            title: None,
            overview: String::new(),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            active: true,
            parent_id: None,
            origin: nuo_protocol::ThreadOrigin::Trunk,
            digest: None,
        };
        let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
            super::monitor::base_row(base_overview, Some(dir.as_path()), "developer"),
            ThreadActivity::Idle,
        )));

        let hosted = Arc::new(HostedThread {
            workspace_root: Some(dir.clone()),
            human_channel,
            session: session.clone(),
            req_tx,
            events: events_tx,
            cancel,
            driver_done: Arc::new(tokio::sync::Semaphore::new(1)),
            shared_confinement: nuo_protocol::SharedConfinement::new(true),
            tracker,
            security: Arc::new(
                nuo_persistence::workspace_security::WorkspaceSecurityStore::load_from(
                    dir.join("security.json"),
                ),
            ),
            sync_buffer,
            command_catalog: nuo_tool::CommandCatalog::default(),
            initial_provider: "mock".to_string(),
            initial_model: "mock-model".to_string(),
            created_at: std::time::Instant::now(),
            last_activity: Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        });

        // Simulate promotion at T=0 when the transcript is still empty.
        let _ = pending.promoted.set(Arc::downgrade(&hosted));
        let initial_snapshot = crate::transport::serve::build_full_session_snapshot(&hosted).await;
        assert!(initial_snapshot.messages.is_empty());
        *pending.latest_snapshot.lock().await = Some(initial_snapshot);

        let handle = AttachHandle::Building(pending);

        // Now append turns to the live session (simulating normal conversation).
        session
            .commit_turn(nuo_persistence::CommitTurn::new(&[
                nuo_tool::Message::new(nuo_tool::Role::User, "turn 1 prompt"),
                nuo_tool::Message::new(nuo_tool::Role::Assistant, "turn 1 answer"),
            ]))
            .await
            .unwrap();
        session
            .commit_turn(nuo_persistence::CommitTurn::new(&[
                nuo_tool::Message::new(nuo_tool::Role::User, "turn 2 prompt"),
                nuo_tool::Message::new(nuo_tool::Role::Assistant, "turn 2 answer"),
            ]))
            .await
            .unwrap();

        // Lag re-anchor / re-subscribe through the `Building` handle MUST see all 4 live messages.
        let live_snapshot = handle
            .snapshot()
            .await
            .expect("promoted building handle must return live snapshot");
        let contents: Vec<&str> = live_snapshot
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(
            contents,
            vec![
                "turn 1 prompt",
                "turn 1 answer",
                "turn 2 prompt",
                "turn 2 answer",
            ],
            "promoted building handle must never replay the frozen T=0 empty snapshot"
        );

        // After an in-place reset (e.g. `/new`), `session_id()` must follow the live session.
        let next_id = session.reset().await.unwrap();
        assert_ne!(next_id, initial_id);
        assert_eq!(handle.session_id().await, next_id);

        // Once the promoted `HostedThread` is dropped/evicted, `snapshot()` must
        // return `None` rather than falling back to the frozen T=0 snapshot.
        drop(hosted);
        assert!(
            handle.snapshot().await.is_none(),
            "evicted promoted handle must not replay stale attach-time snapshot"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
