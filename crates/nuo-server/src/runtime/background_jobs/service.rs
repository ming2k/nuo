//! The protocol-facing background-job service wrapper.

use super::*;

/// Short human-readable rendering of a settled job state, for error messages
/// that explain *why* a control action was refused.
pub(super) fn describe_state(state: &JobState) -> String {
    match state {
        JobState::Succeeded { exit_code, .. } => format!("succeeded, exit code {exit_code}"),
        JobState::Failed { error, .. } => format!("failed: {error}"),
        JobState::Killed { .. } => "killed".to_string(),
        JobState::TimedOut { .. } => "timed out".to_string(),
        JobState::Queued => "queued".to_string(),
        JobState::Running { .. } => "running".to_string(),
        JobState::Ready { .. } => "ready".to_string(),
    }
}

/// Session-scoped wrapper binding [`BackgroundJobManager`] with the session's [`nuo_protocol::ExecutionEnvironment`].
#[derive(Clone)]
pub struct SessionJobService {
    manager: BackgroundJobManager,
    env: Arc<dyn nuo_protocol::ExecutionEnvironment>,
    /// Owning session (D5): stamped onto every task this service
    /// spawns so snapshots and ledger rows answer "whose task is this".
    owner_session: std::sync::OnceLock<String>,
}

impl SessionJobService {
    pub fn new(
        manager: BackgroundJobManager,
        env: Arc<dyn nuo_protocol::ExecutionEnvironment>,
    ) -> Self {
        Self {
            manager,
            env,
            owner_session: std::sync::OnceLock::new(),
        }
    }

    /// Bind the owning session id (called once at driver startup, after the
    /// session id resolves). Later spawns carry it in every snapshot.
    pub fn bind_owner(&self, session_id: String) {
        let _ = self.owner_session.set(session_id);
    }

    fn owner(&self) -> Option<String> {
        self.owner_session.get().cloned()
    }

    pub fn manager(&self) -> &BackgroundJobManager {
        &self.manager
    }
}

#[async_trait::async_trait]
impl nuo_protocol::BackgroundJobService for SessionJobService {
    async fn spawn_process(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
    ) -> Result<BackgroundJobInfo, String> {
        let roots = self.env.additional_roots();
        self.manager
            .spawn_process(
                command,
                ProcessSpawnOptions {
                    label,
                    cwd,
                    workspace_root: self.env.workspace_root(),
                    additional_roots: &roots,
                    detached,
                    timeout,
                    owner_session: self.owner(),
                },
            )
            .await
    }

    async fn spawn_process_ex(
        &self,
        command: String,
        label: Option<String>,
        cwd: Option<PathBuf>,
        detached: bool,
        timeout: Option<Duration>,
        kind: nuo_protocol::JobKind,
        readiness: Option<nuo_protocol::Readiness>,
        restart: Option<nuo_protocol::RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        let roots = self.env.additional_roots();
        self.manager
            .spawn_process_ex(
                command,
                ProcessSpawnOptions {
                    label,
                    cwd,
                    workspace_root: self.env.workspace_root(),
                    additional_roots: &roots,
                    detached,
                    timeout,
                    owner_session: self.owner(),
                },
                kind,
                readiness,
                restart,
            )
            .await
    }

    async fn adopt_process(
        &self,
        command: String,
        label: Option<String>,
        adoption: nuo_protocol::AdoptionInfo,
    ) -> Result<BackgroundJobInfo, String> {
        self.manager
            .adopt_process(command, label, adoption, self.owner())
            .await
    }

    async fn spawn_timer(
        &self,
        label: &str,
        fire_at_ms: u64,
        interval_ms: Option<u64>,
        command: String,
    ) -> Result<BackgroundJobInfo, String> {
        self.manager.spawn_timer(
            Some(label.to_string()),
            fire_at_ms,
            interval_ms,
            command,
            self.owner(),
        )
    }

    fn list_jobs(&self) -> Vec<BackgroundJobInfo> {
        self.manager.list_jobs()
    }

    fn get_job(&self, id: &JobId) -> Option<BackgroundJobInfo> {
        self.manager.get_job(id)
    }

    fn get_logs(&self, id: &JobId, tail_lines: usize) -> Option<Vec<String>> {
        self.manager.get_logs(id, tail_lines)
    }

    fn claim_outcomes(&self, id: &JobId) -> Vec<BackgroundJobOutcome> {
        self.manager
            .claim_outcomes_for_job(id)
            .into_iter()
            .map(|p| p.outcome)
            .collect()
    }

    fn settled_result(&self, id: &JobId) -> Option<BackgroundJobOutcome> {
        self.manager.settled_result(id)
    }

    fn kill_job(&self, id: &JobId) -> Result<(), String> {
        self.manager.kill_job(id)
    }

    fn abort_all(&self) {
        self.manager.abort_all();
    }
}
