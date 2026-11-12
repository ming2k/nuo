pub(super) use super::resolution::overview_of;
use super::*;

impl ThreadRegistry {
    pub fn server_tasks(&self) -> Arc<crate::background_jobs::BackgroundJobManager> {
        self.server_tasks.clone()
    }

    /// Spawn a task on the server fabric: monitor-visible, owner-less.
    pub async fn spawn_server_task(
        &self,
        command: String,
        label: Option<String>,
        kind: nuo_protocol::JobKind,
    ) -> Result<nuo_protocol::BackgroundJobInfo, String> {
        let mgr = self.server_tasks.clone();
        let roots: Vec<std::path::PathBuf> = Vec::new();
        let workspace = std::env::temp_dir();
        mgr.spawn_process_ex(
            command,
            crate::background_jobs::ProcessSpawnOptions {
                label,
                cwd: None,
                workspace_root: &workspace,
                additional_roots: &roots,
                detached: true,
                timeout: None,
                owner_session: None,
            },
            kind,
            Some(nuo_protocol::Readiness::FirstOutput),
            None,
        )
        .await
    }

    /// Stop a server-level task (the human-side control the TUI panel will
    /// drive).
    pub fn stop_server_task(&self, task_id: &str) -> Result<(), String> {
        self.server_tasks
            .kill_job(&nuo_protocol::JobId::from(task_id))
    }

    /// Subscribe the durability-health tap (D4): the persistence
    /// supervisor's transitions are folded into the snapshot cache and
    /// published as `MonitorEvent::PersistenceHealth` diffs, so every
    /// frontend (and `nuo status`) sees degradation, not just the log.
    #[allow(clippy::unwrap_used)] // A poisoned snapshot mutex is an unrecoverable registry invariant.
    pub fn start_persistence_health_monitor(self: &Arc<Self>) {
        let registry = Arc::downgrade(self);
        let mut rx = nuo_persistence::db::get_persistence_handle().subscribe_health();
        tokio::spawn(async move {
            loop {
                if rx.changed().await.is_err() {
                    break; // supervisor gone: nothing left to report
                }
                let wire = rx.borrow().clone().to_wire();
                if let Some(r) = registry.upgrade() {
                    *r.persistence_health.lock().unwrap() = Some(wire.clone());
                    r.publish_host_event(MonitorEvent::PersistenceHealth(wire));
                }
            }
        });
    }

    /// Cross-session picker fan-out: refresh the server-level latest-picker
    /// cache and rebroadcast the snapshot to every *other* hosted session's
    /// bus, so a model switch made in one session is visible (and hydratable
    /// at attach time) everywhere. Called by the per-session broadcast tap
    /// whenever its driver emits a `ProviderPicker`.
    pub async fn broadcast_picker_to_other_sessions(&self, origin: &str, response: AgentResponse) {
        {
            let mut latest = self.latest_picker.lock().await;
            *latest = Some(response.clone());
        }
        let map = self.sessions.lock().await;
        for (id, session) in map.iter() {
            if id != origin {
                let _ = session.events.send(response.clone());
            }
        }
    }

    /// The server-level latest picker snapshot, for attach-time hydration
    /// (`None` before any session has pushed a picker this process lifetime —
    /// per-session attach-sync buffers still cover that case).
    pub async fn latest_picker(&self) -> Option<AgentResponse> {
        self.latest_picker.lock().await.clone()
    }

    /// Ask the Archivist one question, synchronously. The round
    /// borrows the live provider of any hosted session's agent (all sessions
    /// resolve the same shared channel after a switch); with no hosted
    /// session the round runs on the last-bound channel or refuses via the
    /// `NoProvider` sentinel.
    pub async fn ask_archivist(&self, text: String) -> crate::archivist_service::ArchivistAnswer {
        let provider: Option<Arc<dyn nuo_protocol::Provider>> = {
            let map = self.sessions.lock().await;
            map.values()
                .find_map(|e| e.agent_for_session_end.as_ref())
                .map(|agent| agent.provider.clone())
        };
        let borrowed = provider.unwrap_or_else(|| Arc::new(nuo_harness::NoProvider));
        self.archivist.ask(borrowed, &text).await
    }

    /// The server's ACP communication fabric.
    pub fn fabric(&self) -> &acp::Fabric {
        &self.fabric
    }

