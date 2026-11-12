pub(super) use super::monitor::base_row;
use super::*;

impl ThreadRegistry {
    pub async fn get(&self, session_id: &str) -> Option<Arc<HostedThread>> {
        self.sessions.lock().await.get(session_id).cloned()
    }

    /// Record the host's provenance (project root + start time) so monitor
    /// snapshots can identify the host. Called once by `host::run` before
    /// the listener starts accepting monitor clients.

    pub async fn resolve(
        &self,
        action: AttachAction,
        caller_project: &std::path::Path,
    ) -> ResolveOutcome {
        self.resolve_with_declaration(action, caller_project, true)
            .await
    }

    /// [`Self::resolve`] with the caller's declaration status made explicit.
    /// A *declared* project (modern client's `Select.project`) forbids
    /// silently auto-binding a session from another project — that is the
    /// "launched in project A, working in project B" trap. An undeclared
    /// project (legacy client; the server guessing its own cwd) keeps the
    /// historical lone-session auto-bind because the server cannot tell a
    /// cross-project attach from a same-project one.
    pub async fn resolve_with_declaration(
        &self,
        action: AttachAction,
        caller_project: &std::path::Path,
        declared: bool,
    ) -> ResolveOutcome {
        match action {
            AttachAction::New(options) => {
                self.create_session_outcome(
                    caller_project.to_path_buf(),
                    options.unwrap_or_default(),
                )
                .await
            }
            AttachAction::Attach(None) => self.resolve_auto(caller_project, declared).await,
            AttachAction::Attach(Some(id)) => self.resolve_id(&id, caller_project).await,
            // The picker carrier: a throwaway session whose only
            // job is to host the client's picker modal. The bootstrap skips
            // restore and hooks for it; `/sessions <id>` switches to the
            // real session through the ordinary re-attach path.
            AttachAction::Picker(init_options) => {
                let opts = init_options.unwrap_or_default();
                let role_id = opts.role.clone();
                // `claim` applies the role's workspace policy (Fixed/Inherit/
                // None); the picker only needs the one special case where the
                // carrier has no workspace at all.
                let ws = if role_id.as_deref() == Some("philosophist") {
                    None
                } else {
                    Some(nuo_protocol::WorkspaceBinding::new(
                        caller_project.to_path_buf(),
                    ))
                };
                self.claim(
                    crate::startup::SessionStart::Picker,
                    SessionBinding {
                        workspace: ws,
                        role: role_id,
                    },
                    opts,
                )
                .await
                .map(ResolveOutcome::Welcome)
                .unwrap_or_else(|e| match e {
                    AssembleErr::NoHost => {
                        ResolveOutcome::refused_untaxed("this host cannot create sessions")
                    }
                    AssembleErr::AssembleFailed(e) => ResolveOutcome::refused_untaxed(format!(
                        "could not open the session picker: {e}"
                    )),
                })
            }
            // Monitor handshakes are intercepted by the WS layer
            // (`serve::handle_connection`) and never reach `resolve`.
            AttachAction::Monitor(_) => {
                ResolveOutcome::refused_untaxed("monitor connections are served directly")
            }
            AttachAction::Control(_) => {
                ResolveOutcome::refused_untaxed("control connections are served directly")
            }
        }
    }

    /// Control plane: create a session for `project` and return
    /// its id. The session is server-held; no client is attached yet.

    pub async fn host(&self, entry: HostedThread) -> AttachHandle {
        let entry = Arc::new(entry);
        let id = entry.session.id().await;
        let tracker = entry.tracker.clone();
        self.publish(MonitorEvent::ThreadAdded(tracker.lock().await.row()))
            .await;
        self.sessions.lock().await.insert(id, entry.clone());
        AttachHandle::Hosted(entry)
    }

