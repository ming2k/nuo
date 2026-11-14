use super::*;

impl ThreadRegistry {
    pub async fn create_session(&self, project: PathBuf) -> Result<String, String> {
        self.create_session_with_options(project, nuo_protocol::ThreadInitOptions::default())
            .await
    }

    /// Control plane: create a session for `project` with explicit initial posture options.
    ///
    /// Claims synchronously, then waits for the visible readiness transition.
    /// The control verb has no Welcome/snapshot stream, so it must not return
    /// an id whose harness is still assembling.
    pub async fn create_session_with_options(
        &self,
        project: PathBuf,
        options: nuo_protocol::ThreadInitOptions,
    ) -> Result<String, String> {
        let handle = self
            .claim(
                crate::composition::startup::ThreadStart::Fresh,
                SessionBinding::workspace(project),
                options,
            )
            .await
            .map_err(|e| match e {
                AssembleErr::NoHost => "this host cannot create sessions".to_string(),
                AssembleErr::AssembleFailed(e) => format!("could not start a new session: {e}"),
            })?;
        let id = handle.session_id().await;
        match &handle {
            AttachHandle::Hosted(_) => Ok(id),
            AttachHandle::Building(pending) => {
                let mut rx = pending.phase.subscribe();
                tokio::time::timeout(BUILD_BUDGET, async {
                    loop {
                        let phase = rx.borrow().clone();
                        match phase {
                            nuo_protocol::ThreadPhase::Ready => return Ok(id.clone()),
                            nuo_protocol::ThreadPhase::Failed { message, .. } => {
                                return Err(message);
                            }
                            _ => {}
                        }
                        if rx.changed().await.is_err() {
                            return Err("session assembly task ended".to_string());
                        }
                    }
                })
                .await
                .unwrap_or_else(|_| Err("timed out waiting for session assembly".to_string()))
            }
        }
    }

