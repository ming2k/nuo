//! The driver for one live agent session.
//!
//! [`ThreadDriver`] owns the request receiver and every long-lived dependency
//! needed to serve that session: the `Agent`, session store, configuration,
//! provider telemetry, cancellation state, and frontend
//! bridge. [`ThreadDriver::run`] consumes the driver, dispatches each
//! [`AgentRequest`] to its handler, and exits when all request senders are
//! dropped.
//!
//! The implementation still destructures the driver into the local names used
//! by the original inline task. This keeps the dispatch body unchanged while
//! making its ownership boundary explicit.
use crate::handlers::handlers_chat::ChatEnv;
use crate::handlers::handlers_slash::SlashEnv;
use crate::runtime::mcp::McpRuntime;
use nuo_engine::cognition::skills::SkillRegistry;
use nuo_engine::orchestration::round_response;
use nuo_engine::{Agent, RoundLifecycle, SubagentRegistry};
use nuo_persistence::{thread::ThreadSession, workspace_security::WorkspaceSecurityStore};
use nuo_protocol::{AgentRequest, AgentResponse, LoopStatus, Provider};

pub async fn send_harness_state_for_session(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    session: &ThreadSession,
    loop_status: LoopStatus,
) {
    let round_counter = agent
        .round_count()
        .saturating_add(u64::from(!loop_status.is_idle()));
    let retry_pending = loop_status.is_idle() && session.retry_pending().await.is_some();
    let _ = tx.send(round_response(
        session_id,
        nuo_protocol::RoundEvent::HarnessState(nuo_protocol::HarnessSnapshot {
            loop_status,
            round_counter,
            unattended: agent.unattended(),
            confined: agent.is_confined(),
            workspace_security: agent.workspace_security(),
            retry_pending,
            role: agent.active_role().or_else(|| session.role()),
            workspace: session
                .workspace()
                .map(|w| w.root.to_string_lossy().to_string())
                .or_else(|| {
                    agent
                        .workspace_root()
                        .map(|p| p.to_string_lossy().to_string())
                }),
        }),
    ));
}

/// Compact a session's history in place.
///
/// A **delegation**, not a second implementation. This used to be a verbatim
/// copy of the harness's compactor, which meant every compaction fix had to be
/// written twice — and the copy is what `/compact` actually calls, so a fix that
/// landed only in the harness would have left the user-facing command broken.
pub async fn compact_round_history(
    history: &mut Vec<nuo_tool::Message>,
    session: &ThreadSession,
    settings: &nuo_engine::orchestration::ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<nuo_engine::orchestration::CompactionReport>, String> {
    nuo_engine::orchestration::compact_round_history(
        history,
        session,
        settings,
        provider,
        extra_context,
    )
    .await
}

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc;

use crate::UiBridge;
use crate::composition::startup::ThreadStart;
use crate::transport::thread_view::{build_sessions_overview, provider_key_status};

mod driver;
mod followup;
mod recovery;

pub use self::driver::ThreadDriver;
pub(crate) use self::followup::FollowUpQueue;
