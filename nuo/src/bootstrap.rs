//! The shared session-harness factory for every frontend binary (ADR-0037
//! Step 6).
//!
//! [`assemble`] performs the full session startup that used to live inline in
//! the `muta` binary's `main`: channel creation, custom-command discovery,
//! config load + migrations, persisted model catalogs, store opens,
//! the repeat scheduler, provider/skills/toolset wiring, `SubagentTool` layering,
//! agent construction, MCP background connect, pursuit/todo/session-state
//! restore, and finally [`SessionDriver`] construction — in the exact order
//! the original `main` did, with the same background spawns.
//!
//! The crate stays application-neutral (ADR-0054): the caller supplies the
//! [`AgentIdentity`], the [`AgentRoleProfile`], and the [`UiBridge`] as
//! parameters. Nothing here names a product.
//!
//! `SessionStart::Version`, `SessionStart::Doctor`, `SessionStart::Attach`, and
//! `SessionStart::Showcase` are **not** handled here: they are purely local
//! (or client-side) short-circuits and must be dispatched by the caller
//! before invoking [`assemble`].

use crate::catalog;
use nuo_harness::orchestration::{ProxyProvider, round_response};
use nuo_harness::{Agent, AgentIdentity, AgentRoleProfile, RoundLifecycle, SubagentTool};
use nuo_wire::{
    AgentNotice, AgentRequest, AgentResponse, Message, NoticeKind, NoticeSeverity, NoticeSource,
    NoticeSurface, Provider, RoundEvent, SubAgentProfile, ToolContextBuilder, ToolSet,
    WorkspaceTrustState, collect_toolset,
};

use crate::mcp::{McpCatalog, McpRuntime};
use nuo_persistence::{
    config::Config, connection_usage, paths, session::SessionStore,
    workspace_security::WorkspaceSecurityStore,
};
use nuo_harness::skills::SkillRegistry;

use crate::startup::SessionStart;
use crate::{SessionDriver, UiBridge};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

/// Everything a frontend binary must supply to assemble a session harness.
///
/// The identity and master are the *only* application-specific inputs; all
/// other behavior is shared across frontends.
pub struct BootstrapParams {
    /// The agent's identity (name + mission), bound at construction.
    pub identity: AgentIdentity,
    /// The declarative agent preset profile (ADR-0053), applied after
    /// construction and before the `[agent]` config overlay.
    pub preset: AgentRoleProfile,
    /// The frontend's clipboard/UI bridge (used by `/export`).
    pub ui: Arc<dyn UiBridge>,
    /// How the session begins (ADR-0116: only the assembly-relevant
    /// shapes exist here; one-shot CLI modes never reach the harness).
    pub startup: SessionStart,
    /// `--project` override: the workspace partition. `None` = workspace-free.
    pub project_root: Option<PathBuf>,
    /// The role staffing this session, if any (ADR-0225). Recorded on
    /// the session as metadata; `None` for the default coding principal.
    pub role: Option<String>,
    /// `--unattended` at start (unattended execution): auto-approve tool permissions.
    pub unattended: bool,
    /// Workspace filesystem confinement (default true). False (`--no-confinement`) bypasses confinement.
    pub confined: bool,
    /// ADR-0141: the human-channel accountant this session reports into.
    /// Attach/detach on the WS layer ORs client postures into it; the
    /// harness's posture gate reads it before parking a human request.
    /// `None` on one-shot CLI paths that never attach.
    pub human_channel: Option<Arc<nuo_wire::human_request::HumanChannelAccountant>>,
    /// Session-lifetime cancellation token (ADR-0125): passed through to the
    /// background `/schedule` scheduler so it stops when the harness is torn
    /// down (suspension, kill, daemon drain) instead of ticking forever.
    /// `None` = process-lifetime scheduling (single-session frontends).
    pub teardown_token: Option<tokio_util::sync::CancellationToken>,
    /// ADR-0209: Daemon-wide authoritative configuration shared across all hosted sessions.
    pub shared_config: Option<crate::SharedConfig>,
    /// ADR-0209: Daemon-wide authoritative connection usage shared across all hosted sessions.
    pub shared_provider_usage: Option<crate::SharedConnectionUsage>,
}

/// The assembled session harness: the driver (ready to `run`), the frontend
/// ends of the request/response channels, and the values the frontend needs
/// to start its UI and wind the session down.
pub struct Bootstrap {
    /// The session driver, fully wired. The caller moves it into a task
    /// (`tokio::spawn(driver.run())`).
    pub driver: SessionDriver,
    /// The frontend's request sender (the driver holds the receiver).
    pub req_tx: mpsc::Sender<AgentRequest>,
    /// The frontend's response receiver (the driver holds the sender).
    pub resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
    /// An `Arc` handle on the primary agent so the caller can fire
    /// SessionEnd hooks (ADR-0025) after its UI returns — the driver task
    /// owns the agent by then.
    pub agent_for_session_end: Arc<Agent>,
    /// The primary session store, shared with the driver.
    pub session: Arc<SessionStore>,
    /// Shared token-source ledger, shared with the driver; the frontend reads
    /// it for the token-source report.
    pub token_ledger: Arc<nuo_wire::TokenSourceLedger>,
    /// The provider name the UI should display at startup.
    pub initial_provider_name: String,
    /// The model name the UI should display at startup.
    pub initial_model_name: String,
    /// The session's restored transcript (empty for a fresh session).
    pub restored_messages: Vec<Message>,
    /// Complete daemon-owned command/completion vocabulary for this session.
    pub command_catalog: nuo_wire::CommandCatalog,
    /// The primary agent (same `Arc` as `agent_for_session_end`), exposed so
    /// the registry can publish session-scoped tools onto it.
    pub agent: Arc<Agent>,
    /// The workspace-authority store assembled for this session's project
    /// root. The registry surfaces it on [`crate::registry::BoundSession`]
    /// so the WS attach path can detect an unconfigured workspace and push
    /// the trust decision to the attaching client. Bootstrap itself no
    /// longer emits the trust question over `resp_rx` (see `assemble`): the
    /// broadcast channel does not exist yet at that point, so the event
    /// reached nobody. Attach-time replay in `serve.rs` is the delivery
    /// mechanism instead.
    pub security: Arc<WorkspaceSecurityStore>,
    /// Live additional-roots handle sharing state with the execution
    /// environment. `/trust` grant/revoke and `/settings reload` recompute
    /// the admitted set through it — effective on the next tool call.
    pub shared_additional_roots: nuo_wire::SharedAdditionalRoots,
    /// Live handle for toggling session-level workspace confinement.
    pub shared_confinement: nuo_wire::SharedConfinement,
}