    async fn resolve_auto(
        &self,
        caller_project: &std::path::Path,
        declared: bool,
    ) -> ResolveOutcome {
        // Prefer the caller's own project's sessions; only when that project
        // has none do we consider the global set, and only a single global
        // session auto-binds (otherwise the client must choose).
        //
        // The lock is released before any overview read: `overview_of` opens a
        // SQLite summary through `spawn_blocking`, and holding the registry
        // mutex across it stalled every control verb for the duration.
        let (mine, total) = {
            let map = self.sessions.lock().await;
            let mine: Vec<Arc<HostedThread>> = map
                .values()
                .filter(|e| e.workspace_root.as_deref() == Some(caller_project))
                .cloned()
                .collect();
            (mine, map.len())
        };
        match mine.len() {
            1 => return ResolveOutcome::Welcome(AttachHandle::Hosted(mine[0].clone())),
            n if n > 1 => {
                return ResolveOutcome::Pick {
                    sessions: self.overview_arcs(&mine).await,
                };
            }
            _ => {}
        }
        // A build already in flight in the caller's project is joinable: the
        // second attacher gets the same id and current phase instead of
        // minting a duplicate session (ADR-0133).
        if let Some(pending) = {
            let map = self.building.lock().await;
            map.values()
                .find(|p| p.workspace_root.as_deref() == Some(caller_project))
                .cloned()
        } {
            return ResolveOutcome::Welcome(AttachHandle::Building(pending));
        }
        if total == 0 {
            return self
                .create_session_outcome(
                    caller_project.to_path_buf(),
                    nuo_protocol::ThreadInitOptions::default(),
                )
                .await;
        }
        let entries: Vec<Arc<HostedThread>> = {
            let map = self.sessions.lock().await;
            map.values().cloned().collect()
        };
        if total == 1 && !declared {
            // A legacy client that declared no project keeps the historical
            // auto-bind: the server cannot distinguish a cross-project attach
            // from a same-project one. A declared client gets the picker — the
            // "launched in project A, working in project B" trap.
            return ResolveOutcome::Welcome(AttachHandle::Hosted(entries[0].clone()));
        }
        ResolveOutcome::Pick {
            sessions: self.overview_arcs(&entries).await,
        }
    }

    async fn resolve_id(&self, id: &str, _caller_project: &std::path::Path) -> ResolveOutcome {
        if let Some(entry) = self.sessions.lock().await.get(id).cloned() {
            return ResolveOutcome::Welcome(AttachHandle::Hosted(entry));
        }
        if let Some(pending) = self.building.lock().await.get(id).cloned() {
            return ResolveOutcome::Welcome(AttachHandle::Building(pending));
        }
        if self.params.is_none() {
            // No host: nothing can ever assemble this id on this server.
            return ResolveOutcome::refused(
                nuo_protocol::ERR_SESSION_GONE,
                format!("session '{id}' is not hosted on this server"),
            );
        }
        // Lazy resume resolves the session's own durable binding (scope +
        // workspace) so an unbound session is reachable from any caller
        // (0220), not only the caller's workspace.
        //
        // `[INV-ATTACH-01]`: no hosted entry **and** no durable binding is the
        // one case the server can prove is unrecoverable — the session is gone,
        // so the client must retire the tab that names it (ADR-0123).
        let Some(binding) = lookup_session_workspace(id).await else {
            return ResolveOutcome::refused(
                nuo_protocol::ERR_SESSION_GONE,
                format!("session '{id}' no longer exists"),
            );
        };
        match self
            .claim(
                crate::startup::SessionStart::Resume(id.to_string()),
                binding,
                nuo_protocol::ThreadInitOptions::default(),
            )
            .await
        {
            Ok(handle) => ResolveOutcome::Welcome(handle),
            Err(AssembleErr::NoHost) => ResolveOutcome::refused(
                nuo_protocol::ERR_SESSION_GONE,
                format!("session '{id}' is not hosted on this server"),
            ),
            // The session exists durably but could not be claimed: a retry may
            // work, so the tab must be *kept* and the failure surfaced
            // (`ERR_SESSION_UNRESUMABLE`).
            Err(AssembleErr::AssembleFailed(e)) => ResolveOutcome::refused(
                nuo_protocol::ERR_SESSION_UNRESUMABLE,
                format!("could not resume session {id}: {e}"),
            ),
        }
    }

