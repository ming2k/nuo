use super::*;
use std::time::Duration;
use tempfile::tempdir;

#[tokio::test]
async fn test_mcp_watcher_ignores_non_mcp_workspace_config() {
    let tmp = tempdir().unwrap();
    let ws_root = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let dot_nuo = ws_root.join(".nuo");
    std::fs::create_dir_all(&dot_nuo).unwrap();

    let cfg_file = dot_nuo.join("config.toml");
    std::fs::write(&cfg_file, "[workspace]\nadditional_roots = [\"../foo\"]\n").unwrap();

    let sec_file = tmp.path().join("workspace_security.json");
    let security = Arc::new(WorkspaceSecurityStore::load_from(sec_file));

    let agent = Arc::new(Agent::new(
        Arc::new(nuo_engine::NoProvider),
        vec![],
        nuo_protocol::AgentIdentity::default(),
    ));
    let mcp = Arc::new(McpRuntime::start_background(
        Default::default(),
        agent.dynamic_tool_sink(),
    ));

    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
    spawn_mcp_config_watcher(
        mcp,
        Some(ws_root.clone()),
        security,
        resp_tx,
        "test-session".to_string(),
    );

    // Give watcher thread a moment to initialize inotify
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Modify .nuo/config.toml with non-MCP content
    std::fs::write(&cfg_file, "[workspace]\nadditional_roots = [\"../bar\"]\n").unwrap();

    // Wait for debounce window (500ms + margin)
    let notice = tokio::time::timeout(Duration::from_millis(800), resp_rx.recv()).await;

    // Should NOT receive any TrustChanged warning
    if let Ok(Some(AgentResponse::Round {
        scope:
            nuo_protocol::RoundScope::Session {
                event: RoundEvent::Notice(n),
            },
        ..
    })) = notice
    {
        panic!(
            "Unexpected notice for non-MCP config modification: {:?}",
            n.title
        );
    }
}

#[tokio::test]
async fn test_mcp_watcher_warns_on_untrusted_workspace_mcp() {
    let tmp = tempdir().unwrap();
    let ws_root = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let dot_nuo = ws_root.join(".nuo");
    std::fs::create_dir_all(&dot_nuo).unwrap();

    let sec_file = tmp.path().join("workspace_security.json");
    let security = Arc::new(WorkspaceSecurityStore::load_from(sec_file));

    let agent = Arc::new(Agent::new(
        Arc::new(nuo_engine::NoProvider),
        vec![],
        nuo_protocol::AgentIdentity::default(),
    ));
    let mcp = Arc::new(McpRuntime::start_background(
        Default::default(),
        agent.dynamic_tool_sink(),
    ));

    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
    spawn_mcp_config_watcher(
        mcp,
        Some(ws_root.clone()),
        security,
        resp_tx,
        "test-session".to_string(),
    );

    // Give watcher thread a moment to initialize
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Write an MCP server into .nuo/mcp.json
    let mcp_json = dot_nuo.join("mcp.json");
    std::fs::write(
        &mcp_json,
        r#"{"mcpServers": {"test": {"command": "echo", "args": ["hello"]}}}"#,
    )
    .unwrap();

    // Should receive TrustChanged warning
    let mut got_warning = false;
    while let Ok(Some(resp)) =
        tokio::time::timeout(Duration::from_millis(3000), resp_rx.recv()).await
    {
        if let AgentResponse::Round {
            scope:
                nuo_protocol::RoundScope::Session {
                    event: RoundEvent::Notice(n),
                },
            ..
        } = resp
            && n.kind == nuo_tool::NoticeKind::TrustChanged
        {
            got_warning = true;
            break;
        }
    }
    assert!(
        got_warning,
        "Expected TrustChanged warning for untrusted workspace MCP"
    );
}

