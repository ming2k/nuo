//! Tool-execution tail of [`Agent`] rounds: output normalization, image
//! plumbing, and the PostToolUse/PostToolUseFailure hook fire points.
use super::*;

/// Ceiling for one structured tool result. The text half already has a 64 KiB
/// cap; a JSON payload has no natural line structure, so it gets an explicit
/// byte budget of its own.
const MAX_STRUCTURED_OUTPUT_BYTES: usize = 512 * 1024;

/// Resolve and admit one model-supplied path against the live workspace
/// boundary. Kept pure so the lexical and canonical checks are testable
/// without constructing an [`Agent`].
fn admit_model_path(
    workspace_root: &std::path::Path,
    additional_roots: &[std::path::PathBuf],
    raw: &str,
) -> Result<std::path::PathBuf, String> {
    let supplied = std::path::Path::new(raw);
    let candidate = if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        workspace_root.join(supplied)
    };
    let normalized = protocol::execution::lexical_normalize(&candidate);

    let lexical_roots = std::iter::once(workspace_root.to_path_buf())
        .chain(additional_roots.iter().cloned())
        .map(|root| protocol::execution::lexical_normalize(&root))
        .collect::<Vec<_>>();
    let lexically_admitted = lexical_roots
        .iter()
        .any(|root| normalized.starts_with(root))
        || protocol::execution::admits_temp_path(&normalized)
        || protocol::execution::admits_skills_path(&normalized);
    if !lexically_admitted {
        return Err(format!(
            "path `{raw}` resolves outside the admitted workspace roots"
        ));
    }

    // Lexical containment is not enough when a symlink inside the workspace
    // points out of it. Canonicalize the longest existing prefix and compare
    // against the canonical roots. A path that does not exist yet (a write) is
    // checked through its nearest existing parent.
    let canonical_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut probe = normalized.as_path();
    let canonical = loop {
        if let Ok(path) = std::fs::canonicalize(probe) {
            break Some(path);
        }
        match probe.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => probe = parent,
            _ => break None,
        }
    };
    if let Some(canonical) = canonical {
        let admitted = canonical.starts_with(&canonical_root)
            || additional_roots.iter().any(|root| {
                let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                canonical.starts_with(root)
            })
            || protocol::execution::admits_temp_path(&canonical)
            || protocol::execution::admits_skills_path(&canonical);
        if !admitted {
            return Err(format!(
                "path `{raw}` escapes the admitted workspace roots through a symlink"
            ));
        }
    }

    Ok(candidate)
}

/// Adapter that turns a tool's context stream/progress sink into session events.
struct ChannelStreamSink {
    tx: mpsc::UnboundedSender<AgentEvent>,
    call_id: String,
}

fn event_stream_sink(tx: mpsc::UnboundedSender<AgentEvent>, call_id: String) -> ChannelStreamSink {
    ChannelStreamSink { tx, call_id }
}

impl tool::ToolStreamSink for ChannelStreamSink {
    fn emit(&self, stream: tool::ToolStream) {
        let _ = self.tx.send(AgentEvent::ToolStream {
            id: self.call_id.clone(),
            stream,
        });
    }
}

/// A short "did you mean" suffix for an unknown tool call.
fn did_you_mean(names: &[String], requested: &str) -> String {
    let requested = requested.to_ascii_lowercase();
    let mut best: Option<(usize, &str)> = None;
    for name in names {
        let candidate = name.to_ascii_lowercase();
        let distance = levenshtein(&requested, &candidate);
        if distance <= 3 && best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, name.as_str()));
        }
    }
    match best {
        Some((_, name)) => format!(" Did you mean `{name}`?"),
        None => " Use one of the advertised tool names.".to_string(),
    }
}

