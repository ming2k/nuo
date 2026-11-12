//! The live session driver and its request loop.

use super::followup::FollowUpQueue;
use super::recovery::{
    needs_activity_reconcile, recover_crashed_round, request_may_mutate_context,
    request_starts_a_round, ship_queued_follow_ups,
};
use super::*;

pub struct SessionDriver {
    /// Inbound requests consumed by this driver.
    pub req_rx: mpsc::Receiver<AgentRequest>,
    /// Responses bound for the frontend (`resp_tx` in the old code).
    pub tx: mpsc::UnboundedSender<AgentResponse>,
    /// Inbound request sender, cloned so `/repeat` can self-fire a `Chat`
    /// (`req_tx_for_commands` in the old code).
    pub req_tx: mpsc::Sender<AgentRequest>,
    /// The primary agent.
    pub agent: Arc<Agent>,
    /// The primary session store.
    pub session: Arc<SessionStore>,
    /// Authoritative runtime configuration shared across all hosted sessions.
    pub config: crate::SharedConfig,
    /// Authoritative connection usage shared across all hosted sessions.
    pub provider_usage: crate::SharedConnectionUsage,
    /// The shared provider holder backing the `ProxyProvider`
    /// (`provider_for_task` in the old code).
    pub provider_holder: Arc<RwLock<Arc<dyn Provider>>>,
    /// Shared skills registry.
    pub skills_registry: Arc<SkillRegistry>,
    /// Full-duplex subagent registry (ADR-0029): maps the parent tool-call
    /// id to the live child handle so a permission / ask_user reply can be
    /// routed back down into the specific subagent that surfaced it.
    pub subagent_registry: Arc<SubagentRegistry>,
    /// Live MCP runtime: the connected server set, their tools, and status.
    /// Mutated by the `/mcp` modal (toggle / reconnect) and the periodic
    /// catalog refresh; read for the session-context snapshot's MCP pane.
    pub mcp_runtime: Arc<McpRuntime>,
    /// Workspace execution authority and content-bound extension trust.
    pub workspace_security: Arc<WorkspaceSecurityStore>,
    /// Live additional-roots handle: trust decisions recompute the admitted
    /// set through it, effective on the next confined tool call.
    pub shared_additional_roots: nuo_protocol::SharedAdditionalRoots,
    /// Live handle for toggling session-level workspace confinement.
    pub shared_confinement: nuo_protocol::SharedConfinement,
    /// Backend-owned command vocabulary used by both attach metadata and the
    /// composer completion engine.
    pub command_catalog: nuo_protocol::CommandCatalog,
    /// Primary round lifecycle: at most one active round, superseded by the
    /// next begin (replaces the old token-slot + generation-counter pair).
    pub lifecycle: Arc<RoundLifecycle>,
    /// Workspace root of the session.
    pub project_root: Option<PathBuf>,
    /// Startup mode of the session.
    pub startup: SessionStart,
    /// Whether the sessions picker should open on launch (`nuo attach`
    /// with no id).
    pub open_picker_on_start: bool,
    /// Frontend clipboard bridge (ADR-0037 step 3). The TUI provides a real
    /// impl; a future browser frontend provides its own. Used only by the
    /// `/export` slash command.
    pub ui: Arc<dyn UiBridge>,
    /// Shared token-source ledger (reported vs. estimated token accounting).
    /// Installed into `agent` once at startup; the TUI reads it for the
    /// token-source report modal.
    pub token_ledger: Arc<nuo_protocol::TokenSourceLedger>,
    /// Application-registered slash command handlers (the extension point for
    /// commands that run Rust logic, e.g. a sibling binary's custom command).
    /// The dispatcher consults this in its unknown-built-in arm
    /// before falling back to the markdown-template path. Empty for `nuo`
    /// today; populated by embeddings that need it.
    pub extra_commands: Arc<crate::slash_handler::SlashCommandRegistry>,
    /// Shared hot-reloadable resolved `[web]` configuration. The web tools hold
    /// the same handle; `UpdateWebSearchConfig` replaces one versioned snapshot
    /// so changes take effect on the next call without rebuilding the toolset.
    pub websearch_shared: nuo_protocol::SharedWebConfig,
    /// Background job manager for asynchronous processes and sub-subagents.
    pub background_jobs: crate::background_jobs::BackgroundJobManager,
}

