#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests: durable `/retry` resume point survives session close and resume
//! across the entire bootstrap, slash handler, and harness projection lifecycle.
use std::sync::Arc;

use super::sandbox_once;
use persistence::thread::ThreadSession;
use protocol::{AgentResponse, LoopStatus, RetryPoint, RoundEvent};
use server::UiBridge;
use server::composition::bootstrap::{self, BootstrapParams};
use server::composition::startup::ThreadStart;
use server::runtime::thread_driver::send_harness_state_for_session;
use tokio::sync::mpsc;

struct HeadlessProbe;

#[async_trait::async_trait]
impl UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<server::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

fn params(project_root: std::path::PathBuf, startup: ThreadStart) -> BootstrapParams {
    let identity = protocol::AgentIdentity::new("probe", "retry probe");
    BootstrapParams {
        human_channel: None,
        identity: identity.clone(),
        preset: protocol::AgentRoleProfile::with_identity("probe", identity),
        ui: Arc::new(HeadlessProbe),
        startup,
        project_root: Some(project_root),
        role: None,
        unattended: false,
        confined: true,
        teardown_token: None,
        shared_config: None,
        shared_provider_usage: None,
        phase_sink: None,
    }
}

async fn assemble_for(project: &std::path::Path, resume_id: &str) -> bootstrap::Bootstrap {
    bootstrap::assemble(params(
        project.to_path_buf(),
        ThreadStart::Resume(resume_id.to_string()),
    ))
    .await
    .expect("assemble succeeds")
}

#[tokio::test]
async fn retry_point_survives_process_death_and_projects_accurate_harness_state() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("retry-project");

    let store = Arc::new(ThreadSession::load_for_project(project.clone()));
    store
        .replace_messages(vec![
            tool::Message::new(tool::Role::User, "run task"),
            tool::Message::new(tool::Role::Assistant, "partial progress"),
        ])
        .await
        .unwrap();
    store.set_round_counter(1).await.unwrap();

    let point = RetryPoint {
        round: 1,
        turns_committed: 1,
        history_watermark: 2,
        paused_ms: 0,
        at_ms: 1000,
    };
    store.arm_retry_pending(point.clone()).await.unwrap();
    let session_id = store.id().await;

    // Simulate "closing and resuming the session in a new process"
    let boot = assemble_for(&project, &session_id).await;

    // Verify session store has the retry point
    let pending = boot.session.retry_pending().await;
    assert_eq!(pending, Some(point.clone()));
    assert_eq!(boot.session.round_counter().await, 1);
    assert_eq!(boot.agent.round_count(), 1);

    // Verify projection emission reflects retry_pending = true
    let (tx, mut rx) = mpsc::unbounded_channel();
    send_harness_state_for_session(
        &tx,
        &session_id,
        &boot.agent,
        &boot.session,
        LoopStatus::Idle,
    )
    .await;

    let received = rx.recv().await.expect("received harness state");
    match received {
        AgentResponse::Round {
            scope:
                protocol::RoundScope::Session {
                    event: RoundEvent::HarnessState(snapshot),
                },
            ..
        } => {
            assert!(
                snapshot.retry_pending,
                "resumed session idle snapshot must broadcast retry_pending = true"
            );
            assert_eq!(snapshot.round_counter, 1);
        }
        other => panic!("unexpected event: {:?}", other),
    }
}

/// 0226: a workspace-free explicit grouping assembles without binding
/// a filesystem workspace. The store is pinned to the persona's named grouping
/// and carries no workspace binding, so no workspace tool root is provided.
#[tokio::test]
async fn workspace_free_scope_assembles_without_a_workspace() {
    sandbox_once();
    let identity = protocol::AgentIdentity::new("practice", "a language practice partner");
    let boot = bootstrap::assemble(BootstrapParams {
        human_channel: None,
        identity: identity.clone(),
        preset: protocol::AgentRoleProfile::with_identity("practice", identity),
        ui: Arc::new(HeadlessProbe),
        startup: ThreadStart::Fresh,
        project_root: None,
        role: Some("english-practice".to_string()),
        unattended: false,
        confined: true,
        teardown_token: None,
        shared_config: None,
        shared_provider_usage: None,
        phase_sink: None,
    })
    .await
    .expect("workspace-free assemble succeeds");

    assert_eq!(
        boot.session.role().as_deref(),
        Some("english-practice"),
        "the staffing role is recorded as metadata"
    );
    assert!(
        boot.session.workspace().is_none(),
        "no workspace binding is bound for a workspace-free session"
    );
}

#[tokio::test]
async fn hermetic_manifest_restores_even_when_role_config_is_deleted() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("hermetic-project");
    let dot_nuo = project.join(".nuo");
    std::fs::create_dir_all(&dot_nuo).unwrap();

    // 1. Author a custom role in the global roles.toml (ADR-0254).
    let roles_file = host::paths::get().roles_file();
    if let Some(parent) = roles_file.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(
        &roles_file,
        r#"
[roles.sec-auditor]
name = "Security Auditor"
instructions = "Role: sec-auditor. Strictly examine vulnerabilities without mercy."
admit_mcp = ["security-scan"]
"#,
    )
    .unwrap();

    // 2. Create a session store and initialize it with this custom role
    let store = Arc::new(ThreadSession::load_for_project(project.clone()));
    let session_id = store
        .reset_with(
            Some(protocol::WorkspaceBinding::new(project.clone())),
            Some("sec-auditor".to_string()),
        )
        .await
        .unwrap();

    // Persist a turn into SQLite so the session is written with its manifest
    let turn = vec![
        tool::Message::new(tool::Role::User, "audit this codebase"),
        tool::Message::new(tool::Role::Assistant, "auditing now"),
    ];
    store
        .commit_turn(persistence::CommitTurn::new(&turn))
        .await
        .unwrap();

    // Verify the store captured the manifest at birth
    let manifest = store.role_manifest().await;
    assert!(manifest.is_some(), "manifest must be captured at birth");
    let m = manifest.unwrap();
    assert_eq!(m.role_id, "sec-auditor");
    assert_eq!(
        m.identity.preamble(),
        "Role: sec-auditor. Strictly examine vulnerabilities without mercy."
    );
    assert_eq!(m.admit_mcp, vec!["security-scan"]);

    // 3. NOW DELETE the roles.toml file completely!
    std::fs::remove_file(&roles_file).unwrap();
    assert!(!roles_file.exists(), "roles.toml is completely deleted");

    // 4. Assemble and resume the session: without manifest snapshotting this would crash with
    // "unknown role 'sec-auditor'". With, it resumes seamlessly with 100% byte fidelity!
    let boot = assemble_for(&project, &session_id).await;
    let resumed_manifest = boot.session.role_manifest().await;
    assert!(
        resumed_manifest.is_some(),
        "manifest must be preserved on resume"
    );
    let rm = resumed_manifest.unwrap();
    assert_eq!(rm.role_id, "sec-auditor");
    assert_eq!(
        rm.identity.preamble(),
        "Role: sec-auditor. Strictly examine vulnerabilities without mercy."
    );
    assert_eq!(rm.admit_mcp, vec!["security-scan"]);
    assert_eq!(boot.session.role().as_deref(), Some("sec-auditor"));
}
