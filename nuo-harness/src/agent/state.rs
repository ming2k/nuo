//! Configuration, budget, and hook-fire methods on [`Agent`].
//!
//! Everything an embedder sets up before the first round: tool variant
//! selection, context budgets, the trajectory guard, bash policy, hook registries,
//! todo lists, and the identity/preset accessors.

use super::*;

/// Whether any message in `messages` carries an inline image attachment.
pub(crate) fn carries_images(messages: &[Message]) -> bool {
    messages.iter().any(|message| {
        message
            .images
            .as_ref()
            .is_some_and(|images| !images.is_empty())
    })
}

/// Strip every inline image attachment from `messages`, returning how many were
/// dropped. Text content is untouched.
///
/// The unconditional half of the image projection (ADR-0230): callers decide
/// *whether* the route can be given attachments and use
/// [`Agent::project_images_away_if_unusable`] for that. The image-cause probe
/// calls this directly, because it deliberately withholds attachments from a
/// route that has not been latched — testing that hypothesis is the experiment.
pub(crate) fn strip_images(messages: &mut [Message]) -> usize {
    let mut dropped = 0usize;
    for message in messages.iter_mut() {
        if let Some(images) = message.images.take() {
            dropped += images.len();
        }
    }
    dropped
}

impl Agent {
    /// Start configuring an agent from a flat tool list.
    pub fn builder(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        identity: AgentIdentity,
    ) -> AgentBuilder {
        AgentBuilder::new(
            provider,
            nuo_contracts::ToolSet::from_tools(tools),
            identity,
        )
    }

    /// Start configuring an agent from a full multi-variant tool set.
    pub fn builder_from_toolset(
        provider: Arc<dyn Provider>,
        toolset: nuo_contracts::ToolSet,
        identity: AgentIdentity,
    ) -> AgentBuilder {
        AgentBuilder::new(provider, toolset, identity)
    }