/// Ensure the four XDG application roots exist. Best-effort.
pub fn ensure_app_roots() {
    let dirs = paths::get();
    for dir in [
        &dirs.config_dir,
        &dirs.data_dir,
        &dirs.state_dir,
        &dirs.cache_dir,
    ] {
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(?error, dir = %dir.display(), "bootstrap: could not create app dir");
        }
    }
}

/// Assemble one live session harness. See the module docs for the contract.
///
/// The ordering and background-spawn behavior are identical to the original
/// inline `main`: live catalog sync, skill catalog
/// refresh, MCP connect/refresh, and the schedule scheduler (which holds a
/// `req_tx` clone) all run in the background so they never delay the first
/// frame.
#[allow(clippy::too_many_lines)]
pub async fn assemble(params: BootstrapParams) -> Result<Bootstrap, Box<dyn std::error::Error>> {
    nuo_persistence::db::get_persistence_handle().ensure_ready()?;
    let BootstrapParams {
        identity,
        preset,
        ui,
        startup,
        project_root: project_override,
        role,
        unattended: unattended_at_start,
        confined: confined_at_start,
        human_channel,
        teardown_token: _,
        shared_config,
        shared_provider_usage,
    } = params;
    debug_assert!(
        matches!(
            startup,
            SessionStart::Fresh
                | SessionStart::FreshWithPrompt(_)
                | SessionStart::Resume(_)
                | SessionStart::Picker
        ),
        "assemble only handles Fresh/FreshWithPrompt/Resume/Picker; other modes must short-circuit in the caller"
    );

    // First-run friendliness: this harness opens stores eagerly (the session
    // store and embedding index under data_dir) and does not create their
    // parent dirs first — on a developer's machine those dirs usually already
    // exist from prior runs, but any binary may be started into a fresh XDG
    // root (wrappers, CI, sandboxes, a spawned session server). Create the
    // four app roots up front, BEFORE any store opens; best-effort,
    // everything deeper stays lazy as in production.
    {
        let dirs = paths::get();
        for dir in [
            &dirs.config_dir,
            &dirs.data_dir,
            &dirs.state_dir,
            &dirs.cache_dir,
        ] {
            if let Err(error) = std::fs::create_dir_all(dir) {
                tracing::warn!(?error, dir = %dir.display(), "bootstrap: could not create app dir");
            }
        }
    }

    /// Bound capacity for inbound session requests to prevent unbounded memory growth.
    const SESSION_REQUEST_CAPACITY: usize = 512;
    let (req_tx, req_rx) = mpsc::channel::<AgentRequest>(SESSION_REQUEST_CAPACITY);
    let (resp_tx, resp_rx) = mpsc::unbounded_channel::<AgentResponse>();

    let shared_config =
        shared_config.unwrap_or_else(|| Arc::new(tokio::sync::RwLock::new(Config::load())));
    let shared_provider_usage = shared_provider_usage.unwrap_or_else(|| {
        Arc::new(tokio::sync::RwLock::new(
            connection_usage::ConnectionUsage::load(),
        ))
    });

    let mut config = shared_config.read().await.clone();
    // Overlay persisted fitted-model metadata onto model resolution, so ids a
    // trusted provider advertised (but the static registry does not know)
    // resolve with their real capabilities from the very first request.
    catalog::sync_fitted_model_registry();

    // Startup is read-only for the remote catalog (ADR-0227). The persisted
    // per-connection `RemoteCatalogCache` plus the compiled baseline/seed are the
    // source of truth for the first frame; no network sync or scheduled
    // refresh runs here. A refresh happens only on explicit user action
    // (`AgentRequest::RefreshProviderModels`) or a connection lifecycle event.

    // A session's partition is its workspace: `--project`/cwd when present, or
    // `None` for a workspace-free persona (ADR-0226). `persona` is recorded on
    // the session as metadata (which principal staffed it), used for `--resume`.
    let workspace_root: Option<PathBuf> = project_override;
    let workspace = workspace_root
        .as_ref()
        .map(nuo_wire::WorkspaceBinding::new);

    // Initialize Agent logic. The provider is resolved through the model
    // catalog (`build_provider_for`), the single source of truth for the
    // env-var-then-config resolution rules shared with runtime switching. See
    // `docs/adr/0002-model-channel-abstraction.md`.

    // ADR-0116: the pre-0018 per-project exclusive lock is gone — the
    // unified daemon owns every session and the CLI flag was dead (parsed,
    // discarded). Sessions still pin their own `sessions/<id>.{json,jsonl}`
    // (ADR-0018), so concurrency is safe without a project-wide lock.

    // Session loading honors the startup mode. Under ADR-0018
    // `load_for_project` pins a fresh `sessions/<id>.{json,jsonl}`, so a bare
    // start always begins a new session; prior sessions stay on disk and are
    // reachable through the picker or `attach`. `mutx attach <id>` opens
    // that exact session — a missing target is a hard error (propagated via
    // `?`) rather than a silent fresh-session fallback, so the operator knows
    // the attach never happened. `mutx attach` (no id) opens the sessions
    // picker overlay instead of guessing.
    let session = Arc::new(SessionStore::for_workspace(workspace, role));
    let open_picker_on_start = match &startup {
        SessionStart::Fresh | SessionStart::FreshWithPrompt(_) => false,
        SessionStart::Picker => true,
        SessionStart::Resume(id) => {
            session.resume(Some(id.as_str())).await?;
            false
        }
    };

    // Background `/schedule` scheduler, bound to THIS session. Every 30s it prunes
    // expired jobs and fires any that are due, dispatching each prompt as a
    // normal `AgentRequest::Chat` round. Drives both recurring cron jobs and
    // one-shot (countdown / absolute-time) jobs. Jobs are session-scoped state
    // now, so a resumed session's schedule is already loaded above and the
    // scheduler runs against it from the first tick. Supervised: a panic in
    // the tick loop restarts with backoff instead of silently killing every
    // scheduled job in the session.
    //
    // Teardown token (ADR-0125): the registry passes the hosted session's
    // cancellation token, so suspension/kill stops the tick with the harness.
    // Before this the task leaked past teardown and ticked against a dead
    // channel forever. `None` (a plain process-lifetime scheduler) remains
    // available for single-session frontends that tear down with the process.
    // C6: overlay the session's provider/model pin onto the effective config
    // before building the initial provider. A session that previously ran
    // `/models` reopens on its own provider instead of the global default,
    // so one session's choice never bleeds into another. Done after the session
    // is loaded (and, for resume, after `resume` swapped in its data).
    if let Some(selection) = session.provider_selection().await {
        config.default_connection = selection.connection.clone();
        if let Some(model) = selection.model {
            config.default_model = Some(model);
        }
    }

    // The catalog returns `None` when no real channel resolves (empty config or
    // an unknown default). Install the explicit `NoProvider` sentinel so the
    // holder type is satisfied; the chat dispatch refuses up-front with a
    // user-facing notification while this sentinel is live.
    let provider_id = catalog::default_provider_id(&config);
    crate::handlers_provider::refresh_oauth_if_needed(&config, provider_id).await;

    let session_id = session.id().await;
    let initial_provider: Arc<dyn Provider> = catalog::build_provider_for_model(
        &config,
        provider_id,
        config.default_model.as_deref(),
        Some(&session_id),
    )
    .or_else(|| catalog::build_provider_for(&config, provider_id))
    .unwrap_or_else(|| Arc::new(nuo_harness::NoProvider));

    let provider_holder = Arc::new(RwLock::new(initial_provider));
    let provider_for_task = provider_holder.clone();

    let agent_provider = Arc::new(ProxyProvider::new(provider_holder));

    // Shared skills registry for the skill tools and session context. Discover
    // and load all available skills immediately on session assembly (ADR-0165),
    // then spawn a reactive filesystem watcher so subsequent disk mutations
    // automatically hot-reload in place without requiring manual `/trust`.
    //
    // Pin the session's project root into the skills config so the
    // project-local sources (`.nuo/skills`, `skills`) resolve from this
    // session's project — not the daemon process's cwd, which under the
    // unified daemon (ADR-0096) belongs to whichever client first spawned it.
    let mut skills_config = config.skills.clone();
    skills_config.project_root = workspace_root.clone();
    let skills_registry = Arc::new(
        SkillRegistry::load(&skills_config)
            .await,
    );
    // A content-admitted `.nuo/skills/<name>/SKILL.md` wins over a same-named
    // user or remote skill by priority. Surface every newly observed shadow so
    // that prompt injection cannot hide behind normal precedence. Install the
    // sink before background refresh so startup, `/skills reload`, and
    // `/trust` reports through the same path.
    {
        let resp_tx_for_shadows = resp_tx.clone();
        let session_id_for_shadows = session.id().await;
        skills_registry.set_shadow_sink(Some(Arc::new(move |shadowed| {
            for shadow in shadowed {
                let _ = resp_tx_for_shadows.send(round_response(
                    &session_id_for_shadows,
                    RoundEvent::Notice(
                        AgentNotice::trust_changed(format!(
                            "Project skill '{}' overrides the {}-scope skill of the same name",
                            shadow.name, shadow.overridden_scope
                        ))
                        .with_body(format!(
                            "Loading {} instead. Project-local skills win by priority; \
                             if this is unexpected, inspect the project's skills directories \
                             (.nuo/skills, skills) or run \
                             `/untrust`.",
                            shadow.winner_source.display()
                        )),
                    ),
                ));
            }
        })));
    }
    skills_registry.spawn_reactive_watcher();

    // Built-in tools self-register via `inventory` (most tools carry a
    // `register_tool!` submission at its definition site) and are collected
    // here from a single opaque context. Tools that need runtime state (the web
    // tools' search config, the shared skill registry, the embedding index +
    // session store) pull it out of the context by type — see
    // `nuo_wire::tool_registry`. Stateful/meta tools that genuinely depend on the
    // *rest* of the toolset (the subagent dispatch `task`) cannot
    // self-register and are assembled explicitly below. MCP tools are
    // discovered at runtime and published directly to the master Agent;
    // they are not part of this static capability set.
    // Spatial admission resolves global and trusted project-declared roots.
    let workspace_security = Arc::new(WorkspaceSecurityStore::load());
    let mut security_snapshot = match &workspace_root {
        Some(root) => workspace_security.snapshot(root),
        None => nuo_wire::WorkspaceSecuritySnapshot::new("workspace-free"),
    };
    security_snapshot.user_assets =
        crate::handlers_slash::security_ops::compute_user_assets_trust();
    let mut additional_roots: Vec<std::path::PathBuf> = Vec::new();
    let resolved_additional = match &workspace_root {
        Some(root) => {
            if security_snapshot.ex_workspace.is_trusted() {
                config.merge_project_additional_roots(Config::load_project_additional_roots(root));
            }
            let resolved = config
                .resolve_workspace_additional_roots_detailed(root)
                .unwrap_or_default();
            additional_roots = resolved.admitted.clone();
            resolved
        }
        None => nuo_persistence::config::ResolvedAdditionalRoots::default(),
    };
    for (raw, reason) in &resolved_additional.skipped {
        tracing::warn!(
            root = %raw,
            %reason,
            "additional workspace root skipped"
        );
    }
    // Hot-updatable `[web]` handle: the web tools receive the same handle via
    // the tool context, and `UpdateWebSearchConfig` atomically replaces its
    // resolved snapshot. Changes reach the next call without a toolset rebuild.
    let resolved_web = nuo_persistence::config::resolve_web_config(
        &config.web,
        &nuo_persistence::config::Credentials::load(),
    );
    let websearch_shared = nuo_wire::SharedWebConfig::new(resolved_web.runtime);
    let (execution_env, shared_additional_roots, shared_confinement): (
        Arc<dyn nuo_wire::ExecutionEnvironment>,
        nuo_wire::SharedAdditionalRoots,
        nuo_wire::SharedConfinement,
    ) = match &workspace_root {
        Some(root) => {
            let env = Arc::new(
                nuo_harness::execution::WorkspaceExecutionEnvironment::with_additional_roots(
                    root.clone(),
                    additional_roots.clone(),
                ),
            );
            let additional = env.shared_additional_roots();
            let confinement = env.shared_confinement();
            (
                env as Arc<dyn nuo_wire::ExecutionEnvironment>,
                additional,
                confinement,
            )
        }
        None => {
            // Workspace-free sessions (such as ops or custom unconfined roles) operate
            // against the host environment using current_dir as the baseline directory.
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let env = Arc::new(
                nuo_harness::execution::WorkspaceExecutionEnvironment::with_additional_roots(
                    cwd,
                    additional_roots.clone(),
                ),
            );
            let additional = env.shared_additional_roots();
            let confinement = env.shared_confinement();
            (
                env as Arc<dyn nuo_wire::ExecutionEnvironment>,
                additional,
                confinement,
            )
        }
    };
    let background_jobs = crate::background_jobs::BackgroundJobManager::new();

    // One host for the whole assembly: the agent and the tool context share it,
    // so both see the same declared roles, the same path policy, and the same
    // dialogue memory.
    let agent_kernel_host = crate::kernel_host::kernel_host(workspace_root.clone());
    let tool_ctx = {
        let mut builder = ToolContextBuilder::new();
        builder.provide(websearch_shared.clone());
        builder.provide(websearch_shared.get());
        builder.provide(skills_registry.clone());
        // The host's dialogue memory reaches the recall tool as a service
        // (ADR-0300 §1): the tool holds the port, the product decides where the
        // memory lives. The same host the agent is built with, so there is one
        // memory per process rather than one per construction site.
        builder.provide(nuo_harness::tools::RecallMemoryService::new(
            agent_kernel_host.memory_arc(),
        ));
        builder.provide(session.clone());
        builder.provide(execution_env.clone());
        // The session's workspace root: every workspace-relative tool
        // operation (bash cwd, relative path resolution, search bases)
        // anchors here instead of the daemon process's cwd. Under the
        // unified daemon (ADR-0096) one process hosts sessions for many
        // projects, so the process cwd is whichever directory the first
        // client spawned it from — correct only by coincidence. This is the
        // fix for "launched in project A, session edits project B".
        // A workspace-free session (ADR-0220) provides no root, so
        // workspace-relative tools are not admitted against a directory.
        if let Some(root) = &workspace_root {
            builder.provide(nuo_wire::WorkspaceRoot(root.clone()));
            builder.provide(nuo_wire::WorkspaceRoots::new(
                root.clone(),
                additional_roots.clone(),
            ));
        }
        builder.provide(shared_additional_roots.clone());
        builder.provide(shared_confinement.clone());
        builder.build()
    };
    let mut toolset: ToolSet = collect_toolset(&tool_ctx);

    // Decentralized capability-provided tools (nuo-host, nuo-persistence)
    // following zero-runtime contracts (ADR-0002 / INV-TOOL-01).
    if let Some(root) = &workspace_root {
        let sys_ctx = Arc::new(nuo_host::SystemToolContext::new(root.clone()));
        let sys_tools = nuo_host::create_system_tools(sys_ctx);
        for bridged in nuo_harness::bridge_substrate_tools(sys_tools) {
            toolset.upsert(bridged);
        }
    }
    if let Ok(store) = nuo_persistence::get_role_memory_store() {
        let p_tools = nuo_persistence::create_persistence_tools(Arc::new(store));
        for bridged in nuo_harness::bridge_substrate_tools(p_tools) {
            toolset.upsert(bridged);
        }
    }
    // MCP tools are discovered after Agent construction and published through
    // its connector-neutral dynamic-tool sink. The MCP runtime owns protocol
    // and connection state; the agent owns advertisement and dispatch.
    // Snapshot of the shared toolset (built-in default variants) before the
    // `SubagentTool` is layered on. A `/btw` side session (ADR-0017) rebuilds
    // its `Agent` from this same snapshot — minus its own `SubagentTool` and
    // without inheriting the master's session-scoped connector sources.
    let base_tools: Arc<Vec<Arc<dyn nuo_wire::Tool>>> = Arc::new(toolset.default_view());
    // SubagentTool gets the static capability set (excluding itself) so spawned
    // subagents cannot recurse and inherit the live provider. Dynamic connector
    // sources are master-only unless a future policy explicitly delegates
    // them. It binds the SubAgentProfile::EXPLORE profile (read-only / non-interactive /
    // non-recursive).
    let subagent_tool = Arc::new(SubagentTool::new(
        agent_provider.clone(),
        toolset.clone(),
        &SubAgentProfile::EXPLORE,
    ));
    // Subagents resolve relative write-grants against the session's project
    // root, not the daemon process's cwd (ADR-0096).
    subagent_tool.set_workspace_root(workspace_root.clone());
    // Subagents inherit the session's connection retry configuration.
    subagent_tool.bind_retry_policy(
        config.connection_retry_max_attempts,
        config.connection_retry_base_ms,
        config.connection_retry_max_ms,
    );
    // Full-duplex (ADR-0029): capture the subagent tool's subagent registry so the
    // request loop can route a user's permission / ask_user reply down into the
    // specific live child that surfaced the request (looked up by the parent
    // tool-call id the frontend tags onto the reply). Captured before
    // `subagent_tool` is layered into the capability set.
    let subagent_registry = subagent_tool.registry();
    // Keep a typed handle so we can bind the parent's variant selection into the
    // subagent tool once the agent (which owns that selection) exists. The same
    // underlying `Arc<SubagentTool>` is what gets layered into the toolset.
    let subagent_tool_handle = subagent_tool.clone();
    toolset.insert(subagent_tool);

    // Demand-paged epistemic memory (ADR-0262): provide the `inspect` tool to the
    // master agent for recalling subagent sessions, pruned tool outputs, and compacted history.
    let current_session_id = session.id().await;
    let offstream_reg = crate::offstream::build_offstream_registry(
        &current_session_id,
        session.blob_store().clone(),
    );
    let inspect_tool = Arc::new(nuo_harness::tools::InspectTool::new(
        offstream_reg,
        current_session_id,
    ));
    toolset.insert(inspect_tool);

    let mut agent = Agent::builder_from_toolset(agent_provider, toolset, identity)
        .with_host(agent_kernel_host.clone())
        .with_skills((*skills_registry).clone())
        .build();
    // Surface the admitted additional roots to the model (ADR-0142): the
    // system prompt tells it cross-project paths are legal instead of letting
    // it discover the widened boundary through trial and error.
    agent.set_additional_workspace_roots(additional_roots.clone());
    agent.bind_shared_confinement(shared_confinement.clone());
    let agent = Arc::new(agent);
    // Override axis (model): subagents are agents on the same model, so they
    // inherit the parent's tool-variant selection. The profile still owns the
    // orthogonal scope axis.
    subagent_tool_handle.bind_variant_selection(agent.variant_selection_handle());
    subagent_tool_handle.bind_workspace_security(agent.workspace_security_handle());
    subagent_tool_handle.bind_execution_policy(agent.execution_policy());
    // ADR-0141: subagents inherit the session's live human channel.
    if let Some(accountant) = human_channel.as_ref() {
        subagent_tool_handle.bind_human_channel(Arc::clone(accountant));
    }
    // Wire the per-project "always allow" allowlist so prior `Always`
    // approvals survive across sessions in this project. Best-effort: a
    // missing or unreadable permissions.json just means we re-prompt.
    agent.set_project_root(workspace_root.clone());
    // Seed declarative permission rules from `[permissions]` config so default
    // policies are data-driven. Runtime "Always" decisions still write to
    // permissions.json; these config rules re-apply on every start.
    agent.seed_permissions_from_config(&config.permissions.allow);
    // Asset trust, filesystem boundaries, and runtime execution grants are
    // independent axes. Opening a path grants none of them.
    // `.nuo/config.toml` may declare `[mcp.*]` servers (which execute
    // processes) and `[[hooks]]` entries (which run shell commands at lifecycle
    // points); its `.nuo/skills` and `.nuo/commands` trees inject
    // project-authored prompt text (skills can also shadow the user's own
    // same-named skills by priority). Loading those automatically from a
    // cloned or vendored working tree is the same class of hazard as an npm
    // `postinstall` script or a git hook: a malicious repo must not gain code
    // execution — or prompt injection, which for an agent holding tools is
    // execution-by-proxy — merely because the user opened it. Every concrete
    // domain loads only after its own exact content has been
    // explicitly trusted. Global config is user-authored and trusted
    // unconditionally.
    agent.set_workspace_security(security_snapshot.clone());
    if let Some(root) = &workspace_root {
        let project_mcp = Config::load_project_mcp(root);
        let project_hooks = Config::load_project_hooks(root);
        if security_snapshot.mcp.is_trusted() && !project_mcp.is_empty() {
            config.merge_project_mcp(project_mcp);
        }
        if security_snapshot.hooks.is_trusted() && !project_hooks.is_empty() {
            config.merge_project_hooks(project_hooks);
        }
        if security_snapshot.instructions.is_trusted() {
            match crate::project::load_project_rules(root) {
                Ok(rules) => agent.set_project_rules(rules),
                Err(error) => tracing::warn!(%error, "trusted project rules could not be loaded"),
            }
        }
    }
    let gated = [
        ("mcp", security_snapshot.mcp),
        ("skills", security_snapshot.skills),
        ("hooks", security_snapshot.hooks),
        ("instructions", security_snapshot.instructions),
        ("ex-workspace", security_snapshot.ex_workspace),
    ]
    .into_iter()
    .filter_map(|(domain, state)| {
        matches!(
            state,
            WorkspaceTrustState::Quarantined | WorkspaceTrustState::Changed
        )
        .then_some(format!("{domain} ({})", state.as_str()))
    })
    .collect::<Vec<_>>();
    if !gated.is_empty() {
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::trust_changed("Project assets are quarantined")
                    .with_surface(NoticeSurface::Banner)
                    .with_body(format!(
                        "Quarantined domains: {}. Inspect them, then run `/trust` or `/trust <domain>` (e.g. `/trust instructions`).",
                        gated.join(", ")
                    )),
            ),
        ));
    }
    if !resolved_additional.skipped.is_empty() {
        let details = resolved_additional
            .skipped
            .iter()
            .map(|(p, r)| format!("`{p}` ({r})"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    NoticeKind::ReviewAlert,
                    NoticeSeverity::Warning,
                    "Some additional workspace roots could not be loaded",
                    NoticeSource::Harness,
                )
                .with_surface(NoticeSurface::Inline)
                .with_body(format!("Skipped roots: {details}")),
            ),
        ));
    }
    let command_catalog = crate::startup::command_catalog(&[]);
    // Wire universal asset attestation verifier into muta-mcp (ADR-0243, ADR-0252).
    let attestation_ledger = nuo_persistence::AssetAttestationLedger::load();
    let ledger_for_mcp = attestation_ledger.clone();
    crate::mcp::set_attestation_verifier(Arc::new(move |locator, spec| {
        ledger_for_mcp.is_trusted(locator, spec)
    }));

    // Wire workspace security trust verifier into muta-mcp so MCP server
    // connections can verify sandbox trust without depending on muta-persistence.
    let ws_security_for_mcp = workspace_security.clone();
    crate::mcp::set_trust_verifier(Arc::new(move |root| {
        ws_security_for_mcp
            .snapshot(root)
            .is_trusted(nuo_wire::TrustDomain::Mcp)
    }));
    // Connect every configured MCP server in the BACKGROUND so a slow/unreachable
    // server (8s connect timeout each) never delays the first frame. The
    // runtime is ready immediately with every enabled server in `Connecting`;
    // a spawned task performs the real concurrent connects and seeds the
    // agent's dynamic tool sink as each comes online. The frontend's status
    // snapshot reflects this transient state, and the periodic McpCatalog
    // refresh keeps it live thereafter.
    let mcp_runtime = Arc::new(McpRuntime::start_background(
        config.mcp.clone(),
        agent.dynamic_tool_sink(),
    ));
    let mcp_runtime_for_bg = Arc::clone(&mcp_runtime);
    tokio::spawn(async move {
        mcp_runtime_for_bg.refresh_all().await;
    });
    nuo_harness::dynamic::spawn_refresh(McpCatalog::new(mcp_runtime.clone()));

    // ADR-0240: Kernel-driven reactive configuration watcher for MCP hot-updates.
    spawn_mcp_config_watcher(
        Arc::clone(&mcp_runtime),
        workspace_root.clone(),
        Arc::clone(&workspace_security),
        resp_tx.clone(),
        session.id().await,
    );
    if unattended_at_start {
        agent.set_unattended(true);
        if let Err(error) = session.set_unattended(true).await {
            tracing::warn!(
                error = %error,
                "could not persist --unattended startup posture"
            );
        }
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::UnattendedChanged(true),
        ));
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    nuo_wire::NoticeKind::CommandAck,
                    nuo_wire::NoticeSeverity::Info,
                    "Unattended mode ON",
                    nuo_wire::NoticeSource::Harness,
                )
                .with_surface(nuo_wire::NoticeSurface::Toast)
                .with_body(
                    "All tool permissions are auto-approved this session.\n\
                     Use `/unattended off` to return to interactive mode.",
                ),
            ),
        ));
    }
    if !confined_at_start {
        shared_confinement.set_confined(false);
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::ConfinementChanged(false),
        ));
        let _ = resp_tx.send(round_response(
            &session.id().await,
            RoundEvent::Notice(
                AgentNotice::new(
                    nuo_wire::NoticeKind::CommandAck,
                    nuo_wire::NoticeSeverity::Warning,
                    "Workspace Confinement OFF",
                    nuo_wire::NoticeSource::Harness,
                )
                .with_surface(nuo_wire::NoticeSurface::Toast)
                .with_body(
                    "Tools may access and edit any file on the host system.\n\
                     Use `/confinement on` to restore workspace confinement.",
                ),
            ),
        ));
    }

    // ADR-0209: The authoritative usage telemetry lives in shared_provider_usage.
    // Read the current state for startup model resolution without redundant disk I/O.

    // `mutx attach` (no id) opens the sessions picker at startup instead of
    // loading any session: no transcript, todos, or SessionStart hooks should
    // run against the throwaway fresh session — the real session is restored
    // only once the user picks one from the picker (`/session open`). Fresh and
    // `mutx attach <id>` loads eagerly as before.
    let is_picker = matches!(startup, SessionStart::Picker);

    let restored_messages = if is_picker {
        Vec::new()
    } else {
        session.full_transcript().await
    };

    // Mid-turn context projection: when pruning is enabled, install a gate that
    if config.compaction.prune {
        crate::agent_setup::reseed_prune_threshold(&agent, &config);
    }

    // Seed per-model tool-variant selection for the startup model. Each listed
    // capability is realized by its chosen variant in the schemas sent to the
    // provider; re-seeded on provider/model switch.
    crate::agent_setup::reseed_tool_variants(&agent, &config);

    // Bind the caller-supplied agent preset (ADR-0053). Identity was
    // supplied to the constructor above (immutable past build); this applies
    // the profile's capability scope, operation boundary, runtime knobs, and
    // attended flag in one call. The profile makes the role declarative.
    agent.apply_preset(&preset);

    // Wire the `[agent]` config table (or legacy `[master]`): the opt-in hard-stop
    // budget, the model-supplied-stdin toggle, the interactive-input-panel opt-out,
    // and the anti-anchoring nudge config. All default to sensible values
    // when the table is absent, so this is a no-op for the common case.
    // These run *after* the profile binding so per-installation config wins.
    // ADR-0141: bind the human-channel posture source. With an accountant
    // the agent reads the OR of attached clients (live). Without one
    // (one-shot CLI paths that never attach) the static posture from the
    // startup flags applies — headless no-TTY bootstraps Autonomous.
    if let Some(accountant) = human_channel.clone() {
        agent.set_human_channel_accountant(accountant);
    } else {
        // One-shot paths: a TUI bootstrap is interactive by construction;
        // headless (`-p` runs, remote automation) declares Autonomous.
        agent.set_human_posture(nuo_wire::human_request::HumanChannelPosture::Interactive);
    }
    agent.set_hard_stop_turns(config.agent.hard_stop_turns);
    agent.set_trajectory_guard_config(config.agent.trajectory_guard);
    agent.set_allow_model_stdin(config.agent.allow_model_stdin);
    agent.set_skip_interactive_input(config.agent.skip_interactive_input);
    agent.set_autonomous_fallback_policy(config.agent.ask_user_fallback);
    // Bash safety is action-based and independent from project-extension trust.
    // Workspace authority is enforced by the permission chain; unconditional
    // destructive denies and explicit high-risk confirmations remain here.
    agent.set_bash_policy(&config.bash_policy);

    // Lifecycle event hooks (ADR-0025): each `[[hooks]]` entry runs a shell
    // command at one lifecycle point (PreToolUse / PostToolUse / Stop / …).
    agent.set_hooks(crate::hooks::build_hook_registry(&config.hooks, &agent));

    // Tie the agent to this session/thread.
    let thread_id = session.id().await;
    agent.set_thread_id(&thread_id);

    // Restore the session-scoped runtime state and fire SessionStart hooks.
    // Skipped entirely in Picker mode: the bootstrap session is a throwaway
    // fresh one, and the user has not chosen a session yet. The full restore
    // (todos + disabled tools + round counter + the delegated flag + SessionStart
    // hooks) runs when a real session is opened from the picker — see
    // `handlers_slash`'s `restore_session_runtime`.
    if !is_picker {
        // Restore the unified task list so resume re-shows the sticky panel with
        // the same items (and identity) the model last persisted. An empty list
        // is the "no active task list" state and needs no restore.
        let persisted_todos = session.todos().await;
        if !persisted_todos.is_empty() {
            agent.set_todos(persisted_todos);
        }

        // Restore the remaining session-scoped runtime state (ADR-0048 Phase 2):
        // the orthogonal tool mask and round counter.
        agent.restore_disabled_tools(session.disabled_tools().await);
        agent.restore_round_count(session.round_counter().await);

        // Restore the session-scoped unattended posture (ADR-0132). This is
        // the daemon-restart recovery path: a session that died unattended
        // reopens unattended — attach, lazy-resume, and boot rehost all flow
        // through here. `--unattended` ran earlier and may already have set
        // the flag live; the store read is idempotent either way (same
        // value, and `set_unattended` on the store is a no-op guard), but the
        // explicit flag above wins when both apply, matching the user's most
        // recent explicit intent.
        let persisted_unattended = session.unattended().await;
        if persisted_unattended && !agent.unattended() {
            agent.set_unattended(true);
            let restored_session_id = session.id().await;
            tracing::info!(
                session = %restored_session_id,
                "restored unattended-mode posture from session store"
            );
        }

        // SessionStart hooks (ADR-0025): inject setup context before the first
        // round. Resume vs fresh start is surfaced so a hook can branch.
        {
            let source = match &startup {
                SessionStart::Resume(_) => nuo_wire::SessionSource::Resume,
                _ => nuo_wire::SessionSource::Startup,
            };
            let mut messages = session.model_window().await;
            let before_len = messages.len();
            agent.fire_session_start(source, &mut messages).await;
            // Persist the hook-injected setup context through the single write
            // path so the session stays the source of truth (ADR-0048).
            if messages.len() > before_len
                && let Err(err) = session.append_turn(&messages).await
            {
                tracing::warn!(error = %err, "failed to persist SessionStart hook context");
            }
        }
    }

    // Load per-model usage telemetry (recency signal for the picker,
    // ADR-0002 phase 2). Moved into the agent task so both the startup
    // activation and runtime switches record through one instance.
    let provider_usage = shared_provider_usage.read().await.clone();

    // Primary round lifecycle: at most one active round, superseded by the
    // next begin (replaces the old token-slot + generation-counter pair).
    let lifecycle = Arc::new(RoundLifecycle::new());
    let req_tx_for_commands = req_tx.clone();
    // `/btw` aside state (ADR-0017, lifted to a multi-slot registry by
    // ADR-0103). The primary round machinery is left exactly as-is; the
    // registry peers it with any number of live asides + an explicit
    // "which aside is the composer targeting" pointer that routes `Chat` to
    // whichever session the user is currently composing into. Leaving an
    // aside view detaches non-destructively — the aside keeps running.
    let side: Arc<AsyncRwLock<crate::side::SideRegistry>> =
        Arc::new(AsyncRwLock::new(crate::side::SideRegistry::new()));
    let base_tools_for_side = base_tools.clone();
    let project_root_for_side = workspace_root.clone();

    // Initial values for the frontend
    let initial_provider_name = catalog::default_provider_id(&config).to_string();
    let initial_model_name =
        catalog::resolved_model_name_with_usage(&config, &initial_provider_name, &provider_usage)
            .unwrap_or_default();

    // Keep an Arc handle for the caller so SessionEnd hooks (ADR-0025) can
    // fire after its UI returns — the driver below moves `agent`.
    let agent_for_session_end = Arc::clone(&agent);
    // Shared token-source ledger: the agent books each turn's token usage
    // (reported vs. estimated) into it, and the frontend reads it for the
    // token-source report.
    let token_ledger = nuo_wire::TokenSourceLedger::shared();
    // Durable cross-session usage mirror (ADR-0122): every terminal settle is
    // forwarded into the day-partitioned store under `data/usage/` — a
    // sibling of `projects/`, so session cleanup can never touch it.
    token_ledger.install_usage_sink(Arc::new(
        nuo_persistence::usage_stats::UsageStatsStore::new(),
    ));
    if let Some(root) = &workspace_root {
        token_ledger.set_usage_project(nuo_persistence::paths::project_bucket_name(root));
    }
    subagent_tool_handle.bind_accounting(
        token_ledger.clone(),
        agent.thread_id_handle(),
        agent.round_counter_handle(),
    );

    let driver = SessionDriver {
        req_rx,
        tx: resp_tx,
        req_tx: req_tx_for_commands,
        agent,
        session: session.clone(),
        config: shared_config,
        provider_usage: shared_provider_usage,
        provider_holder: provider_for_task,
        skills_registry,
        subagent_registry,
        mcp_runtime,
        workspace_security: workspace_security.clone(),
        shared_additional_roots: shared_additional_roots.clone(),
        shared_confinement: shared_confinement.clone(),
        command_catalog: command_catalog.clone(),
        lifecycle,
        side,
        base_tools: base_tools_for_side,
        project_root: project_root_for_side,
        startup,
        open_picker_on_start,
        ui,
        token_ledger: token_ledger.clone(),
        extra_commands: Arc::new(crate::slash_handler::SlashCommandRegistry::new()),
        websearch_shared,
        background_jobs,
    };

    Ok(Bootstrap {
        driver,
        req_tx,
        resp_rx,
        agent_for_session_end: agent_for_session_end.clone(),
        session,
        token_ledger,
        initial_provider_name,
        initial_model_name,
        restored_messages,
        command_catalog,
        agent: agent_for_session_end.clone(),
        security: workspace_security.clone(),
        shared_additional_roots,
        shared_confinement,
    })
}