#[tokio::test]
async fn test_mcp_watcher_user_config_does_not_trigger_workspace_warning() {
    let tmp = tempdir().unwrap();
    // Simulate a workspace root containing user config directory
    let ws_root = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|_| tmp.path().to_path_buf());
    let user_config_file = nuo_host::paths::get().config_file();

    let sec_file = tmp.path().join("workspace_security.json");
    let security = Arc::new(WorkspaceSecurityStore::load_from(sec_file));

    let agent = Arc::new(Agent::new(
        Arc::new(nuo_engine::NoProvider),
        vec![],
        nuo_protocol::AgentIdentity::default(),
    ));
    let mcp = Arc::new(McpRuntime::start_background(
        Default::default(),
        agent.dynamic_tool_sink(),
    ));

    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel();
    // If workspace_root is the parent of user_config_file (e.g. $HOME)
    let home_root = user_config_file
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or(ws_root);

    spawn_mcp_config_watcher(
        mcp,
        Some(home_root),
        security,
        resp_tx,
        "test-session".to_string(),
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify that no spurious TrustChanged notice is emitted
    let notice = tokio::time::timeout(Duration::from_millis(300), resp_rx.recv()).await;
    if let Ok(Some(AgentResponse::Round {
        scope:
            nuo_protocol::RoundScope::Session {
                event: RoundEvent::Notice(n),
            },
        ..
    })) = notice
        && n.kind == nuo_tool::NoticeKind::TrustChanged
    {
        panic!("User config modification must not trigger TrustChanged warning!");
    }
}

#[tokio::test]
async fn test_user_assets_zero_bypass_and_ttl_lifecycle() {
    use nuo_protocol::WorkspaceTrustState;
    use nuo_protocol::security::{AssetLocator, AssetSpec, AttestationStatus};

    let tmp = tempfile::tempdir().unwrap();
    let handle = nuo_persistence::db::PersistenceHandle::spawn(tmp.path().join("assets.db"), None);
    let ledger = nuo_persistence::AssetAttestationLedger::for_handle(handle);

    let locator = AssetLocator::UserMcp {
        name: "sqlite_audit".into(),
    };
    let spec_v1 = AssetSpec::Process {
        command: vec!["uvx".into(), "mcp-server-sqlite".into()],
        env: std::collections::BTreeMap::new(),
    };

    // 1. Zero implicit trust: user assets start strictly Quarantined
    assert_eq!(
        ledger.status(&locator, &spec_v1),
        AttestationStatus::Quarantined
    );
    assert!(!ledger.is_trusted(&locator, &spec_v1));

    // 2. Trust asset grants a 30-day lease
    ledger.trust_asset(&locator, &spec_v1).unwrap();
    assert_eq!(
        ledger.status(&locator, &spec_v1),
        AttestationStatus::Trusted
    );
    assert!(ledger.is_trusted(&locator, &spec_v1));

    // 3. Modifying command on the same locator triggers Changed (replacement semantics)
    let spec_v2 = AssetSpec::Process {
        command: vec![
            "uvx".into(),
            "mcp-server-sqlite".into(),
            "--read-only".into(),
        ],
        env: std::collections::BTreeMap::new(),
    };
    assert_eq!(
        ledger.status(&locator, &spec_v2),
        AttestationStatus::Changed
    );
    assert!(!ledger.is_trusted(&locator, &spec_v2));

    // 4. Re-trusting restores Trusted state
    ledger.trust_asset(&locator, &spec_v2).unwrap();
    assert_eq!(
        ledger.status(&locator, &spec_v2),
        AttestationStatus::Trusted
    );

    // 5. Expired lease (> 30 days) fails closed
    ledger
        .trust_asset_with_expiry(&locator, &spec_v2, 100, 200)
        .unwrap();
    assert_eq!(
        ledger.status(&locator, &spec_v2),
        AttestationStatus::Expired
    );
    assert!(!ledger.is_trusted(&locator, &spec_v2));

    // 6. Snapshot aggregate reflects user-level quarantine even in workspace-free sessions
    let mut snapshot = nuo_protocol::WorkspaceSecuritySnapshot::new("workspace-free");
    assert_eq!(snapshot.aggregate(), WorkspaceTrustState::Absent);
    snapshot.user_assets = WorkspaceTrustState::Quarantined;
    assert_eq!(snapshot.aggregate(), WorkspaceTrustState::Quarantined);
    snapshot.user_assets = WorkspaceTrustState::Trusted;
    assert_eq!(snapshot.aggregate(), WorkspaceTrustState::Trusted);
}