    /// Construct an agent from a flat tool list. The tools are grouped into a
    /// [`nuo_contracts::ToolSet`] (one capability per [`Tool::name`], one variant
    /// per [`Tool::variant`]) — the common case for a single-variant toolset or
    /// an already-resolved subagent toolset. Use [`Agent::from_toolset`] to
    /// preserve a multi-variant set so per-model variant selection can switch
    /// between variants at runtime.
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        identity: AgentIdentity,
    ) -> Self {
        Self::from_toolset(
            provider,
            nuo_contracts::ToolSet::from_tools(tools),
            identity,
        )
    }

    /// Construct an agent from a full [`nuo_contracts::ToolSet`], preserving every
    /// capability's variants so [`Agent::set_variant_selection`] can swap the
    /// model-visible variant at runtime.
    pub fn from_toolset(
        provider: Arc<dyn Provider>,
        toolset: nuo_contracts::ToolSet,
        identity: AgentIdentity,
    ) -> Self {
        Self::builder_from_toolset(provider, toolset, identity).build()
    }

    pub(super) fn from_toolset_with_model_request_assembler(
        provider: Arc<dyn Provider>,
        toolset: nuo_contracts::ToolSet,
        skills_registry: skills::SkillRegistry,
        identity: AgentIdentity,
        model_request_assembler: crate::model_request::ModelRequestAssembler,
    ) -> Self {
        let thread_id = Arc::new(std::sync::Mutex::new(None));

        let mut toolset = toolset;
        let round_counter = Arc::new(std::sync::Mutex::new(0u64));
        let todos = Arc::new(std::sync::Mutex::new(nuo_contracts::TodoList::default()));
        crate::tool_integration::install_agent_owned_tools(
            &mut toolset,
            Arc::clone(&todos),
            Arc::clone(&round_counter),
        );

        // Seed the model-visible view by resolving the pool for the live model
        // with no role restriction and no model variant overrides yet: the
        // master's identity selection (unrestricted) composed with the
        // model's capability limits. `set_variant_selection` re-resolves once
        // the model's `[tool_variants]` selection is known and on every switch.
        // The provider's capability snapshot (route-resolved, ADR-0149)
        // overrides the static registry's vision flag — a fitted relay model
        // the baseline does not know would otherwise lose its vision-gated
        // tools at seed time.
        let tools = nuo_contracts::ToolSelection::unrestricted();
        let capabilities = provider.model_capabilities();
        let seed_model = nuo_contracts::Model {
            vision: capabilities.accepts_images(),
            ..nuo_contracts::resolve_model(&provider.model())
        };
        let resolved_tools = Arc::new(std::sync::RwLock::new(toolset.resolve_for(
            &seed_model,
            &tools,
            &nuo_contracts::ToolSelection::unrestricted(),
        )));
        let dynamic_tools = Arc::new(crate::dynamic_tools::DynamicToolRegistry::default());
        let disabled_tools = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let scoped_disabled_tools = Arc::new(std::sync::Mutex::new(ScopedToolDisable::default()));
        let admit_mcp = Arc::new(std::sync::RwLock::new(vec!["*".to_string()]));
        // The unified ToolManager view owns the single authority for
        // classification, per-turn schema, and dispatch lookup. It shares the
        // storage Arcs with the agent so both reach the same live state. See
        // `tool_manager`.
        let tool_manager = crate::tool_manager::ToolManager::new(
            Arc::clone(&resolved_tools),
            Arc::clone(&dynamic_tools),
            Arc::clone(&disabled_tools),
            Arc::clone(&scoped_disabled_tools),
            Arc::clone(&admit_mcp),
        );

        let pool = Arc::new(std::sync::RwLock::new(nuo_contracts::ToolPool::new(
            toolset.clone(),
        )));

        Self {
            host: crate::host::KernelHost::none(),
            provider,
            execution_policy: std::sync::RwLock::new(
                nuo_contracts::ExecutionPolicy::root_default(),
            ),
            pool,
            toolset,
            resolved_tools,

            dynamic_tools,
            disabled_tools,
            scoped_disabled_tools,
            tool_manager,
            todos,
            round_counter,
            permissions: crate::permission_store::PermissionStore::new(),
            additional_workspace_roots: Vec::new(),
            workspace_security: Arc::new(std::sync::Mutex::new(
                nuo_contracts::WorkspaceSecuritySnapshot::default(),
            )),
            confinement: nuo_contracts::SharedConfinement::default(),
            project_rules: Arc::new(std::sync::RwLock::new(String::new())),
            skills_registry,
            thread_id,
            accounting_actor_id: std::sync::Mutex::new(
                nuo_contracts::token_ledger::ROOT_ACTOR_ID.to_string(),
            ),
            context_prune_threshold_tokens: Arc::new(std::sync::Mutex::new(0)),
            context_projection_gate: Arc::new(std::sync::Mutex::new(None)),
            images_suppressed: Arc::new(std::sync::RwLock::new(None)),
            hard_stop_turns: Arc::new(std::sync::Mutex::new(0)),
            trajectory_guard_config: Arc::new(std::sync::RwLock::new(
                nuo_contracts::TrajectoryGuardConfig::default(),
            )),
            interaction: Arc::new(crate::interaction::InteractionController::default()),
            human_broker: crate::human_broker::HumanRequestBroker::new(),
            bash_policy: std::sync::RwLock::new(crate::bash_policy::BashPolicy::default()),

            hooks: crate::hook_runner::HookRunner::new(),
            inbox_tx: std::sync::Mutex::new(None),
            inbox_rx: std::sync::Mutex::new(None),
            session_queues: std::sync::Mutex::new(None),
            steering_mode: std::sync::RwLock::new(nuo_contracts::QueueMode::default()),
            follow_up_mode: std::sync::RwLock::new(nuo_contracts::QueueMode::default()),
            round_paused_ms: std::sync::atomic::AtomicU64::new(0),
            identity: std::sync::RwLock::new(identity),
            turn_persist: std::sync::Mutex::new(None),
            request_projection_persist: std::sync::Mutex::new(None),
            title_established: std::sync::Mutex::new(None),
            model_request_assembler,
            variant_selection: Arc::new(std::sync::Mutex::new(
                nuo_contracts::VariantSelection::new(),
            )),
            tools: std::sync::Mutex::new(tools),
            token_ledger: std::sync::Mutex::new(None),
            token_weights: std::sync::Arc::new(nuo_contracts::MessageTokenWeights::new()),
            tool_schema_weights: std::sync::Arc::new(nuo_contracts::ToolSchemaWeights::new()),
            extensions: Arc::new(std::sync::RwLock::new(vec![Arc::new(
                crate::extension::CodeIntelligenceExtension,
            )])),
            active_role: std::sync::RwLock::new(Some("developer".to_string())),
        }
    }

    /// Context-pressure threshold (in tokens) for mid-turn relief. `0` (the
    /// default) disables the mid-turn [`ContextProjectionGate`]. Re-seed on provider
    /// switch so the threshold tracks the new model's context window.
    pub fn set_context_prune_threshold(&self, budget_tokens: usize) {
        *self
            .context_prune_threshold_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = budget_tokens;
    }

    /// Replace the per-model tool-variant selection and re-resolve the
    /// model-visible toolset to match. Seeded from `[tool_variants."<model-id>"]`
    /// config and re-applied on model switch so the resolved variants — and the
    /// live model's hard capability limits (e.g. vision) — always track the
    /// live model. An empty map (the default) realizes every capability with its
    /// model-chosen / default variant.
    pub fn set_variant_selection(&self, selection: nuo_contracts::VariantSelection) {
        self.reresolve_tools(&selection);
        *self
            .variant_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selection;
    }

    /// Replace this agent's identity-side selection (capability scope + variant
    /// pins) and re-resolve the model-visible toolset. The master is
    /// unrestricted by default; this narrows it (e.g. confining a role-bound
    /// master to a capability subset). The current per-model variant
    /// selection is preserved and re-composed.
    pub fn set_tools(&self, selection: nuo_contracts::ToolSelection) {
        *self.tools.lock().unwrap_or_else(|e| e.into_inner()) = selection;
        let model_variants = self
            .variant_selection
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.reresolve_tools(&model_variants);
    }

    /// Re-resolve [`resolved_tools`](Self::resolved_tools) from the pool for the
    /// live model, composing this agent's identity selection with the model's
    /// selection (`model_variants` overrides + the model's hard capability
    /// limits). The single choke point through which both the master seed and
    /// every model/selection switch flow, so the schema sent to the provider and
    /// the dispatch table always reflect `agent_scope ∩ model_caps`.
    fn reresolve_tools(&self, model_variants: &nuo_contracts::VariantSelection) {
        // The provider's own capability snapshot is the authority — it is the
        // full ADR-0149 route resolution (`Channel::capabilities()`: baseline ⊕
        // remote advertisement ⊕ user overrides). Resolving the model id
        // through the static registry here would miss the fitted-model overlay
        // and per-route overrides, and silently drop vision-gated tools (e.g.
        // `read_image`) from a relay model that is in fact vision-capable.
        let capabilities = self.provider.model_capabilities();
        let model = nuo_contracts::Model {
            vision: capabilities.accepts_images(),
            ..nuo_contracts::resolve_model(&self.provider.model())
        };
        let tools = self.tools.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let model_selection =
            nuo_contracts::ToolSelection::unrestricted().with_variants(model_variants.clone());
        *self
            .resolved_tools
            .write()
            .unwrap_or_else(|e| e.into_inner()) =
            self.toolset.resolve_for(&model, &tools, &model_selection);
    }

    /// Every currently installed tool, including dynamic sources. Static
    /// capabilities win name collisions; dynamic source order is deterministic.
    pub fn installed_tools(&self) -> Vec<Arc<dyn Tool>> {
        // Delegate to the unified ToolManager — the single authority for the
        // two-bucket classification (builtin/mcp) and name-clash priority.
        self.tool_manager
            .installed()
            .into_iter()
            .map(|s| s.tool)
            .collect()
    }

    /// The unified tool manager (kimi-code port). Exposed so the dispatcher /
    /// model-request assembly can call its authoritative methods directly.
    pub(crate) fn tool_manager(&self) -> &crate::tool_manager::ToolManager {
        &self.tool_manager
    }

    /// The permission policy chain for this agent. Built fresh per call.
    /// Holds the complete authority policy chain. Interaction-only behavior
    /// (`ask_user`, stdin, and missing-authority prompting) stays in
    /// `execute_tool`, outside the authority context by construction.
    pub(crate) fn permission_chain(&self) -> crate::permission_policy::PermissionChain {
        crate::permission_policy::PermissionChain::new(crate::permission_policy::default_chain())
    }

    /// Snapshot the live state available to declarative system-prompt policy.
    fn system_prompt_context(&self, tools: &[Arc<dyn Tool>]) -> crate::SystemPromptContext {
        let mut tool_names: Vec<String> =
            tools.iter().map(|tool| tool.name().to_string()).collect();
        tool_names.sort();
        let model_guidance = nuo_contracts::resolve_model(&self.provider.model()).model_guidance;
        let provider_guidance = self.provider.prompt_hints().system_guidance;

        crate::SystemPromptContext {
            identity_preamble: self
                .identity
                .read()
                .map(|guard| guard.preamble())
                .unwrap_or_default(),
            tool_names,
            has_subagent_tool: tools.iter().any(|tool| tool.spawns_subagent()),
            model_guidance,
            provider_guidance,
            project_rules: self
                .project_rules
                .read()
                .map(|rules| rules.clone())
                .unwrap_or_default(),
            additional_workspace_roots: self
                .additional_workspace_roots
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            workspace_root: self.workspace_root().map(|p| p.display().to_string()),
        }
    }

    /// Build one immutable provider request from a borrowed conversation window.
    ///
    /// No disk I/O happens here, ever: `@file:` injection ran once when the
    /// prompt entered the live window (turn preparation), so this projection
    /// — and every estimate built on it — reuses those bytes. The weights
    /// cache makes the resulting request cheap to re-estimate; nothing in
    /// this path can stall the executor behind filesystem reads.
    ///
    /// Image visibility is decided **here, per request** (ADR-0230), never on
    /// the durable transcript: a route that declares no image input, or one the
    /// harness has already seen reject an image, has its attachments projected
    /// away for this request only. That is what makes switching to a text-only
    /// model mid-session survivable even when the history carries images.
    pub(crate) fn model_request(&self, messages: &[Message]) -> nuo_contracts::ModelRequest {
        // One clone of the provider-relevant window: system rows are rare, so
        // filtering first halves the per-turn memcpy of the old
        // clone-then-clone pipeline. Skill injection still sees the
        // pre-echo-filter copy (same scan semantics as before); the echo and
        // empty-assistant filters then run in place — no second clone.
        let mut enriched: Vec<Message> = messages
            .iter()
            .filter(|message| message.role != Role::System)
            .cloned()
            .collect();
        // ADR-0288 `[INV-REF-03]`/`[INV-REF-04]`: canonicalize `@`-addresses
        // and consume suppressed escapes on the provider-facing view only, and
        // only where the user actually wrote a reference. Harness-injected
        // messages (the hidden `<file>`/`<skill>` envelopes and every note) are
        // resolved asset content, not reference sites — rewriting them would
        // corrupt the asset (a referenced file whose body contains `@files:` or
        // `\@file:` must reach the model verbatim). The durable `messages` are
        // untouched, so the transcript stays verbatim (ADR-0050's fidelity
        // axis). Handled here — not at injection — because this runs for every
        // turn, including ones whose mentions were already resolved earlier.
        for message in enriched.iter_mut() {
            if crate::conversation_context::is_reference_site(message) {
                message.content =
                    crate::conversation_context::canonicalize_addresses(&message.content);
            }
        }
        // Skill injection is memory-only (bodies are cached in the registry),
        // so keeping it here costs no I/O and keeps the debug preview honest
        // about implicit loads.
        crate::conversation_context::inject_mentioned_skills(&self.skills_registry, &mut enriched);
        crate::agent::remove_empty_assistant_messages(&mut enriched);
        enriched.retain(|message| !message.is_command_echo());
        self.project_images_away_if_unusable(&mut enriched);

        // `E_n` (the request-local temporary context) is empty by default
        // (ADR-0213/ADR-0214/ADR-0217): code structure is delivered on demand
        // via `code_query`, not as an automatic ambient Repo Map. The facet
        // loop remains the generic extension point for explicitly-approved,
        // budgeted temporary-context producers. A projection is bounded and
        // stays strictly request-local: it travels in `temporary_context`,
        // never in `messages`, so it is excluded from the cacheable prefix and
        // is never committed to the durable transcript.
        let mut temporary_context: Vec<Message> = Vec::new();
        let ws_root = self.workspace_root();
        let hook_ctx =
            nuo_contracts::extension::HookContext::temporary_context(ws_root.as_deref());
        for extension in self.extensions() {
            if let nuo_contracts::extension::HookOutcome::TemporaryContext(projection) = extension
                .run(
                    nuo_contracts::HookPhase::ProjectTemporaryContext,
                    &hook_ctx,
                )
            {
                if projection.is_empty() {
                    continue;
                }
                temporary_context.push(crate::conversation_context::hidden_user(
                    nuo_contracts::InjectionKind::SystemReminder,
                    bound_temporary_context(projection),
                ));
            }
        }

        let tools = self.visible_tools();
        let context = self.system_prompt_context(&tools);
        self.model_request_assembler
            .assemble_prepared(enriched, temporary_context, &context, &tools)
            .with_route_state(
                &self.provider.route_fingerprint(),
                self.provider.continuation_mode(),
            )
    }

    /// Compile a [`nuo_contracts::ModelRequest`] directly from a canonical [`nuo_contracts::SessionIR`]
    /// via the multi-pass compiler pipeline (ADR-0241/ADR-0249, INV-EXEC-02).
    pub fn model_request_from_ir(
        &self,
        ir: &nuo_contracts::SessionIR,
    ) -> Result<nuo_contracts::CompilationArtifact, nuo_contracts::CompilerError> {
        let mut temporary_context: Vec<Message> = Vec::new();
        let ws_root = self.workspace_root();
        let hook_ctx =
            nuo_contracts::extension::HookContext::temporary_context(ws_root.as_deref());
        for extension in self.extensions() {
            if let nuo_contracts::extension::HookOutcome::TemporaryContext(projection) = extension
                .run(
                    nuo_contracts::HookPhase::ProjectTemporaryContext,
                    &hook_ctx,
                )
                && !projection.is_empty()
            {
                temporary_context.push(crate::conversation_context::hidden_user(
                    nuo_contracts::InjectionKind::SystemReminder,
                    bound_temporary_context(projection),
                ));
            }
        }

        let tools = self.visible_tools();
        let dialect = Some(self.provider.provider_id().to_string());
        let protocol = self.provider.wire_protocol();
        self.model_request_assembler
            .compile_from_ir(ir, temporary_context, &tools, dialect, protocol)
            .map(|mut artifact| {
                artifact.request = artifact.request.with_route_state(
                    &self.provider.route_fingerprint(),
                    self.provider.continuation_mode(),
                );
                artifact
            })
    }

    /// Project inline images away when this route cannot be given them
    /// (ADR-0230). Two independent reasons qualify, and both are *projection*
    /// facts rather than history edits:
    ///
    /// - the route **declared** no image input (the wire builders strip these
    ///   too, for callers that bypass the harness — this keeps a single
    ///   observable behavior regardless of transport);
    /// - the harness has already **learned** that this route rejects them
    ///   ([`Self::suppress_images_for_current_route`]).
    ///
    /// Returns the number of attachments dropped, so a caller can report it.
    /// Only the pixels go: the prose of each message is preserved verbatim, so
    /// the conversation still reads correctly to a text-only model.
    ///
    /// The image-cause *probe* uses [`strip_images`] instead: it must withhold
    /// attachments from a route that is not yet latched, because testing that
    /// hypothesis is the point of the experiment.
    pub(crate) fn project_images_away_if_unusable(&self, messages: &mut [Message]) -> usize {
        // The overwhelmingly common case is a request with no attachments at
        // all: answer it with one scan and no capability lookup, so the
        // per-request projection stays free for text-only traffic.
        if !carries_images(messages) {
            return 0;
        }
        let declared_false = !self.provider.model_capabilities().accepts_images();
        let learned = self.images_suppressed_for_current_route();
        if !declared_false && !learned {
            return 0;
        }
        let dropped = strip_images(messages);
        tracing::debug!(
            model = %self.provider.model(),
            dropped,
            declared_false,
            learned,
            "projecting images away from a request this route cannot take them on"
        );
        dropped
    }

    /// Whether images are currently being withheld from the active route,
    /// whether because the route declared no image input or because a rejection
    /// taught us so.
    pub fn images_withheld_from_current_route(&self) -> bool {
        !self.provider.model_capabilities().accepts_images()
            || self.images_suppressed_for_current_route()
    }

    /// Whether a *learned* suppression is armed for the **current** route.
    ///
    /// The latch is keyed by [`nuo_contracts::RouteFingerprint`], so a model or
    /// endpoint switch disarms it by construction: the correction belongs to the
    /// route that demonstrated it, never to the model id in the abstract
    /// (ADR-0149/ADR-0230).
    pub fn images_suppressed_for_current_route(&self) -> bool {
        let current = self.provider.route_fingerprint();
        self.images_suppressed
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_deref()
            == Some(current.0.as_str())
    }

    /// Learn that the active route rejects image input, and withhold images
    /// from it from now on (ADR-0230).
    ///
    /// Called when a provider refuses a request with an image-shaped error while
    /// that request carried attachments. Recording it as a *route* fact is what
    /// keeps a session alive across a model switch that leaves images in the
    /// durable history: the transcript is append-only (ADR-0186), so the only
    /// thing that may change is what a request projects.
    ///
    /// Returns `true` when this call armed the latch (i.e. the route was not
    /// already suppressed), so the caller can decide whether to retry.
    pub fn suppress_images_for_current_route(&self) -> bool {
        let current = self.provider.route_fingerprint();
        let mut guard = self
            .images_suppressed
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if guard.as_deref() == Some(current.0.as_str()) {
            return false;
        }
        tracing::info!(
            model = %self.provider.model(),
            provider = %self.provider.provider_id(),
            "route rejected image input; withholding images from this route"
        );
        *guard = Some(current.0);
        true
    }

    /// Weights for an assembled request through the session-wide
    /// content-addressed cache. Semantics are byte-identical to tokenizing
    /// every message afresh — only the cost model changed: each message's
    /// bytes are BPE-tokenized exactly once per session lifetime (messages
    /// are immutable once written), and every later estimate reuses the
    /// cached weight. The dominant per-pass cost of an estimate therefore
    /// scales with *new* bytes, not total session bytes.
    pub(super) fn layered_weights(
        &self,
        request: &nuo_contracts::ModelRequest,
    ) -> nuo_contracts::LayeredRequestWeights {
        nuo_contracts::layered_request_weights(
            request,
            &self.token_weights,
            &self.tool_schema_weights,
        )
    }

    pub(super) fn estimate_model_request(
        &self,
        request: &nuo_contracts::ModelRequest,
    ) -> RequestTokenEstimate {
        let weights = self.layered_weights(request);
        // Per-message wire weight (not `estimate_tokens`, which intentionally
        // includes persisted subagent children the provider never sees).
        let history_tokens = weights.history_tokens(&request.messages);
        let prepared_message_tokens = weights.prepared_tokens();
        let tool_schema_tokens = weights.tool_schema_tokens;
        let total_tokens = prepared_message_tokens.saturating_add(tool_schema_tokens);

        RequestTokenEstimate {
            history_tokens,
            overhead_tokens: total_tokens.saturating_sub(history_tokens),
            total_tokens,
            temporary_context_tokens: weights.temporary_context_tokens,
        }
    }

    /// Estimate the complete next request at the same immutable request
    /// boundary the provider call uses.
    pub fn estimate_next_request_tokens(&self, messages: &[Message]) -> RequestTokenEstimate {
        self.estimate_model_request(&self.model_request(messages))
    }

    /// Dev-only dry run: rebuild the head system message and auto-load any
    /// skills mentioned in the latest visible user round against a borrowed
    /// message list, exactly as the next turn would, but with no provider call
    /// and no mutation of live round history. Powers
    /// the `/debug preview` so it captures the *real* request shape —
    /// including the freshly composed system prompt and injected skills —
    /// rather than a degenerate reconstruction.
    pub fn prepare_request_messages_debug(&self, messages: &mut Vec<Message>) {
        let req = self.model_request(messages);
        let mut debug_messages = Vec::new();
        if !req.instructions.is_empty() {
            debug_messages.push(
                Message::new(Role::System, req.instructions.render_combined()).with_origin(
                    nuo_contracts::InjectionOrigin::new(
                        nuo_contracts::InjectionKind::SystemPrompt,
                    ),
                ),
            );
        }
        debug_messages.extend(req.messages);
        *messages = debug_messages;
    }

    /// A shared handle to this agent's live variant selection (the **override**
    /// axis). Handed to a spawned subagent's dispatch tool so the subagent — an
    /// agent on the same model — resolves its admitted capabilities to the same
    /// variants the parent uses, tracking model switches live. The profile still
    /// owns the orthogonal **scope** axis.
    pub fn variant_selection_handle(
        &self,
    ) -> Arc<std::sync::Mutex<nuo_contracts::VariantSelection>> {
        Arc::clone(&self.variant_selection)
    }

    /// Override the opt-in hard-stop budget. Mirrors `[agent] hard_stop_turns`
    /// in `config.toml` but can be flipped at runtime. `0` (the default) leaves
    /// the round uncapped, matching ADR-0009. The reviewer subagent gets a
    /// tight non-zero bound so a runaway diagnostic cannot loop.
    pub fn set_hard_stop_turns(&self, turns: usize) {
        *self
            .hard_stop_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = turns;
    }

    /// Current hard-stop budget. Read by the `/hard-stop` slash command (if
    /// present) and by `check_hard_stop` at each ReAct-turn boundary.
    pub fn get_hard_stop_turns(&self) -> usize {
        *self
            .hard_stop_turns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Replace the live trajectory-guard configuration atomically. The next round
    /// reconstructs its per-round guard from the new settings; the current
    /// round, if any, keeps its already-built guard state.
    ///
    /// Wired from `[agent.trajectory_guard]` in `config.toml` at startup and forced to
    /// [`nuo_contracts::TrajectoryGuardConfig::disabled`] on subagents and the review
    /// diagnostic so they run unobstructed regardless of user settings.
    pub fn set_trajectory_guard_config(&self, config: nuo_contracts::TrajectoryGuardConfig) {
        *self
            .trajectory_guard_config
            .write()
            .unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// Snapshot of the live trajectory-guard configuration. The turn boundary reads
    /// `enabled` to gate the pre-dispatch trajectory check.
    pub fn trajectory_guard_config(&self) -> nuo_contracts::TrajectoryGuardConfig {
        *self
            .trajectory_guard_config
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the trajectory guard is currently armed (allowed to block). Convenience
    /// wrapper over [`Self::trajectory_guard_config`] for the turn-boundary fast path.
    pub fn trajectory_guard_enabled(&self) -> bool {
        self.trajectory_guard_config().enabled
    }

    /// Enable or disable the model-supplied-stdin path for `bash` (L3.5 α).
    /// Mirrors `[agent] allow_model_stdin` in `config.toml`. When off
    /// (the default), the bash schema exposes no `stdin` parameter and a
    /// command needing input either gets it from a human (interactive
    /// classifier → input panel) or fails fast. When on, the model may feed
    /// a command's stdin directly — for delegated automatic flows.
    pub fn set_allow_model_stdin(&self, enabled: bool) {
        self.interaction.set_allow_model_stdin(enabled);
    }

    /// Replace the command-aware bash safety policy from `[bash_policy]` config.
    /// Built-in dangerous-command rules remain compiled into the policy; config
    /// only supplies toggles and user-defined overrides/additions.
    pub fn set_bash_policy(&self, config: &nuo_persistence::config::BashPolicyConfig) {
        let policy = crate::bash_policy::BashPolicy::from_config(config);
        for error in policy.invalid_rules() {
            tracing::warn!(error = %error, "ignoring invalid bash policy rule");
        }
        *self.bash_policy.write().unwrap_or_else(|e| e.into_inner()) = policy;
    }

    /// Whether the model may supply stdin for a `bash` call. Read at the
    /// dispatch site to decide the [`InputContract`](nuo_contracts::InputContract)
    /// and whether the bash schema exposes a `stdin` parameter.
    pub fn allow_model_stdin(&self) -> bool {
        self.interaction.allow_model_stdin()
    }

    /// Mirrors `[agent] skip_interactive_input` in `config.toml`. When on,
    /// an interactive command (matched by the interactive classifier) is
    /// never supervised — it runs sealed (immediate-EOF stdin), failing fast
    /// with a non-interactive remedy, as in unattended mode. Lets an operator
    /// who finds the prompt disruptive opt out without turning the agent
    /// itself unattended.
    pub fn set_skip_interactive_input(&self, enabled: bool) {
        self.interaction.set_skip_interactive_input(enabled);
    }

    /// Whether an interactive command should skip operator-input supervision
    /// and run sealed instead. Read at the command dispatch site to decide the
    /// [`InputContract`](nuo_contracts::InputContract).
    pub fn skip_interactive_input(&self) -> bool {
        self.interaction.skip_interactive_input()
    }

    /// Install (or clear with `None`) the mid-turn model-context projection gate.
    pub fn set_context_projection_gate(&self, gate: Option<Arc<dyn ContextProjectionGate>>) {
        *self
            .context_projection_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = gate;
    }

    /// Install the shared token-source ledger so this agent books each turn's
    /// token counts (reported vs. estimated) into it. The embedding shares the
    /// same `Arc` with the TUI so the token-source report modal reads live.
    /// No-op for subagents/tests that never call this (the ledger stays `None`
    /// and booking is skipped).
    pub fn install_token_ledger(&self, ledger: Arc<nuo_contracts::TokenSourceLedger>) {
        *self.token_ledger.lock().unwrap_or_else(|e| e.into_inner()) = Some(ledger);
    }

    /// A handle to the token-source ledger, if one was installed. The TUI uses
    /// this to snapshot the report for the modal.
    pub fn token_ledger(&self) -> Option<Arc<nuo_contracts::TokenSourceLedger>> {
        self.token_ledger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Shared handle to the agent's content-addressed token-weights cache.
    /// Handed to off-executor estimate tasks and context-projection gates so
    /// every estimate path — wire projection, prune, mid-turn pressure —
    /// pays each message's BPE cost exactly once per session.
    pub fn token_weights_handle(&self) -> Arc<nuo_contracts::MessageTokenWeights> {
        std::sync::Arc::clone(&self.token_weights)
    }

    /// Book one turn's token usage into [`RoundState::token_usage`] and, when a
    /// ledger is installed, into the token-source ledger.
    ///
    /// `reported_usage` is the usage carried by the request's completion
    /// metadata (or a legacy mid-stream usage event), if any. When absent, we
    /// fall back to the local estimator.
    ///
    /// This is the single point that decides whether a turn counts as
    /// **reported** (authoritative) or **estimated** (heuristic), and records
    /// that classification so the token-source report modal can render it.
    pub(super) fn book_turn_usage(
        &self,
        state: &mut RoundState,
        response: &Message,
        reported_usage: Option<TokenUsage>,
        request: &mut RequestAccountingGuard,
    ) -> nuo_contracts::TurnPerformanceSnapshot {
        // Seal the generation clock now, while we hold a validated assistant
        // response and before any tool dispatch, so tool execution never
        // inflates the measured generation span.
        request.seal_generation();
        state.generation_ms = state.generation_ms.saturating_add(request.generation_ms);
        let reported = reported_usage;
        // Any streamed-but-unfinalized tail (the last open pretoken) belongs
        // to the completion count too: close the incremental counter before
        // settling so the estimate matches what a whole-text count would say.
        // (This is a maximum, never a downgrade: `finish_output` keeps the
        // larger of the finalized total and the already-observed count.)
        request.finish_output();
        if let Some(usage) = reported {
            state.token_usage.total_tokens += usage.total_tokens;
            state.token_usage.prompt_tokens += usage.prompt_tokens;
            state.token_usage.completion_tokens += usage.completion_tokens;
            state.token_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            state.token_usage.cache_read_input_tokens += usage.cache_read_input_tokens;
            state.token_usage.cache_miss_input_tokens += usage.cache_miss_input_tokens;
            request.settle(
                nuo_contracts::RequestUsageStatus::Completed,
                Some(usage),
                0,
            );
            request.performance_snapshot(
                usage.completion_tokens,
                nuo_contracts::RequestUsageSource::Reported,
            )
        } else {
            // Estimate both sides of the request. The old fallback counted
            // only the assistant response while the reported path counted
            // prompt + completion, making mixed-source totals incomparable.
            let completion = pressure::estimate_message_tokens(response).max(0);
            let prompt = request.projected_prompt_tokens.max(0);
            let estimated = prompt.saturating_add(completion);
            state.token_usage.total_tokens += estimated;
            state.token_usage.prompt_tokens += prompt;
            state.token_usage.completion_tokens += completion;
            request.settle(
                nuo_contracts::RequestUsageStatus::Completed,
                None,
                completion,
            );
            request.performance_snapshot(completion, nuo_contracts::RequestUsageSource::Estimated)
        }
    }

    /// Install the lifecycle hook registry (ADR-0025). Replaces any prior
    /// registry; intended to be called once at startup after the `[hooks]`
    /// config is parsed. Subagents and tests leave the default empty registry.
    pub fn set_hooks(&self, registry: crate::hooks::HookRegistry) {
        self.hooks.set(registry);
    }

    /// Install the mid-round save point fired at every ReAct-turn boundary
    /// (ADR-0048). The closure receives the current full round history and
    /// should durably append only the new tail (see
    /// `SessionStore::append_turn`). Called once by orchestration after the
    /// agent is built and the session is open; subagents and the review
    /// diagnostic never call this, so the default `None` keeps their turn
    /// boundaries no-ops.
    pub fn set_turn_persist(&self, f: TurnPersistFn) {
        *self.turn_persist.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Install the title-established observer fired by the background session
    /// titler (ADR-0022). The closure receives the freshly persisted title
    /// and should push a sessions-overview snapshot so attached clients see
    /// the new picker title without reopening the dialog. Called once by the
    /// session driver; subagents, the review diagnostic, and tests never call
    /// this, so titling stays a silent background write there.
    pub fn set_title_established(&self, f: TitleEstablishedFn) {
        *self
            .title_established
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Fire the mid-round save point if installed. Returns `Ok(())` when no
    /// closure is set (the subagent / review / test path) so the call site
    /// stays unconditional. Invoked at the turn boundary — after a turn's
    /// tool results are in `messages` and before the next model request.
    pub(super) async fn fire_turn_persist(&self, messages: &[Message]) -> Result<(), HarnessError> {
        let f = self
            .turn_persist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match f {
            Some(f) => f(messages).await.map_err(|error| {
                HarnessError::Other(format!("could not persist mid-round turn: {error}"))
            }),
            None => {
                if let Some(ledger) = self.token_ledger() {
                    ledger
                        .persist_pending(&self.thread_id().unwrap_or_default())
                        .await
                        .map_err(HarnessError::Other)?;
                }
                Ok(())
            }
        }
    }

    /// Install the request-projection archive sink (ADR-0218). Called by the
    /// session driver; `None` (subagents, tests) makes recording a no-op.
    pub fn set_request_projection_persist(&self, f: RequestProjectionFn) {
        *self
            .request_projection_persist
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// Fire the request-projection archive sink if installed. Synchronous and
    /// infallible: the sink only enqueues the record, so request dispatch is
    /// never blocked or failed by forensic persistence.
    pub(super) fn fire_request_projection_persist(
        &self,
        projection: nuo_contracts::RequestProjection,
    ) {
        let f = self
            .request_projection_persist
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(f) = f {
            f(projection);
        }
    }

    /// Snapshot the hook registry as a cheap `Arc` clone, so insertion points
    /// fire hooks without holding the swap lock across the async `fire`.
    pub(super) fn hooks(&self) -> Arc<crate::hooks::HookRegistry> {
        self.hooks.get()
    }

    /// The session id hooks see (the live thread id, if any).
    pub(super) fn hook_session_id(&self) -> String {
        self.thread_id().unwrap_or_default()
    }

    /// The cwd hooks run under (the persisted project root, if any).
    pub(super) fn hook_cwd(&self) -> Option<std::path::PathBuf> {
        self.workspace_root()
    }

    /// The persisted project root — the workspace sandbox for `@file:` injection
    /// and the base relative file-tool paths resolve against. `None` when no
    /// project was designated (subagents, tests, or a detached session), in which
    /// case file injection is disabled.
    /// Record the session's additional workspace roots (ADR-0142). Called
    /// once by the assembling bootstrap after they validate; they surface to
    /// the model through the `WorkspaceRootsGuidance` system-prompt section.
    pub fn set_additional_workspace_roots(&mut self, roots: Vec<std::path::PathBuf>) {
        self.additional_workspace_roots = roots;
    }

    /// The session's additional workspace roots, if any (ADR-0142).
    pub fn additional_workspace_roots(&self) -> &[std::path::PathBuf] {
        &self.additional_workspace_roots
    }

    pub fn workspace_root(&self) -> Option<std::path::PathBuf> {
        self.permissions.project_root()
    }

    // Public hook entry points (ADR-0025)
    // The PreToolUse / PostToolUse / Stop insertion points are inline in the
    // loop above (they need local control flow); the lifecycle entry points
    // below are called by the driver / orchestration at the session, turn, and
    // compaction boundaries.

    /// `UserPromptSubmit` gate. Called by `execute_round` before the prompt
    /// enters the transcript: a `Deny` drops it, a `Prepend` prefixes context.
    pub async fn fire_user_prompt_submit(&self, prompt: &str) -> crate::hooks::UserPromptVerdict {
        self.hooks()
            .check_user_prompt_submit(prompt, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `PreCompact` observers. Returns any injected context to fold into the
    /// upcoming summarization (ADR-0025).
    pub async fn fire_pre_compact(&self) -> Vec<String> {
        self.hooks()
            .pre_compact(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `PostCompact` observers. Informational only.
    pub async fn fire_post_compact(&self) {
        self.hooks()
            .post_compact(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// `SessionStart` observers; injected context becomes hidden setup messages.
    pub async fn fire_session_start(
        &self,
        source: nuo_contracts::SessionSource,
        messages: &mut Vec<Message>,
    ) {
        self.hooks()
            .session_start(
                source,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
                messages,
            )
            .await
    }

    /// `SessionEnd` observers. Informational only.
    pub async fn fire_session_end(&self) {
        self.hooks()
            .session_end(&self.hook_session_id(), self.hook_cwd().as_deref())
            .await
    }

    /// Between ReAct turns, if context pressure exceeds the configured budget,
    /// hand the live message list to the [`ContextProjectionGate`] so it can
    /// produce and persist the next model-visible window.
    /// Gate the assembled request on context pressure. Takes the
    /// already-assembled request so the turn's estimate reuses it instead of
    /// rebuilding the full request a second time (ADR-0187 hot path).
    /// Returns `true` when the projection replaced `messages` (the caller
    /// must re-assemble before calling the provider).
    pub(super) async fn project_context_if_needed(
        &self,
        messages: &mut Vec<Message>,
        request: &nuo_contracts::ModelRequest,
        cancel: &CancellationToken,
    ) -> Result<bool, HarnessError> {
        let budget = *self
            .context_prune_threshold_tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // With the content-addressed weights caches warm, the estimate here
        // is a fingerprint walk over unchanged bytes plus O(new bytes) of
        // fresh BPE — cheap enough to stay inline; the gate runs before the
        // provider call, never concurrently with stream forwarding.
        if budget == 0 || self.estimate_model_request(request).total_tokens <= budget {
            return Ok(false);
        }
        let gate = self
            .context_projection_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(gate) = gate else {
            return Ok(false);
        };
        let replacement = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(HarnessError::Interrupted),
            replacement = gate.project_context(messages.clone()) => replacement,
        };
        if let Some(replacement) = replacement
            && !replacement.is_empty()
        {
            *messages = replacement;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn set_thread_id(&self, thread_id: impl Into<String>) {
        *self.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(thread_id.into());
    }

    pub fn thread_id_handle(&self) -> Arc<std::sync::Mutex<Option<String>>> {
        Arc::clone(&self.thread_id)
    }

    pub fn round_counter_handle(&self) -> Arc<std::sync::Mutex<u64>> {
        Arc::clone(&self.round_counter)
    }

    pub fn set_accounting_actor_id(&self, actor_id: impl Into<String>) {
        *self
            .accounting_actor_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = actor_id.into();
    }

    pub(super) fn accounting_actor_id(&self) -> String {
        self.accounting_actor_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear_thread_id(&self) {
        *self.thread_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Current task list snapshot. Read by the harness to mirror into the
    /// session and by the TUI to render the sticky panel.
    pub fn todos(&self) -> nuo_contracts::TodoList {
        self.todos.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the task list. Used by session-restore paths on resume.
    pub fn set_todos(&self, todos: nuo_contracts::TodoList) {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = todos;
    }

    /// Drop the task list.
    pub fn clear_todos(&self) {
        *self.todos.lock().unwrap_or_else(|e| e.into_inner()) = nuo_contracts::TodoList::default();
    }

    /// Access the harness internal task pipeline for out-of-band typed execution (ADR-0211).
    pub fn harness_tasks(&self) -> crate::cognitive::HarnessTaskPipeline {
        crate::cognitive::HarnessTaskPipeline::new(self.provider.clone())
    }

    /// Access the harness cognitive pipeline for out-of-band typed execution.
    pub fn cognitive(&self) -> crate::cognitive::CognitivePipeline {
        crate::cognitive::CognitivePipeline::new(self.provider.clone())
    }

    /// Access the Spatiotemporal Aspect Engine governing lifecycle phases (ADR-0183 / ADR-0211).
    pub fn aspects(&self) -> crate::aspects::AspectEngine {
        crate::aspects::AspectEngine::new(self.harness_tasks())
    }

    /// Access the atomic extensions bound to this agent (ADR-0224).
    pub fn extensions(&self) -> Vec<Arc<dyn nuo_contracts::Extension>> {
        self.extensions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Add an atomic extension to this agent (ADR-0224).
    pub fn add_extension(&self, extension: Arc<dyn nuo_contracts::Extension>) {
        self.extensions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(extension);
    }
}

/// Hard byte budget for a single request-local temporary-context producer
/// (ADR-0213 §2). Temporary context is optional enrichment, not a channel for
/// bulk repository state, so the budget is deliberately small.
pub(crate) const TEMPORARY_CONTEXT_BUDGET_BYTES: usize = 8 * 1024;

/// Bound a producer's output to [`TEMPORARY_CONTEXT_BUDGET_BYTES`], splitting on
/// a UTF-8 boundary and disclosing the truncation. Keeping this in the assembly
/// path means a facet cannot widen the request without an explicit budget.
fn bound_temporary_context(mut text: String) -> String {
    if text.len() <= TEMPORARY_CONTEXT_BUDGET_BYTES {
        return text;
    }
    let mut cut = TEMPORARY_CONTEXT_BUDGET_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str("\n[temporary context truncated to its byte budget]");
    text
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn short_temporary_context_is_unchanged() {
        let text = "outline".to_string();
        assert_eq!(bound_temporary_context(text.clone()), text);
    }

    #[test]
    fn oversized_temporary_context_is_bounded_and_disclosed() {
        let text = "x".repeat(TEMPORARY_CONTEXT_BUDGET_BYTES + 100);
        let bounded = bound_temporary_context(text);
        assert!(bounded.len() <= TEMPORARY_CONTEXT_BUDGET_BYTES + 64);
        assert!(bounded.ends_with("[temporary context truncated to its byte budget]"));
    }

    /// ADR-0218: the archive sink fires with the record and a write failure is
    /// non-fatal (forensic, never authoritative).
    #[tokio::test]
    async fn request_projection_sink_fires_and_is_non_fatal() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let agent = crate::Agent::new(
            std::sync::Arc::new(crate::NoProvider),
            Vec::new(),
            crate::AgentIdentity::new("dev", "developer"),
        );
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        agent.set_request_projection_persist(std::sync::Arc::new(move |_record| {
            seen.fetch_add(1, Ordering::SeqCst);
        }));

        agent.fire_request_projection_persist(nuo_contracts::RequestProjection {
            round: 1,
            turn: 0,
            created_at_ms: 0,
            prefix_fingerprint: "sha256:test".to_string(),
            conversation_messages: 0,
            temporary_context_tokens: 0,
            temporary_context: Vec::new(),
        });

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