    pub(super) async fn create_session_outcome(
        &self,
        project: PathBuf,
        options: nuo_protocol::ThreadInitOptions,
    ) -> ResolveOutcome {
        match self
            .claim(
                crate::composition::startup::ThreadStart::Fresh,
                SessionBinding::workspace(project),
                options,
            )
            .await
        {
            Ok(handle) => ResolveOutcome::Welcome(handle),
            Err(AssembleErr::NoHost) => {
                ResolveOutcome::refused_untaxed("this host cannot create sessions")
            }
            Err(AssembleErr::AssembleFailed(e)) => {
                ResolveOutcome::refused_untaxed(format!("could not start a new session: {e}"))
            }
        }
    }
    /// Control plane: send an interrupt to a hosted session.
    pub async fn interrupt(&self, session_id: &str) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e.req_tx.send(AgentRequest::Interrupt).await;
        Ok(())
    }

    /// Control plane: answer a pending permission request.
    pub async fn resolve_permission(
        &self,
        session_id: &str,
        request_id: String,
        decision: PermissionDecision,
    ) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e
            .req_tx
            .send(AgentRequest::PermissionReply {
                request_id,
                decision,
                parent_call_id: None,
            })
            .await;
        Ok(())
    }

    /// Control plane: send a prompt to a hosted session as a new
    /// round (how a panel or web UI "starts a task" on an existing session).
    pub async fn send_prompt(&self, session_id: &str, text: String) -> Result<(), String> {
        let map = self.sessions.lock().await;
        let Some(e) = map.get(session_id) else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        let _ = e
            .req_tx
            .send(AgentRequest::Prompt {
                text,
                images: Vec::new(),
                sent_at_ms: None,
            })
            .await;
        Ok(())
    }

    /// Control plane: tear down a hosted session — cancel its
    /// driver, fire its SessionEnd hooks (a killed session is a
    /// session that ended), drop it from the registry, and tell monitors it
    /// is gone.
    ///
    /// `hook_budget` bounds each `fire_session_end` call. A user-configured
    /// SessionEnd hook runs an external process; a hung one must never pin
    /// the server's shutdown (or this verb) open. On timeout the remaining
    /// hook work is abandoned — the hook's side effects are best-effort by
    /// design — and the teardown continues.
    pub async fn kill_session(&self, session_id: &str) -> Result<(), String> {
        self.kill_session_with_hook_budget(session_id, DEFAULT_SESSION_END_HOOK_BUDGET)
            .await
    }

    /// Tear down a session whose driver task panicked (the "evict" leg of
    /// task supervision). Reuses the kill path's cleanup — entry removal,
    /// terminal `Exit` broadcast, bounded SessionEnd hooks,
    /// `SessionRemoved` — but is tolerant of racing callers (another client
    /// may have already killed the session). Uses a tighter hook budget:
    /// this runs from inside the crashed task, so a hanging SessionEnd hook
    /// must not pin the teardown.
    pub(super) async fn evict_crashed_session(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
        events: broadcast::Sender<AgentResponse>,
        agent_for_session_end: Option<Arc<Agent>>,
    ) {
        // Only act if this exact entry is still live: kill_session or a
        // concurrent crash-eviction may have removed it already. Comparing
        // the cancel token distinguishes "already gone" from "a newer
        // session under the same id".
        {
            let map = self.sessions.lock().await;
            if map.get(session_id).is_none() {
                tracing::debug!(
                    session = %session_id,
                    "crash eviction: entry already removed"
                );
                return;
            }
        }
        // Same cleanup sequence as kill_session_with_hook_budget, with the
        // tight crash budget (2s) — an external-process SessionEnd hook must
        // not pin a teardown that already represents a failure path.
        self.sessions.lock().await.remove(session_id);
        self.drop_session_mailbox(session_id).await;
        cancel.cancel();
        let _ = events.send(AgentResponse::Exit);
        if let Some(agent) = agent_for_session_end
            && tokio::time::timeout(std::time::Duration::from_secs(2), agent.fire_session_end())
                .await
                .is_err()
        {
            tracing::warn!(
                session = %session_id,
                "crash eviction: SessionEnd hook exceeded 2s; abandoning it"
            );
        }
        self.publish(MonitorEvent::ThreadRemoved {
            thread_id: session_id.to_string(),
        })
        .await;
    }

    /// Suspend a hosted session: tear it down **in memory** without ending
    /// it. Unlike [`Self::kill_session`], no terminal `Exit` is broadcast
    /// (there are no clients to receive it — suspension requires zero
    /// receivers) and SessionEnd hooks do **not** fire: the session is not
    /// over, merely parked. Its transcript is durable, so the next attach
    /// resolves through the standard lazy-resume path and rebuilds
    /// everything. Scheduled jobs live in the session store, so they resume
    /// with it.
    ///
    /// Returns `Ok(())` when the session was suspended, `Err` when it was
    /// already gone (a racing kill/suspend) or is not suspendable.
    pub async fn suspend_session(&self, session_id: &str) -> Result<(), String> {
        let removed = self.sessions.lock().await.remove(session_id);
        self.drop_session_mailbox(session_id).await;
        let Some(e) = removed else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        // Cancel the driver (drops the run future). The req_tx senders in
        // clients' BoundSession clones die with the entry removal, so no
        // further requests queue into a drained channel.
        e.cancel.cancel();
        // Deliberately NOT sending AgentResponse::Exit and NOT firing
        // SessionEnd: suspension is invisible by design (no receivers exist
        // as a precondition) and the session continues on resume.
        self.publish(MonitorEvent::ThreadRemoved {
            thread_id: session_id.to_string(),
        })
        .await;
        tracing::info!(session = %session_id, "suspended idle hosted session");
        Ok(())
    }

    /// Control plane: park a hosted session on demand (the
    /// `suspend_session` verb). The same in-memory teardown as the idle
    /// reaper's [`Self::suspend_session`], but guarded for an explicit
    /// human/panel request: a session with an attached client or an active
    /// round (running / needs-approval / needs-input) is refused with a
    /// message naming what to do instead — detach or interrupt first. A
    /// never-persisted empty session is refused too: suspending it would
    /// silently discard it (there is no transcript to lazy-resume from),
    /// so killing is the honest verb for that case.
    pub async fn suspend_session_control(&self, session_id: &str) -> Result<(), String> {
        // Snapshot the probes under the lock; suspend outside it (never
        // hold the map lock across an await). `is_empty_unpersisted` needs
        // an await on the store, so the entry is cloned out and probed
        // lock-free.
        let entry = {
            let map = self.sessions.lock().await;
            let Some(e) = map.get(session_id) else {
                return Err(format!(
                    "session '{session_id}' is not hosted on this server"
                ));
            };
            Arc::clone(e)
        };
        if entry.events.receiver_count() > 0 {
            return Err(format!(
                "session '{session_id}' has {} attached client(s); detach before suspending",
                entry.events.receiver_count()
            ));
        }
        let status = entry.tracker.lock().await.row().status;
        if status.is_active() {
            return Err(format!(
                "session '{session_id}' is {} — interrupt it before suspending",
                status.as_str()
            ));
        }
        if entry.session.is_empty_unpersisted().await {
            return Err(format!(
                "session '{session_id}' has no content to keep — kill it instead"
            ));
        }
        self.suspend_session(session_id).await
    }

    /// Sweep for idle hosted sessions to suspend. A session is suspendable
    /// when (a) no client is attached (`receiver_count == 0`), (b) its
    /// monitor status is not active (not running / awaiting approval /
    /// awaiting input), and (c) it has had no tap activity for the TTL.
    /// Empty unpersisted sessions are left to the tighter empty-reaper
    /// above; this path exists for *real* sessions whose memory the server
    /// would otherwise hold forever.
    pub async fn suspend_idle_sessions(&self) -> Vec<String> {
        self.suspend_idle_sessions_with(IDLE_HOSTED_SESSION_TTL)
            .await
    }

    /// [`Self::suspend_idle_sessions`] with an explicit TTL (tests).
    pub async fn suspend_idle_sessions_with(&self, ttl: std::time::Duration) -> Vec<String> {
        // Snapshot under the lock; probe + suspend outside it (same pattern
        // as the empty reaper: never hold the map lock across awaits).
        let candidates: Vec<(String, Arc<HostedThread>)> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), Arc::clone(e)))
            .collect();
        let mut suspended = Vec::new();
        for (id, entry) in candidates {
            if entry.events.receiver_count() != 0 {
                continue; // someone is attached — theirs to keep alive
            }
            // Active-looking status (running round, pending approval/input)
            // disqualifies even with no receivers: the work must be allowed
            // to finish or be interrupted explicitly.
            let status = entry.tracker.lock().await.row().status;
            if status.is_active() {
                continue;
            }
            // An armed `/schedule` job is work that *will* run unattended
            // Activity clock: starts at host time and is refreshed below
            // whenever the tap tick advanced since the last sweep, so "idle"
            // means "no folded events for the whole TTL".
            let tick_now = entry
                .activity_tick
                .load(std::sync::atomic::Ordering::Relaxed);
            let tick_seen = entry
                .last_seen_tick
                .load(std::sync::atomic::Ordering::Relaxed);
            if tick_now != tick_seen {
                // Events were folded since the last sweep: refresh the idle
                // clock and record the watermark. Not a suspension candidate
                // this round.
                *entry.last_activity.lock().await = std::time::Instant::now();
                entry
                    .last_seen_tick
                    .store(tick_now, std::sync::atomic::Ordering::Relaxed);
                continue;
            }
            if entry.last_activity.lock().await.elapsed() < ttl {
                continue;
            }
            // Never-persisted empties belong to the tighter reaper above.
            if entry.session.is_empty_unpersisted().await {
                continue;
            }
            if self.suspend_session(&id).await.is_ok() {
                suspended.push(id);
            }
        }
        suspended
    }

    /// [`Self::kill_session`] with an explicit hook budget, so server
    /// shutdown can size it against its own remaining grace.
    pub async fn kill_session_with_hook_budget(
        &self,
        session_id: &str,
        hook_budget: std::time::Duration,
    ) -> Result<(), String> {
        let removed = self.sessions.lock().await.remove(session_id);
        self.drop_session_mailbox(session_id).await;
        // A closed session's retained results are dropped with it —
        // no cross-session delivery, and a closed session is not revived to
        // receive one. Server-level jobs (`owner_session: None`) are untouched.
        let dropped = self.server_tasks.discard_pending_for_session(session_id);
        if dropped > 0 {
            tracing::debug!(
                session = %session_id,
                dropped,
                "registry: discarded retained background job outcomes on session close"
            );
        }
        let Some(e) = removed else {
            return Err(format!(
                "session '{session_id}' is not hosted on this server"
            ));
        };
        // Record the terminated-with-process stop (C11) *before* dropping the
        // driver future: the round task it kills here never runs its own tail,
        // so this durable record is the only trace the round ever stopped.
        // Best-effort — a persistence failure must not block teardown. The
        // monitor tracker is the registry-side view of "a round is live";
        // NeedsApproval/NeedsInput also count (parked work dies with the
        // driver too).
        let status = e.tracker.lock().await.row().status;
        if status.is_active() {
            let record = nuo_protocol::RoundInterrupt {
                reason: nuo_protocol::RoundInterruptReason::Terminated,
                at_ms: unix_epoch_ms(),
                round: Some(e.session.round_counter().await),
                detail: None,
            };
            if let Err(error) = e.session.record_round_interrupt(record).await {
                tracing::warn!(
                    session = %session_id,
                    %error,
                    "registry: could not record round interrupt on kill"
                );
            }
        }
        e.cancel.cancel();
        let _ = e.events.send(AgentResponse::Exit);
        // SessionEnd observers fire best-effort after the driver is cancelled;
        // the hook context (session id + cwd) does not depend on it. Bounded:
        // an external-process hook that hangs cannot pin the server open.
        if let Some(agent) = &e.agent_for_session_end
            && tokio::time::timeout(hook_budget, agent.fire_session_end())
                .await
                .is_err()
        {
            tracing::warn!(
                session = %session_id,
                timeout_secs = hook_budget.as_secs_f32(),
                "registry: SessionEnd hook exceeded its budget; abandoning it"
            );
        }
        self.publish(MonitorEvent::ThreadRemoved {
            thread_id: session_id.to_string(),
        })
        .await;
        Ok(())
    }

    /// Broadcast an agent response to every hosted session's event bus.
    pub async fn broadcast_all_sessions(&self, response: AgentResponse) {
        let map = self.sessions.lock().await;
        for session in map.values() {
            let _ = session.events.send(response.clone());
        }
    }

    /// Graceful server shutdown: tear down every hosted session
    /// via [`Self::kill_session_with_hook_budget`], so each one's SessionEnd
    /// hooks fire before the process exits. `host::run` calls
    /// this after the listeners stop accepting and the connections drain
    ///
    /// Sessions are torn down **concurrently** and each hook is bounded by
    /// `hook_budget` individually: N sessions cost max(hook) — not the sum —
    /// so one slow hook cannot starve the others of their own budget.
    /// Best-effort per session: one failure does not skip the rest.
    /// ADR-0034 Level 1 (soft reload): re-read the configuration matrix into
    /// the server-wide snapshot and ask every hosted session to re-sync its
    /// MCP servers and skills — without dropping a single connection.
    pub async fn reload_runtime(&self) -> Result<(), String> {
        let config = nuo_persistence::config::Config::load();
        *self.shared_config.write().await = config;
        let sessions: Vec<Arc<HostedThread>> = {
            let guard = self.sessions.lock().await;
            guard.values().cloned().collect()
        };
        for session in sessions {
            let _ = session
                .req_tx
                .send(nuo_protocol::AgentRequest::ReloadRuntime)
                .await;
        }
        Ok(())
    }

    pub async fn shutdown_all_sessions(&self) {
        self.shutdown_all_sessions_with_hook_budget(DEFAULT_SESSION_END_HOOK_BUDGET)
            .await;
    }

    pub async fn shutdown_all_sessions_with_hook_budget(&self, hook_budget: std::time::Duration) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        let mut tears: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::with_capacity(ids.len());
        for id in ids {
            let this = self.clone();
            tears.push(Box::pin(async move {
                if let Err(error) = this.kill_session_with_hook_budget(&id, hook_budget).await {
                    tracing::warn!(session = %id, %error, "registry: session teardown failed on shutdown");
                }
            }));
        }
        futures::future::join_all(tears).await;
    }

    /// One pass of the idle-empty reaper: remove every hosted session that is
    /// still `is_empty_unpersisted` (no user-facing content, never written to
    /// disk) **and** has been hosted longer than `IDLE_EMPTY_SESSION_TTL`
    /// **and** currently has no attached client. Returns the reclaimed ids.
    ///
    /// Only never-persisted sessions are eligible — a session that has real
    /// content (however brief) is user history and is never reaped. The
    /// `receiver_count() == 0` probe treats any live event subscriber as an
    /// attached client, so a session someone is actively watching is left
    /// alone even while still empty. This is the in-memory counterpart of the
    /// persistence layer's lazy-materialisation guard (ADR-0018): that guard
    /// keeps empty sessions off disk, this one keeps them from piling up in
    /// the server's memory when clients create-then-abandon them.
    pub async fn reap_idle_empty_sessions(&self) -> Vec<String> {
        self.reap_idle_empty_sessions_with(IDLE_EMPTY_SESSION_TTL)
            .await
    }

    /// TTL-parameterised core of [`Self::reap_idle_empty_sessions`], split out
    /// so tests can sweep immediately instead of waiting out the real TTL.
    #[doc(hidden)]
    pub async fn reap_idle_empty_sessions_with(&self, ttl: std::time::Duration) -> Vec<String> {
        // Snapshot candidates under the lock, then probe + kill outside it so
        // we never hold the map lock across an `await` on a session's state.
        let candidates: Vec<(String, Arc<HostedThread>)> = self
            .sessions
            .lock()
            .await
            .iter()
            .map(|(id, e)| (id.clone(), Arc::clone(e)))
            .collect();
        let mut reaped = Vec::new();
        for (id, entry) in candidates {
            let idle_long_enough = entry.created_at.elapsed() >= ttl;
            let no_clients = entry.events.receiver_count() == 0;
            if !idle_long_enough || !no_clients {
                continue;
            }
            if !entry.session.is_empty_unpersisted().await {
                continue;
            }
            // Guard against the id-key stability hazard: an in-session
            // `/session open` or `/new` may have repointed the store to a
            // *different*, now persisted session while the map key still holds
            // the original id.
            // Only kill when the store is still on the entry's original id —
            // otherwise the entry has been repurposed and must be left alone.
            if entry.session.id().await != id {
                continue;
            }
            if self.kill_session(&id).await.is_ok() {
                tracing::info!(session_id = %id, "reaped idle never-persisted session");
                reaped.push(id);
            }
        }
        reaped
    }

    /// Spawn the background idle-empty reaper, sweeping every
    /// `IDLE_REAPER_INTERVAL` until `cancel` fires. The server calls this
    /// once at startup; the task stops cleanly on shutdown.
    ///
    /// The same tick runs the low-frequency storage-maintenance pass
    /// ([`Self::spawn_storage_maintenance`]) at most once per day: blob
    /// garbage collection and usage-day retention. Both are whole-data-dir
    /// scans, far too expensive to run per session event.
    pub fn spawn_idle_reaper(self: &Arc<Self>, cancel: CancellationToken) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(IDLE_REAPER_INTERVAL);
            // `interval` fires immediately on creation; skip the first tick so
            // a just-started server does not reap sessions still being set up.
            tick.tick().await;
            let mut last_maintenance = std::time::Instant::now();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("idle-session reaper: shutdown");
                        break;
                    }
                    _ = tick.tick() => {
                        registry.reap_idle_empty_sessions().await;
                        registry.suspend_idle_sessions().await;
                        if last_maintenance.elapsed() >= STORAGE_MAINTENANCE_INTERVAL {
                            last_maintenance = std::time::Instant::now();
                            registry.spawn_storage_maintenance();
                        }
                    }
                }
            }
        });
    }

    /// One storage-maintenance pass (ADR-0090): sweep unreferenced blobs,
    /// prune over-age usage day files, reclaim orphaned fact entries, and
    /// generate an autonomous rolling hot-backup snapshot.
    pub fn spawn_storage_maintenance(self: &Arc<Self>) {
        let usage = nuo_persistence::usage_stats::UsageStatsStore::new();
        tokio::task::spawn_blocking(move || {
            let handle = nuo_persistence::db::get_persistence_handle();
            let report = handle
                .perform_storage_maintenance_blocking(
                    nuo_persistence::DEFAULT_BACKUP_RETENTION_LIMIT,
                )
                .unwrap_or_default();
            let days = usage.prune_old_days();
            if report.reclaimed_blobs > 0
                || report.reclaimed_entries > 0
                || report.created_backup.is_some()
                || days > 0
            {
                tracing::info!(
                    blobs = report.reclaimed_blobs,
                    bytes = report.reclaimed_blob_bytes,
                    entries = report.reclaimed_entries,
                    backup = ?report.created_backup,
                    pruned_backups = report.pruned_backups,
                    pruned_days = days,
                    "storage maintenance: reclaimed unreferenced data and refreshed rolling snapshot"
                );
            }
        });
    }

    pub(super) async fn cancel_building_and_wait(&self, id: &str) -> Result<(), String> {
        let (cancel, done) = {
            let map = self.building.lock().await;
            match map.get(id) {
                Some(pending) => (pending.cancel.clone(), pending.done.clone()),
                None => return Err(format!("thread '{id}' is not building")),
            }
        };
        cancel.cancel();
        self.building.lock().await.remove(id);
        let _permit = tokio::time::timeout(DRIVER_SHUTDOWN_BUDGET, done.acquire())
            .await
            .map_err(|_| {
                format!(
                    "thread '{id}' build did not settle within {}s; refusing to delete underneath it",
                    DRIVER_SHUTDOWN_BUDGET.as_secs()
                )
            })?
            .map_err(|_| format!("thread '{id}' build shutdown channel closed"))?;
        // A cancel that raced promotion leaves a hosted entry; the caller's
        // next is_hosted probe handles it, but a second cancel is a no-op.
        Ok(())
    }

    /// Stop a hosted session and wait until its driver has fully unwound.
    /// Purge uses this barrier so no save can land after the durable delete.
    pub async fn kill_session_and_wait(&self, id: &str) -> Result<(), String> {
        let done = {
            let map = self.sessions.lock().await;
            match map.get(id) {
                Some(entry) => entry.driver_done.clone(),
                None => return Err(format!("thread '{id}' is not hosted")),
            }
        };
        self.kill_session_with_hook_budget(id, DEFAULT_SESSION_END_HOOK_BUDGET)
            .await?;
        let _permit = tokio::time::timeout(DRIVER_SHUTDOWN_BUDGET, done.acquire())
            .await
            .map_err(|_| {
                format!(
                    "thread '{id}' driver did not stop within {}s; refusing to delete underneath it",
                    DRIVER_SHUTDOWN_BUDGET.as_secs()
                )
            })?
            .map_err(|_| format!("thread '{id}' driver shutdown channel closed"))?;
        Ok(())
    }
}