    /// Claim a session identity and start its harness assembly in the
    /// background.
    ///
    /// `[INV-ATTACH-03]` (ADR-0133): everything before the returned handle is
    /// bounded local work — binding resolution, a store open that does not load
    /// the transcript, channel construction, and spawning the build task. No
    /// network I/O, no agent, no skills, no hooks, no transcript read.
    pub(super) async fn claim(
        &self,
        mut startup: crate::startup::SessionStart,
        binding: SessionBinding,
        init_options: nuo_protocol::ThreadInitOptions,
    ) -> Result<AttachHandle, AssembleErr> {
        // A purge owns the write side: no claim may insert a driver between
        // the purge's liveness probe and its durable delete.
        let _purge_guard = self.purge_gate.read().await;
        let HostParams {
            identity,
            preset,
            ui,
        } = self.params.as_ref().ok_or(AssembleErr::NoHost)?.clone();
        // 0226: a persona overrides the server's fixed identity and
        // preset. An explicit selection (`init_options.persona`) also applies
        // the persona's workspace policy; a resume uses the stored workspace
        // binding unchanged. `workspace = None` is the unbound set.
        let mut identity = identity;
        let mut preset = preset;
        let mut workspace = binding.workspace;
        // A fresh selection canonicalizes built-in aliases (`dev` → `developer`)
        // before the id becomes a durable scope key. Custom role ids are
        // preserved. Unknown ids are refused here, not silently downgraded.
        let role_selected = match init_options.role.clone() {
            Some(role) => Some(
                nuo_persistence::roles::canonical_role_id(
                    &role,
                    workspace.as_ref().map(|binding| binding.root.as_path()),
                )
                .map_err(|error| {
                    AssembleErr::AssembleFailed(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        error,
                    )))
                })?,
            ),
            None => None,
        };
        let role_id = role_selected.clone().or(binding.role);

        // An explicit role owns the workspace binding before `--resume`
        // resolves its target. Without this, `--resume --role philosophist`
        // would look in the caller's workspace instead of the unbound set.
        // Keep the original binding for agent assembly; the later role
        // resolution block applies the same policy against that context.
        let resume_workspace = if role_selected.is_some()
            && let Some(role) = role_id.as_deref()
        {
            let scope = nuo_persistence::roles::resolve_thread_scope(
                Some(role),
                workspace.as_ref().map(|binding| binding.root.as_path()),
            )
            .map_err(|error| {
                AssembleErr::AssembleFailed(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    error,
                )))
            })?;
            scope.workspace.map(nuo_protocol::WorkspaceBinding::new)
        } else {
            workspace.clone()
        };

        // `--resume`: pick the most recent matching session instead
        // of a fresh one, inside the exact role + workspace scope.
        if init_options.resume && matches!(startup, crate::startup::SessionStart::Fresh) {
            let scope = nuo_protocol::ThreadScope::from_binding(
                role_id.as_deref(),
                resume_workspace.as_ref(),
            );
            if let Some(id) = lookup_latest_session_in_scope(&scope) {
                startup = crate::startup::SessionStart::Resume(id);
            }
        }

        // Hermetic session manifest restoration:
        // If resuming an existing session, restore directly from its immutable birth manifest.
        // This guarantees 100% byte-identical prompt cache and zero crash if external roles.toml was modified/deleted.
        let resumed_session_id = match &startup {
            crate::startup::SessionStart::Resume(id) => Some(id.as_str()),
            _ => None,
        };
        let manifest = match resumed_session_id {
            Some(id) => lookup_session_manifest(id).await,
            None => None,
        };

        if let Some(manifest) = manifest {
            let role_identity = manifest.identity;
            let mut role = nuo_protocol::AgentRoleProfile::with_identity(
                manifest.role_id.clone(),
                role_identity.clone(),
            );
            let mut admitted_tools = manifest.tools.clone();
            if manifest.role_id == "developer" {
                admitted_tools.retain(|t| t != "recall_memory");
            }
            role.tools = nuo_protocol::ToolSelection::from_allowlist(&admitted_tools);
            role.admit_mcp = manifest.admit_mcp.clone();
            identity = role_identity;
            preset = role;
        } else if let Some(role_id) = role_id.as_deref() {
            let ws_root = workspace.as_ref().map(|b| b.root.as_path());
            let roles_cfg = nuo_persistence::roles::RolesConfig::load_for_workspace(ws_root);
            if let Some(role_entry) = roles_cfg.get(role_id) {
                if role_selected.is_some() {
                    workspace = match role_entry.resolved_workspace() {
                        nuo_persistence::roles::RoleWorkspace::None => None,
                        nuo_persistence::roles::RoleWorkspace::Inherit => workspace,
                        nuo_persistence::roles::RoleWorkspace::Fixed(root) => {
                            Some(nuo_protocol::WorkspaceBinding::new(root))
                        }
                    };
                }
                let role_identity = role_entry.identity();
                let mut role = nuo_protocol::AgentRoleProfile::with_identity(
                    role_id.to_string(),
                    role_identity.clone(),
                );
                role.tools = nuo_protocol::ToolSelection::from_allowlist(&role_entry.tools);
                role.admit_mcp = role_entry.admit_mcp.clone();
                identity = role_identity;
                preset = role;
            } else if let Some(builtin) = nuo_protocol::MainAgentRole::parse(role_id) {
                if role_selected.is_some() && !builtin.requires_workspace() {
                    workspace = None;
                }
                let role = nuo_protocol::AgentRoleProfile::from_role(builtin, &identity);
                identity = role.identity.clone();
                preset = role;
            } else {
                return Err(AssembleErr::AssembleFailed(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("unknown role '{role_id}'"),
                ))));
            }
        }
        let workspace_root = workspace.as_ref().map(|binding| binding.root.clone());
        let effective_confined = if let Some(builtin) = role_id
            .as_deref()
            .and_then(nuo_protocol::MainAgentRole::parse)
        {
            if !builtin.requires_workspace() {
                builtin.default_confined() && init_options.confined
            } else {
                init_options.confined
            }
        } else {
            init_options.confined
        };
        // The session's lifetime token: shared by the build task and the
        // background `/schedule` scheduler inside the assemble.
        let cancel = CancellationToken::new();
        // Per-session channel accounting — attach/detach on the
        // WS layer keeps this fresh; the agent reads it live.
        let human_channel = Arc::new(nuo_protocol::human_request::HumanChannelAccountant::new());
        // The client-facing bus exists before Welcome: a build-phase event is
        // buffered for every attacher that subscribes during the claim window.
        let (events_tx, _) = broadcast::channel::<AgentResponse>(1024);
        // Defensive request buffer: a rogue client may send before `Ready`;
        // the build task drains this into the agent's inbox once it exists.
        let (client_tx, client_rx) = mpsc::channel::<AgentRequest>(SESSION_REQUEST_BUFFER);
        let sync_buffer = Arc::new(Mutex::new(AttachSyncBuffer::new()));
        let (phase_tx, _) = tokio::sync::watch::channel(nuo_protocol::ThreadPhase::Binding);
        // Claiming the store is bounded: `for_workspace` creates the session
        // directory and pins an id; it does not load the transcript. Resume
        // does that inside the build task, after Welcome. It runs on the
        // blocking pool so the async worker is never parked by directory and
        // role-manifest I/O.
        let store_workspace = workspace.clone();
        let store_role = role_id.clone();
        let store = tokio::task::spawn_blocking(move || {
            SessionStore::for_workspace(store_workspace, store_role)
        })
        .await
        .map_err(|error| {
            AssembleErr::AssembleFailed(Box::new(std::io::Error::other(format!(
                "session store open task failed: {error}"
            ))))
        })?;
        let store = Arc::new(store);
        let session_id = match &startup {
            crate::startup::SessionStart::Resume(id) => id.clone(),
            _ => store.id().await,
        };
        let pending = Arc::new(PendingSession {
            id: session_id.clone(),
            workspace_root: workspace_root.clone(),
            role: role_id.clone().unwrap_or_else(|| "developer".to_string()),
            events: events_tx,
            req_tx: client_tx,
            phase: Arc::new(phase_tx),
            cancel: cancel.clone(),
            done: Arc::new(tokio::sync::Semaphore::new(0)),
            human_channel: human_channel.clone(),
            sync_buffer,
            latest_snapshot: Arc::new(Mutex::new(None)),
            started_at: std::time::Instant::now(),
        });
        let phase_sink: crate::bootstrap::PhaseSink = {
            let pending = pending.clone();
            Arc::new(move |stage| {
                let phase = nuo_protocol::ThreadPhase::Preparing { stage };
                pending.phase.send_replace(phase.clone());
                let _ = pending.events.send(AgentResponse::ThreadPhase(
                    nuo_protocol::ThreadPhaseUpdate {
                        thread_id: pending.id.clone(),
                        phase,
                    },
                ));
            })
        };
        let params = BootstrapParams {
            identity,
            preset,
            ui,
            startup: startup.clone(),
            project_root: workspace_root,
            role: role_id,
            unattended: init_options.unattended,
            confined: effective_confined,
            human_channel: Some(human_channel),
            teardown_token: Some(cancel),
            shared_config: Some(self.shared_config.clone()),
            shared_provider_usage: Some(self.shared_provider_usage.clone()),
            phase_sink: Some(phase_sink),
        };
        self.building
            .lock()
            .await
            .insert(session_id, pending.clone());
        let resume_code = matches!(startup, crate::startup::SessionStart::Resume(_))
            .then_some(nuo_protocol::ERR_SESSION_UNRESUMABLE);
        let registry = self.clone();
        let build_pending = pending.clone();
        let build_done = pending.done.clone();
        tokio::spawn(async move {
            registry
                .build(build_pending, store, params, client_rx, resume_code)
                .await;
            build_done.add_permits(1);
        });
        Ok(AttachHandle::Building(pending))
    }

    async fn build(
        self,
        pending: Arc<PendingSession>,
        store: Arc<SessionStore>,
        params: BootstrapParams,
        client_rx: mpsc::Receiver<AgentRequest>,
        resume_code: Option<&'static str>,
    ) {
        let assembled = tokio::time::timeout(BUILD_BUDGET, async {
            crate::bootstrap::assemble_with_store(store, params)
                .await
                .map_err(|error| error.to_string())
        })
        .await;
        let boot = match assembled {
            Ok(Ok(boot)) => boot,
            Ok(Err(message)) => {
                self.fail_building(&pending, resume_code, message).await;
                return;
            }
            Err(_) => {
                self.fail_building(
                    &pending,
                    resume_code,
                    format!("session assembly exceeded {BUILD_BUDGET:?}"),
                )
                .await;
                return;
            }
        };
        if let Err(error) = self.finish_hosted(pending.clone(), boot, client_rx).await {
            self.fail_building(&pending, resume_code, error).await;
        }
    }

    async fn fail_building(
        &self,
        pending: &Arc<PendingSession>,
        code: Option<&'static str>,
        message: String,
    ) {
        let phase = nuo_protocol::ThreadPhase::Failed {
            code: code.map(str::to_string),
            message: message.clone(),
        };
        pending.phase.send_replace(phase.clone());
        let _ = pending.events.send(AgentResponse::ThreadPhase(
            nuo_protocol::ThreadPhaseUpdate {
                thread_id: pending.id.clone(),
                phase,
            },
        ));
        self.building.lock().await.remove(&pending.id);
        tracing::warn!(session = %pending.id, error = %message, "hosted session assembly failed");
    }

    async fn finish_hosted(
        &self,
        pending: Arc<PendingSession>,
        boot: crate::bootstrap::Bootstrap,
        client_rx: mpsc::Receiver<AgentRequest>,
    ) -> Result<(), String> {
        // A purge or an explicit cancel may have removed the claim while the
        // build was assembling. Never promote a cancelled build: doing so
        // would resurrect a thread the operator already deleted.
        if pending.cancel.is_cancelled() {
            self.building.lock().await.remove(&pending.id);
            return Ok(());
        }
        let session = boot.session.clone();
        let session_id = session.id().await;
        // Mesh registration: the session's master joins the server
        // mesh at `session/<id>`, parented to the Hypervisor. Failure is not
        // session-fatal (mesh delivery is fail-open observability), so a
        // duplicate registration simply replaces the mailbox.
        let mailbox_handle = self.register_session_mailbox(&session_id).await;
        // ADR-0008 step 8 / ADR-0009 §3: wire ACP collaboration tools into the
        // live agent through its dynamic-tool sink (the same seam MCP uses).
        #[cfg(feature = "collaboration")]
        {
            if let (Ok(addr), Some(mailbox)) = (
                acp::AgentAddress::parse(&format!("agent://local/session/{session_id}")),
                mailbox_handle,
            ) {
                let ctx = Arc::new(acp::AcpToolContext::for_fabric(
                    addr,
                    self.fabric.clone(),
                    mailbox,
                    std::time::Duration::from_secs(120),
                ));
                let acp_tools = acp::create_acp_tools(ctx);
                boot.agent.dynamic_tool_sink().replace("acp", acp_tools);
            }
        }
        #[cfg(not(feature = "collaboration"))]
        {
            let _ = mailbox_handle;
        }
        let req_tx = boot.req_tx.clone();
        let command_catalog = boot.command_catalog.clone();
        let events_tx = pending.events.clone();
        let tap = events_tx.clone();
        let mut rr = boot.resp_rx;

        // Monitor wiring: seed the tracker from the session header,
        // then fold every broadcast response into the row and publish diffs.
        // A lazily-resumed session whose round was cut off mid-flight will
        // re-drive from the first `TurnStarted` the driver emits, so the
        // tracker needs no transcript-tail heuristic — it starts Idle and
        // catches up from the live stream.
        let base = overview_of(&session, true).await;
        let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
            base_row(base, pending.workspace_root.as_deref(), &pending.role),
            ThreadStatus::Idle,
        )));
        let tracker_for_tap = tracker.clone();
        let monitor_bus = self.monitor.clone();
        let sync_buffer = pending.sync_buffer.clone();
        let sync_buffer_for_tap = sync_buffer.clone();
        let cross_for_tap = self.clone();
        let origin_for_tap = session_id.clone();
        let activity_tick = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tick_for_tap = activity_tick.clone();
        tokio::spawn(async move {
            use futures::FutureExt;
            while let Some(r) = rr.recv().await {
                // Isolate per event (the "isolate" supervision policy): a
                // poison response costs one dropped frame, not the session's
                // entire observability path.
                let fold = std::panic::AssertUnwindSafe(async {
                    {
                        tick_for_tap.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let mut guard = tracker_for_tap.lock().await;
                        guard.observe(&r);
                        let row = guard.row();
                        drop(guard);
                        let _ = monitor_bus.send(MonitorEvent::ThreadUpdated(row));
                    }
                    // Buffer the attach-sync events before broadcasting so a
                    // snapshot built after the driver's startup burst sees
                    // them. Latest-value semantics retain startup keys/picker
                    // state across turns.
                    if is_attach_sync_event(&r) {
                        let mut buf = sync_buffer_for_tap.lock().await;
                        buf.observe(&r);
                    }
                    if matches!(r, AgentResponse::ProviderPicker(_)) {
                        cross_for_tap
                            .broadcast_picker_to_other_sessions(&origin_for_tap, r.clone())
                            .await;
                    }
                    let _ = tap.send(r);
                });
                if let Err(payload) = fold.catch_unwind().await {
                    tracing::error!(
                        panic = %crate::task_fault_tolerance::panic_detail(payload),
                        "monitor tap panicked folding an event; dropped one event"
                    );
                }
            }
        });
        let cd = pending.cancel.clone();
        let driver = boot.driver;
        // Supervised driver spawn (the "evict" policy). The select arm keeps
        // kill_session's cancel semantics; the catch_unwind wrapper adds the
        // panic leg so a driver panic evicts the entry instead of leaving a
        // zombie that never drains requests.
        let crash_registry = self.clone();
        let crash_id = session_id.clone();
        let crash_events = events_tx.clone();
        let cancel_for_crash = pending.cancel.clone();
        let agent_for_crash = Some(boot.agent_for_session_end.clone());
        let driver_done = Arc::new(tokio::sync::Semaphore::new(0));
        let driver_done_signal = driver_done.clone();
        tokio::spawn(async move {
            use futures::FutureExt;
            let run = async {
                tokio::select! {
                    _ = cd.cancelled() => (),
                    _ = driver.run() => (),
                }
            };
            let outcome = std::panic::AssertUnwindSafe(run).catch_unwind().await;
            if let Err(payload) = outcome {
                let detail = crate::task_fault_tolerance::panic_detail(payload);
                tracing::error!(
                    session = %crash_id,
                    panic = %detail,
                    "session driver panicked; evicting the hosted session"
                );
                let _ = crash_events.send(AgentResponse::Error(format!(
                    "internal error: session driver panicked: {detail}"
                )));
                crash_registry
                    .evict_crashed_session(
                        &crash_id,
                        &cancel_for_crash,
                        crash_events.clone(),
                        agent_for_crash,
                    )
                    .await;
            }
            driver_done_signal.add_permits(1);
        });
        // Forward buffered/pre-Ready client requests into the agent inbox once
        // the driver exists. The channel is bounded, so an eager client applies
        // backpressure instead of growing server memory.
        let forward_tx = req_tx.clone();
        tokio::spawn(async move {
            let mut client_rx = client_rx;
            while let Some(request) = client_rx.recv().await {
                if forward_tx.send(request).await.is_err() {
                    break;
                }
            }
        });
        let hosted = Arc::new(HostedThread {
            workspace_root: pending.workspace_root.clone(),
            human_channel: pending.human_channel.clone(),
            security: boot.security.clone(),
            session: session.clone(),
            shared_confinement: boot.shared_confinement.clone(),
            req_tx,
            events: events_tx.clone(),
            cancel: pending.cancel.clone(),
            driver_done,
            tracker,
            sync_buffer,
            command_catalog,
            initial_provider: boot.initial_provider_name.clone(),
            initial_model: boot.initial_model_name.clone(),
            created_at: std::time::Instant::now(),
            last_activity: Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick,
            agent_for_session_end: Some(boot.agent_for_session_end),
        });
        // Let the driver emit its startup state before the snapshot is read;
        // the snapshot remains authoritative for durable data and the client
        // follows it with live events.
        tokio::task::yield_now().await;
        let snapshot = crate::serve::build_full_session_snapshot(&hosted).await;
        *pending.latest_snapshot.lock().await = Some(snapshot.clone());
        self.publish(MonitorEvent::ThreadAdded(hosted.tracker.lock().await.row()))
            .await;
        self.sessions
            .lock()
            .await
            .insert(session_id.clone(), hosted);
        self.building.lock().await.remove(&session_id);
        // `[INV-ATTACH-04]`: the authoritative snapshot is on the bus before
        // `Ready`, and the phase cell flips only after the snapshot is cached.
        pending.phase.send_replace(nuo_protocol::ThreadPhase::Ready);
        let _ = events_tx.send(AgentResponse::ThreadSnapshot(Box::new(snapshot)));
        let _ = events_tx.send(AgentResponse::ThreadPhase(
            nuo_protocol::ThreadPhaseUpdate {
                thread_id: session_id,
                phase: nuo_protocol::ThreadPhase::Ready,
            },
        ));
        Ok(())
    }
}

#[allow(clippy::collapsible_if)]
pub(super) async fn overview_of(session: &SessionStore, active: bool) -> ThreadOverview {
    let id = session.id().await;
    if let Ok(items) = session.list().await {
        if let Some(item) = items.into_iter().find(|i| i.id == id) {
            return ThreadOverview {
                id: item.id,
                title: item.title,
                overview: item.overview,
                created_at: item.created_at,
                updated_at: item.updated_at,
                message_count: item.message_count,
                active,
                parent_id: item.parent_id,
                fork_kind: item.fork_kind,
                fork_point: item.fork_point,
                digest: item.digest,
            };
        }
    }
    let mc = session.full_transcript().await.len();
    let digest = session.digest().await.0;
    let (real_title, _) = session.title().await;
    ThreadOverview {
        id,
        title: real_title,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: mc,
        active,
        // Not on disk yet (never persisted or empty): no lineage to report.
        parent_id: None,
        fork_kind: nuo_protocol::ThreadForkKind::Trunk,
        fork_point: None,
        digest,
    }
}
