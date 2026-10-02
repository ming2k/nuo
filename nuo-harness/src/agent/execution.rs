//! Tool-execution tail of [`Agent`] rounds: output normalization, image
//! plumbing, and the PostToolUse/PostToolUseFailure hook fire points.

use super::*;

impl Agent {
    /// Fire PostToolUse (success) or PostToolUseFailure (error) hooks and append
    /// any injected context as hidden user messages (ADR-0025). No-op when the
    /// registry is empty, which is the common case (subagents, tests, no
    /// `[hooks]` config).
    pub(crate) async fn run_post_tool_hooks(
        &self,
        call: &ToolCall,
        result: &ToolOutput,
        duration_ms: u64,
        messages: &mut Vec<Message>,
    ) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        let summary = result.to_text();
        let session_id = self.hook_session_id();
        let cwd = self.hook_cwd();
        let is_error = result.is_error();
        let injected = if is_error {
            registry
                .run_post_tool_use_failure(
                    call.name.as_str(),
                    &summary,
                    &session_id,
                    cwd.as_deref(),
                )
                .await
        } else {
            registry
                .run_post_tool_use(
                    call.name.as_str(),
                    &summary,
                    duration_ms,
                    &session_id,
                    cwd.as_deref(),
                )
                .await
        };
        let kind = if is_error {
            InjectionKind::Hook(HookEventKind::PostToolUseFailure)
        } else {
            InjectionKind::Hook(HookEventKind::PostToolUse)
        };
        for context in injected {
            messages.push(crate::conversation_context::hidden_user(kind, context));
        }
    }

    /// Whether a tool call's [`ScopeTarget`] is [`ScopeTarget::Unspecified`] —
    /// i.e. the tool declares no locatable target (a pure read/search like
    /// `read_text`, `search_text`). Used to classify a turn as read-only for the
    /// turn-hook streak counter. An unknown tool name reads as `true`
    /// (unspecified), matching the trait default.
    pub(crate) fn tool_target_is_unspecified(&self, name: &str, arguments: &str) -> bool {
        match self
            .resolved_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|t| t.name() == name)
        {
            Some(t) => matches!(
                t.scope_target(arguments),
                nuo_contracts::ScopeTarget::Unspecified
            ),
            None => true,
        }
    }

    /// Fire user-configured `Turn` hooks at the turn boundary and fold any
    /// `Inject` context into hidden user messages. `Deny` is already discarded
    /// by [`HookRegistry::run_turn`], so a turn hook cannot abort the round.
    /// `ScopeTools` disables are applied to the scoped mask.
    pub(super) async fn run_turn_hooks(
        &self,
        messages: &mut Vec<Message>,
        state: &RoundState,
        turn: usize,
    ) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        let side = registry
            .run_turn(
                self.round_count(),
                turn,
                state.consecutive_readonly_turns,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await;
        for context in side.injected {
            messages.push(crate::conversation_context::hidden_user(
                InjectionKind::Hook(HookEventKind::Turn),
                context,
            ));
        }
        self.apply_scoped_disables(&side.scoped_disables);
    }

    /// Fire `TurnStart` hooks at the start of each ReAct
    /// turn (after tools are prepared, before the next model completion) and
    /// fold any `Inject` context into hidden user messages. `Deny` is already
    /// discarded by [`HookRegistry::run_turn_start`], so this hook
    /// cannot abort the round. `ScopeTools` disables are applied to the scoped
    /// mask. The symmetric partner of [`Self::run_turn_hooks`].
    pub(super) async fn run_turn_start_hooks(
        &self,
        messages: &mut Vec<Message>,
        state: &RoundState,
        turn: usize,
    ) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        let side = registry
            .run_turn_start(
                self.round_count(),
                turn,
                state.consecutive_readonly_turns,
                &self.hook_session_id(),
                self.hook_cwd().as_deref(),
            )
            .await;
        for context in side.injected {
            messages.push(crate::conversation_context::hidden_user(
                InjectionKind::Hook(HookEventKind::TurnStart),
                context,
            ));
        }
        self.apply_scoped_disables(&side.scoped_disables);
    }

    /// Fire `PermissionRequest` hooks at the moment the agent is about to block
    /// on a permission decision. Observe-only: hooks run for side effects (the
    /// canonical use is a fire-and-forget notification so the user notices the
    /// agent is parked); outcomes are ignored by the registry. No-op without a
    /// `[hooks]` config.
    async fn fire_permission_request_hooks(&self, request: &nuo_contracts::PermissionRequest) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        registry
            .run_permission_request(request, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await;
    }

    /// Fire `UserQuestion` hooks at the moment the agent is about to block on
    /// an `ask_user` question. Observe-only, same contract as
    /// [`Self::fire_permission_request_hooks`].
    async fn fire_user_question_hooks(&self, request: &nuo_contracts::UserQuestionRequest) {
        let registry = self.hooks();
        if registry.is_empty() {
            return;
        }
        registry
            .run_user_question(request, &self.hook_session_id(), self.hook_cwd().as_deref())
            .await;
    }

    /// The opt-in hard-stop gate (ADR-0018). Called once per continuing ReAct
    /// turn with the count of turns already run in this round. Returns
    /// `ControlFlow::Break` only when a finite `hard_stop_turns` budget was
    /// configured and `turns` has reached it — the caller converts that into
    /// a terminal `HarnessError` via [`Self::hard_stop_error`]. The default
    /// budget (`0`) keeps the round uncapped, exactly matching ADR-0009.
    ///
    pub(super) fn check_hard_stop(&self, turns: usize) -> std::ops::ControlFlow<()> {
        let budget = self.get_hard_stop_turns();
        if budget > 0 && turns >= budget {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    }

    /// Terminal error surfaced when an opt-in `hard_stop_turns` budget is
    /// exhausted. Echoes the configured budget so the user can tell this apart
    /// from a normal completion in the transcript. The review itself never
    /// produces this — only an explicit user-configured budget does.
    pub(super) fn hard_stop_error(&self) -> HarnessError {
        let budget = self.get_hard_stop_turns();
        HarnessError::Other(format!(
            "Agent stopped: the configured hard-stop budget of {budget} ReAct \
             turns was reached. This budget is opt-in (`hard_stop_turns`); \
             raise it or set it to 0 (the default) for an uncapped round."
        ))
    }

    /// Emit a [`AgentEvent::TodosUpdated`] snapshot whenever a tool mutates
    /// the task list (`todo` full-replace or surgical edit; the legacy
    /// `write_todos`/`update_todo`/`todo_update` names predate ADR-0215 and
    /// match only so persisted prompts replaying them still refresh the
    /// panel).
    /// The TUI stores the snapshot and re-renders the sticky panel above the
    /// input box.
    pub(super) fn emit_todos_change<F>(&self, call: &ToolCall, on_event: &mut F)
    where
        F: FnMut(AgentEvent) + Send,
    {
        if matches!(
            call.name.as_str(),
            "todo" | "write_todos" | "update_todo" | "todo_update"
        ) {
            on_event(AgentEvent::TodosUpdated(self.todos()));
        }
    }

    async fn execute_ask_user(
        &self,
        call: &ToolCall,
        _call_id: &str,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> ToolOutput {
        let args: serde_json::Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutput::Text(format!("Invalid ask_user arguments: {}", e));
            }
        };
        let questions: Vec<UserQuestion> = match serde_json::from_value(
            args.get("questions")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        ) {
            Ok(q) => q,
            Err(e) => {
                return ToolOutput::Text(format!("Invalid ask_user questions: {}", e));
            }
        };
        if !(1..=5).contains(&questions.len()) {
            return ToolOutput::Text(
                "ask_user requires between one and five questions.".to_string(),
            );
        }
        for (i, q) in questions.iter().enumerate() {
            if !(2..=4).contains(&q.options.len()) {
                return ToolOutput::Text(format!(
                    "ask_user question {} requires between two and four options.",
                    i + 1
                ));
            }
        }

        let request = UserQuestionRequest {
            id: format!("ask_user_{}", uuid::Uuid::new_v4()),
            questions,
            origin: None,
        };

        // ADR-0141 posture gate: an ask_user call only parks when a human
        // channel exists. Autonomous sessions (headless no-TTY, CI, subagents
        // with `allow_user_interaction: false`) never fabricate a user —
        // they settle by the configured fallback policy, labeled as such.
        let posture = self.human_posture();
        if posture == nuo_contracts::human_request::HumanChannelPosture::Autonomous {
            return match self.autonomous_fallback_policy() {
                nuo_contracts::human_request::AutonomousFallbackPolicy::FailClosed => {
                    self.human_broker
                        .metrics_note_refused(HumanRequestKind::Question);
                    ToolOutput::Text(
                        "ask_user is unavailable: no human channel is attached to this \
                         session, so nobody can answer. Resolve the ambiguity yourself — \
                         choose the safest option, state the assumption in your reply, and \
                         continue. Do not call ask_user again this turn."
                            .to_string(),
                    )
                }
                nuo_contracts::human_request::AutonomousFallbackPolicy::RecommendedLabeled => {
                    // Take each question's first option — the schema's
                    // "recommended" convention — and label the source so the
                    // model can never mistake it for a human decision.
                    let answers: Vec<Vec<String>> = request
                        .questions
                        .iter()
                        .map(|q| {
                            q.options
                                .first()
                                .map(|opt| vec![opt.label.clone()])
                                .unwrap_or_default()
                        })
                        .collect();
                    let reply = UserQuestionReply {
                        request_id: request.id.clone(),
                        answers,
                    };
                    let settled = self.human_broker.settle_by_policy_owned(
                        request.id.clone(),
                        HumanReply::Question(Some(reply.clone())),
                        nuo_contracts::human_request::AutonomousFallbackPolicy::RecommendedLabeled,
                    );
                    debug_assert!(settled, "policy settlement on a fresh request must succeed");
                    let _ = settled;
                    let output = serde_json::to_string_pretty(&reply.answers)
                        .unwrap_or_else(|_| format!("{:?}", reply.answers));
                    ToolOutput::Text(format!(
                        "[answered by policy, not by user] No human channel is attached. \
                         Each question was answered with its first (recommended) option \
                         per the session's autonomous fallback policy:\n{}",
                        output
                    ))
                }
            };
        }

        let receiver = self
            .human_broker
            .park(request.id.clone(), HumanRequestKind::Question);
        tracing::info!(questions = request.questions.len(), "asking user");
        let _ = event_tx.send(AgentEvent::UserQuestionRequest(request.clone()));
        // Observe-only interrupt hook: fire notifications (desktop/bell) so the
        // user notices the agent is blocked on their input. No-op without
        // `[hooks]`. Outcomes are ignored — this never gates the question.
        let parked_at = std::time::Instant::now();
        self.fire_user_question_hooks(&request).await;

        let settled = receiver.await.ok().map(|s| s.reply);
        let reply = match settled {
            Some(HumanReply::Question(reply)) => reply,
            // Channel closed without a settlement (agent teardown) or a
            // mismatched kind (harness bug): treat as cancel.
            _ => None,
        };
        // Charge the human-thinking pause to the round so the exit gate can
        // subtract it for an honest tokens/sec.
        self.book_pause(parked_at.elapsed().as_millis() as u64);
        match reply {
            Some(reply) => {
                let output = serde_json::to_string_pretty(&reply.answers)
                    .unwrap_or_else(|_| format!("{:?}", reply.answers));
                ToolOutput::Text(format!(
                    "User answered the question(s). Selected option labels:\n{}",
                    output
                ))
            }
            None => {
                ToolOutput::Text("User cancelled the question; no answer was provided.".to_string())
            }
        }
    }

    /// Park a runtime-input request for a supervised command and await the
    /// operator's reply. Called by the command tool's examiner (through the
    /// [`InputHandler`] this type implements) when a run of the command is
    /// detected waiting on an input channel. Emits [`AgentEvent::StdinRequest`];
    /// the TUI shows an inline input panel and the reply travels back via
    /// [`Self::reply_input`].
    ///
    /// Returns `Some(line)` with the operator's input, or `None` if no human
    /// channel exists or the operator cancelled (the caller then kills the
    /// command → `ShellTermination::InputUnanswered`).
    async fn collect_runtime_input(
        &self,
        prompt: &nuo_contracts::InputPrompt,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Option<String> {
        // ADR-0141 posture gate: with no human channel there is nobody to type
        // into the panel; do not park.
        if self.human_posture() == HumanChannelPosture::Autonomous {
            self.human_broker
                .metrics_note_refused(HumanRequestKind::Stdin);
            tracing::info!("autonomous posture: runtime input refused");
            return None;
        }
        let request = InputRequest {
            id: format!("input_{}", uuid::Uuid::new_v4()),
            command: prompt.command.clone(),
            prompt: prompt.prompt.clone(),
            secret: prompt.secret,
        };
        let receiver = self
            .human_broker
            .park(request.id.clone(), HumanRequestKind::Stdin);
        tracing::info!(secret = prompt.secret, "requesting operator input for a command waiting on input");
        let _ = event_tx.send(AgentEvent::StdinRequest(request));
        let settled = receiver.await.ok().map(|s| s.reply);
        match settled {
            Some(HumanReply::Stdin(Some(reply))) if !reply.text.is_empty() => Some(reply.text),
            _ => None,
        }
    }

    /// Decide the [`InputContract`](nuo_contracts::InputContract) for a command
    /// call (before spawn). The three-way decision, in order:
    ///
    /// 1. **Model stdin** (opt-in): `allow_model_stdin` on AND the model
    ///    supplied a `stdin` arg → `Prefilled{model}`.
    /// 2. **Pre-spawn refusal / supervision**: the interactive classifier
    ///    matched. Under unattended (or `skip_interactive_input`), or when the
    ///    platform cannot fully supervise (`input_supervision()` is not
    ///    `Supervised`), seal stdin — the command fails fast with the
    ///    classifier's non-interactive remedy. Otherwise supervise.
    /// 3. **Sealed** (default hard floor): everything else.
    ///
    /// `arguments` is the raw JSON tool arguments.
    pub(super) fn decide_command_input(&self, arguments: &str) -> nuo_contracts::InputContract {
        // (α) opt-in model stdin.
        if self.allow_model_stdin()
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments)
            && let Some(data) = v.get("stdin").and_then(|s| s.as_str())
            && !data.is_empty()
        {
            return nuo_contracts::InputContract::Prefilled {
                data: data.to_string(),
            };
        }
        let command = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(str::to_string))
            .unwrap_or_default();
        if let Some(kind) = crate::shell_input::classify(&command) {
            // No human is reachable, the operator opted out, or the platform
            // cannot fully supervise (no terminal, or no reliable wait
            // detection): seal stdin so the command fails fast with a
            // non-interactive remedy instead of hanging or misfiring.
            if self.unattended()
                || self.skip_interactive_input()
                || nuo_host::supervised::input_supervision()
                    != nuo_host::supervised::InputSupervision::Supervised
            {
                return nuo_contracts::InputContract::Sealed;
            }
            return nuo_contracts::InputContract::Supervised {
                expectation: Some(crate::shell_input::expectation(&command, kind)),
            };
        }
        nuo_contracts::InputContract::Sealed
    }

    pub(crate) async fn execute_tool(
        &self,
        call: &ToolCall,
        call_id: &str,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> ToolOutput {
        let tool: Arc<dyn Tool> = match self.tool_manager.find(&call.name) {
            Some(sourced) => sourced.tool,
            None => {
                return ToolOutput::Error {
                    message: format!("Tool '{}' not found", call.name),
                    detail: None,
                };
            }
        };

        // Permission policy chain (full async chain)
        // Every permission gate — PreToolUse hook, disabled mask, schema
        // validation, operation-scope gate, bash policy, and the broker's
        // explicit-grant/development fast paths — runs
        // as one chain evaluation (see `permission_policy`). The chain is
        // async because some gates await (hooks, bash policy). Outcomes:
        //   • Deny    → short-circuit with the policy's output.
        //   • Approve → proceed under existing authority.
        //   • MissingAuthority → attended: ask once; delegated: fail now.
        //   • Pass    → (chain fallback) proceed.
        let target = tool.scope_target(&call.arguments);
        // Snapshot the disable masks and scope *before* the chain runs, then
        // drop the guards — the chain is async and MutexGuards are not Send, so
        // they must not live across the `.await`.
        let (disabled_snapshot, scoped_snapshot) = {
            let disabled = self
                .disabled_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let scoped = self
                .scoped_disabled_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            (disabled, scoped)
        };
        let pctx = crate::permission_policy::PolicyContext {
            tool: &tool,
            call_name: call.name.as_str(),
            arguments: &call.arguments,
            scope_target: target.clone(),
            disabled: disabled_snapshot,
            scoped_disabled: scoped_snapshot,
            unattended: self.unattended(),
            ctx: self, // Agent: PermissionContext
        };

        match self.permission_chain().evaluate(&pctx).await {
            crate::permission_policy::PolicyDecision::Pass
            | crate::permission_policy::PolicyDecision::Approve => {}
            crate::permission_policy::PolicyDecision::Deny { output, .. } => {
                return output;
            }
            crate::permission_policy::PolicyDecision::MissingAuthority { request, rule } => {
                if self.unattended() {
                    // Under unattended mode, missing authority is auto-approved.
                } else {
                    // The single interactive-park path. Both the broker (a
                    // write/execute the user must approve) and the bash
                    // dangerous-command confirm reach here; `request.one_off`
                    // distinguishes them. Fill the request id, emit, fire
                    // observe hooks, await the user's decision.
                    let one_off = request.one_off;
                    let request = nuo_contracts::PermissionRequest {
                        id: format!("permission_{}", uuid::Uuid::new_v4()),
                        ..*request
                    };
                    // ADR-0141 posture gate: permissions fail closed when no
                    // human channel exists — a missing human cannot grant
                    // authority. (Delegated sessions reach this arm only with
                    // an interactive watcher attached, so this is the belt to
                    // that braces.)
                    if self.human_posture() == HumanChannelPosture::Autonomous {
                        self.human_broker
                            .metrics_note_refused(HumanRequestKind::Permission);
                        tracing::warn!(
                            tool = %request.tool,
                            "autonomous posture: permission refused (fail closed)"
                        );
                        return permission_required_output(&request);
                    }
                    let receiver = self
                        .human_broker
                        .park(request.id.clone(), HumanRequestKind::Permission);
                    let parked_at = std::time::Instant::now();
                    tracing::info!(tool = %request.tool, scope = %request.scope, one_off, "permission requested");
                    let _ = event_tx.send(AgentEvent::PermissionRequest(request.clone()));
                    self.fire_permission_request_hooks(&request).await;
                    let decision = match receiver.await.ok().map(|s| s.reply) {
                        Some(HumanReply::Permission(decision)) => decision,
                        _ => PermissionDecision::Reject,
                    };
                    // Charge the human-thinking pause to the round so the exit
                    // gate can subtract it for an honest tokens/sec.
                    self.book_pause(parked_at.elapsed().as_millis() as u64);
                    match decision {
                        PermissionDecision::Once => {
                            tracing::info!(tool = %tool.name(), decision = "once", "permission granted for single invocation");
                        }
                        PermissionDecision::Session => {
                            tracing::info!(tool = %tool.name(), decision = "session", "permission granted for current session");
                            self.permissions.add_session(rule);
                        }
                        PermissionDecision::Always => {
                            if one_off {
                                // A bash dangerous-command confirm: honour the
                                // grant for this one call but do NOT persist it.
                                // A dangerous-command confirmation is sharper
                                // than ordinary tool permission and must stay
                                // one-off unless the user writes an explicit
                                // `[bash_policy.rules] action = "allow"` override.
                                tracing::info!(
                                    tool = %tool.name(),
                                    decision = "always",
                                    "one-off permission granted (not persisted)"
                                );
                            } else {
                                tracing::info!(tool = %tool.name(), decision = "always", "permission granted permanently for workspace");
                                self.permissions.add_always(rule);
                            }
                        }
                        PermissionDecision::Reject => {
                            tracing::warn!(tool = %tool.name(), "permission denied");
                            return ToolOutput::PermissionDenied {
                                tool: tool.name().to_string(),
                            };
                        }
                    }
                }
            }
        }

        if call.name == "ask_user" {
            if !self.execution_policy().allow_human_interaction {
                return ToolOutput::Text(
                    "ask_user is forbidden by ExecutionPolicy: delegated child agents operating in ephemeral scratchpads cannot directly interact with the human user. Report findings or blockers to your parent agent."
                        .to_string(),
                );
            }
            if self.unattended() {
                return ToolOutput::Text(
                    "ask_user is unavailable: this session is running in Unattended mode and no human \
                     is reachable to answer. Resolve the ambiguity yourself — pick the most \
                     reasonable default and proceed."
                        .to_string(),
                );
            }
            return self.execute_ask_user(call, call_id, event_tx).await;
        }

        // Input contract decision (before spawn), for run_command only:
        //   1. opt-in model input (α): `allow_model_stdin` on AND the model
        //      supplied a `stdin` arg → Prefilled{model}.
        //   2. pre-spawn refusal / runtime supervision: the interactive
        //      classifier matched → Sealed (unattended / skip / no platform
        //      terminal) or Supervised (a controlled terminal + examiner).
        //   3. sealed (default hard floor): everything else.
        // For other tools, Sealed is always correct (they ignore input).
        let input = if call.name == "run_command" {
            self.decide_command_input(&call.arguments)
        } else {
            nuo_contracts::InputContract::default()
        };

        // The Subagent / ToolStream events must carry the same id as the
        // up-front ToolCall event (the dispatch-generated `call_id`), not the
        // model's `call.id` — the UI keys its step off the ToolCall event id,
        // so using `call.id` here would orphan every subagent child stream and
        // every live tool stream, leaving the subagent view empty.
        let parent_call_id = call_id.to_string();
        let stream_call_id = call_id.to_string();
        let stream_tx = event_tx.clone();
        let mut on_stream = move |stream: ToolStream| {
            let _ = stream_tx.send(AgentEvent::ToolStream {
                id: stream_call_id.clone(),
                stream,
            });
        };
        // Per-invocation input supervisor: captures this call's event channel so
        // the command tool's examiner can park a detected input wait on the
        // operator. Built here (not at factory time) because it is bound to the
        // live event stream of this call.
        let input_supervisor = AgentInputSupervisor {
            agent: self,
            event_tx: event_tx.clone(),
        };
        let invocation = nuo_contracts::ToolInvocation {
            call_id,
            arguments: &call.arguments,
            input,
            input_handler: Some(&input_supervisor),
        };
        match tool
            .call_structured_with_events(
                invocation,
                Box::new(|event| {
                    let _ = event_tx.send(AgentEvent::Subagent {
                        parent_call_id: parent_call_id.clone(),
                        event,
                    });
                }),
                &mut on_stream,
            )
            .await
        {
            Ok(output) => output,
            Err(err) => ToolOutput::Error {
                message: err,
                detail: None,
            },
        }
    }

    /// Single-call wrapper that forwards channel events to a mutable callback.
    /// Used by text-fallback paths (one tool call at a time).
    ///
    /// Cancellation-aware: if `cancel` fires while the tool is in flight, a
    /// cooperatively-cancellable tool (a subagent) is given a bounded grace
    /// period to drain and return its terminal result — the interrupted
    /// result is carried in [`SingleToolOutcome`] so the caller can record the
    /// partial work before ending the round. A non-cancellable tool keeps the
    /// historical fast path: its in-flight call is paired with a terminal
    /// [`AgentEvent::ToolCancelled`] and the outcome reports no result.
    pub(crate) async fn execute_tool_evented<F>(
        &self,
        call: &ToolCall,
        call_id: &str,
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<SingleToolOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let fut = self.execute_tool(call, call_id, &tx);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let cancellable = self
                        .tool_manager
                        .find(&call.name)
                        .is_some_and(|sourced| sourced.tool.supports_cooperative_cancel());
                    if !cancellable {
                        while let Ok(event) = rx.try_recv() {
                            on_event(event);
                        }
                        on_event(AgentEvent::ToolCancelled {
                            id: call_id.to_string(),
                            name: call.name.clone(),
                        });
                        return Ok(SingleToolOutcome {
                            result: None,
                            interrupted: true,
                        });
                    }
                    // Cooperative drain: signal the tool, then race its
                    // future against a bounded grace period. The subagent stops
                    // at its next safe boundary and returns its partial
                    // transcript as a terminal result.
                    if let Some(sourced) = self.tool_manager.find(&call.name) {
                        sourced.tool.request_cancel(call_id);
                    }
                    let grace = tokio::time::sleep(SUBAGENT_DRAIN_GRACE);
                    tokio::pin!(grace);
                    loop {
                        tokio::select! {
                            biased;
                            _ = &mut grace => {
                                while let Ok(event) = rx.try_recv() {
                                    on_event(event);
                                }
                                on_event(AgentEvent::ToolCancelled {
                                    id: call_id.to_string(),
                                    name: call.name.clone(),
                                });
                                return Ok(SingleToolOutcome {
                                    result: None,
                                    interrupted: true,
                                });
                            }
                            event = rx.recv() => {
                                if let Some(event) = event {
                                    on_event(event);
                                }
                            }
                            result = &mut fut => {
                                while let Ok(event) = rx.try_recv() {
                                    on_event(event);
                                }
                                return Ok(SingleToolOutcome {
                                    result: Some(result),
                                    interrupted: true,
                                });
                            }
                        }
                    }
                }
                event = rx.recv() => {
                    if let Some(event) = event {
                        on_event(event);
                    }
                }
                result = &mut fut => {
                    while let Ok(event) = rx.try_recv() {
                        on_event(event);
                    }
                    return Ok(SingleToolOutcome {
                        result: Some(result),
                        interrupted: false,
                    });
                }
            }
        }
    }

    /// Resolve `call`'s tool (resolved → dynamic fallback) and return its
    /// declared [`ToolAccesses`]. Used by the scheduler to arbitrate which
    /// calls of a batch may run concurrently. A tool that can't be resolved
    /// yields [`ToolAccesses::none`] (freely parallel) — it will report its
    /// own "not found" error inside `execute_tool`; there's no point
    /// serializing an error.
    pub(crate) fn accesses_for_call(&self, call: &ToolCall) -> nuo_contracts::ToolAccesses {
        let tool: Option<Arc<dyn Tool>> = self
            .resolved_tools
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|t| t.name() == call.name)
            .cloned()
            .or_else(|| self.dynamic_tools.find(&call.name));
        match tool {
            Some(tool) => tool.accesses(&call.arguments),
            None => nuo_contracts::ToolAccesses::none(),
        }
    }
}

/// Per-invocation [`InputHandler`](nuo_contracts::InputHandler): bridges the
/// command tool's runtime examiner back into the agent's human-input channel.
/// Constructed in [`Agent::execute_tool`] for the duration of one call, so it
/// captures that call's event channel and emits the TUI's input panel request
/// on the right stream.
struct AgentInputSupervisor<'a> {
    agent: &'a Agent,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
}

#[async_trait::async_trait]
impl nuo_contracts::InputHandler for AgentInputSupervisor<'_> {
    async fn resolve(&self, prompt: nuo_contracts::InputPrompt) -> Option<String> {
        self.agent
            .collect_runtime_input(&prompt, &self.event_tx)
            .await
    }
}
