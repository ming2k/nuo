//! The driver for one live agent session.
//!
//! [`SessionDriver`] owns the request receiver and every long-lived dependency
//! needed to serve that session: the `Agent`, session store, configuration,
//! provider telemetry, `/btw` side session, cancellation state, and frontend
//! bridge. [`SessionDriver::run`] consumes the driver, dispatches each
//! [`AgentRequest`] to its handler, and exits when all request senders are
//! dropped.
//!
//! The implementation still destructures the driver into the local names used
//! by the original inline task. This keeps the dispatch body unchanged while
//! making its ownership boundary explicit.

use crate::handlers_slash::SlashEnv;
use crate::side::{SideEnv, resolve_turn_target};
use crate::catalog;
use nuo_harness::orchestration::round_response;
use nuo_harness::{Agent, RoundLifecycle, SubagentRegistry};
use nuo_wire::{AgentRequest, AgentResponse, LoopStatus, Provider, Tool};
use crate::mcp::McpRuntime;
use nuo_persistence::{session::SessionStore, workspace_security::WorkspaceSecurityStore};
use nuo_harness::skills::SkillRegistry;

pub async fn send_harness_state_for_session(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    session_id: &str,
    agent: &Agent,
    session: &SessionStore,
    loop_status: LoopStatus,
) {
    let round_counter = agent
        .round_count()
        .saturating_add(u64::from(!loop_status.is_idle()));
    let retry_pending = loop_status.is_idle() && session.retry_pending().await.is_some();
    let _ = tx.send(round_response(
        session_id,
        nuo_wire::RoundEvent::HarnessState(nuo_wire::HarnessSnapshot {
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

pub async fn compact_round_history(
    history: &mut Vec<nuo_wire::Message>,
    session: &SessionStore,
    settings: &nuo_harness::orchestration::ContextProjectionSettings,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<nuo_wire::ContextProjectionCheckpoint>, String> {
    let mut ir = session.session_ir().await;
    let preserve_rounds = settings.preserve_rounds.max(1);
    let mode = nuo_harness::compaction::CompactionCutMode::PreserveTailRounds(preserve_rounds);
    let provider = if settings.summarize { provider } else { None };
    if let Some(outcome) = nuo_harness::compaction::CausalCompactor::compact_session_ir(
        &mut ir,
        provider,
        mode,
        extra_context,
    )
    .await?
    {
        session.commit_session_ir(&ir).await?;
        let active_msgs = ir.resolve_active_messages();
        let tokens_after = nuo_wire::pressure::estimate_tokens(&active_msgs);
        let checkpoint = nuo_wire::ContextProjectionCheckpoint {
            operation: nuo_wire::ContextProjectionKind::Compact,
            archived_messages: outcome.nodes_folded,
            active_messages: active_msgs.len(),
            window_tokens_before: outcome.tokens_before,
            window_tokens_after: tokens_after,
            summary: Some(outcome.summary),
            tracked_files: outcome.tracked_files,
        };
        *history = active_msgs;
        Ok(Some(checkpoint))
    } else {
        Ok(None)
    }
}

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::UiBridge;
use crate::session_view::{build_sessions_overview, provider_key_status};
use crate::startup::SessionStart;

/// The owned state and request loop for one live session.
///
/// A frontend currently assembles the driver after startup wiring and moves it
/// into a Tokio task. (The ADR-0037 §6 `SessionRegistry` factory was removed
/// as dormant; if the server move resumes, the fields can become private
/// without changing the driver model.)
#[allow(clippy::type_complexity)]
/// The driver-owned follow-up queue authority (ADR-0197 M4).
///
/// One queue per hosted session driver, keyed by target session (the
/// primary *and* live asides — each item carries its target's id, and the
/// busy check is against the *target's* own round lifecycle). The frontend
/// sends `AgentRequest::FollowUp` and renders the `QueueUpdated` snapshots;
/// it never decides when an item ships.
pub(crate) struct FollowUpQueue {
    items: Vec<QueuedFollowUp>,
    paused: std::collections::HashSet<String>,
}

pub(crate) struct QueuedFollowUp {
    pub session_id: String,
    pub message: nuo_wire::QueuedMessage,
}

impl FollowUpQueue {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            paused: std::collections::HashSet::new(),
        }
    }

    pub(crate) fn is_paused(&self, session_id: &str) -> bool {
        self.paused.contains(session_id)
    }

    pub(crate) fn set_paused(&mut self, session_id: &str, paused: bool) {
        if paused {
            self.paused.insert(session_id.to_string());
        } else {
            self.paused.remove(session_id);
        }
    }

    pub(crate) fn enqueue(&mut self, session_id: &str, message: nuo_wire::QueuedMessage) {
        self.items.push(QueuedFollowUp {
            session_id: session_id.to_string(),
            message,
        });
    }

    pub(crate) fn remove(&mut self, session_id: &str, input_id: &str) {
        self.items
            .retain(|item| !(item.session_id == session_id && item.message.id == input_id));
    }

    pub(crate) fn clear(&mut self, session_id: &str) {
        self.items.retain(|item| item.session_id != session_id);
    }

    /// Reorder one item within its session's queue by `delta` positions
    /// (queue modal `K`/`J`), clamped at the session's slice boundaries.
    /// Unknown ids are a no-op.
    pub(crate) fn reorder(&mut self, session_id: &str, input_id: &str, delta: i32) {
        let positions: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.session_id == session_id)
            .map(|(position, _)| position)
            .collect();
        let Some(local) = positions
            .iter()
            .position(|&position| self.items[position].message.id == input_id)
        else {
            return;
        };
        let target_local =
            (local as i64 + delta as i64).clamp(0, positions.len() as i64 - 1) as usize;
        if target_local == local {
            return;
        }
        let from = positions[local];
        let to = positions[target_local];
        let item = self.items.remove(from);
        self.items.insert(to, item);
    }

    /// The authoritative snapshot for one session.
    pub(crate) fn snapshot(&self, session_id: &str) -> (Vec<nuo_wire::QueuedMessage>, bool) {
        let items = self
            .items
            .iter()
            .filter(|item| item.session_id == session_id)
            .map(|item| item.message.clone())
            .collect();
        (items, self.paused.contains(session_id))
    }

    /// Dequeue and return the first shippable item for `session_id`, if any.
    pub(crate) fn dequeue_front(
        &mut self,
        session_id: &str,
    ) -> Option<nuo_wire::QueuedMessage> {
        let position = self
            .items
            .iter()
            .position(|item| item.session_id == session_id)?;
        let item = self.items.remove(position);
        Some(item.message)
    }

    pub(crate) fn has_items_for(&self, session_id: &str) -> bool {
        self.items.iter().any(|item| item.session_id == session_id)
    }
}

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
    /// Authoritative runtime configuration shared across all hosted sessions (ADR-0209).
    pub config: crate::SharedConfig,
    /// Authoritative connection usage shared across all hosted sessions (ADR-0209).
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
    pub shared_additional_roots: nuo_wire::SharedAdditionalRoots,
    /// Live handle for toggling session-level workspace confinement.
    pub shared_confinement: nuo_wire::SharedConfinement,
    /// Backend-owned command vocabulary used by both attach metadata and the
    /// composer completion engine.
    pub command_catalog: nuo_wire::CommandCatalog,
    /// Primary round lifecycle: at most one active round, superseded by the
    /// next begin (replaces the old token-slot + generation-counter pair).
    pub lifecycle: Arc<RoundLifecycle>,
    /// Live `/btw` aside registry (ADR-0017, multi-slot per ADR-0103).
    pub side: Arc<AsyncRwLock<crate::side::SideRegistry>>,
    /// Cached base toolset snapshot for side-session construction
    /// (`base_tools_for_side`).
    pub base_tools: Arc<Vec<Arc<dyn Tool>>>,
    /// Workspace root for side-session pinning; `None` for a workspace-free
    /// scope (ADR-0220).
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
    pub token_ledger: Arc<nuo_wire::TokenSourceLedger>,
    /// Application-registered slash command handlers (the extension point for
    /// commands that run Rust logic, e.g. a sibling binary's custom command).
    /// The dispatcher consults this in its unknown-built-in arm
    /// before falling back to the markdown-template path. Empty for `nuo`
    /// today; populated by embeddings that need it.
    pub extra_commands: Arc<crate::slash_handler::SlashCommandRegistry>,
    /// Shared hot-reloadable resolved `[web]` configuration. The web tools hold
    /// the same handle; `UpdateWebSearchConfig` replaces one versioned snapshot
    /// so changes take effect on the next call without rebuilding the toolset.
    pub websearch_shared: nuo_wire::SharedWebConfig,
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
            side,
            base_tools: base_tools_for_side,
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
        // Crash-residue recovery (ADR-0128). The round path arms the durable
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
            // ADR-0236 D4: the residue read above is the store's last word on
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
            nuo_wire::RoundEvent::ContextTokens(nuo_wire::ContextTokenSnapshot::new(
                initial_context,
                nuo_wire::ContextTokenSource::Projection,
            )),
        ));
        // Session-scoped idle snapshot (ADR-0128): publishes the `/retry`
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
                let initial_id = catalog::default_provider_id(&config).to_string();
                // Resolve the model the way `build_provider_for` did when main.rs
                // constructed the startup provider: `config.default_model` when the
                // entry serves it, otherwise the entry's default-channel model. The
                // config-only resolver (`resolved_model_name`, *not* the `_with_usage`
                // variant) mirrors that precedence exactly — it ignores `last_models`,
                // so it never pins a model the live provider was not actually built
                // with. Pinning the exact live model (rather than a usage-derived one)
                // is what lets the next launch re-open this provider on the same model.
                let initial_model = catalog::resolved_model_name(&config, &initial_id);
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
            catalog::prune_stale_models(&mut config, &mut provider_usage);
            // Push the initial model-picker snapshot (default id + per-model
            // favorite / key-ready / last-used) so the picker is ready the moment
            // the user opens it.
            let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
                &config,
                &provider_usage,
            )));
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
                let provider = catalog::default_provider_id(&config).to_string();
                let model = catalog::resolved_model_name(&config, &provider).unwrap_or_default();
                let _ = resp_tx.send(AgentResponse::ProviderSwitched { provider, model });
            }
        }
        if open_picker_on_start {
            let _ = resp_tx.send(AgentResponse::SessionsOverview(
                build_sessions_overview(&session).await,
            ));
        }
        enum OAuthResult {
            Authorize {
                auth: nuo_wire::ConnectionAuth,
                provider: String,
                tokens: Option<nuo_provider_adapters::oauth::TokenSet>,
            },
            Connect {
                provider_id: String,
                success: bool,
            },
        }
        struct CatalogSyncTaskResult {
            outcome: catalog::CatalogSyncOutcome,
            session_id: Option<String>,
        }
        let mut active_oauth_task: Option<tokio::task::JoinHandle<OAuthResult>> = None;
        let mut active_catalog_sync_task: Option<tokio::task::JoinHandle<CatalogSyncTaskResult>> =
            None;
        // ADR-0227: the sync streams one update per connection as it completes.
        // This sender stays alive for the driver's lifetime, so `recv()` yields
        // updates without ever observing a closed channel.
        let (catalog_sync_tx, mut catalog_sync_rx) =
            mpsc::unbounded_channel::<catalog::ConnectionUpdate>();
        let mut pending_oauth_authorization: Option<
            crate::handlers_provider::PendingOAuthAuthorization,
        > = None;
        // ADR-0197 M4: the driver owns the follow-up queue authority. The
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
                                        nuo_wire::ConnectStatus::Done { provider },
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
                    // Round-boundary wake (ADR-0197 M4): a target round ended.
                    // A round the operator interrupted parks its queue — the
                    // user said stop; auto-firing more prompts after an Esc is
                    // never the intent. Ctrl+P / a fresh send re-arms it.
                    if let Some(target) =
                        resolve_turn_target(&side, &agent, &session, &lifecycle, &wake_target).await
                        && target.lifecycle.was_interrupted()
                    {
                        followup_queue.set_paused(&wake_target, true);
                        crate::handlers_chat::emit_queue_snapshot(
                            &resp_tx,
                            &followup_queue,
                            &wake_target,
                        );
                    }
                    // Ship the next queued item for any idle, un-paused target
                    // the queue knows about (in queue order).
                    let targets: Vec<String> = {
                        let mut seen = Vec::new();
                        // order-preserving distinct scan of the queue's targets
                        for item in &followup_queue.items {
                            if !seen.contains(&item.session_id) {
                                seen.push(item.session_id.clone());
                            }
                        }
                        seen
                    };
                    for target_id in targets {
                        let Some(_item) = (followup_queue.has_items_for(&target_id)).then_some(())
                        else {
                            continue;
                        };
                        let Some(target) =
                            resolve_turn_target(&side, &agent, &session, &lifecycle, &target_id)
                                .await
                        else {
                            // Target gone (aside closed): drop its items.
                            followup_queue.clear(&target_id);
                            crate::handlers_chat::emit_queue_snapshot(
                                &resp_tx,
                                &followup_queue,
                                &target_id,
                            );
                            continue;
                        };
                        if target.lifecycle.is_running().await
                            || followup_queue.is_paused(&target_id)
                        {
                            continue;
                        }
                        if let Some(message) = followup_queue.dequeue_front(&target_id) {
                            let config = shared_config.read().await;
                            crate::handlers_chat::start_queued_follow_up(
                                SideEnv {
                                    side: &side,
                                    agent: &agent,
                                    primary_session: &session,
                                    primary_lifecycle: &lifecycle,
                                    tx: &resp_tx,
                                    config: &config,
                                },
                                target_id.clone(),
                                message,
                                &mut followup_queue,
                            )
                            .await;
                            // Watch this round's boundary so the *next* queued
                            // item ships when it ends.
                            if let Some(target) =
                                resolve_turn_target(&side, &agent, &session, &lifecycle, &target_id)
                                    .await
                            {
                                crate::handlers_chat::spawn_boundary_watcher(
                                    target.lifecycle.clone(),
                                    followup_wake_tx.clone(),
                                    target_id.clone(),
                                );
                            }
                        }
                    }
                    continue;
                }
            };
            let pre_session_id = session.id().await;
            let pre_provider = agent.provider.provider_id();
            let pre_model = agent.provider.model();
            // Requests that own the round lifecycle close their own activity
            // resolution: the round task (or the shell-command round) always
            // emits a terminal `HarnessState(Idle)` on exit. Every other
            // request is a control-plane op that does not. The TUI no longer
            // arms optimistic activity state for control-plane dispatches at
            // all (ADR-0110: a command is outside the round state machine),
            // but other frontends may still paint their own optimistic
            // state, so the driver keeps reconciling every non-round request
            // back to the authoritative harness state — see the reconcile
            // below the match and ADR-0091/0110.
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
                    // Scene-scoped interrupt (ADR-0205): stop only the viewed
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
                        &side,
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
                            side: &side,
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
                            side: &side,
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
                    client_profile,
                    user_agent,
                    catalog_format,
                    dialect,
                } => {
                    if let Err(e) = crate::handlers_provider::register_provider(
                        id,
                        label,
                        root_url,
                        protocol,
                        client_profile,
                        user_agent,
                        catalog_format,
                        dialect,
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
                    client_identity,
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
                            client_identity,
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
                    let provider = auth.oauth_provider_id().unwrap_or("oauth").to_string();
                    active_oauth_task = Some(tokio::spawn(async move {
                        let tokens =
                            crate::handlers_provider::authorize(&resp_tx_clone, method, auth.clone()).await;
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
                        nuo_wire::ConnectStatus::Failed {
                            provider: "oauth".to_string(),
                            message: "cancelled".to_string(),
                        },
                    ));
                }
                AgentRequest::EditConnection {
                    name,
                    provider,
                    api_key,
                    client_identity,
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
                        client_identity,
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
                        let outcome = catalog::sync_remote_catalog_streaming(sink).await;
                        CatalogSyncTaskResult {
                            outcome,
                            session_id,
                        }
                    }));
                }
                AgentRequest::DeleteSession { id } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::delete(&session, &resp_tx, id).await;
                    });
                }
                AgentRequest::RenameSession { id, title } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::rename(&session, &resp_tx, id, title).await;
                    });
                }
                AgentRequest::QuerySessionDetail { id } => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::detail(&session, &resp_tx, id).await;
                    });
                }
                AgentRequest::QueryConnectionDetail { id } => {
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_provider::query_connection_detail(&resp_tx, id).await;
                    });
                }
                AgentRequest::QuerySessionsOverview => {
                    let session = session.clone();
                    let resp_tx = resp_tx.clone();
                    tokio::spawn(async move {
                        crate::handlers_session::overview(&session, &resp_tx).await;
                    });
                }
                AgentRequest::QuerySessionTree => {
                    // Keep id + tree capture ordered with session-switch
                    // requests handled by this driver, so the tagged snapshot
                    // can never pair one session's id with another's DAG.
                    crate::handlers_session::tree(&session, &resp_tx).await;
                }
                AgentRequest::QueryTokenUsage { session_id } => {
                    crate::handlers_session::token_usage(&token_ledger, &resp_tx, session_id);
                }
                AgentRequest::QueryUsageStats { event_cap } => {
                    crate::handlers_session::usage_stats(&resp_tx, event_cap).await;
                }
                AgentRequest::QuerySessionContext => {
                    crate::handlers_session::query_context(
                        &agent,
                        &skills_registry,
                        &mcp_runtime,
                        &config,
                        &resp_tx,
                    );
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
                            side: &side,
                            base_tools_for_side: &base_tools_for_side,
                            provider_for_task: &provider_for_task,
                            provider_usage: &mut provider_usage,
                            skills_registry: &skills_registry,
                            req_tx_for_commands: &req_tx_for_commands,
                            project_root_for_side: project_root_for_side.as_deref(),
                            startup: &startup,
                            ui: &*ui,
                            extra_commands: &extra_commands,
                            websearch_shared: &websearch_shared,
                            background_jobs: &background_jobs,
                        },
                    )
                    .await;
                }
                AgentRequest::TrustWorkspace { domains } => {
                    let user_assets_state =
                        crate::handlers_slash::security_ops::compute_user_assets_trust();
                    let user_assets_needed_review = matches!(
                        user_assets_state,
                        nuo_wire::WorkspaceTrustState::Quarantined
                            | nuo_wire::WorkspaceTrustState::Changed
                            | nuo_wire::WorkspaceTrustState::Expired
                    );
                    if domains.contains(&nuo_wire::TrustDomain::UserAssets) {
                        crate::handlers_slash::security_ops::trust_user_assets();
                    } else if user_assets_needed_review {
                        crate::handlers_slash::security_ops::deny_user_assets();
                    }
                    let effective = if let Some(root) = &project_root_for_side {
                        let ws_domains: Vec<nuo_wire::TrustDomain> = domains
                            .iter()
                            .copied()
                            .filter(|d| *d != nuo_wire::TrustDomain::UserAssets)
                            .collect();

                        // Candidate workspace domains D: domains requiring human review (ADR-0253)
                        let pre_snap = workspace_security.snapshot(root);
                        let candidate_ws: Vec<nuo_wire::TrustDomain> = [
                            nuo_wire::TrustDomain::Mcp,
                            nuo_wire::TrustDomain::Skills,
                            nuo_wire::TrustDomain::Hooks,
                            nuo_wire::TrustDomain::Instructions,
                            nuo_wire::TrustDomain::ExWorkspace,
                        ]
                        .into_iter()
                        .filter(|&d| {
                            matches!(
                                pre_snap.state(d),
                                nuo_wire::WorkspaceTrustState::Quarantined
                                    | nuo_wire::WorkspaceTrustState::Changed
                                    | nuo_wire::WorkspaceTrustState::Expired
                            )
                        })
                        .collect();

                        if !ws_domains.is_empty()
                            && let Err(error) = workspace_security.trust_domains(root, &ws_domains)
                        {
                            tracing::error!(?error, "failed to persist workspace trust");
                            let _ = resp_tx.send(round_response(
                                &session.id().await,
                                nuo_wire::RoundEvent::Notice(
                                    nuo_wire::AgentNotice::trust_changed(format!(
                                        "Workspace trust failed: {error}"
                                    )),
                                ),
                            ));
                        }

                        // Explicit negative attestation on unselected candidate domains: D \ S (ADR-0253)
                        let unselected_ws: Vec<nuo_wire::TrustDomain> = candidate_ws
                            .into_iter()
                            .filter(|d| !ws_domains.contains(d))
                            .collect();

                        if !unselected_ws.is_empty() {
                            let _ = workspace_security.deny_domains(root, &unselected_ws);
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
                        agent
                            .set_hooks(crate::hooks::build_hook_registry(&effective.hooks, &agent));
                        effective
                    } else {
                        // Workspace-free session: user-level assets only.
                        let mut snapshot =
                            nuo_wire::WorkspaceSecuritySnapshot::new("workspace-free");
                        snapshot.user_assets =
                            crate::handlers_slash::security_ops::compute_user_assets_trust();
                        agent.set_workspace_security(snapshot);
                        nuo_persistence::config::Config::load()
                    };

                    // Immediately broadcast Trusted HarnessState so the client unblocks instantly
                    send_harness_state_for_session(
                        &resp_tx,
                        &session.id().await,
                        &agent,
                        &session,
                        LoopStatus::Idle,
                    )
                    .await;

                    // Asynchronously background heavy reconfigure/reload (MCP connections & skills scanner)
                    let mcp_clone = mcp_runtime.clone();
                    let skills_clone = skills_registry.clone();
                    tokio::spawn(async move {
                        let _ = mcp_clone.reconfigure(effective.mcp).await;
                        skills_clone.reload().await;
                    });
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
                        SideEnv {
                            side: &side,
                            agent: &agent,
                            primary_session: &session,
                            primary_lifecycle: &lifecycle,
                            tx: &resp_tx,
                            config: &config,
                        },
                        text,
                        images,
                        sent_at_ms,
                    )
                    .await;
                }
                AgentRequest::Steer {
                    session_id,
                    message,
                } => {
                    crate::handlers_chat::steer(
                        &side, &agent, &session, &resp_tx, session_id, message,
                    )
                    .await;
                }
                AgentRequest::CancelSteer {
                    session_id,
                    input_id,
                } => {
                    crate::handlers_chat::cancel_steer(
                        &side, &agent, &session, &resp_tx, session_id, input_id,
                    )
                    .await;
                }
                AgentRequest::FollowUp {
                    session_id,
                    message,
                } => {
                    crate::handlers_chat::follow_up(
                        SideEnv {
                            side: &side,
                            agent: &agent,
                            primary_session: &session,
                            primary_lifecycle: &lifecycle,
                            tx: &resp_tx,
                            config: &config,
                        },
                        &mut followup_queue,
                        &followup_wake_tx,
                        session_id,
                        message,
                    )
                    .await;
                }
                AgentRequest::QueueRemove {
                    session_id,
                    input_id,
                } => {
                    crate::handlers_chat::queue_remove(
                        SideEnv {
                            tx: &resp_tx,
                            side: &side,
                            agent: &agent,
                            primary_session: &session,
                            primary_lifecycle: &lifecycle,
                            config: &config,
                        },
                        &mut followup_queue,
                        session_id,
                        input_id,
                    )
                    .await;
                }
                AgentRequest::QueueClear { session_id } => {
                    crate::handlers_chat::queue_clear(
                        SideEnv {
                            tx: &resp_tx,
                            side: &side,
                            agent: &agent,
                            primary_session: &session,
                            primary_lifecycle: &lifecycle,
                            config: &config,
                        },
                        &mut followup_queue,
                        session_id,
                    )
                    .await;
                }
                AgentRequest::QueueReorder {
                    session_id,
                    input_id,
                    delta,
                } => {
                    crate::handlers_chat::queue_reorder(
                        SideEnv {
                            tx: &resp_tx,
                            side: &side,
                            agent: &agent,
                            primary_session: &session,
                            primary_lifecycle: &lifecycle,
                            config: &config,
                        },
                        &mut followup_queue,
                        session_id,
                        input_id,
                        delta,
                    )
                    .await;
                }
                AgentRequest::QueuePaused { session_id, paused } => {
                    crate::handlers_chat::queue_paused(
                        SideEnv {
                            tx: &resp_tx,
                            side: &side,
                            agent: &agent,
                            primary_session: &session,
                            primary_lifecycle: &lifecycle,
                            config: &config,
                        },
                        &mut followup_queue,
                        session_id,
                        paused,
                    )
                    .await;
                }
                AgentRequest::ExitSideView => {
                    crate::handlers_session::detach_side_view(&side, &resp_tx).await;
                }
                AgentRequest::FocusSide { side_id } => {
                    crate::handlers_session::focus_side(&side, &session, &resp_tx, side_id).await;
                }
                AgentRequest::InterruptSide { side_id } => {
                    crate::handlers_session::interrupt_side(&side, &resp_tx, side_id).await;
                }
                AgentRequest::CloseSide { side_id } => {
                    crate::handlers_session::close_side(&side, &resp_tx, side_id).await;
                }
                AgentRequest::QueryBtwList => {
                    crate::side::publish_btw_list(&side, &resp_tx).await;
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
                    );
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
                AgentRequest::EndSession => {
                    // Unreachable in the normal topology: the WS attach path
                    // intercepts `EndSession` at the connection layer
                    // (serve.rs) precisely so it cannot queue behind work
                    // the teardown is about to cancel. This arm exists only
                    // for completeness / future in-process embedders.
                    tracing::warn!(
                        "session_driver: EndSession reached the driver queue; the \
                         connection layer should have intercepted it"
                    );
                }
            }

            // Activity-state reconcile (ADR-0091)
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
            // arms anything for control-plane dispatches — ADR-0110 — so for
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
                // ADR-0236 D4: a switched-to session may itself carry crash
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
                    nuo_wire::RoundEvent::ContextTokens(
                        nuo_wire::ContextTokenSnapshot::new(
                            post_projection,
                            nuo_wire::ContextTokenSource::Projection,
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
fn request_may_mutate_context(req: &AgentRequest) -> bool {
    match req {
        AgentRequest::SlashCommand(cmd) => {
            let name = cmd.split_whitespace().next().unwrap_or("");
            matches!(
                name,
                "/compact"
                    | "/new"
                    | "/clear"
                    | "/resume"
                    | "/session"
                    | "/fork"
                    | "/retry"
                    | "/unsend"
                    | "/undo"
                    | "/skills"
                    | "/tools"
                    | "/mcp"
            )
        }
        _ => false,
    }
}

/// Whether `req` owns the round lifecycle and therefore resolves the TUI's
/// optimistic "queued" activity state on its own, via the round task's
/// terminal `HarnessState(Idle)`.
///
/// - Chat-family requests start (or feed) a round; the round task emits the
///   closing idle snapshot when it finishes, errors, or is interrupted.
///
/// Everything else is a control-plane operation (slash command, provider/
/// session/tool/mcp toggle, query, layout update, …) that runs inline in the
/// driver loop and emits no lifecycle event of its own. The driver
/// reconciles those after dispatch (see [`SessionDriver::run`]) by
/// re-publishing the authoritative harness state.
fn round_owned_request(req: &AgentRequest) -> bool {
    matches!(
        req,
        AgentRequest::Prompt { .. }
            | AgentRequest::FollowUp { .. }
            | AgentRequest::Steer { .. }
            | AgentRequest::CancelSteer { .. }
    )
}

/// Whether the driver must reconcile the TUI's optimistic activity state after
/// dispatching `req`: true for every control-plane (non-round) request when no
/// round is live. When a round is running the reconcile is deliberately a
/// no-op — the round's own events own the display, and re-emitting a running
/// snapshot would reset the TUI's round timer/turn counters (ADR-0091).
async fn needs_activity_reconcile(req: &AgentRequest, lifecycle: &RoundLifecycle) -> bool {
    !matches!(req, AgentRequest::CompleteComposer { .. })
        && !round_owned_request(req)
        && !lifecycle.is_running().await
}

/// The durable residue of one round the host process abandoned mid-flight.
#[derive(Debug, Default)]
struct CrashResidue {
    /// `Terminated` interrupt records to append (C11), one per distinct
    /// in-flight round, so the resumed transcript explains its dangling
    /// round instead of leaving it unexplained.
    interrupts: Vec<nuo_wire::RoundInterrupt>,
    /// A `/retry` resume point for the highest in-flight round, so a session
    /// re-hosted after a crash offers `/retry` instead of answering
    /// "Nothing to retry" (ADR-0128).
    retry_point: Option<nuo_wire::RetryPoint>,
}

/// Decide, from durable state alone, what a hard process death left dangling
/// (ADR-0128 + C11).
///
/// The crash signal is a request-usage record still `InFlight` **in the
/// session store**: every live settlement path (`RequestAccountingGuard`'s
/// Drop on completion/interrupt/failure) rewrites a terminal status through
/// `set_request_usage_records` before the round ends, and a graceful daemon
/// kill records a `Terminated` interrupt instead. A store-side `InFlight`
/// record therefore means the process vanished with the request on the wire.
///
/// Guards:
/// - Only the *highest* in-flight round is considered. The handler and
///   `start_resolved_turn` reject a point whose `round` no longer equals the
///   session's counter, so a lower one could never fire anyway.
/// - The point names only the *root* actor's round. Subagent (`task`)
///   agents bill their own requests under `subagent:<call-id>` against the same
///   session; a child's key must not decide the root agent's resume point.
/// - `turns_committed` is recovered from the transcript itself: the round's
///   committed turns are the assistant messages after its opening prompt
///   (the last visible, non-echo user message — the transcript carries no
///   round delimiters), because the in-flight turn was never committed.
/// - No point unless the round is the session's *current* one — the counter
///   below the record means the session moved past it (nothing to resurrect),
///   above it means the counter's durable write never landed.
///
/// A pre-existing terminal interrupt for the round does **not** suppress the
/// point. The two are orthogonal: the record explains the transcript, the
/// point offers recovery — and every stop a *graceful* path can observe
/// (interrupt, failure, completion) settles the usage record to a terminal
/// status, leaving store-side `InFlight` exclusively to process death. A
/// graceful kill therefore leaves the same residue as a crash, and its round
/// is just as resumable; suppressing on the interrupt would also break a
/// crash during a `/retry` resume, which reuses the same round number.
///
/// Performs only read access on the store; the caller applies the
/// interrupts / resume point through the store's normal durable setters.
async fn recover_crashed_round(
    session: &Arc<SessionStore>,
    records: Vec<nuo_wire::RequestUsageRecord>,
    now_ms: u64,
) -> CrashResidue {
    use nuo_wire::{RequestUsageStatus, Role};
    let mut residue = CrashResidue::default();
    let Some(latest) = records
        .iter()
        .filter(|record| record.status == RequestUsageStatus::InFlight)
        .max_by_key(|record| record.key.round)
    else {
        return residue;
    };
    let round = latest.key.round;
    residue.interrupts.push(nuo_wire::RoundInterrupt {
        reason: nuo_wire::RoundInterruptReason::Terminated,
        at_ms: now_ms,
        round: Some(round),
        detail: None,
    });
    // Counter guard: the point may only name the session's *current* round —
    // the handler (and `start_resolved_turn`) reject anything else, and a
    // round below the counter means the session already moved past it (a
    // resume-then-more-work history), while a round above it means the
    // counter's durable write never landed.
    let round_counter = session.round_counter().await;
    if round != round_counter {
        return residue;
    }
    let window = session.model_window().await;
    // Committed ReAct turns of the crashed round. The transcript carries no
    // round delimiters, so the round's opener is approximated as the last
    // visible, non-echo user message: every turn this round committed follows
    // its opening prompt, and the in-flight one was never committed (its
    // partial stream died with the process). A hidden round input opens its
    // round the same way, while a command echo (`/cmd`, `!cmd`) is
    // non-driving and must not be taken for an opener. Mid-round
    // `InsertUserInput` admissions make this an undercount (the ordinal
    // resumes lower than reality) — cosmetic: the number only labels the
    // transcript band and usage keys, history itself is seeded from the exact
    // `history_watermark`.
    let opener = window
        .iter()
        .rposition(|message| {
            matches!(message.role, Role::User) && !message.hidden && !message.is_command_echo()
        })
        .map(|index| index + 1)
        .unwrap_or(0);
    let turns_committed = window[opener..]
        .iter()
        .filter(|message| matches!(message.role, Role::Assistant))
        .count();
    residue.retry_point = Some(nuo_wire::RetryPoint {
        round,
        turns_committed,
        history_watermark: window.len(),
        paused_ms: 0,
        at_ms: now_ms,
    });
    residue
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_harness::RoundLifecycle;
    use nuo_wire::{Message, RequestUsageStatus, Role};
    use std::sync::Arc;

    fn image() -> nuo_wire::ImagePart {
        nuo_wire::ImagePart {
            mime: "image/png".to_string(),
            data: "AAAA".to_string(),
        }
    }

    #[test]
    fn round_owned_requests_close_their_own_activity_lifecycle() {
        assert!(round_owned_request(&AgentRequest::Prompt {
            text: "hi".to_string(),
            images: vec![image()],
            sent_at_ms: Some(1),
        }));
        assert!(round_owned_request(&AgentRequest::FollowUp {
            session_id: "s".to_string(),
            message: nuo_wire::QueuedMessage {
                id: "i".to_string(),
                text: "hi".to_string(),
                display_text: None,
                images: Vec::new(),
                sent_at_ms: None,
            },
        }));
        assert!(round_owned_request(&AgentRequest::Steer {
            session_id: "s".to_string(),
            message: nuo_wire::QueuedMessage {
                id: "i".to_string(),
                text: "hi".to_string(),
                display_text: None,
                images: Vec::new(),
                sent_at_ms: None,
            },
        }));
        assert!(round_owned_request(&AgentRequest::CancelSteer {
            session_id: "s".to_string(),
            input_id: "i".to_string(),
        }));
    }

    #[test]
    fn control_plane_requests_need_the_driver_reconcile() {
        // The TUI optimistically paints "queued" for these; none of them emit
        // a terminal lifecycle event of their own, so the driver must.
        assert!(!round_owned_request(&AgentRequest::SlashCommand(
            "/delegate on".to_string()
        )));
        assert!(!round_owned_request(&AgentRequest::TrustWorkspace {
            domains: Vec::new()
        }));
        assert!(!round_owned_request(&AgentRequest::Interrupt));
        assert!(!round_owned_request(&AgentRequest::SwitchConnection {
            provider: "openai".to_string(),
            model: "gpt".to_string(),
            api_key: None,
            base_url: None,
        }));
        assert!(!round_owned_request(&AgentRequest::ToggleTool {
            name: "execute_command".to_string(),
            enabled: false,
        }));
        assert!(!round_owned_request(&AgentRequest::ToggleMcpServer {
            name: "github".to_string(),
            enabled: true,
        }));
        assert!(!round_owned_request(&AgentRequest::RefreshProviderModels));
        assert!(!round_owned_request(&AgentRequest::QuerySessionContext));
        assert!(!round_owned_request(&AgentRequest::PermissionReply {
            request_id: "r".to_string(),
            decision: nuo_wire::PermissionDecision::Always,
            parent_call_id: None,
        }));
        assert!(!round_owned_request(&AgentRequest::UserQuestionReply {
            request_id: "r".to_string(),
            answers: Vec::new(),
            parent_call_id: None,
        }));
        assert!(!round_owned_request(&AgentRequest::StdinReply {
            request_id: "r".to_string(),
            text: "y".to_string(),
            parent_call_id: None,
        }));
        assert!(!round_owned_request(&AgentRequest::ExitSideView));
        assert!(!round_owned_request(&AgentRequest::FocusSide {
            side_id: "s".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::InterruptSide {
            side_id: "s".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::InterruptSubagent {
            call_id: "call_1".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::CloseSide {
            side_id: "s".to_string()
        }));
        assert!(!round_owned_request(&AgentRequest::QueryBtwList));
        assert!(!round_owned_request(&AgentRequest::UpdateTuiLayout(
            "default".to_string()
        )));
        assert!(!round_owned_request(&AgentRequest::DeleteSession {
            id: "s".to_string(),
        }));
        assert!(!round_owned_request(&AgentRequest::RenameSession {
            id: "s".to_string(),
            title: Some("t".to_string()),
        }));
        assert!(!round_owned_request(&AgentRequest::QuerySessionDetail {
            id: "s".to_string(),
        }));
    }

    #[tokio::test]
    async fn activity_reconcile_fires_only_for_control_plane_requests_with_no_live_round() {
        let lifecycle = Arc::new(RoundLifecycle::new());
        let unattended = AgentRequest::SlashCommand("/unattended on".to_string());

        // Idle harness + control-plane request → the driver must reconcile.
        assert!(
            needs_activity_reconcile(&unattended, &lifecycle).await,
            "idle + slash command needs the reconcile"
        );

        // Round-owned requests never need the reconcile — the round task emits
        // its own terminal idle snapshot.
        assert!(
            !needs_activity_reconcile(
                &AgentRequest::Prompt {
                    text: "hi".to_string(),
                    images: Vec::new(),
                    sent_at_ms: None,
                },
                &lifecycle,
            )
            .await,
            "prompt closes its own lifecycle"
        );

        // A live round owns the display: even a control-plane request is left
        // alone so the round's timer/turn counters are not reset.
        let begin = lifecycle.begin().await;
        assert!(
            !needs_activity_reconcile(&unattended, &lifecycle).await,
            "live round suppresses the reconcile"
        );
        assert!(lifecycle.finish(begin.generation).await);

        // Back to idle → the reconcile is armed again.
        assert!(
            needs_activity_reconcile(&unattended, &lifecycle).await,
            "idle again → reconcile re-arms"
        );
    }

    fn usage_record(
        session_id: &str,
        actor: &str,
        round: u64,
        turn: u32,
        status: nuo_wire::RequestUsageStatus,
    ) -> nuo_wire::RequestUsageRecord {
        nuo_wire::RequestUsageRecord {
            key: nuo_wire::RequestUsageKey {
                session_id: session_id.to_string(),
                actor_id: actor.to_string(),
                round,
                turn,
                attempt: 1,
            },
            provider: "relay".to_string(),
            model: "m".to_string(),
            status,
            ..Default::default()
        }
    }

    fn residue_store(directory: &std::path::Path) -> Arc<SessionStore> {
        std::fs::create_dir_all(directory).expect("create test directory");
        Arc::new(SessionStore::for_path(directory.join("session.json")))
    }

    #[tokio::test]
    async fn crash_residue_arms_retry_point_for_the_in_flight_round() {
        // The scenario from the wild: the process died mid-round, so no
        // graceful path armed `/retry` and the resumed session answered
        // "Nothing to retry". Recovery must arm it from the durable
        // `InFlight` usage record.
        let directory =
            std::env::temp_dir().join(format!("nuo-crash-retry-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        let session_id = store.id().await;
        // Round 2 in flight (turn 2 = the second ReAct turn, the one that
        // died), one committed assistant turn from turn 1, a settled round 1.
        // The transcript carries no round delimiters: the crashed round's
        // opener is the *last* visible user message, so round 1's assistant
        // reply must not be counted into round 2.
        store
            .set_request_usage_records(vec![
                usage_record(&session_id, "root", 1, 1, RequestUsageStatus::Completed),
                usage_record(&session_id, "root", 2, 1, RequestUsageStatus::Completed),
                usage_record(&session_id, "root", 2, 2, RequestUsageStatus::InFlight),
                usage_record(
                    &session_id,
                    "subagent:c1",
                    2,
                    5,
                    RequestUsageStatus::InFlight,
                ),
            ])
            .await
            .unwrap();
        store
            .replace_messages(vec![
                Message::new(Role::User, "round 1 prompt"),
                Message::new(Role::Assistant, "round 1 answer"),
                Message::new(Role::User, "round 2 prompt"),
                Message::new(Role::Assistant, "round 2 turn 1"),
                Message::new(Role::Tool, "ok"),
            ])
            .await
            .unwrap();
        store.set_round_counter(2).await.unwrap();

        let residue = recover_crashed_round(
            &store,
            store.request_usage_records().await,
            1_700_000_000_000,
        )
        .await;

        // Interrupt for the dangling round, so the transcript explains it.
        assert_eq!(residue.interrupts.len(), 1);
        assert_eq!(residue.interrupts[0].round, Some(2));
        assert_eq!(
            residue.interrupts[0].reason,
            nuo_wire::RoundInterruptReason::Terminated
        );
        // The point names the highest in-flight round (not the subagent's key),
        // counts only committed root turns, and watermarks the durable
        // window.
        let point = residue
            .retry_point
            .expect("crash residue arms a retry point");
        assert_eq!(point.round, 2);
        assert_eq!(point.turns_committed, 1, "one committed ReAct turn");
        assert_eq!(point.history_watermark, 5);
        assert_eq!(point.at_ms, 1_700_000_000_000);

        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn crash_residue_ignores_settled_sessions_and_lower_in_flight_rounds() {
        let directory =
            std::env::temp_dir().join(format!("nuo-crash-settled-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        // Nothing in flight → no residue at all.
        let empty = recover_crashed_round(&store, Vec::new(), 1).await;
        assert!(empty.interrupts.is_empty() && empty.retry_point.is_none());

        let session_id = store.id().await;
        store
            .set_request_usage_records(vec![
                usage_record(&session_id, "root", 1, 1, RequestUsageStatus::Completed),
                // Round 2 still in flight — but round 3 has since completed,
                // so 2 is history: the counter guard must retire it.
                usage_record(&session_id, "root", 2, 1, RequestUsageStatus::InFlight),
                usage_record(&session_id, "root", 3, 1, RequestUsageStatus::Completed),
            ])
            .await
            .unwrap();
        store.set_round_counter(3).await.unwrap();
        let superseded =
            recover_crashed_round(&store, store.request_usage_records().await, 1).await;
        assert!(superseded.retry_point.is_none(), "superseded round retired");
        // The dangling-round interrupt is still recorded for the transcript.
        assert_eq!(superseded.interrupts.len(), 1);
        assert_eq!(superseded.interrupts[0].round, Some(2));

        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn crash_residue_arms_even_over_an_existing_terminated_interrupt() {
        // Two scenarios land here. (a) A graceful daemon kill: the registry
        // records `Terminated` but the round is exactly as resumable as a
        // crash's — suppressing the point would resurrect the bug for every
        // `nuo stop`. (b) A crash *during* a `/retry` resume: the resumed
        // round keeps its number, so the earlier run's `Terminated` record is
        // already present when the second crash is recovered. The interrupt
        // explains the transcript; the point offers recovery — independent.
        let directory =
            std::env::temp_dir().join(format!("nuo-crash-graceful-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        let session_id = store.id().await;
        store
            .set_request_usage_records(vec![usage_record(
                &session_id,
                "root",
                1,
                1,
                RequestUsageStatus::InFlight,
            )])
            .await
            .unwrap();
        store
            .record_round_interrupt(nuo_wire::RoundInterrupt {
                reason: nuo_wire::RoundInterruptReason::Terminated,
                at_ms: 1,
                round: Some(1),
                detail: None,
            })
            .await
            .unwrap();
        store.set_round_counter(1).await.unwrap();
        let residue = recover_crashed_round(&store, store.request_usage_records().await, 2).await;
        // The record already exists — `record_round_interrupt` dedupes on
        // (reason, round) — but the resume point is armed regardless.
        assert_eq!(residue.interrupts.len(), 1);
        assert!(
            residue.retry_point.is_some(),
            "an existing interrupt must not suppress the resume point"
        );

        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn crash_residue_counts_turns_from_the_rounds_opening_user_message() {
        // A crash on the round's very first request: the user message is
        // durable, nothing streamed through yet → zero committed turns. A
        // prior round's assistant reply must not be counted into it.
        let directory =
            std::env::temp_dir().join(format!("nuo-crash-first-{}", uuid::Uuid::new_v4()));
        let store = residue_store(&directory);
        let session_id = store.id().await;
        store
            .set_request_usage_records(vec![usage_record(
                &session_id,
                "root",
                2,
                1,
                RequestUsageStatus::InFlight,
            )])
            .await
            .unwrap();
        store
            .replace_messages(vec![
                Message::new(Role::User, "round 1"),
                Message::new(Role::Assistant, "answer 1"),
                Message::new(Role::User, "round 2"),
            ])
            .await
            .unwrap();
        store.set_round_counter(2).await.unwrap();
        let residue = recover_crashed_round(&store, store.request_usage_records().await, 1).await;
        let point = residue
            .retry_point
            .expect("first-request crash arms a point");
        assert_eq!(point.turns_committed, 0, "nothing streamed through");
        assert_eq!(point.history_watermark, 3);

        let _ = std::fs::remove_dir_all(directory);
    }
}

#[cfg(test)]
mod followup_queue_tests {
    //! The driver-owned follow-up queue authority (ADR-0197 M4).

    use super::*;

    fn message(id: &str, text: &str) -> nuo_wire::QueuedMessage {
        nuo_wire::QueuedMessage {
            id: id.to_string(),
            text: text.to_string(),
            display_text: None,
            images: Vec::new(),
            sent_at_ms: None,
        }
    }

    #[test]
    fn enqueue_preserves_fifo_order_per_target() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "first"));
        queue.enqueue("primary", message("b", "second"));
        queue.enqueue("aside-1", message("c", "aside"));

        let (primary_items, paused) = queue.snapshot("primary");
        assert_eq!(primary_items.len(), 2);
        assert_eq!(primary_items[0].id, "a");
        assert_eq!(primary_items[1].id, "b");
        assert!(!paused);

        let (aside_items, _) = queue.snapshot("aside-1");
        assert_eq!(aside_items.len(), 1);
        assert_eq!(aside_items[0].id, "c");
    }

    #[test]
    fn dequeue_front_is_fifo_and_target_scoped() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("primary", message("b", "2"));
        queue.enqueue("aside-1", message("c", "aside"));

        let first = queue.dequeue_front("primary").expect("must dequeue");
        assert_eq!(first.id, "a");
        assert!(queue.has_items_for("primary"));

        let second = queue.dequeue_front("primary").expect("must dequeue");
        assert_eq!(second.id, "b");
        assert!(!queue.has_items_for("primary"));
        // The aside's item is untouched.
        assert!(queue.has_items_for("aside-1"));
    }

    #[test]
    fn reorder_clamps_within_the_target_slice() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("primary", message("b", "2"));
        queue.enqueue("primary", message("c", "3"));
        queue.enqueue("aside-1", message("z", "aside"));

        // Move `c` two toward the front: clamped to the slice head.
        queue.reorder("primary", "c", -5);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items[0].id, "c");
        assert_eq!(items[1].id, "a");
        assert_eq!(items[2].id, "b");
        // The aside slice is untouched.
        let (aside, _) = queue.snapshot("aside-1");
        assert_eq!(aside[0].id, "z");

        // Move `c` back one.
        queue.reorder("primary", "c", 1);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items[0].id, "a");

        // Moving past the tail clamps.
        queue.reorder("primary", "a", 7);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items.last().map(|m| m.id.as_str()), Some("a"));

        // Unknown id: no-op.
        queue.reorder("primary", "nope", -1);
        let (items, _) = queue.snapshot("primary");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn remove_and_clear_are_target_scoped_and_idempotent() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("aside-1", message("z", "aside"));

        queue.remove("primary", "a");
        assert!(!queue.has_items_for("primary"));
        queue.remove("primary", "a"); // idempotent
        assert!(queue.has_items_for("aside-1"));

        queue.clear("aside-1");
        assert!(!queue.has_items_for("aside-1"));
    }

    #[test]
    fn pause_gates_only_the_target() {
        let mut queue = FollowUpQueue::new();
        queue.enqueue("primary", message("a", "1"));
        queue.enqueue("aside-1", message("z", "aside"));

        queue.set_paused("primary", true);
        assert!(queue.is_paused("primary"));
        assert!(!queue.is_paused("aside-1"));
        queue.set_paused("primary", false);
        assert!(!queue.is_paused("primary"));
    }

    /// The boundary contract (ADR-0197 M4): the round lifecycle's interrupt
    /// parking must be observable at the round boundary, so the queue parks
    /// after an interrupted round instead of auto-firing more prompts.
    #[tokio::test]
    async fn lifecycle_reports_interruption_at_the_boundary() {
        let lifecycle = RoundLifecycle::new();
        assert!(!lifecycle.was_interrupted());

        let begin = lifecycle.begin().await;
        assert!(!lifecycle.was_interrupted());

        // The operator requests a stop mid-round…
        lifecycle.record_interrupt(nuo_wire::RoundInterruptReason::User);
        assert!(lifecycle.was_interrupted());

        // …and the round unwinds to its boundary.
        lifecycle.cancel_current().await;
        assert!(lifecycle.finish(begin.generation).await);
        assert!(lifecycle.was_interrupted(), "the queue must park");

        // The next round clears it.
        let _ = lifecycle.begin().await;
        assert!(!lifecycle.was_interrupted());
    }
}