impl SessionDriver {
    /// Run the session to completion, exiting when the request channel closes.
    ///
    /// The driver is destructured into locals with the original inline-task
    /// names so the established dispatch body remains unchanged.
    //
    // NOTE: a `refresh_agent_pursuit` + SessionStart-hooks block inside the
    // `/pursue status` branch has inconsistent indentation and looks misplaced —
    // it fires session-start hooks every time `/pursue status` runs. Preserved
    // verbatim here; not this refactor's job to fix.
    pub async fn run(self) {
        let SessionDriver {
            mut req_rx,
            tx: resp_tx,
            req_tx: req_tx_for_commands,
            agent,
            session,
            config: shared_config,
            provider_usage: shared_provider_usage,
            provider_holder: provider_for_task,
            skills_registry,
            subagent_registry,
            mcp_runtime,
            workspace_security,
            shared_additional_roots,
            shared_confinement,
            command_catalog,
            lifecycle,
            project_root: project_root_for_side,
            startup,
            open_picker_on_start,
            ui,
            token_ledger,
            extra_commands,
            websearch_shared,
            background_jobs,
        } = self;
        // Hand the shared token-source ledger to the agent so each turn's token
        // usage (reported vs. estimated) is booked into it for the report modal.
        agent.install_token_ledger(token_ledger.clone());
        let completion_engine = crate::input_completion::InputCompletionEngine::new(
            command_catalog,
            project_root_for_side.clone().unwrap_or_default(),
        )
        .with_skills((*skills_registry).clone());

        let initial_session_id = session.id().await;
        token_ledger.restore_session(&initial_session_id, session.request_usage_records().await);
        token_ledger.set_active_session(initial_session_id.clone());
        // Crash-residue recovery. The round path arms the durable
        // `/retry` resume point only on stops it can observe — a terminal
        // error after the provider retry budget, an interrupt past the
        // phase-1 unsend window. A process that dies with a round on the
        // wire (SIGKILL, panic, power loss) runs none of those paths, so
        // the point is never armed and a resumed session answers `/retry`
        // with "Nothing to retry" even though its last round visibly died
        // mid-flight.
        //
        // The reliable residue marker is a request-usage record that is
        // still `InFlight` in the *session store*. Every live settlement
        // path writes a terminal status back before the round ends, and
        // `TokenSourceLedger::restore_session` (called just above) flips
        // the copies it loads to `Abandoned` — but only in the ledger's
        // in-memory map, never in the store. So the store's own `InFlight`
        // means "nobody ever settled this request", and reading it here —
        // before any new round can rewrite the ledger — is exactly the
        // crash signal. (The old comment claimed `restore_session` had
        // already flipped the store copy; it had not, which is why the
        // previous `Abandoned` filter never fired on the first reload and
        // could only ever fire on stale records from an *earlier* round
        // after a resume-then-crash-again sequence — the opposite of the
        // intent.)
        {
            let residue = recover_crashed_round(
                &session,
                session.request_usage_records().await,
                crate::registry::unix_epoch_ms(),
            )
            .await;
            for record in residue.interrupts {
                if let Err(error) = session.record_round_interrupt(record).await {
                    tracing::warn!(?error, "could not record crash-residue interrupt");
                }
            }
            if let Some(point) = residue.retry_point {
                tracing::info!(
                    session = %initial_session_id,
                    round = point.round,
                    "armed crash-resume /retry point for the terminated round"
                );
                if let Err(error) = session.arm_retry_pending(point).await {
                    tracing::warn!(%error, "could not arm crash-resume retry point");
                }
            }
            // D4: the residue read above is the store's last word on
            // the crash. Commit the reclassification so every reader — not just
            // this process's ledger — sees the abandoned attempt resolved,
            // rather than having it filtered out as an unresolved in-flight.
            match session.settle_abandoned_attempts().await {
                Ok(0) => {}
                Ok(settled) => tracing::info!(
                    session = %initial_session_id,
                    settled,
                    "durably settled abandoned request attempts"
                ),
                Err(error) => {
                    tracing::warn!(%error, "could not settle abandoned request attempts");
                }
            }
        }
        let initial_context = agent
            .estimate_next_request_tokens(&session.model_window().await)
            .total_tokens;
        let _ = resp_tx.send(round_response(
            &initial_session_id,
            nuo_protocol::RoundEvent::ContextTokens(nuo_protocol::ContextTokenSnapshot::new(
                initial_context,
                nuo_protocol::ContextTokenSource::Projection,
            )),
        ));
        // Session-scoped idle snapshot: publishes the `/retry`
        // affordance from the durable resume point so a session whose round
        // stopped before a detach/reattach offers `/retry` from frame one.
        send_harness_state_for_session(
            &resp_tx,
            &initial_session_id,
            &agent,
            &session,
            LoopStatus::Idle,
        )
        .await;
        {
            let mut config = shared_config.write().await;
            let mut provider_usage = shared_provider_usage.write().await;
            let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(&config)));
            // Session title (ADR-0022): when the background titler durably
            // persists a session's first title, push a fresh sessions overview.
            // The registry's broadcast-tap folds the snapshot into the monitor
            // tracker and republishes `MonitorEvent::SessionUpdated`, so every
            // attached client (TUI picker, web panel) sees the new title without
            // reopening the dialog — the same refresh path a manual
            // `RenameSession` takes. Absent on subagent/side sessions, where
            // titling stays a silent background write.
            {
                let session_for_titler = Arc::clone(&session);
                let resp_tx_for_titler = resp_tx.clone();
                agent.set_title_established(std::sync::Arc::new(move |_title| {
                    let session = Arc::clone(&session_for_titler);
                    let resp_tx = resp_tx_for_titler.clone();
                    Box::pin(async move {
                        crate::handlers_session::overview(&session, &resp_tx).await;
                    }) as futures::future::BoxFuture<'static, ()>
                }));
            }
            // Record that the default provider + model were activated on startup, so
            // the picker's recency ordering reflects "last used = now" for both
            // stages, and the provider is pinned to the exact model it booted under.
            // Both signals are needed: `record` drives stage-1 provider ordering and
            // `record_model` drives stage-2 model ordering *and* writes the
            // `last_models` pin that `active_model_id_for_entry` consults on the next
            // launch to re-open the provider on its exact model instead of a
            // re-derived default. Recording only the provider (the previous behavior)
            // left `last_models` stale, so a session that booted into a provider —
            // never manually switched its model — reopened on the default-channel
            // model rather than the one it actually ran with. Best-effort: usage
            // tracking is rebuildable state and must never block startup.
            {
                let initial_id = nuo_model_routing::default_provider_id(&config).to_string();
                // Resolve the model the way `build_provider_for` did when main.rs
                // constructed the startup provider: `config.default_model` when the
                // entry serves it, otherwise the entry's default-channel model. The
                // config-only resolver (`resolved_model_name`, *not* the `_with_usage`
                // variant) mirrors that precedence exactly — it ignores `last_models`,
                // so it never pins a model the live provider was not actually built
                // with. Pinning the exact live model (rather than a usage-derived one)
                // is what lets the next launch re-open this provider on the same model.
                let initial_model = nuo_model_routing::resolved_model_name(&config, &initial_id);
                provider_usage.record(&initial_id);
                // Skip the model pin when the startup provider is unbuildable
                // (`resolved_model_name` returns `None`): there is no real channel,
                // so pinning a (non-existent) model would be a spurious `last_models`
                // entry. The provider recency bump above still runs so the picker
                // ordering is correct.
                if let Some(model) = initial_model.as_deref() {
                    provider_usage.record_model(&initial_id, model);
                }
                if let Err(error) = provider_usage.save() {
                    tracing::warn!(?error, "could not persist provider/model usage telemetry");
                }
            }
            nuo_model_routing::prune_stale_models(&mut config, &mut provider_usage);
            // Push the initial model-picker snapshot (default id + per-model
            // favorite / key-ready / last-used) so the picker is ready the moment
            // the user opens it.
            let _ = resp_tx.send(AgentResponse::ProviderPicker(
                crate::handlers_provider::build_live_picker_state(&config, &provider_usage),
            ));
            // Announce the active provider/model as a synthetic `ProviderSwitched`
            // so an attach client (which subscribes to the broadcast only after the
            // handshake and so misses the startup emissions) can seed its hint bar
            // — model name, reasoning effort, `@instance`, context meter — from the
            // same single source the in-process TUI reads. The driver resolved this
            // pair from the global default overlaid with the session's provider pin
            // (C6), so it is authoritative for this session. Emitting it here (after
            // the picker snapshot) also lets the registry's attach-sync buffer
            // capture and replay it. Resolved config-only (`resolved_model_name`,
            // not the usage variant) to mirror exactly the model the live provider
            // was built with.
            {
                let provider = nuo_model_routing::default_provider_id(&config).to_string();
                let model =
                    nuo_model_routing::resolved_model_name(&config, &provider).unwrap_or_default();
                let _ = resp_tx.send(AgentResponse::ProviderSwitched { provider, model });
            }
        }
        if open_picker_on_start {
            let _ = resp_tx.send(AgentResponse::ThreadsOverview(
                build_sessions_overview(&session).await,
            ));
        }
        enum OAuthResult {
            Authorize {
                auth: nuo_protocol::ConnectionAuth,
                provider: String,
                tokens: Option<nuo_provider::TokenSet>,
            },
            Connect {
                provider_id: String,
                success: bool,
            },
        }
        struct CatalogSyncTaskResult {
            outcome: nuo_model_routing::RoutingSyncOutcome,
            session_id: Option<String>,
        }
        let mut active_oauth_task: Option<tokio::task::JoinHandle<OAuthResult>> = None;
        let mut active_catalog_sync_task: Option<tokio::task::JoinHandle<CatalogSyncTaskResult>> =
            None;
        // The sync streams one update per connection as it completes.
        // This sender stays alive for the driver's lifetime, so `recv()` yields
        // updates without ever observing a closed channel.
        let (catalog_sync_tx, mut catalog_sync_rx) =
            mpsc::unbounded_channel::<nuo_model_routing::ConnectionUpdate>();
        let mut pending_oauth_authorization: Option<
            crate::handlers_provider::PendingOAuthAuthorization,
        > = None;
        // M4: the driver owns the follow-up queue authority. The
        // boundary watchers wake this loop when a target round ends; the
        // ship pass dequeues the next shippable item.
        let (followup_wake_tx, mut followup_wake_rx) = mpsc::channel::<String>(16);
        let mut followup_queue = FollowUpQueue::new();
        enum Incoming {
            Request(AgentRequest),
            Wake(String),
        }
        loop {
            let incoming = tokio::select! {
                res_opt = req_rx.recv() => {
                    let Some(req) = res_opt else { break; };
                    Incoming::Request(req)
                }
                wake_res = followup_wake_rx.recv() => {
                    match wake_res {
                        Some(target) => Incoming::Wake(target),
                        None => break,
                    }
                }
                catalog_sync_res = async {
                    if let Some(ref mut task) = active_catalog_sync_task {
                        task.await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    active_catalog_sync_task = None;
                    if let Ok(res) = catalog_sync_res {
                        let mut config = shared_config.write().await;
                        let mut provider_usage = shared_provider_usage.write().await;
                        crate::handlers_provider::apply_catalog_sync_outcome(
                            &mut config,
                            &resp_tx,
                            &mut provider_usage,
                            res.outcome,
                            res.session_id,
                        );
                    }
                    continue;
                }
                update = catalog_sync_rx.recv() => {
                    if let Some(update) = update {
                        let mut config = shared_config.write().await;
                        let mut provider_usage = shared_provider_usage.write().await;
                        crate::handlers_provider::apply_connection_update(
                            &mut config,
                            &resp_tx,
                            &mut provider_usage,
                            update,
                        );
                    }
                    continue;
                }
                oauth_res = async {
                    if let Some(ref mut task) = active_oauth_task {
                        task.await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    active_oauth_task = None;
                    if let Ok(res) = oauth_res {
                        match res {
                            OAuthResult::Authorize { auth, provider, tokens } => {
                                pending_oauth_authorization = tokens.map(|tokens| {
                                    crate::handlers_provider::PendingOAuthAuthorization {
                                        auth,
                                        tokens,
                                    }
                                });
                                if pending_oauth_authorization.is_some() {
                                    let _ = resp_tx.send(AgentResponse::ConnectStatus(
                                        nuo_protocol::ConnectStatus::Done { provider },
                                    ));
                                }
                            }
                            OAuthResult::Connect { provider_id, success } => {
                                if success {
                                    let mut config = shared_config.write().await;
                                    let mut provider_usage = shared_provider_usage.write().await;
                                    crate::handlers_provider::connect_post_oauth(
                                        &mut config,
                                        &agent,
                                        &provider_for_task,
                                        &resp_tx,
                                        &mut provider_usage,
                                        provider_id,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    continue;
                }
            };
            let req = match incoming {
                Incoming::Request(req) => req,
                Incoming::Wake(wake_target) => {
                    // Round-boundary wake (M4): a target round ended.
                    // A round the operator interrupted parks its queue — the
                    // user said stop; auto-firing more prompts after an Esc is
                    // never the intent. Ctrl+P / a fresh send re-arms it.
                    if session.id().await == wake_target && lifecycle.was_interrupted() {
                        followup_queue.set_paused(&wake_target, true);
                        crate::handlers_chat::emit_queue_snapshot(
                            &resp_tx,
                            &followup_queue,
                            &wake_target,
                        );
                    }
                    // The ship pass itself is NOT here: it is evaluated once per
                    // loop iteration below (see `ship_queued_follow_ups`). A wake
                    // is a *hint* that a boundary may have opened, and hints are
                    // lossy by nature: `Notify::notified()` only resolves once
                    // per stored permit, and the boundary watcher drops a wake
                    // when nobody is listening. Making the pass depend on
                    // receiving a hint meant the queue could park forever after a
                    // lost notification, an interrupted-then-resumed round, or a
                    // wake that arrived while the channel was full.
                    let _ = wake_target;
                    continue;
                }
            };
            // `[INV-OUTBOX-07]` — the ship pass is a per-iteration invariant, not
            // a branch of the wake arm.
            //
            // Everything it needs is checkable here (`is_running`, `is_paused`,
            // queue depth), and this loop iterates on every request, every wake,
            // and every background completion — so the queue cannot be left
            // parked by a missed notification, and a `resume` cannot be
            // overtaken by a request that was already queued on the channel.
            // It runs for every iteration *except* when the request about to be
            // dispatched will start a round itself; that path decides inline.
            if !request_starts_a_round(&req) {
                ship_queued_follow_ups(
                    &agent,
                    &session,
                    &lifecycle,
                    &resp_tx,
                    &shared_config,
                    &mut followup_queue,
                    &followup_wake_tx,
                )
                .await;
            }
            let pre_session_id = session.id().await;
            let pre_provider = agent.provider.provider_id();
            let pre_model = agent.provider.model();
            // Requests that own the round lifecycle close their own activity
            // resolution: the round task (or the shell-command round) always
            // emits a terminal `HarnessState(Idle)` on exit. Every other
            // request is a control-plane op that does not. The TUI no longer
            // arms optimistic activity state for control-plane dispatches at
            // all (a command is outside the round state machine),
            // but other frontends may still paint their own optimistic
            // state, so the driver keeps reconciling every non-round request
            // back to the authoritative harness state — see the reconcile
            // below the match and /0110.
            let reconcile_activity = needs_activity_reconcile(&req, &lifecycle).await;
            let may_mutate_context = request_may_mutate_context(&req);
            let mut config = shared_config.write().await;
            let mut provider_usage = shared_provider_usage.write().await;
            match req {
                AgentRequest::Interrupt => {
                    crate::handlers_permission::interrupt(&agent, &session, &resp_tx, &lifecycle)
                        .await;
                }
                AgentRequest::InterruptSubagent { call_id } => {
                    // Scene-scoped interrupt: stop only the viewed
                    // subagent — its token races its in-flight request, the
                    // parent round and every sibling keep running. A no-op for
                    // a finished or unknown call.
                    crate::handlers_permission::interrupt_subagent(&subagent_registry, &call_id);
                }
                AgentRequest::PermissionReply {
                    request_id,
                    decision,
                    parent_call_id,
                } => {
                    crate::handlers_permission::reply(
                        &agent,
                        &subagent_registry,
                        &resp_tx,
                        request_id,
                        decision,
                        parent_call_id,
                    )
                    .await;
                }
                AgentRequest::UserQuestionReply {
                    request_id,
                    answers,
                    parent_call_id,
                } => {
                    crate::handlers_permission::reply_question(
                        crate::handlers_permission::ReplyEnv {
                            agent: &agent,
                            subagent_registry: &subagent_registry,
                            resp_tx: &resp_tx,
                        },
                        request_id,
                        answers,
                        parent_call_id,
                    )
                    .await;
                }
                AgentRequest::StdinReply {
                    request_id,
                    text,
                    parent_call_id,
                } => {
                    crate::handlers_permission::reply_input(
                        crate::handlers_permission::ReplyEnv {
                            agent: &agent,
                            subagent_registry: &subagent_registry,
                            resp_tx: &resp_tx,
                        },
                        request_id,
                        text,
                        parent_call_id,
                    )
                    .await;
                }
                AgentRequest::SwitchConnection {
                    provider,
                    model,
                    api_key,
                    base_url,
                } => {
                    crate::handlers_provider::switch(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        provider,
                        model,
                        api_key,
                        base_url,
                    )
                    .await;
                }
                AgentRequest::RegisterProvider {
                    id,
                    label,
                    root_url,
                    protocol,
                    user_agent,
                    headers,
                    catalog_format,
                    dialect,
                } => {
                    if let Err(e) = crate::handlers_provider::register_provider(
                        crate::handlers_provider::RegisterProviderParams {
                            id,
                            label,
                            root_url,
                            protocol,
                            user_agent,
                            headers,
                            catalog_format,
                            dialect,
                        },
                    ) {
                        let _ = resp_tx.send(AgentResponse::Error(format!(
                            "Could not register provider: {e}"
                        )));
                    }
                }
                AgentRequest::AddConnection {
                    name,
                    provider,
                    api_key,
                    models,
                    auth,
                    headers,
                    budget,
                    entitlement,
                } => {
                    let pending_authorization = pending_oauth_authorization.take();
                    crate::handlers_provider::add(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        crate::handlers_provider::AddConnectionParams {
                            name,
                            provider,
                            api_key,
                            models,
                            auth,
                            headers,
                            budget,
                            entitlement,
                        },
                        pending_authorization,
                    )
                    .await;
                }
                AgentRequest::ConnectConnection { name, method } => {
                    if let Some(task) = active_oauth_task.take() {
                        task.abort();
                    }
                    let resp_tx_clone = resp_tx.clone();
                    let connection = name.clone();
                    active_oauth_task = Some(tokio::spawn(async move {
                        let success = crate::handlers_provider::run_oauth_for_connect(
                            &resp_tx_clone,
                            connection.clone(),
                            method,
                        )
                        .await;
                        OAuthResult::Connect {
                            provider_id: connection,
                            success,
                        }
                    }));
                }
                AgentRequest::RenameConnection { from, to } => {
                    crate::handlers_provider::rename(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        from,
                        to,
                    )
                    .await;
                }
                AgentRequest::AuthorizeOAuth { method, auth } => {
                    if let Some(task) = active_oauth_task.take() {
                        task.abort();
                    }
                    pending_oauth_authorization = None;
                    let resp_tx_clone = resp_tx.clone();
                    let provider = auth.negotiated_provider().unwrap_or("oauth").to_string();
                    active_oauth_task = Some(tokio::spawn(async move {
                        let tokens = crate::handlers_provider::authorize(
                            &resp_tx_clone,
                            method,
                            auth.clone(),
                        )
                        .await;
                        OAuthResult::Authorize {
                            auth,
                            provider,
                            tokens,
                        }
                    }));
                }
                AgentRequest::CancelAuthorizeOAuth => {
                    if let Some(task) = active_oauth_task.take() {
                        task.abort();
                    }
                    pending_oauth_authorization = None;
                    let _ = resp_tx.send(AgentResponse::ConnectStatus(
                        nuo_protocol::ConnectStatus::Failed {
                            provider: "oauth".to_string(),
                            message: "cancelled".to_string(),
                        },
                    ));
                }
                AgentRequest::EditConnection {
                    name,
                    provider,
                    api_key,
                    headers,
                    budget,
                    entitlement,
                } => {
                    crate::handlers_provider::edit(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        name,
                        provider,
                        api_key,
                        headers,
                        Some(budget),
                        entitlement,
                    )
                    .await;
                }
                AgentRequest::IncludeModel { scope, model } => {
                    crate::handlers_provider::include_model(
                        &mut config,
                        &resp_tx,
                        &mut provider_usage,
                        scope,
                        model,
                    )
                    .await;
                }
                AgentRequest::ExcludeModel { scope, model_id } => {
                    crate::handlers_provider::exclude_model(
                        &mut config,
                        &resp_tx,
                        &mut provider_usage,
                        scope,
                        model_id,
                    )
                    .await;
                }
                AgentRequest::ClearModelRule { scope, model_id } => {
                    crate::handlers_provider::clear_model_rule(
                        &mut config,
                        &resp_tx,
                        &mut provider_usage,
                        scope,
                        model_id,
                    )
                    .await;
                }
                AgentRequest::SetModelCapabilities {
                    scope,
                    model_id,
                    overrides,
                } => {
                    crate::handlers_provider::set_model_capabilities(
                        &mut config,
                        &resp_tx,
                        &mut provider_usage,
                        scope,
                        model_id,
                        overrides,
                    )
                    .await;
                }
                AgentRequest::EditConnectionModel {
                    connection,
                    model,
                    effort,
                    thinking,
                    overrides,
                } => {
                    crate::handlers_provider::edit_model(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        connection,
                        model,
                        effort,
                        thinking,
                        overrides,
                    )
                    .await;
                }
                AgentRequest::EditModelReasoning {
                    model,
                    effort,
                    thinking,
                    overrides,
                } => {
                    crate::handlers_provider::edit_model_reasoning(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        model,
                        effort,
                        thinking,
                        overrides,
                    )
                    .await;
                }
                AgentRequest::DeleteConnection { name } => {
                    crate::handlers_provider::delete(
                        crate::handlers_provider::ProviderEnv {
                            config: &mut config,
                            agent: &agent,
                            provider_for_task: &provider_for_task,
                            session: &session,
                            resp_tx: &resp_tx,
                            provider_usage: &mut provider_usage,
                        },
                        name,
                    )
                    .await;
                }
                AgentRequest::ToggleFavorite { id } => {
                    crate::handlers_provider::toggle_favorite(
                        &mut config,
                        &resp_tx,
                        &provider_usage,
                        id,
                    )
                    .await;
                }
                AgentRequest::SetDefaultModel { id } => {
                    crate::handlers_provider::set_default_model(
                        &mut config,
                        &agent,
                        &provider_for_task,
                        &resp_tx,
                        &mut provider_usage,
                        id,
                    )
                    .await;
                }
                AgentRequest::RefreshProviderModels => {
                    // A user refresh always supersedes an in-flight one.
                    if let Some(task) = active_catalog_sync_task.take() {
                        task.abort();
                    }
                    let session_id = Some(session.id().await);
                    let sink = catalog_sync_tx.clone();
                    active_catalog_sync_task = Some(tokio::spawn(async move {
                        let outcome = nuo_model_routing::sync_remote_routes_streaming(sink).await;
                        CatalogSyncTaskResult {
                            outcome,
                            session_id,
                        }
                    }));
                    let quota_resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_provider::query_provider_quotas(&quota_resp_tx, None, true)
                            .await;
                    });
                }
                AgentRequest::DeleteThread { id } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::delete(&session, &resp_tx, id).await;
                    });
                }
                AgentRequest::RenameThread { id, title } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::rename(&session, &resp_tx, id, title).await;
                    });
                }
                AgentRequest::QueryThreadDetail { id } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::detail(&session, &resp_tx, id).await;
                    });
                }
                AgentRequest::QueryConnectionDetail { id, force_refresh } => {
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_provider::query_connection_detail(
                            &resp_tx,
                            id,
                            force_refresh,
                        )
                        .await;
                    });
                }
                AgentRequest::QueryAllConnectionsUsage { force_refresh } => {
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_provider::query_all_connections_usage(
                            &resp_tx,
                            force_refresh,
                        )
                        .await;
                    });
                }
                AgentRequest::QueryProviderQuotas {
                    provider,
                    force_refresh,
                } => {
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_provider::query_provider_quotas(
                            &resp_tx,
                            provider,
                            force_refresh,
                        )
                        .await;
                    });
                }
                AgentRequest::QueryThreadsOverview => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::overview(&session, &resp_tx).await;
                    });
                }
                AgentRequest::QueryThreadTree => {
                    // Keep id + tree capture ordered with session-switch
                    // requests handled by this driver, so the tagged snapshot
                    // can never pair one session's id with another's DAG.
                    crate::handlers_session::tree(&session, &resp_tx).await;
                }
                AgentRequest::QueryTokenUsage { thread_id } => {
                    crate::handlers_session::token_usage(&token_ledger, &resp_tx, thread_id);
                }
                AgentRequest::QueryUsageStats { event_cap } => {
                    crate::handlers_session::usage_stats(&resp_tx, event_cap).await;
                }
                AgentRequest::QueryThreadContext => {
                    crate::handlers_session::query_context(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                    );
                }
                AgentRequest::QueryServerEndpoints => {
                    let report = crate::serve::get_current_endpoints().await;
                    let _ = resp_tx.send(AgentResponse::ServerEndpointsStatus(report));
                }
                AgentRequest::ToggleServerPort { port, enable } => {
                    let report = if enable {
                        crate::serve::open_dynamic_tcp(port).await
                    } else {
                        crate::serve::close_dynamic_tcp().await
                    };
                    if let Ok(rep) = report {
                        let _ = resp_tx.send(AgentResponse::ServerEndpointsStatus(rep));
                    }
                }
                AgentRequest::RevokePermission { tool, scope } => {
                    crate::handlers_session::revoke_permission(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        tool,
                        scope,
                    );
                }
                AgentRequest::ClearAllPermissions => {
                    crate::handlers_session::clear_all_permissions(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                    );
                }
                AgentRequest::ToggleTool { name, enabled } => {
                    crate::handlers_session::toggle_tool(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        name,
                        enabled,
                    );
                }
                AgentRequest::ToggleMcpServer { name, enabled } => {
                    crate::handlers_session::toggle_mcp_server(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        name,
                        enabled,
                    )
                    .await;
                }
                AgentRequest::ReconnectMcpServer { name } => {
                    crate::handlers_session::reconnect_mcp_server(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                        name,
                    )
                    .await;
                }
                AgentRequest::SlashCommand(cmd) => {
                    crate::handlers_slash::dispatch(
                        cmd,
                        SlashEnv {
                            config: &config,
                            agent: &agent,
                            mcp_runtime: &mcp_runtime,
                            workspace_security: &workspace_security,
                            shared_additional_roots: &shared_additional_roots,
                            shared_confinement: &shared_confinement,
                            resp_tx: &resp_tx,
                            session: &session,
                            lifecycle: &lifecycle,
                            provider_for_task: &provider_for_task,
                            provider_usage: &mut provider_usage,
                            skills_registry: &skills_registry,
                            req_tx_for_commands: &req_tx_for_commands,
                            startup: &startup,
                            ui: &*ui,
                            extra_commands: &extra_commands,
                            websearch_shared: &websearch_shared,
                            background_jobs: &background_jobs,
                        },
                    )
                    .await;
                }
                AgentRequest::TrustWorkspace { domains, decision } => {
                    let user_assets_state =
                        crate::handlers_slash::security_ops::compute_user_assets_trust();
                    let user_assets_needed_review = user_assets_state.needs_review();
                    // The decision is carried explicitly, never inferred from the
                    // domain list. `domains` means "these" — granting the refused
                    // set would be a security inversion, so the refusal has its
                    // own arm below and never reaches the grant path.
                    let granting = decision == nuo_protocol::TrustDecision::Grant;
                    if granting {
                        if domains.contains(&nuo_protocol::TrustDomain::UserAssets) {
                            crate::handlers_slash::security_ops::trust_user_assets();
                        } else if user_assets_needed_review {
                            crate::handlers_slash::security_ops::deny_user_assets();
                        }
                    } else {
                        // A refusal names what to refuse; everything else is
                        // untouched, including grants the user made earlier.
                        if domains.contains(&nuo_protocol::TrustDomain::UserAssets) {
                            crate::handlers_slash::security_ops::deny_user_assets();
                        }
                    }
                    let effective = if let Some(root) = &project_root_for_side {
                        let ws_domains: Vec<nuo_protocol::TrustDomain> = domains
                            .iter()
                            .copied()
                            .filter(|d| *d != nuo_protocol::TrustDomain::UserAssets)
                            .collect();

                        // Candidate workspace domains D: every domain awaiting a
                        // human decision, per the one predicate. Enumerating this
                        // inline is how the gate and the driver came to disagree
                        // about whether `Changed` was a candidate.
                        let pre_snap = workspace_security.snapshot(root);
                        let candidate_ws: Vec<nuo_protocol::TrustDomain> = pre_snap
                            .domains_needing_review()
                            .into_iter()
                            .filter(|d| *d != nuo_protocol::TrustDomain::UserAssets)
                            .collect();

                        if granting {
                            if !ws_domains.is_empty()
                                && let Err(error) =
                                    workspace_security.trust_domains(root, &ws_domains)
                            {
                                tracing::error!(?error, "failed to persist workspace trust");
                                let _ = resp_tx.send(round_response(
                                    &session.id().await,
                                    nuo_protocol::RoundEvent::Notice(
                                        nuo_protocol::AgentNotice::trust_failure(format!(
                                            "Workspace trust failed: {error}"
                                        )),
                                    ),
                                ));
                            }

                            // Explicit negative attestation on unselected candidate domains: D \ S
                            let unselected_ws: Vec<nuo_protocol::TrustDomain> = candidate_ws
                                .into_iter()
                                .filter(|d| !ws_domains.contains(d))
                                .collect();

                            if !unselected_ws.is_empty() {
                                let _ = workspace_security.deny_domains(root, &unselected_ws);
                            }
                        } else {
                            // A refusal records exactly what was named and touches
                            // nothing else: `deny_domains` leaves existing grants
                            // intact, unlike the revoke path.
                            let refused: Vec<nuo_protocol::TrustDomain> = ws_domains
                                .into_iter()
                                .filter(|d| candidate_ws.contains(d))
                                .collect();
                            if !refused.is_empty()
                                && let Err(error) = workspace_security.deny_domains(root, &refused)
                            {
                                tracing::error!(?error, "failed to persist workspace refusal");
                                let _ = resp_tx.send(round_response(
                                    &session.id().await,
                                    nuo_protocol::RoundEvent::Notice(
                                        nuo_protocol::AgentNotice::trust_failure(format!(
                                            "Workspace trust refusal failed: {error}"
                                        )),
                                    ),
                                ));
                            }
                        }

                        let mut snapshot = workspace_security.snapshot(root);
                        snapshot.user_assets =
                            crate::handlers_slash::security_ops::compute_user_assets_trust();
                        agent.set_workspace_security(snapshot.clone());

                        // Fast path: load in-memory configs, rules, hooks, roots immediately
                        let mut effective = nuo_persistence::config::Config::load();
                        if snapshot.mcp.is_trusted() {
                            effective.merge_project_mcp(
                                nuo_persistence::config::Config::load_project_mcp(root),
                            );
                        }
                        if snapshot.hooks.is_trusted() {
                            effective.merge_project_hooks(
                                nuo_persistence::config::Config::load_project_hooks(root),
                            );
                        }
                        if snapshot.ex_workspace.is_trusted() {
                            effective.merge_project_additional_roots(
                                nuo_persistence::config::Config::load_project_additional_roots(
                                    root,
                                ),
                            );
                        }
                        crate::handlers_slash::session_ops::apply_additional_roots(
                            &shared_additional_roots,
                            &effective,
                            root,
                        );
                        let rules = if snapshot.instructions.is_trusted() {
                            crate::project::load_project_rules(root).unwrap_or_default()
                        } else {
                            String::new()
                        };
                        agent.set_project_rules(rules);
                        let instructions = if snapshot.instructions.is_trusted() {
                            match crate::project::load_project_instructions(root) {
                                Ok(instructions) => instructions,
                                Err(error) => {
                                    tracing::error!(
                                        %error,
                                        "project instructions could not be loaded; continuing without them"
                                    );
                                    let _ = resp_tx.send(round_response(
                                        &session.id().await,
                                        nuo_protocol::RoundEvent::Notice(
                                            nuo_protocol::AgentNotice::new(
                                                nuo_protocol::NoticeKind::Configuration,
                                                nuo_protocol::NoticeSeverity::Error,
                                                "Project instructions could not be loaded",
                                                nuo_protocol::NoticeSource::Harness,
                                            )
                                            .with_body(error),
                                        ),
                                    ));
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        };
                        agent.set_project_instructions(instructions);
                        agent
                            .set_hooks(crate::hooks::build_hook_registry(&effective.hooks, &agent));
                        effective
                    } else {
                        // Workspace-free session: user-level assets only.
                        let mut snapshot =
                            nuo_protocol::WorkspaceSecuritySnapshot::new("workspace-free");
                        snapshot.user_assets =
                            crate::handlers_slash::security_ops::compute_user_assets_trust();
                        agent.set_workspace_security(snapshot);
                        nuo_persistence::config::Config::load()
                    };

                    // Immediately broadcast Trusted HarnessState so the client
                    // unblocks instantly — **only when no round is live**. The
                    // snapshot is a state sample, and every frontend treats it as
                    // a boundary (it collapses the activity bar, retires pending
                    // commands, and sweeps provisional tool rows). Publishing
                    // `Idle` while a round is running would therefore tear down a
                    // live round's UI. When a round *is* running its own events
                    // own the display; the next boundary publishes the trust
                    // snapshot.
                    if !lifecycle.is_running().await {
                        send_harness_state_for_session(
                            &resp_tx,
                            &session.id().await,
                            &agent,
                            &session,
                            LoopStatus::Idle,
                        )
                        .await;
                    }

                    // Asynchronously background heavy reconfigure/reload (MCP connections & skills scanner)
                    let mcp_clone = mcp_runtime.clone();
                    let skills_clone = skills_registry.clone();
                    tokio::spawn(async move {
                        let _ = mcp_clone.reconfigure(effective.mcp).await;
                        skills_clone.reload().await;
                    });
                }
                AgentRequest::ReloadRuntime => {
                    // ADR-0034 Level 1 (soft reload): re-read the domain
                    // configuration and re-sync MCP servers + skills without
                    // disturbing the connection or any in-flight round.
                    let effective = nuo_persistence::config::Config::load();
                    let mcp_clone = mcp_runtime.clone();
                    let skills_clone = skills_registry.clone();
                    tokio::spawn(async move {
                        let _ = mcp_clone.reconfigure(effective.mcp).await;
                        skills_clone.reload().await;
                    });
                    let _ = resp_tx.send(round_response(
                        &session.id().await,
                        nuo_protocol::RoundEvent::Notice(nuo_protocol::AgentNotice::command_ack(
                            "Runtime reloaded: configuration, credentials, MCP servers, and skills.",
                        )),
                    ));
                }
                AgentRequest::CompleteComposer {
                    request_id,
                    text,
                    cursor,
                } => {
                    let _ =
                        resp_tx.send(completion_engine.complete(request_id, text, cursor).await);
                }
                AgentRequest::Prompt {
                    text,
                    images,
                    sent_at_ms,
                } => {
                    crate::handlers_chat::chat(
                        ChatEnv {
                            agent: &agent,
                            session: &session,
                            lifecycle: &lifecycle,
                            tx: &resp_tx,
                            config: &config,
                        },
                        text,
                        images,
                        sent_at_ms,
                    )
                    .await;
                }
                AgentRequest::Steer { thread_id, message } => {
                    crate::handlers_chat::steer(&agent, &session, &resp_tx, thread_id, message)
                        .await;
                }
                AgentRequest::CancelSteer {
                    thread_id,
                    input_id,
                } => {
                    crate::handlers_chat::cancel_steer(
                        &agent, &session, &resp_tx, thread_id, input_id,
                    )
                    .await;
                }
                AgentRequest::FollowUp { thread_id, message } => {
                    crate::handlers_chat::follow_up(
                        ChatEnv {
                            agent: &agent,
                            session: &session,
                            lifecycle: &lifecycle,
                            tx: &resp_tx,
                            config: &config,
                        },
                        &mut followup_queue,
                        &followup_wake_tx,
                        thread_id,
                        message,
                    )
                    .await;
                }
                AgentRequest::QueueRemove {
                    thread_id,
                    input_id,
                } => {
                    crate::handlers_chat::queue_remove(
                        &resp_tx,
                        &mut followup_queue,
                        thread_id,
                        input_id,
                    )
                    .await;
                }
                AgentRequest::QueueClear { thread_id } => {
                    crate::handlers_chat::queue_clear(&resp_tx, &mut followup_queue, thread_id)
                        .await;
                }
                AgentRequest::QueueReorder {
                    thread_id,
                    input_id,
                    delta,
                } => {
                    crate::handlers_chat::queue_reorder(
                        &resp_tx,
                        &mut followup_queue,
                        thread_id,
                        input_id,
                        delta,
                    )
                    .await;
                }
                AgentRequest::QueuePaused { thread_id, paused } => {
                    crate::handlers_chat::queue_paused(
                        &resp_tx,
                        &mut followup_queue,
                        &followup_wake_tx,
                        thread_id,
                        paused,
                    )
                    .await;
                }
                AgentRequest::QueueSnapshot { thread_id } => {
                    // A read, not a change: the snapshot is the same one every
                    // mutation emits, so the client's projection is rebuilt from
                    // the driver's record (attaching to a live session would
                    // otherwise start empty while items sit queued).
                    crate::handlers_chat::emit_queue_snapshot(
                        &resp_tx,
                        &followup_queue,
                        &thread_id,
                    );
                }
                AgentRequest::QueryInputHistory => {
                    crate::handlers_history::query_input_history(&resp_tx);
                }
                AgentRequest::RecordInputHistory { entries, dedup } => {
                    crate::handlers_history::record_input_history(entries, dedup);
                }
                AgentRequest::DeleteInputHistoryEntry {
                    text,
                    created_at_ms,
                } => {
                    crate::handlers_history::delete_input_history_entry(&text, created_at_ms);
                }
                AgentRequest::QueryRouteSettings { provider_id, model } => {
                    crate::handlers_history::query_route_settings(&provider_id, &model, &resp_tx);
                }
                AgentRequest::SearchHistory {
                    query,
                    workspace,
                    limit,
                } => {
                    crate::handlers_history::search_history(
                        &query,
                        workspace.as_deref(),
                        limit,
                        &resp_tx,
                    )
                    .await;
                }
                AgentRequest::UpdateTuiLayout(layout) => {
                    let _ = resp_tx.send(AgentResponse::TuiLayoutUpdated(layout));
                }
                AgentRequest::UpdateTuiColorScheme { name, custom } => {
                    let _ = resp_tx.send(AgentResponse::TuiColorSchemeUpdated { name, custom });
                }
                AgentRequest::QueryWebSearchConfig => {
                    crate::handlers_websearch::query(&config, &websearch_shared, &resp_tx);
                }
                AgentRequest::UpdateWebSearchConfig(update) => {
                    crate::handlers_websearch::update(
                        &mut config,
                        &websearch_shared,
                        *update,
                        &resp_tx,
                    )
                    .await;
                }
                AgentRequest::EndThread => {
                    // Unreachable in the normal topology: the WS attach path
                    // intercepts `EndThread` at the connection layer
                    // (serve.rs) precisely so it cannot queue behind work
                    // the teardown is about to cancel. This arm exists only
                    // for completeness / future in-process embedders.
                    tracing::warn!(
                        "session_driver: EndThread reached the driver queue; the \
                         connection layer should have intercepted it"
                    );
                }
            }

            // Activity-state reconcile
            // Round-owned requests resolve themselves via the round task's
            // terminal `HarnessState(Idle)`. Control-plane requests must be
            // resolved here instead: re-publish the authoritative harness
            // state now that the handler has run. When a round is live the
            // reconcile is a no-op (the round's own events own the display —
            // and re-emitting a running snapshot would reset the TUI's round
            // timer/turn counters); when idle it is `HarnessState(Idle)`,
            // which the TUI maps to "collapse the activity bar". This keeps
            // "every dispatched request lands the harness back in its
            // authoritative state" a structural invariant regardless of what
            // a frontend optimistically painted (the TUI itself no longer
            // arms anything for control-plane dispatches — — so for
            // it this reconcile is a no-op safety net).
            if reconcile_activity {
                send_harness_state_for_session(
                    &resp_tx,
                    &session.id().await,
                    &agent,
                    &session,
                    LoopStatus::Idle,
                )
                .await;
            }

            // Re-publish a session-scoped projection only when the AI-visible
            // context actually changed this request (session switch, `/new`,
            // `/compact`, provider/tool/skill change, …). Control-plane commands
            // (like `/delegate`, `/help`, UI queries) never compute token estimates,
            // keeping the driver event loop zero-stall.
            let post_session_id = session.id().await;
            let provider_or_model_changed =
                pre_provider != agent.provider.provider_id() || pre_model != agent.provider.model();
            let session_changed = post_session_id != pre_session_id;

            if session_changed {
                token_ledger
                    .restore_session(&post_session_id, session.request_usage_records().await);
                token_ledger.set_active_session(post_session_id.clone());
                agent.set_thread_id(post_session_id.clone());
                agent.restore_round_count(session.round_counter().await);
                // D4: a switched-to session may itself carry crash
                // residue; classify it durably now that its store is live.
                if let Err(error) = session.settle_abandoned_attempts().await {
                    tracing::warn!(%error, "could not settle abandoned request attempts");
                }
            }

            if session_changed || provider_or_model_changed || may_mutate_context {
                let post_projection = agent
                    .estimate_next_request_tokens(&session.model_window().await)
                    .total_tokens;
                let _ = resp_tx.send(round_response(
                    &post_session_id,
                    nuo_protocol::RoundEvent::ContextTokens(
                        nuo_protocol::ContextTokenSnapshot::new(
                            post_projection,
                            nuo_protocol::ContextTokenSource::Projection,
                        ),
                    ),
                ));
            }
        }
        if let Some(task) = active_oauth_task.take() {
            task.abort();
        }
        if let Some(task) = active_catalog_sync_task.take() {
            task.abort();
        }
    }
}

/// Whether `req` may mutate the conversation context window or loaded capabilities.

#[cfg(test)]
mod tests;