    /// The Hypervisor station, once materialized (lazily constructed on the
    /// first hosted-session assemble). `None` before any session has hosted —
    /// a server with no fleet has nothing to orchestrate.
    pub async fn hypervisor(&self) -> Option<Arc<crate::hypervisor::Hypervisor>> {
        self.hypervisor.lock().await.clone()
    }

    /// Register this session's master at `agent://local/session/<id>` on the ACP fabric.
    ///
    /// Returns the session's [`acp::MailboxHandle`] so the caller can wire ACP
    /// collaboration tools into the live agent (ADR-0008 step 8 / ADR-0009 §3).
    pub(super) async fn register_session_mailbox(
        &self,
        session_id: &str,
    ) -> Option<acp::MailboxHandle> {
        // Mesh registration is fail-open observability: an unaddressable session
        // id skips the mailbox instead of aborting the session, and the caller
        // simply wires no collaboration tools.
        let addr = acp::AgentAddress::parse(&format!("agent://local/session/{session_id}")).ok()?;
        let manifest = acp::AgentManifest::new(
            addr.clone(),
            format!("Session {session_id}"),
            "Hosted session root agent",
        );
        let mailbox = self.fabric.join(manifest, 64).await;
        let handle = mailbox.handle();
        self.session_mailboxes
            .lock()
            .await
            .insert(session_id.to_string(), mailbox);

        let mut hypervisor = self.hypervisor.lock().await;
        if hypervisor.is_none() {
            *hypervisor = Some(Arc::new(
                crate::hypervisor::Hypervisor::new(
                    Arc::new(nuo_harness::NoProvider),
                    self.clone(),
                    self.fabric.clone(),
                )
                .await,
            ));
        }
        Some(handle)
    }

    /// Drop one session's ACP mailbox (teardown path).
    pub(super) async fn drop_session_mailbox(&self, session_id: &str) {
        if let Some(_mb) = self.session_mailboxes.lock().await.remove(session_id) {
            if let Ok(addr) =
                acp::AgentAddress::parse(&format!("agent://local/session/{session_id}"))
            {
                self.fabric.leave(&addr).await;
            }
        }
    }