/// ADR-0240: Asynchronous inotify-backed filesystem watcher for automatic MCP hot-updates.
fn spawn_mcp_config_watcher(
    mcp_runtime: Arc<McpRuntime>,
    workspace_root: Option<PathBuf>,
    workspace_security: Arc<WorkspaceSecurityStore>,
    resp_tx: mpsc::UnboundedSender<AgentResponse>,
    session_id: String,
) {
    let workspace_root = workspace_root.map(|r| r.canonicalize().unwrap_or(r));

    let mut watcher =
        match nuo_host::FsWatcher::new(nuo_host::FsWatcher::DEFAULT_DEBOUNCE) {
            Ok(w) => w,
            Err(err) => {
                tracing::warn!(error = %err, "could not initialize MCP config filesystem watcher");
                return;
            }
        };

    // 1. Watch user config directory
    let user_config_file = nuo_host::paths::Dirs::system().config_file();
    let canon_user_config = user_config_file.canonicalize().ok();
    if let Some(user_config_dir) = user_config_file.parent()
        && user_config_dir.exists()
    {
        let _ = watcher.watch(user_config_dir, false);
    }

    // 2. Watch workspace directory and .nuo directory
    if let Some(ref root) = workspace_root {
        if root.exists() {
            let _ = watcher.watch(root, false);
        }
        let workspace_nuo_dir = root.join(".nuo");
        if workspace_nuo_dir.exists() {
            let _ = watcher.watch(&workspace_nuo_dir, false);
        }
    }

    let mut events_rx = watcher.subscribe();

    tokio::spawn(async move {
        let mut watcher = watcher;
        while let Ok(event) = events_rx.recv().await {
            // Dynamic watch attachment: if .nuo was just created, start watching it.
            if let Some(ref root) = workspace_root {
                let ws_nuo = root.join(".nuo");
                let canon_muta = root
                    .canonicalize()
                    .unwrap_or_else(|_| root.clone())
                    .join(".nuo");
                if event.paths.iter().any(|p| {
                    p == &ws_nuo
                        || p == &canon_muta
                        || p.canonicalize().ok().as_ref() == Some(&canon_muta)
                }) && ws_nuo.exists()
                {
                    let _ = watcher.watch(&ws_nuo, false);
                }
            }

            let is_user_config = event.paths.iter().any(|p| {
                p == &user_config_file
                    || canon_user_config.as_ref().is_some_and(|c| p == c)
                    || p.canonicalize().ok().as_ref() == canon_user_config.as_ref()
            });

            let is_workspace_event = workspace_root.as_ref().is_some_and(|root| {
                let ws_config = root.join(".nuo/config.toml");
                let ws_mcp = root.join(".nuo/mcp.json");
                let canon_root = root.canonicalize().unwrap_or_else(|_| root.clone());
                let canon_ws_config = canon_root.join(".nuo/config.toml");
                let canon_ws_mcp = canon_root.join(".nuo/mcp.json");

                event.paths.iter().any(|p| {
                    if p == &user_config_file || canon_user_config.as_ref().is_some_and(|c| p == c)
                    {
                        return false;
                    }
                    if p == &ws_config
                        || p == &ws_mcp
                        || p == &canon_ws_config
                        || p == &canon_ws_mcp
                    {
                        return true;
                    }
                    let canon_p = p.canonicalize().unwrap_or_else(|_| p.clone());
                    if canon_p == canon_ws_config || canon_p == canon_ws_mcp {
                        return true;
                    }
                    let is_target_file = p
                        .file_name()
                        .is_some_and(|n| n == "config.toml" || n == "mcp.json");
                    let is_in_muta = p
                        .parent()
                        .is_some_and(|parent| parent.file_name().is_some_and(|d| d == ".nuo"));
                    is_target_file
                        && is_in_muta
                        && (p.starts_with(root)
                            || p.starts_with(&canon_root)
                            || canon_p.starts_with(root)
                            || canon_p.starts_with(&canon_root))
                })
            });

            if !is_user_config && !is_workspace_event {
                continue;
            }

            let project_mcp = workspace_root
                .as_ref()
                .map(|root| Config::load_project_mcp(root))
                .unwrap_or_default();
            let has_project_mcp = !project_mcp.is_empty();

            // If a workspace file changed but the workspace has no MCP declarations,
            // this event doesn't affect MCP unless the user-level config was also touched.
            if is_workspace_event && !has_project_mcp && !is_user_config {
                continue;
            }

            // Check workspace trust for workspace-level config
            let mcp_trust = workspace_root
                .as_ref()
                .map(|root| workspace_security.snapshot(root).mcp)
                .unwrap_or(nuo_wire::WorkspaceTrustState::Trusted);

            if is_workspace_event
                && has_project_mcp
                && matches!(
                    mcp_trust,
                    nuo_wire::WorkspaceTrustState::Quarantined
                        | nuo_wire::WorkspaceTrustState::Changed
                )
            {
                let _ = resp_tx.send(round_response(
                    &session_id,
                    RoundEvent::Notice(
                        AgentNotice::new(
                            nuo_wire::NoticeKind::TrustChanged,
                            nuo_wire::NoticeSeverity::Warning,
                            "Workspace MCP configuration changed",
                            nuo_wire::NoticeSource::Harness,
                        )
                        .with_surface(nuo_wire::NoticeSurface::Toast)
                        .with_body(
                            "Untrusted workspace MCP configuration detected. Run `/trust mcp` to review and enable.",
                        ),
                    ),
                ));
                continue;
            }

            // Reload configuration and reconfigure MCP runtime
            let mut effective = Config::load();
            if mcp_trust.is_trusted() && has_project_mcp {
                effective.merge_project_mcp(project_mcp);
            }

            let report = mcp_runtime.reconfigure(effective.mcp).await;
            let connected_count = report.connected.iter().filter(|(_, ok)| *ok).count();
            let removed_count = report.removed.len();

            if connected_count > 0 || removed_count > 0 {
                let _ = resp_tx.send(round_response(
                    &session_id,
                    RoundEvent::Notice(
                        AgentNotice::new(
                            nuo_wire::NoticeKind::CommandAck,
                            nuo_wire::NoticeSeverity::Info,
                            "MCP configuration reloaded",
                            nuo_wire::NoticeSource::Harness,
                        )
                        .with_surface(nuo_wire::NoticeSurface::Toast)
                        .with_body(format!(
                            "Hot-reload complete: {connected_count} connected, {removed_count} removed."
                        )),
                    ),
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
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
            Arc::new(nuo_harness::NoProvider),
            vec![],
            nuo_wire::AgentIdentity::default(),
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
            event: RoundEvent::Notice(n),
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
            Arc::new(nuo_harness::NoProvider),
            vec![],
            nuo_wire::AgentIdentity::default(),
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
        tokio::time::sleep(Duration::from_millis(300)).await;

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
                event: RoundEvent::Notice(n),
                ..
            } = resp
                && n.kind == nuo_wire::NoticeKind::TrustChanged
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
        let user_config_file = nuo_host::paths::Dirs::system().config_file();

        let sec_file = tmp.path().join("workspace_security.json");
        let security = Arc::new(WorkspaceSecurityStore::load_from(sec_file));

        let agent = Arc::new(Agent::new(
            Arc::new(nuo_harness::NoProvider),
            vec![],
            nuo_wire::AgentIdentity::default(),
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
            event: RoundEvent::Notice(n),
            ..
        })) = notice
            && n.kind == nuo_wire::NoticeKind::TrustChanged
        {
            panic!("User config modification must not trigger TrustChanged warning!");
        }
    }

    #[tokio::test]
    async fn test_user_assets_zero_bypass_and_ttl_lifecycle() {
        use nuo_wire::WorkspaceTrustState;
        use nuo_wire::security::{AssetLocator, AssetSpec, AttestationStatus};

        let tmp = tempfile::tempdir().unwrap();
        let handle =
            nuo_persistence::db::PersistenceHandle::spawn(tmp.path().join("assets.db"), None);
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
        let mut snapshot = nuo_wire::WorkspaceSecuritySnapshot::new("workspace-free");
        assert_eq!(snapshot.aggregate(), WorkspaceTrustState::Absent);
        snapshot.user_assets = WorkspaceTrustState::Quarantined;
        assert_eq!(snapshot.aggregate(), WorkspaceTrustState::Quarantined);
        snapshot.user_assets = WorkspaceTrustState::Trusted;
        assert_eq!(snapshot.aggregate(), WorkspaceTrustState::Trusted);
    }
}
