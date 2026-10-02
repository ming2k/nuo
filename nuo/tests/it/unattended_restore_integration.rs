#![allow(clippy::unwrap_used, clippy::expect_used)]

//! ADR-0132 integration: the session-scoped unattended execution posture is persisted
//! (`SessionEvent::UnattendedSet`) and restored by the bootstrap resume path —
//! so a daemon that dies mid-unattended-session reopens unattended when the
//! session is re-hosted (attach, lazy-resume, or boot rehost). These tests
//! exercise the real `bootstrap::assemble` resume path against a store on
//! disk, standing in for "process died, new process opened the same session
//! file".

use std::sync::Arc;

use super::sandbox_once;
use nuo_persistence::session::SessionStore;
use nuo::UiBridge;
use nuo::bootstrap::{self, BootstrapParams};
use nuo::startup::SessionStart;

struct HeadlessProbe;

#[async_trait::async_trait]
impl UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<nuo::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

fn params(project_root: std::path::PathBuf, startup: SessionStart) -> BootstrapParams {
    let identity = nuo_contracts::AgentIdentity::new("probe", "unattended probe");
    BootstrapParams {
        human_channel: None,
        identity: identity.clone(),
        preset: nuo_contracts::AgentRoleProfile::with_identity("probe", identity),
        ui: Arc::new(HeadlessProbe),
        startup,
        project_root: Some(project_root),
        role: None,
        unattended: false,
        confined: true,
        teardown_token: None,
        shared_config: None,
        shared_provider_usage: None,
    }
}

async fn assemble_for(project: &std::path::Path, resume_id: &str) -> bootstrap::Bootstrap {
    bootstrap::assemble(params(
        project.to_path_buf(),
        SessionStart::Resume(resume_id.to_string()),
    ))
    .await
    .expect("assemble succeeds")
}

#[tokio::test]
async fn unattended_posture_survives_process_death_and_reopen() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("crashed-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![nuo_contracts::Message::new(
            nuo_contracts::Role::User,
            "mid-task when the daemon was killed",
        )])
        .await
        .unwrap();
    // The `/unattended on` handler's store write.
    store.set_unattended(true).await.unwrap();
    let session_id = store.id().await;

    // "Process death": nothing but the files remain. A new process resumes
    // the same session through the ordinary bootstrap path.
    let boot = assemble_for(&project, &session_id).await;
    assert!(
        boot.agent.unattended(),
        "a rehosted session must reopen in the posture it died in"
    );
    assert!(boot.session.unattended().await);
}

#[tokio::test]
async fn interactive_session_reopens_interactive() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("attended-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![nuo_contracts::Message::new(
            nuo_contracts::Role::User,
            "interactive session",
        )])
        .await
        .unwrap();
    let session_id = store.id().await;

    let boot = assemble_for(&project, &session_id).await;
    assert!(
        !boot.agent.unattended(),
        "an interactive session must not gain unattended mode on reopen"
    );
}

#[tokio::test]
async fn unattended_off_after_on_persists_the_de_escalation() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("toggle-project");

    let store = Arc::new(SessionStore::load_for_project(project.clone()));
    store
        .replace_messages(vec![nuo_contracts::Message::new(
            nuo_contracts::Role::User,
            "toggle history",
        )])
        .await
        .unwrap();
    store.set_unattended(true).await.unwrap();
    store.set_unattended(false).await.unwrap(); // `/unattended off`
    let session_id = store.id().await;

    let boot = assemble_for(&project, &session_id).await;
    assert!(
        !boot.agent.unattended(),
        "the last persisted posture (off) must win over the earlier on"
    );
}

#[tokio::test]
async fn session_init_options_applies_at_fresh_startup_without_command_ledger_entry() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("fresh-project");

    let mut p = params(project, SessionStart::Fresh);
    p.unattended = true;
    p.confined = false;

    let boot = bootstrap::assemble(p).await.expect("assemble succeeds");

    assert!(boot.agent.unattended(), "agent must start unattended");
    assert!(
        boot.session.unattended().await,
        "session data must persist unattended"
    );
    assert!(
        !boot.shared_confinement.is_confined(),
        "confinement must be disabled"
    );

    // Core architectural invariant: startup options must NOT inject fake
    // harness commands into the session command ledger / transcript!
    let commands = boot.session.commands().await;
    assert!(
        commands.is_empty(),
        "command ledger must be clean at startup, but had: {commands:?}"
    );
}