    /// Fold one fabric event into the snapshot cache and publish the diff.
    /// Runs on a dedicated subscriber task per registry instance.
    #[allow(clippy::unwrap_used)] // A poisoned task-row mutex is an unrecoverable registry invariant.
    pub fn start_server_task_monitor(self: &Arc<Self>) {
        let mgr = self.server_tasks.clone();
        let rows = self.server_task_rows.clone();
        let registry = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut rx = mgr.subscribe();
            loop {
                match rx.recv().await {
                    Ok(crate::background_jobs::BackgroundJobEvent::Started(info)) => {
                        Self::upsert_task_row(&rows, &info, None);
                        if let Some(r) = registry.upgrade()
                            && let Some(task) = rows.lock().unwrap().get(&info.id.0)
                        {
                            r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                        }
                    }
                    Ok(crate::background_jobs::BackgroundJobEvent::Progress { job_id, line }) => {
                        let changed = {
                            let mut guard = rows.lock().unwrap();
                            if let Some(row) = guard.get_mut(&job_id.0) {
                                row.latest_output = Some(line);
                                true
                            } else {
                                false
                            }
                        };
                        if changed
                            && let Some(r) = registry.upgrade()
                            && let Some(task) = rows.lock().unwrap().get(&job_id.0)
                        {
                            r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                        }
                    }
                    Ok(crate::background_jobs::BackgroundJobEvent::Ready { job_id }) => {
                        let changed = {
                            let mut guard = rows.lock().unwrap();
                            if let Some(row) = guard.get_mut(&job_id.0) {
                                row.state = nuo_protocol::JobState::Ready {
                                    started_at_ms: row.created_at_ms,
                                    ready_at_ms: crate::registry::unix_epoch_ms(),
                                };
                                true
                            } else {
                                false
                            }
                        };
                        if changed
                            && let Some(r) = registry.upgrade()
                            && let Some(task) = rows.lock().unwrap().get(&job_id.0)
                        {
                            r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                        }
                    }
                    Ok(crate::background_jobs::BackgroundJobEvent::Completed(outcome)) => {
                        if let Some(info) = mgr.get_job(&outcome.job_id) {
                            Self::upsert_task_row(&rows, &info, Some(&outcome));
                            if let Some(r) = registry.upgrade()
                                && let Some(task) = rows.lock().unwrap().get(&outcome.job_id.0)
                            {
                                r.publish_host_event(MonitorEvent::TaskUpdated(task.clone()));
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }

    #[allow(clippy::unwrap_used)] // A poisoned task-row mutex is an unrecoverable registry invariant.
    fn upsert_task_row(
        rows: &Arc<std::sync::Mutex<HashMap<String, nuo_protocol::MonitoredTask>>>,
        info: &nuo_protocol::BackgroundJobInfo,
        outcome: Option<&nuo_protocol::BackgroundJobOutcome>,
    ) {
        let mut guard = rows.lock().unwrap();
        let row = guard.entry(info.id.0.clone()).or_insert_with(|| {
            let (label, spec) = match &info.spec {
                nuo_protocol::JobSpec::Process { command, label, .. } => (
                    label.clone().unwrap_or_else(|| {
                        command
                            .split_whitespace()
                            .next()
                            .unwrap_or("task")
                            .to_string()
                    }),
                    command.clone(),
                ),
                nuo_protocol::JobSpec::Timer { label, prompt, .. } => (
                    label.clone().unwrap_or_else(|| "timer".to_string()),
                    prompt.clone(),
                ),
            };
            let log_path = None;
            nuo_protocol::MonitoredTask {
                id: info.id.0.clone(),
                label,
                spec,
                state: info.state.clone(),
                owner_session: None,
                created_at_ms: info.created_at_ms,
                completed_at_ms: info.completed_at_ms,
                latest_output: info.latest_output.clone(),
                log_path,
            }
        });
        row.state = match outcome {
            Some(o) => o.state.clone(),
            None => info.state.clone(),
        };
        row.completed_at_ms = info.completed_at_ms.or(row.completed_at_ms);
        if let Some(o) = outcome {
            row.latest_output = Some(o.summary.clone());
        }
    }
    /// Look up an active hosted thread by its ID.

    pub async fn set_monitor_meta(&self, project_root: String, started_at: u64) {
        *self.meta.lock().await = MonitorMeta {
            project_root: Some(project_root),
            started_at,
        };
    }
    /// Resolve an attach. `caller_project` scopes creation and lazy resume
    /// `New` / `Attach(None)` create in the caller's project;
    /// `Attach(Some(id))` first looks up every hosted session globally, then
    /// lazy-resumes from the caller's project on disk.

    pub(super) async fn overview_arcs(&self, entries: &[Arc<HostedThread>]) -> Vec<ThreadOverview> {
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            out.push(overview_of(&e.session, true).await);
        }
        out.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        out
    }

    /// Number of currently hosted or building sessions (the idle-exit probe).
    pub async fn session_count(&self) -> usize {
        let hosted = self.sessions.lock().await.len();
        let building = self.building.lock().await.len();
        hosted + building
    }

    /// Cancel an in-flight build by id. Returns `false` when the id is not
    /// building (already hosted, already failed, or unknown).
    pub async fn cancel_building(&self, id: &str) -> bool {
        let pending = self.building.lock().await.remove(id);
        match pending {
            Some(pending) => {
                pending.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Check whether any hosted session has active in-flight work (status != Idle).
    /// Used by client-driven shutdown to prevent killing running background tasks (ADR-0038).
    /// A building session is active by definition: its harness is not ready yet.
    pub async fn has_active_work(&self) -> bool {
        if !self.building.lock().await.is_empty() {
            return true;
        }
        let map = self.sessions.lock().await;
        for hosted in map.values() {
            let status = hosted.tracker.lock().await.row().status;
            if status != nuo_protocol::SessionStatus::Idle {
                return true;
            }
        }
        false
    }

    /// Subscribe to the host-level monitor topic.
    pub fn subscribe_monitor(&self) -> broadcast::Receiver<MonitorEvent> {
        self.monitor.subscribe()
    }

    /// The current monitor snapshot: every hosted session's row, newest
    /// activity first, honouring the client's `include_idle` filter.
    #[allow(clippy::unwrap_used)] // A poisoned snapshot mutex is an unrecoverable registry invariant.
    pub async fn monitor_snapshot(&self, action: MonitorAction) -> MonitorSnapshot {
        let mut sessions = Vec::new();
        {
            let map = self.sessions.lock().await;
            for hosted in map.values() {
                let row = hosted.tracker.lock().await.row();
                if action.include_idle || row.status != ThreadStatus::Idle {
                    sessions.push(row);
                }
            }
        }
        sessions.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        let tasks = self
            .server_task_rows
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        let meta = self.meta.lock().await.clone();
        MonitorSnapshot {
            project_root: meta.project_root.unwrap_or_default(),
            server_started_at: meta.started_at,
            threads: sessions,
            tasks,
            persistence_health: self.persistence_health.lock().unwrap().clone(),
        }
    }

    /// The role-scoped history listing: durable rows plus live rows for the
    /// same scope, merged by id. Durable rows with no resident driver become
    /// `ThreadHosting::Dormant` rows. Subagent runs never enter the user-facing
    /// list.
    pub async fn history_rows(
        &self,
        filter: &ThreadFilter,
    ) -> Result<Vec<MonitoredThread>, String> {
        let filter_for_db = filter.clone();
        let records = tokio::task::spawn_blocking(move || {
            let handle = nuo_persistence::db::get_persistence_handle();
            let reader = handle.reader().map_err(|e| e.to_string())?;
            reader
                .list_sessions(&filter_for_db, false)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("thread history task failed: {e}"))??;

        let mut hosted = self
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await
            .threads;
        hosted.retain(|row| thread_matches_filter(filter, &row.role, &row.project_root));
        let mut by_id: HashMap<String, MonitoredThread> = hosted
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();

        let mut rows = Vec::with_capacity(records.len() + by_id.len());
        for record in records {
            match by_id.remove(&record.id) {
                Some(row) => rows.push(row),
                None => rows.push(dormant_row(record)),
            }
        }
        rows.extend(by_id.into_values());
        rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        Ok(rows)
    }

    /// Stop every target in `ids` that matches `filter`. Runtime-only: the
    /// durable transcript survives and the next attach lazy-resumes it.
    pub async fn stop_threads(
        &self,
        filter: Option<&ThreadFilter>,
        ids: Vec<String>,
    ) -> ThreadRemovalReport {
        let mut report = ThreadRemovalReport::default();
        let hosted: HashMap<String, MonitoredThread> = self
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await
            .threads
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        let records_by_id = self.thread_records(&ids).await;

        for id in ids {
            let record = records_by_id.get(&id).cloned();
            if let Some(filter) = filter {
                let matched = match &record {
                    Some(record) => thread_record_matches(filter, record),
                    None => hosted.get(&id).is_some_and(|row| {
                        thread_matches_filter(filter, &row.role, &row.project_root)
                    }),
                };
                if !matched {
                    report.skipped.push(ThreadRemovalSkip {
                        thread_id: id,
                        reason: "thread is outside the requested role/workspace scope".into(),
                    });
                    continue;
                }
            }
            let result = if self.is_hosted(&id).await {
                self.kill_session_and_wait(&id).await
            } else if self.is_building(&id).await {
                self.cancel_building_and_wait(&id).await
            } else {
                report.already_gone += 1;
                continue;
            };
            match result {
                Ok(()) => report.removed += 1,
                Err(reason) => report.skipped.push(ThreadRemovalSkip {
                    thread_id: id,
                    reason,
                }),
            }
        }
        report
    }

    /// Permanently delete every target in `ids` that matches `filter`.
    ///
    /// The client supplies the exact id snapshot it confirmed. Subagent runs
    /// belonging to a selected parent are expanded into the delete set: they
    /// are internal durable threads and would otherwise be orphaned. Every
    /// hosted target is stopped and awaited before its rows are deleted; a
    /// target that cannot settle is reported as skipped rather than deleted
    /// underneath a live writer.
    pub async fn purge_threads(
        &self,
        filter: Option<&ThreadFilter>,
        ids: Vec<String>,
    ) -> ThreadRemovalReport {
        // No new claim may start while the exact purge set is being quiesced
        // and deleted. Existing claims finish their bounded setup, then the
        // purge sees them as building and cancels them.
        let _purge_guard = self.purge_gate.write().await;
        let mut report = ThreadRemovalReport::default();
        let hosted: HashMap<String, MonitoredThread> = self
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await
            .threads
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();

        let mut seen = std::collections::HashSet::new();
        let mut main_records = Vec::new();
        let records_by_id = self.thread_records(&ids).await;
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            let record = records_by_id.get(&id).cloned();
            if let Some(filter) = filter {
                let matched = match &record {
                    Some(record) => thread_record_matches(filter, record),
                    None => hosted.get(&id).is_some_and(|row| {
                        thread_matches_filter(filter, &row.role, &row.project_root)
                    }),
                };
                if !matched {
                    report.skipped.push(ThreadRemovalSkip {
                        thread_id: id,
                        reason: "thread is outside the requested role/workspace scope".into(),
                    });
                    continue;
                }
            }
            match record {
                Some(record) => main_records.push(record),
                None => {
                    // A hosted but never-persisted empty session has no history
                    // to delete; stopping it is the whole operation.
                    let result = if self.is_hosted(&id).await {
                        self.kill_session_and_wait(&id).await
                    } else if self.is_building(&id).await {
                        self.cancel_building_and_wait(&id).await
                    } else {
                        report.already_gone += 1;
                        continue;
                    };
                    match result {
                        Ok(()) => report.removed += 1,
                        Err(reason) => report.skipped.push(ThreadRemovalSkip {
                            thread_id: id,
                            reason,
                        }),
                    }
                }
            }
        }

        let subagents = self.subagent_records(&main_records, filter).await;
        let mut records = main_records;
        for record in subagents {
            if seen.insert(record.id.clone()) {
                records.push(record);
            }
        }

        for record in records {
            let is_subagent = record.fork_kind == "subagent";
            if self.is_hosted(&record.id).await {
                match self.kill_session_and_wait(&record.id).await {
                    Ok(()) => {}
                    Err(reason) => {
                        report.skipped.push(ThreadRemovalSkip {
                            thread_id: record.id,
                            reason,
                        });
                        continue;
                    }
                }
            } else if self.is_building(&record.id).await {
                match self.cancel_building_and_wait(&record.id).await {
                    Ok(()) => {}
                    Err(reason) => {
                        report.skipped.push(ThreadRemovalSkip {
                            thread_id: record.id,
                            reason,
                        });
                        continue;
                    }
                }
            }

            match nuo_persistence::session::delete_thread(
                &record.id,
                record.workspace_root.as_deref().map(std::path::Path::new),
            )
            .await
            {
                Ok(existed) => {
                    if !existed {
                        report.already_gone += 1;
                        continue;
                    }
                    if is_subagent {
                        report.subagents_removed += 1;
                    } else {
                        report.removed += 1;
                    }
                    self.publish(MonitorEvent::ThreadRemoved {
                        thread_id: record.id,
                    })
                    .await;
                }
                Err(error) => report.skipped.push(ThreadRemovalSkip {
                    thread_id: record.id,
                    reason: format!("could not delete durable thread: {error}"),
                }),
            }
        }
        report
    }

    async fn thread_records(
        &self,
        ids: &[String],
    ) -> HashMap<String, nuo_persistence::db::ThreadRecord> {
        let ids = ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let Ok(reader) = nuo_persistence::db::get_persistence_handle().reader() else {
                return HashMap::new();
            };
            ids.into_iter()
                .filter_map(|id| {
                    reader
                        .get_session(&id)
                        .ok()
                        .flatten()
                        .map(|record| (id, record))
                })
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    async fn subagent_records(
        &self,
        parents: &[nuo_persistence::db::ThreadRecord],
        filter: Option<&ThreadFilter>,
    ) -> Vec<nuo_persistence::db::ThreadRecord> {
        let parent_ids: Vec<String> = parents.iter().map(|record| record.id.clone()).collect();
        let filter = filter.cloned();
        tokio::task::spawn_blocking(move || {
            let Ok(reader) = nuo_persistence::db::get_persistence_handle().reader() else {
                return Vec::new();
            };
            match filter {
                // A scope purge owns every subagent run in that scope,
                // including runs whose parent row was already deleted. Asking
                // only for the selected parents would strand those internal
                // rows forever.
                Some(filter) => {
                    let selected: std::collections::HashSet<String> =
                        parent_ids.into_iter().collect();
                    reader
                        .list_sessions(&filter, true)
                        .map(|rows| {
                            rows.into_iter()
                                .filter(|record| {
                                    record.fork_kind == "subagent"
                                        && record
                                            .parent_id
                                            .as_ref()
                                            .is_none_or(|parent| selected.contains(parent))
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                }
                None => {
                    let mut records = Vec::new();
                    for parent in parent_ids {
                        let Ok(children) = reader.list_subagent_sessions(&parent) else {
                            continue;
                        };
                        records.extend(children);
                    }
                    records
                }
            }
        })
        .await
        .unwrap_or_default()
    }

    async fn is_hosted(&self, id: &str) -> bool {
        self.sessions.lock().await.contains_key(id)
    }

    async fn is_building(&self, id: &str) -> bool {
        self.building.lock().await.contains_key(id)
    }

    /// Publish a host-level diff. Best-effort: with no subscribers the
    /// broadcast send fails, which is fine — a fresh subscriber always takes
    /// a snapshot first.
    pub(super) async fn publish(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    /// Publish a host-level (server-scope) event on the monitor bus
    /// the run loop announces `ServerDraining` when the graceful
    /// drain begins so every watch client learns the server is going away
    /// before its connection closes. Public and synchronous because the bus
    /// is a broadcast sender — sending never blocks on subscribers.
    pub fn publish_host_event(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }

    /// Test-only hook: integration tests construct `HostedSession` by hand
    /// (no driver), so they emulate the registry's broadcast-tap by folding
    /// responses into the tracker themselves and publishing the diff through
    /// this narrow seam. Not part of the production surface. Best-effort:
    /// with no subscribers the send is dropped, exactly like production.
    #[doc(hidden)]
    pub fn publish_for_test(&self, event: MonitorEvent) {
        let _ = self.monitor.send(event);
    }
}

/// Project the picker's cheap header row into the monitor base row; every
/// status/accounting field starts at its zero value and is folded from the
/// live event stream by the [`MonitorTracker`]. `project_root` rides along
/// from the registry's two-level index so clients can name the workspace
/// without a second lookup.
fn workspace_matches(filter: &nuo_protocol::WorkspaceFilter, root: &str) -> bool {
    match filter {
        nuo_protocol::WorkspaceFilter::Any => true,
        nuo_protocol::WorkspaceFilter::Path(path) => {
            !root.is_empty() && std::path::Path::new(root) == path
        }
        nuo_protocol::WorkspaceFilter::Unbound => root.is_empty() || root == "workspace-free",
    }
}

fn thread_matches_filter(filter: &ThreadFilter, role: &str, project_root: &str) -> bool {
    if let Some(role_filter) = &filter.role
        && role != role_filter
    {
        return false;
    }
    workspace_matches(&filter.workspace, project_root)
}

fn thread_record_matches(
    filter: &ThreadFilter,
    record: &nuo_persistence::db::ThreadRecord,
) -> bool {
    let role = record.persona.as_deref().unwrap_or("developer");
    let workspace = record.workspace_root.as_deref().unwrap_or("");
    thread_matches_filter(filter, role, workspace)
}

fn truncate_overview(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut out: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn dormant_row(record: nuo_persistence::db::ThreadRecord) -> MonitoredThread {
    let title = record.title.filter(|title| !title.trim().is_empty());
    let overview = title
        .clone()
        .or_else(|| {
            record
                .last_user_prompt
                .filter(|prompt| !prompt.trim().is_empty())
        })
        .map(|text| truncate_overview(&text, 64))
        .unwrap_or_else(|| "(empty thread)".to_string());
    let fork_kind = match record.fork_kind.as_str() {
        "fork" => nuo_protocol::ThreadForkKind::Fork,
        "subagent" => nuo_protocol::ThreadForkKind::Subagent,
        _ => nuo_protocol::ThreadForkKind::Trunk,
    };
    let digest = record
        .digest
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    MonitoredThread {
        id: record.id,
        title,
        overview,
        created_at: record.created_at_s.max(0) as u64,
        updated_at: record.updated_at_s.max(0) as u64,
        message_count: record.msg_count.max(0) as usize,
        hosting: ThreadHosting::Dormant,
        status: ThreadStatus::Idle,
        round: record.round_counter,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: record
            .workspace_root
            .filter(|root| !root.is_empty())
            .unwrap_or_else(|| "workspace-free".to_string()),
        role: record
            .persona
            .filter(|role| !role.is_empty())
            .unwrap_or_else(|| "developer".to_string()),
        parent_id: record.parent_id,
        fork_kind,
        fork_point: record.fork_point,
        digest,
    }
}

pub(super) fn base_row(
    overview: ThreadOverview,
    workspace_root: Option<&std::path::Path>,
    role: &str,
) -> MonitoredThread {
    MonitoredThread {
        id: overview.id,
        title: overview.title,
        overview: overview.overview,
        created_at: overview.created_at,
        updated_at: overview.updated_at,
        message_count: overview.message_count,
        hosting: ThreadHosting::Hosted,
        status: ThreadStatus::Idle,
        round: 0,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: workspace_root
            .map(|root| root.display().to_string())
            .unwrap_or_else(|| "workspace-free".to_string()),
        role: role.to_string(),
        // Lineage rides from the overview (fork surfacing).
        parent_id: overview.parent_id,
        fork_kind: overview.fork_kind,
        fork_point: overview.fork_point,
        digest: overview.digest,
    }
}