fn levenshtein(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_char != right_char);
            current[j + 1] = substitution.min(previous[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Keep a failed call's final progress tick in its model-facing detail.
///
/// The stream tick is ephemeral and may have been throttled; the error detail is
/// durable and is what a later reader sees when diagnosing where the call
/// stopped.
fn attach_last_progress(output: ToolOutput, progress: Option<tool::ToolProgress>) -> ToolOutput {
    let Some(progress) = progress else {
        return output;
    };
    let line = format!("Last progress: {}", progress.render());
    match output {
        ToolOutput::Error { message, detail } => ToolOutput::Error {
            message,
            detail: Some(match detail {
                Some(detail) if !detail.is_empty() => format!("{detail}\n{line}"),
                _ => line,
            }),
        },
        other => other,
    }
}

/// Bound and validate the structured half of a tool result.
///
/// Kept outside `execute_tool` so the call sequence (resolve → permission →
/// journal → timeout → finalize) stays readable and within the source-size
/// ceiling.
fn finalize_output(
    call: &ToolCall,
    entry: &crate::tool_snapshot::ToolSnapshotEntry,
    output: ToolOutput,
) -> ToolOutput {
    if let Some(value) = output.structured_value()
        && serde_json::to_vec(value)
            .map(|bytes| bytes.len() > MAX_STRUCTURED_OUTPUT_BYTES)
            .unwrap_or(false)
    {
        return ToolOutput::Error {
            message: format!(
                "Tool `{}` returned a structured payload larger than {} KiB",
                call.name,
                MAX_STRUCTURED_OUTPUT_BYTES / 1024
            ),
            detail: Some(
                "Return a bounded summary in `content` and keep large data in an artifact the \
                 model can read by path."
                    .to_string(),
            ),
        };
    }

    if let Some(error) = &entry.output_error {
        ToolOutput::Error {
            message: format!("Tool `{}` has an invalid output schema", call.name),
            detail: Some(error.clone()),
        }
    } else if let Some(schema) = &entry.output {
        match output.structured_value() {
            Some(value) => match schema.validate(value) {
                Ok(()) => output,
                Err(reason) => ToolOutput::Error {
                    message: format!("Tool `{}` returned invalid structured output", call.name),
                    detail: Some(reason),
                },
            },
            None => ToolOutput::Error {
                message: format!(
                    "Tool `{}` declares an output schema but returned no structured value",
                    call.name
                ),
                detail: None,
            },
        }
    } else {
        output
    }
}

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
            messages.push(crate::runtime::message::hidden_user(kind, context));
        }
    }

    /// Whether a tool call's [`ScopeTarget`] is [`ScopeTarget::Unspecified`] —
    /// i.e. the tool declares no locatable target (a pure read/search like
    /// `read`, `search_text`). Used to classify a turn as read-only for the
    /// turn-hook streak counter. An unknown tool name reads as `true`
    /// (unspecified), matching the trait default.
    pub(crate) fn tool_target_is_unspecified(&self, name: &str, arguments: &str) -> bool {
        let value =
            serde_json::from_str::<serde_json::Value>(arguments).unwrap_or(serde_json::Value::Null);
        match self.current_tool_snapshot().find(name) {
            Some(entry) => matches!(
                entry.tool.scope_target(&value),
                tool::ScopeTarget::Unspecified
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
            messages.push(crate::runtime::message::hidden_user(
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
            messages.push(crate::runtime::message::hidden_user(
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
    async fn fire_permission_request_hooks(&self, request: &tool::PermissionRequest) {
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
    async fn fire_user_question_hooks(&self, request: &tool::UserQuestionRequest) {
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
    /// `write_todos`/`update_todo`/`todo_update` names predate and
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

        // Posture gate: an ask_user call only parks when a human
        // channel exists. Autonomous sessions (headless no-TTY, CI, subagents
        // with `allow_user_interaction: false`) never fabricate a user —
        // they settle by the configured fallback policy, labeled as such.
        let posture = self.human_posture();
        if posture == protocol::human_request::HumanChannelPosture::Autonomous {
            return match self.autonomous_fallback_policy() {
                protocol::human_request::AutonomousFallbackPolicy::FailClosed => {
                    self.control
                        .human_broker
                        .metrics_note_refused(HumanRequestKind::Question);
                    ToolOutput::Text(
                        "ask_user is unavailable: no human channel is attached to this \
                         session, so nobody can answer. Resolve the ambiguity yourself — \
                         choose the safest option, state the assumption in your reply, and \
                         continue. Do not call ask_user again this turn."
                            .to_string(),
                    )
                }
                protocol::human_request::AutonomousFallbackPolicy::RecommendedLabeled => {
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
                    let settled = self.control.human_broker.settle_by_policy_owned(
                        request.id.clone(),
                        HumanReply::Question(Some(reply.clone())),
                        protocol::human_request::AutonomousFallbackPolicy::RecommendedLabeled,
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
            .control
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
        prompt: &tool::InputPrompt,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Option<String> {
        // Posture gate: with no human channel there is nobody to type
        // into the panel; do not park.
        if self.human_posture() == HumanChannelPosture::Autonomous {
            self.control
                .human_broker
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
            .control
            .human_broker
            .park(request.id.clone(), HumanRequestKind::Stdin);
        tracing::info!(
            secret = prompt.secret,
            "requesting operator input for a command waiting on input"
        );
        let _ = event_tx.send(AgentEvent::StdinRequest(request));
        let settled = receiver.await.ok().map(|s| s.reply);
        match settled {
            Some(HumanReply::Stdin(Some(reply))) if !reply.text.is_empty() => Some(reply.text),
            _ => None,
        }
    }

    /// Decide the [`InputContract`](tool::InputContract) for a command
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
    pub(super) fn decide_command_input(&self, arguments: &str) -> tool::InputContract {
        // (α) opt-in model stdin.
        if self.allow_model_stdin()
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(arguments)
            && let Some(data) = v.get("stdin").and_then(|s| s.as_str())
            && !data.is_empty()
        {
            return tool::InputContract::Prefilled {
                data: data.to_string(),
            };
        }
        let command = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| {
                v.get("command")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if let Some(kind) = guardrails::shell_input::classify(&command) {
            // No human is reachable, the operator opted out, or the platform
            // cannot fully supervise (no terminal, or no reliable wait
            // detection): seal stdin so the command fails fast with a
            // non-interactive remedy instead of hanging or misfiring.
            if self.unattended()
                || self.skip_interactive_input()
                || host::supervised::input_supervision()
                    != host::supervised::InputSupervision::Supervised
            {
                return tool::InputContract::Sealed;
            }
            return tool::InputContract::Supervised {
                expectation: Some(guardrails::shell_input::expectation(&command, kind)),
            };
        }
        tool::InputContract::Sealed
    }

    /// Load the nested `AGENTS.md` files above a successfully read path.
    ///
    /// The root `AGENTS.md` is already part of the fixed project-rules
    /// section. A subtree's rules are only relevant once the model enters that
    /// subtree, so this is the lazy counterpart: walk from the read path's
    /// directory up to (but excluding) the workspace root, inject each unseen
    /// `AGENTS.md` once, and remember the path for the rest of the session.
    pub(crate) fn inject_nested_project_rules(
        &self,
        call: &ToolCall,
        result: &ToolOutput,
        messages: &mut Vec<Message>,
    ) {
        if call.name != "read" || result.is_error() {
            return;
        }
        let Some(root) = self.workspace_root() else {
            return;
        };
        let Some(raw) = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .ok()
            .and_then(|value| {
                value
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        else {
            return;
        };
        if tool::is_virtual_path(&raw) {
            return;
        }
        let Ok(resolved) = admit_model_path(&root, self.additional_workspace_roots(), &raw) else {
            return;
        };
        let mut directory = if resolved.is_dir() {
            resolved.as_path()
        } else {
            resolved.parent().unwrap_or(resolved.as_path())
        };

        let root = protocol::execution::lexical_normalize(&root);
        let mut injected = 0usize;
        while directory.starts_with(&root) && directory != root && injected < 8 {
            let rules_path = directory.join("AGENTS.md");
            if rules_path.is_file() {
                let already_loaded = {
                    let mut loaded = self
                        .context
                        .loaded_project_rules
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    !loaded.insert(rules_path.clone())
                };
                if !already_loaded && let Ok(text) = std::fs::read_to_string(&rules_path) {
                    let mut text = text;
                    if text.len() > 64 * 1024 {
                        let mut cut = 64 * 1024;
                        while cut > 0 && !text.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        text.truncate(cut);
                        text.push_str("\n\n[nested AGENTS.md truncated at 64 KiB]");
                    }
                    let relative = rules_path
                        .strip_prefix(&root)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| rules_path.to_string_lossy().into_owned());
                    messages.push(crate::runtime::message::hidden_user_with_reason(
                        InjectionKind::ProjectRulesDelta,
                        relative.clone(),
                        format!(
                            "<project-rules path=\"{relative}\">\n{text}\n</project-rules>\n\
                                 These rules apply to files under this directory. Follow them for \
                                 the rest of the session unless the user says otherwise."
                        ),
                    ));
                    injected += 1;
                }
            }
            let Some(parent) = directory.parent() else {
                break;
            };
            directory = parent;
        }
    }

    /// Whether a call may run while the provider stream is still open.
    ///
    /// Only a read-only tool with no user prompt and no control-flow effect is
    /// eligible. A read has no side effect, so starting it against a response
    /// that is later discarded (truncated stream, retry) is safe: the result is
    /// dropped and re-read. Mutating tools still wait for stream validation.
    pub(crate) fn early_tool_eligible(&self, call: &ToolCall) -> bool {
        if call.id.is_empty()
            || call.name.is_empty()
            || serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
        {
            return false;
        }
        let snapshot = self.current_tool_snapshot();
        let Some(entry) = snapshot.find(&call.name) else {
            return false;
        };
        let tool = &entry.tool;
        // Only the shipped local read capabilities are eligible. A custom or
        // dynamic tool that merely *declares* ReadOnly is not assumed bounded:
        // an arbitrary test/plugin read can await a gate forever, and early
        // dispatch would then hold the turn open after the stream ended.
        matches!(
            tool.builtin(),
            Some(
                tool::BuiltinTool::Read
                    | tool::BuiltinTool::FindFiles
                    | tool::BuiltinTool::SearchText
                    | tool::BuiltinTool::CodeQuery
                    | tool::BuiltinTool::RecallMemory
            )
        ) && tool.risk_profile() == tool::RiskProfile::ReadOnly
            && !tool.requires_user()
            && !tool.spawns_subagent()
            && !tool.affects_control_flow()
            && matches!(
                tool.scope_target(
                    &serde_json::from_str::<serde_json::Value>(&call.arguments)
                        .unwrap_or(serde_json::Value::Null)
                ),
                tool::ScopeTarget::Unspecified
            )
    }

    /// Return a terminal tool output when a path-bearing call escapes the
    /// admitted workspace roots. Virtual paths (`skill://`, `offstream://`) are
    /// resolved by their own resolver and are not host paths.
    fn check_model_path_boundary(&self, call: &ToolCall) -> Option<ToolOutput> {
        let raw = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .ok()?
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string)?;
        if tool::is_virtual_path(&raw) {
            return None;
        }
        let root = self.workspace_root()?;
        match admit_model_path(&root, self.additional_workspace_roots(), &raw) {
            Ok(_) => None,
            Err(detail) => Some(ToolOutput::Error {
                message: format!(
                    "Refused `{}`: the path is outside this session's admitted workspace.",
                    call.name
                ),
                detail: Some(format!(
                    "{detail}. Use a workspace-relative path, or ask the operator to add \
                     the root under [workspace].additional_roots."
                )),
            }),
        }
    }

    /// Build the per-call context every tool receives.
    fn tool_context(&self, call_id: &str, cancel: &CancellationToken) -> tool::ToolContext {
        let mut context = tool::ToolContext::new(call_id)
            .with_session_id(self.hook_session_id())
            .with_cancel_token(cancel.clone());
        let mut roots = Vec::new();
        if let Some(root) = self.workspace_root() {
            roots.push(root);
        }
        roots.extend(self.additional_workspace_roots().iter().cloned());
        if !roots.is_empty() {
            context = context.with_workspace_roots(roots);
        }
        context
    }

    pub(crate) async fn execute_tool(
        &self,
        call: &ToolCall,
        event_tx: &mpsc::UnboundedSender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> ToolOutput {
        // A child token isolates this call: a timeout cancels the tool's own
        // work without cancelling the whole turn, while a turn cancellation
        // still propagates down into the tool.
        let call_cancel = cancel.child_token();
        if call_cancel.is_cancelled() {
            return ToolOutput::Error {
                message: format!("Tool `{}` was cancelled before it started", call.name),
                detail: None,
            };
        }
        // ADR-0166: the call id is the call's identity — there is no second,
        // dispatch-local id to thread alongside it.
        let call_id = call.id.as_str();
        // Dispatch against the request snapshot, never the live registry: a
        // tool added after the request was advertised is not callable, and a
        // tool removed after it was advertised still resolves to the exact
        // implementation the model saw.
        let snapshot = self.current_tool_snapshot();
        let entry = match snapshot.find(&call.name) {
            Some(entry) => entry,
            None => {
                return ToolOutput::Error {
                    message: format!(
                        "Tool `{}` is not advertised in this request and cannot be called.",
                        call.name
                    ),
                    detail: Some(did_you_mean(&snapshot.names(), &call.name)),
                };
            }
        };
        let tool: Arc<dyn Tool> = Arc::clone(&entry.tool);

        // One argument boundary for every tool: parse, coerce, validate against
        // the compiled schema captured with the snapshot.
        if let Some(error) = &entry.input_error {
            return ToolOutput::Error {
                message: format!("Tool `{}` has an invalid schema", call.name),
                detail: Some(error.clone()),
            };
        }
        let Some(input_schema) = &entry.input else {
            return ToolOutput::Error {
                message: format!("Tool `{}` has no input schema", call.name),
                detail: None,
            };
        };
        let arguments = match input_schema.prepare(&call.arguments) {
            Ok(prepared) => prepared.value,
            Err(reason) => {
                return ToolOutput::Error {
                    message: format!("Invalid arguments for tool `{}`", call.name),
                    detail: Some(reason),
                };
            }
        };

        // Workspace boundary. This is a correctness gate, not a permission
        // prompt: a model-supplied path that lexically or canonically escapes
        // the admitted roots is refused before the tool can touch the host.
        // `--unconfined` sessions skip it by design.
        if self.is_confined()
            && tool.path_bound()
            && let Some(violation) = self.check_model_path_boundary(call)
        {
            return violation;
        }

        // Permission policy chain (full async chain)
        // Every permission gate — PreToolUse hook, disabled mask, schema
        // validation, operation-scope gate, bash policy, and the broker's
        // explicit-grant/development fast paths — runs
        // as one chain evaluation (see `permission_policy`). The chain is
        // async because some gates await (hooks, bash policy). Outcomes:
        //   • Deny    → short-circuit with the policy's output.
        //   • Approve → proceed under existing authority.
        //   • MissingAuthority → interactive: ask once; auto-approve: proceed without persisting; autonomous (no human channel): fail closed.
        //   • Pass    → (chain fallback) proceed.
        let target = tool.scope_target(&arguments);
        // Snapshot the disable masks and scope *before* the chain runs, then
        // drop the guards — the chain is async and MutexGuards are not Send, so
        // they must not live across the `.await`.
        let (disabled_snapshot, scoped_snapshot) = {
            let disabled = self
                .tools
                .disabled_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let scoped = self
                .tools
                .scoped_disabled_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            (disabled, scoped)
        };
        let pctx = guardrails::permission_policy::PolicyContext {
            tool: &tool,
            call_name: call.name.as_str(),
            arguments: &arguments,
            scope_target: target.clone(),
            disabled: disabled_snapshot,
            scoped_disabled: scoped_snapshot,
            unattended: self.unattended(),
            ctx: self, // Agent: PermissionContext
        };

        match self.permission_chain().evaluate(&pctx).await {
            guardrails::permission_policy::PolicyDecision::Pass
            | guardrails::permission_policy::PolicyDecision::Approve => {}
            guardrails::permission_policy::PolicyDecision::Deny { output, .. } => {
                return output;
            }
            guardrails::permission_policy::PolicyDecision::MissingAuthority { request, rule } => {
                if self.unattended() {
                    // Under auto-approve mode, missing authority is auto-approved
                    // in-flight without persisting a rule to `PermissionStore`.
                } else {
                    // The single interactive-park path. Both the broker (a
                    // write/execute the user must approve) and the bash
                    // dangerous-command confirm reach here; `request.one_off`
                    // distinguishes them. Fill the request id, emit, fire
                    // observe hooks, await the user's decision.
                    let one_off = request.one_off;
                    let request = tool::PermissionRequest {
                        id: format!("permission_{}", uuid::Uuid::new_v4()),
                        ..*request
                    };
                    // Posture gate: permissions fail closed when no
                    // human channel exists — a missing human cannot grant
                    // authority. (Delegated sessions reach this arm only with
                    // an interactive watcher attached, so this is the belt to
                    // that braces.)
                    if self.human_posture() == HumanChannelPosture::Autonomous {
                        self.control
                            .human_broker
                            .metrics_note_refused(HumanRequestKind::Permission);
                        tracing::warn!(
                            tool = %request.tool,
                            "autonomous posture: permission refused (fail closed)"
                        );
                        return permission_required_output(&request);
                    }
                    let receiver = self
                        .control
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
                    let preferred_rule = guardrails::PermissionRule {
                        tool: rule.tool.clone(),
                        scope: request
                            .suggested_scope
                            .clone()
                            .unwrap_or_else(|| rule.scope.clone()),
                    };
                    match decision {
                        PermissionDecision::Once => {
                            tracing::info!(tool = %tool.name(), decision = "once", "permission granted for single invocation");
                        }
                        PermissionDecision::Session => {
                            tracing::info!(tool = %tool.name(), scope = %preferred_rule.scope, decision = "session", "permission granted for current session");
                            self.context.permissions.add_session(preferred_rule);
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
                                tracing::info!(
                                    tool = %tool.name(),
                                    scope = %preferred_rule.scope,
                                    decision = "always",
                                    "permission granted permanently for role & workspace"
                                );
                                self.context.permissions.add_always(preferred_rule);
                            }
                        }
                        PermissionDecision::AlwaysExact => {
                            if one_off {
                                tracing::info!(
                                    tool = %tool.name(),
                                    decision = "always_exact",
                                    "one-off permission granted (not persisted)"
                                );
                            } else {
                                tracing::info!(
                                    tool = %tool.name(),
                                    scope = %rule.scope,
                                    decision = "always_exact",
                                    "exact permission granted permanently for role & workspace"
                                );
                                self.context.permissions.add_always(rule);
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
                    "ask_user is unavailable: this session is running in auto-approve mode and no human \
                     is reachable to answer. Resolve the ambiguity yourself — pick the most \
                     reasonable default and proceed."
                        .to_string(),
                );
            }
            return self.execute_ask_user(call, event_tx).await;
        }

        // Input contract decision (before spawn), for command tools:
        //   1. opt-in model input (α): `allow_model_stdin` on AND the model
        //      supplied a `stdin` arg → Prefilled{model}.
        //   2. pre-spawn refusal / runtime supervision: the interactive
        //      classifier matched → Sealed (unattended / skip / no platform
        //      terminal) or Supervised (a controlled terminal + examiner).
        //   3. sealed (default hard floor): everything else.
        // For other tools, Sealed is always correct (they ignore input).
        let input = if call.name == "execute_command" {
            self.decide_command_input(&call.arguments)
        } else {
            tool::InputContract::default()
        };

        // The Subagent / ToolStream events carry the same id as the up-front
        // `ToolCall` event because ADR-0166 made them one value: `ToolCall.id`.
        // Workspace effect journal (ADR-0125): capture before the mutation.
        //
        // This bracket is deliberately *inside* the permission chain's success
        // path and *outside* the tool invocation: an uncaptured mutation must
        // never run, and a mutation that runs must always be completed. Both
        // halves are skipped wholesale when no journal is installed.
        let journal = self.workspace_journal();
        let pending_write = match journal.as_ref() {
            Some(journal) => {
                let thread_id = self.hook_session_id();
                let round_id = self.round_count().to_string();
                let turn_id = self.current_turn().to_string();
                // Dense seq per call: the journal's ordering unit is the
                // observation, and a batch of parallel calls must not collide.
                let seq = crate::runtime::workspace_journal::next_seq(&round_id, &turn_id, call_id);
                let coordinates = crate::runtime::CallCoordinates {
                    thread_id: &thread_id,
                    round_id: &round_id,
                    turn_id: &turn_id,
                    call_id,
                    seq,
                };
                let journal_arguments = serde_json::to_string(&arguments).unwrap_or_default();
                match crate::runtime::journal_begin(
                    journal.as_ref(),
                    self.workspace_root().as_deref(),
                    coordinates,
                    tool.as_ref(),
                    &journal_arguments,
                )
                .await
                {
                    Ok(pending) => Some((journal, pending)),
                    Err(error) => {
                        // Veto: an uncaptured mutation would be silently
                        // irreversible, which is the failure this exists to
                        // prevent.
                        return ToolOutput::Error {
                            message: format!(
                                "Refused to run `{}`: its effects could not be \
                                 recorded, so the change would not be reversible. {error}",
                                call.name
                            ),
                            detail: Some(
                                "The workspace journal is the only record of what a tool \
                                 changed. Without it /undo would claim more than it can do."
                                    .to_string(),
                            ),
                        };
                    }
                }
            }
            None => None,
        };

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
        let call_ctx = self
            .tool_context(call_id, &call_cancel)
            .with_stream_sink(Arc::new(event_stream_sink(
                event_tx.clone(),
                call_id.to_string(),
            )));
        let invocation = tool::ToolInvocation {
            call_id,
            arguments: &arguments,
            input,
            input_handler: Some(&input_supervisor),
        };
        let execution = tool.execute_streaming(
            &call_ctx,
            invocation,
            Box::new(|event| {
                let _ = event_tx.send(AgentEvent::Subagent {
                    parent_call_id: parent_call_id.clone(),
                    event,
                });
            }),
            &mut on_stream,
        );
        let output = match tool.timeout() {
            Some(budget) => match tokio::time::timeout(budget, execution).await {
                Ok(result) => result,
                Err(_) => {
                    // Make the dropped future's work actually stop:
                    // blocking-pool readers and protocol clients observe this
                    // token and abort/cancel their in-flight operation.
                    call_cancel.cancel();
                    Err(tool::ToolError::timeout(tool.name()))
                }
            },
            None => execution.await,
        }
        .unwrap_or_else(|err| ToolOutput::Error {
            message: err.to_string(),
            detail: None,
        });

        // Second half of the journal bracket: observe post-images and complete
        // the write-ahead rows. Runs on *both* success and failure because a tool
        // that errored halfway may still have written bytes — leaving the row
        // unconfirmed would make those bytes permanently un-undoable.
        if let Some((journal, pending)) = pending_write {
            crate::runtime::journal_finish(journal.as_ref(), pending).await;
        }

        // Finalize first: schema validation may itself turn a nominally
        // successful output into an error, and that error should still carry
        // the last observed progress position.
        let output = finalize_output(call, entry.as_ref(), output);
        attach_last_progress(output, call_ctx.last_progress())
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
        cancel: &CancellationToken,
        on_event: &mut F,
    ) -> Result<SingleToolOutcome, HarnessError>
    where
        F: FnMut(AgentEvent) + Send,
    {
        let call_id = call.id.as_str();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let fut = self.execute_tool(call, &tx, cancel);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let cancellable = self.tools.tool_manager
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
                    if let Some(entry) = self.current_tool_snapshot().find(&call.name) {
                        entry.tool.request_cancel(call_id);
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
    pub(crate) fn accesses_for_call(&self, call: &ToolCall) -> tool::ToolAccesses {
        let snapshot = self.current_tool_snapshot();
        match snapshot.find(&call.name) {
            Some(entry) => {
                let value = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .unwrap_or(serde_json::Value::Null);
                entry.tool.accesses(&value)
            }
            None => tool::ToolAccesses::none(),
        }
    }
}

/// Per-invocation [`InputHandler`](tool::InputHandler): bridges the
/// command tool's runtime examiner back into the agent's human-input channel.
/// Constructed in [`Agent::execute_tool`] for the duration of one call, so it
/// captures that call's event channel and emits the TUI's input panel request
/// on the right stream.
struct AgentInputSupervisor<'a> {
    agent: &'a Agent,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
}

#[async_trait::async_trait]
impl tool::InputHandler for AgentInputSupervisor<'_> {
    async fn resolve(&self, prompt: tool::InputPrompt) -> Option<String> {
        self.agent
            .collect_runtime_input(&prompt, &self.event_tx)
            .await
    }
}

#[cfg(test)]
mod path_boundary_tests {
    use super::admit_model_path;
    use std::path::{Path, PathBuf};

    /// A scratch root outside the platform temp directory. The production
    /// policy deliberately admits temp paths, so a tempdir fixture cannot
    /// exercise a lexical escape.
    fn scratch(tag: &str) -> PathBuf {
        let base = std::env::current_dir()
            .unwrap()
            .join("target/test-scratch/path-boundary")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn relative_paths_inside_the_workspace_are_admitted() {
        let root = scratch("inside");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn main() {}").unwrap();
        assert!(admit_model_path(&root, &[], "src/lib.rs").is_ok());
        assert!(admit_model_path(&root, &[], "./src/../src/lib.rs").is_ok());
        cleanup(&root);
    }

    #[test]
    fn lexical_escapes_are_refused() {
        let root = scratch("lexical");
        assert!(admit_model_path(&root, &[], "../outside.txt").is_err());
        assert!(admit_model_path(&root, &[], "/etc/passwd").is_err());
        cleanup(&root);
    }

    #[test]
    fn additional_roots_are_admitted() {
        let root = scratch("primary");
        let extra = scratch("extra");
        std::fs::write(extra.join("notes.md"), "notes").unwrap();
        assert!(
            admit_model_path(
                &root,
                std::slice::from_ref(&extra),
                &extra.join("notes.md").to_string_lossy()
            )
            .is_ok()
        );
        cleanup(&root);
        cleanup(&extra);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escapes_are_refused() {
        let root = scratch("symlink-root");
        let outside = scratch("symlink-outside");
        std::fs::write(outside.join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        assert!(admit_model_path(&root, &[], "escape/secret").is_err());
        cleanup(&root);
        cleanup(&outside);
    }
}