#[tokio::test]
async fn test_ops_role_assembles_with_full_operational_toolset() {
    struct HeadlessProbe;
    #[async_trait::async_trait]
    impl crate::UiBridge for HeadlessProbe {
        async fn copy_to_clipboard(&self, _text: &str) -> Result<crate::CopyOutcome, String> {
            Err("probe: headless".to_string())
        }
    }

    let base_identity = nuo_protocol::AgentIdentity::new("ops", "system operator");
    let ops_profile = nuo_protocol::AgentRoleProfile::ops();
    let params = BootstrapParams {
        human_channel: None,
        identity: base_identity,
        preset: ops_profile,
        ui: Arc::new(HeadlessProbe),
        startup: ThreadStart::Fresh,
        project_root: None,
        role: Some("ops".to_string()),
        unattended: false,
        confined: false,
        teardown_token: None,
        shared_config: None,
        shared_provider_usage: None,
        phase_sink: None,
    };
    let boot = assemble(params).await.expect("ops assemble succeeds");
    assert_eq!(boot.session.role().as_deref(), Some("ops"));
    assert!(boot.session.workspace().is_none());

    let tool_names: std::collections::HashSet<String> = boot
        .agent
        .installed_tools()
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();

    // Host system and remote operational capabilities must be admitted:
    assert!(
        tool_names.contains("execute_command"),
        "ops must have execute_command"
    );
    assert!(tool_names.contains("read"), "ops must have read");
    assert!(tool_names.contains("edit_text"), "ops must have edit_text");
    assert!(
        tool_names.contains("write_file"),
        "ops must have write_file"
    );
    assert!(
        tool_names.contains("find_files"),
        "ops must have find_files"
    );
    assert!(
        tool_names.contains("search_text"),
        "ops must have search_text"
    );
    assert!(tool_names.contains("read_url"), "ops must have read_url");
    assert!(
        tool_names.contains("search_web"),
        "ops must have search_web"
    );
    assert!(
        tool_names.contains("spawn_agent"),
        "ops must have spawn_agent"
    );

    // AST-level code parsing and dialog memory are excluded by design:
    assert!(
        !tool_names.contains("code_query"),
        "ops must not have code_query"
    );
    assert!(
        !tool_names.contains("recall_memory"),
        "ops must not have recall_memory"
    );
}

#[tokio::test]
async fn test_philosophist_role_assembles_with_only_philosophist_tools() {
    struct HeadlessProbe;
    #[async_trait::async_trait]
    impl crate::UiBridge for HeadlessProbe {
        async fn copy_to_clipboard(&self, _text: &str) -> Result<crate::CopyOutcome, String> {
            Err("probe: headless".to_string())
        }
    }

    let base_identity = nuo_protocol::AgentIdentity::new("philosophist", "philosopher");
    let profile = nuo_protocol::AgentRoleProfile::philosophist();
    let params = BootstrapParams {
        human_channel: None,
        identity: base_identity,
        preset: profile,
        ui: Arc::new(HeadlessProbe),
        startup: ThreadStart::Fresh,
        project_root: None,
        role: Some("philosophist".to_string()),
        unattended: false,
        confined: true,
        teardown_token: None,
        shared_config: None,
        shared_provider_usage: None,
        phase_sink: None,
    };
    let boot = assemble(params)
        .await
        .expect("philosophist assemble succeeds");
    assert_eq!(boot.session.role().as_deref(), Some("philosophist"));
    assert!(boot.session.workspace().is_none());

    let tool_names: std::collections::HashSet<String> = boot
        .agent
        .installed_tools()
        .into_iter()
        .map(|t| t.name().to_string())
        .collect();

    assert!(
        tool_names.contains("read_url"),
        "philosophist must have read_url"
    );
    assert!(
        tool_names.contains("search_web"),
        "philosophist must have search_web"
    );
    assert!(
        tool_names.contains("recall_memory"),
        "philosophist must have recall_memory"
    );

    // System operations and fs tools must NOT be admitted to philosophist:
    assert!(
        !tool_names.contains("execute_command"),
        "philosophist must not have execute_command"
    );
    assert!(
        !tool_names.contains("read"),
        "philosophist must not have read"
    );
    assert!(
        !tool_names.contains("edit_text"),
        "philosophist must not have edit_text"
    );
    assert!(
        !tool_names.contains("write_file"),
        "philosophist must not have write_file"
    );
    assert!(
        !tool_names.contains("find_files"),
        "philosophist must not have find_files"
    );
    assert!(
        !tool_names.contains("search_text"),
        "philosophist must not have search_text"
    );
    assert!(
        !tool_names.contains("spawn_agent"),
        "philosophist must not have spawn_agent"
    );
}
