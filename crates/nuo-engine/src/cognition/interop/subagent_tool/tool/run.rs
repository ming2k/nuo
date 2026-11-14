//! Subagent execution: provider loop, child transcript, and accounting.

use super::*;

impl SubagentTool {
    pub(super) async fn run_subagent_outcome<'a>(
        &self,
        call_id: Option<&str>,
        arguments: &str,
        mut on_event: Box<dyn FnMut(nuo_protocol::SubagentEvent) + Send + 'a>,
    ) -> Result<SubagentOutcome, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let description = args["description"]
            .as_str()
            .ok_or("Missing 'description'")?
            .trim();
        let prompt = args["prompt"].as_str().ok_or("Missing 'prompt'")?;
        if description.is_empty() {
            return Err("'description' must not be empty.".to_string());
        }
        if prompt.trim().is_empty() {
            return Err("'prompt' must not be empty.".to_string());
        }

        // The role is a capability grant, not a free-form label: an explicit
        // value must be one this tool advertises, and is refused with the
        // dispatchable set named instead of being silently downgraded to the
        // bound default (which would hand the caller a read-only child while it
        // planned for a write-capable one).
        let profile = match args.get("role").and_then(|role| role.as_str()) {
            // Absent role: the bound profile, verbatim. It is deliberately
            // resolved without the pool — a dispatch tool may be bound to a
            // caller-supplied preset that has no pool entry (session-specific
            // and test profiles do exactly that), and rewriting that to a pool
            // default would silently change the child's capability grant.
            None => self.profile,
            Some(role) => {
                if !DISPATCH_ROLES.contains(&role) {
                    return Err(format!(
                        "Unknown 'role' '{role}'. Dispatchable roles: {DISPATCH_ROLES:?}. \
                         Omit 'role' to use the default ('{}').",
                        self.profile.name
                    ));
                }
                nuo_protocol::SubAgentProfile::find(role).ok_or_else(|| {
                    format!(
                        "Role '{role}' is advertised by this dispatch tool but has no preset in \
                         the pool. This is a dispatch-tool configuration error, not a caller \
                         error."
                    )
                })?
            }
        };

        let is_background = args
            .get("background")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if is_background {
            // The parameter is gone from the schema, and the
            // execution path below awaits the child round inside this tool
            // call — there is no background job registration, no `job_id`, and
            // no result to poll. Rejecting an explicitly requested background
            // dispatch is honest; silently blocking would let the caller plan
            // around a continuation that never arrives.
            return Err(
                "Background sub-agent dispatch is not available: this sub-agent runs to \
                 completion inside the calling turn. Re-issue the call without `background` \
                 to run it synchronously and receive its result."
                    .to_string(),
            );
        }

        if let Some(delegation) = self
            .parent_delegation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && !delegation.admits_subagent(profile.name)
        {
            return Err(format!(
                "Agent role '{}' does not admit subagent role '{}'. Admitted roles: {:?}",
                delegation.role_id, profile.name, delegation.subagent_roles
            ));
        }

        if let Some(parent_policy) = self
            .parent_execution_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && !parent_policy.can_spawn_subagent()
        {
            return Err(format!(
                "Sub-agent spawning is rejected by ExecutionPolicy: current depth {} has reached max_depth {}.",
                parent_policy.depth, parent_policy.max_depth
            ));
        }

        // Announce the bound profile name first so the parent harness / TUI
        // can label this subagent by its role (explore / plan / verify / …)
        // rather than a generic "Subagent". Emitted before the child runs.
        on_event(nuo_protocol::SubagentEvent::Started {
            profile: profile.name.to_string(),
        });

        // Resolve the pool for this subagent: profile selection ⊓ model selection.
        // The subagent is an agent on the *same* model as the parent, so it carries
        // the parent's model (capability limits + variant overrides). The profile
        // contributes the role scope and any variant pins; the model contributes
        // its variant overrides (snapshotted from the parent) and its hard
        // capability limits. `resolve_tools` composes both and applies the subagent
        // runtime hard rules (no recursion / control-flow / blocking-on-user).
        let model = nuo_protocol::resolve_model(&self.provider.model());
        let model_sel =
            nuo_protocol::ToolSelection::unrestricted().with_variants(self.variant_snapshot());
        let sub_tools = profile.resolve_tools(&self.toolset, &model, &model_sel);

        // The subagent's identity *is* its profile's task prompt — that is the
        // role framing for this child (e.g. SubAgentProfile::EXPLORE's research mission),
        // while posture (no human interaction, ephemeral scratchpad, depth cap)
        // is enforced by the execution policy below, not by prose.
        let identity = crate::AgentIdentity::from_directive(profile.system_prompt);
        let mut subagent = Agent::new(self.provider.clone(), sub_tools, identity);
        subagent.set_kind(nuo_protocol::AgentKind::Subagent);
        // Role-scoped project instructions select on the staffing role. The
        // child's identity is its profile task prompt, so the profile name is
        // the role it must resolve against.
        subagent.set_active_role(Some(profile.name.to_string()));

        if let Some(handle) = self
            .parent_user_instructions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            subagent.bind_user_instructions(Arc::clone(handle));
        }
        if let Some(handle) = self
            .parent_project_instructions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            subagent.bind_project_instructions(Arc::clone(handle));
        }
        if let Some(handle) = self
            .parent_instruction_provider_kind
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            subagent.bind_instruction_provider_kind(Arc::clone(handle));
        }

        // Bind instance-scoped extensions according to the child's mission.
        match profile.name {
            "explore" => {
                subagent.add_extension(std::sync::Arc::new(
                    crate::runtime::extension::CodeIntelligenceExtension::read_only(),
                ));
            }
            "code" => {
                subagent.add_extension(std::sync::Arc::new(
                    crate::runtime::extension::CodeIntelligenceExtension::new(),
                ));
            }
            _ => {}
        }

        let parent_policy = self
            .parent_execution_policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(nuo_protocol::ExecutionPolicy::root_default);
        let child_policy = parent_policy
            .derive_child(Some(profile.tool_policy))
            .unwrap_or_else(|_| nuo_protocol::ExecutionPolicy {
                depth: parent_policy.depth + 1,
                max_depth: parent_policy.max_depth,
                allow_human_interaction: false,
                lifecycle: nuo_protocol::ContextLifecycle::EphemeralScratchpad,
                tool_policy: Some(profile.tool_policy),
                max_children_budget: 0,
            });
        subagent.set_execution_policy(child_policy);

        if let Some(handle) = self
            .parent_workspace_security
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .cloned()
        {
            subagent.bind_workspace_security_handle(handle);
        }
        let subagent = Arc::new(subagent);
        if let Some(accounting) = self
            .accounting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            let session_id = accounting
                .session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default();
            let round = *accounting
                .round_counter
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let actor = call_id
                .map(|id| format!("subagent:{id}"))
                .unwrap_or_else(|| format!("subagent:{}", uuid::Uuid::new_v4()));
            subagent.set_thread_id(session_id);
            subagent.restore_round_count(round);
            subagent.set_accounting_actor_id(actor);
            subagent.install_token_ledger(accounting.ledger);
        }
        // A `task` subagent runs unobstructed: disable the deterministic
        // read-loop guard's nudge (ADR-0034) so a short-lived, parent-supervised
        // subagent is never steered by it. The parent and `abort` remain its
        // backstops.
        subagent.set_trajectory_guard_config(nuo_protocol::TrajectoryGuardConfig::disabled());
        // Full-duplex (ADR-0029): install the child's steering inbox and lodge
        // its handle in the registry keyed by the parent tool-call id. Now any
        // permission / `ask_user` request the child surfaces travels *up* via
        // `forward_event`, and the user's reply can travel *down* via the
        // registry → handle → `reply_permission` / `reply_user_question`,
        // resolving the child's parked oneshot. A `None` call_id (the bare
        // `call` path, no harness involvement) skips registration — there is no
        // one to reply, so the child must stay self-contained.
        let _handle = subagent.install_inbox();
        if let Some(id) = call_id {
            self.registry.register(id, _handle.clone());
        }
        // Full-duplex (ADR-0029): the broker gate is now profile-driven. The
        // built-in profiles keep `delegated: true` to preserve the legacy
        // autonomous contract, but a profile with `delegated: false` lets a
        // subagent's write/execute tool calls surface as
        // `SubagentEvent::PermissionRequest` up to the parent, with the user's
        // reply routed back down via the registry → handle →
        // `reply_permission` (the parked oneshot resolves directly, no inbox
        // drain needed).
        subagent.set_unattended(profile.unattended);
        // The child inherits the parent's human-channel posture
        // source. An interactive session's subagents can ask the user through
        // the parent's channel (permission requests flow up via
        // SubagentEvent); an autonomous parent's children never park on a
        // human that is not there — they fail closed / settle by labeled
        // policy in the child, instead of deadlocking the parent's tool
        // call on an unanswerable question.
        if let Some(accountant) = self.parent_human_channel() {
            subagent.set_human_channel_accountant(accountant);
        }
        // Subagents are short-lived and read-only by profile, and session
        // review is on-demand (`/review`) with no automatic firing — so a
        // research subagent never pays for a diagnostic and review can never
        // recurse. No setup needed here. ADR-0018.

        // The subagent's durable transcript opens with just the task as the user
        // message. Request assembly composes a fresh head system message every
        // round from the profile persona (carried via `AgentIdentity`, set
        // above) and mission-neutral system-prompt policy — see ADR-0061.
        //
        // An earlier `Task: {description}` system message here was dead code:
        // request assembly projects legacy system messages out before adding
        // the profile composition, so the task wrapper never reached the
        // model. Dropping it makes the single-message path honest. The task
        // itself is the user message; `description` remains a required label
        // arg (validated above) for the parent / TUI.
        let mut messages = vec![crate::runtime::message::visible_user(
            nuo_protocol::InjectionKind::SubagentTask,
            prompt,
        )];
        // The subagent runs under its own cancellation token. When the parent
        // turn is interrupted, the harness's executor calls
        // [`Tool::request_cancel`] on this tool with the parent tool-call id,
        // which cancels the stored token below; the child's round loop
        // observes it at its next safe boundary and returns its partial
        // transcript through the error arm of this function — so the parent
        // records the half-finished work instead of dropping it. A `None`
        // call_id (the bare `call` path, no harness involvement) means nothing
        // can cancel the child, so it keeps a fresh never-cancelled token.
        let child_cancel = CancellationToken::new();
        if let Some(id) = call_id {
            self.active_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), child_cancel.clone());
            // ALSO register the token in the shared registry so a scene-scoped
            // interrupt can stop this one child promptly — the token
            // races its in-flight model request, exactly like the primary's —
            // while leaving the parent round and every sibling running. The
            // tool's own `active_cancels` map stays the `Tool::request_cancel`
            // arm the parent's interrupt sweep uses.
            self.registry.register_cancel(id, child_cancel.clone());
        }
        // Open this subagent's own session beneath the parent, if one was bound.
        //
        // Its transcript is now durable *while it runs*, which is what makes
        // context relief possible at all: pruning, mid-turn projection and
        // compaction all target a session. Previously the child had no session
        // mid-run and its transcript was reconstructed after the fact from the
        // parent message's embedded `children`, so a long subagent round could
        // only grow until the provider rejected it.
        let bound_parent = self
            .parent_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let child_session = match bound_parent.as_ref() {
            Some(parent) => {
                let id = uuid::Uuid::new_v4().to_string();
                match nuo_persistence::thread::ThreadSession::for_subagent(parent, id) {
                    Ok((store, _parent_id)) => Some(std::sync::Arc::new(store)),
                    Err(error) => {
                        // Refusing is the honest outcome: a child without a parent
                        // pointer is invisible to `sub:` handles and permanently
                        // orphaned. Running session-free keeps the sub-task alive
                        // (its result still reaches the caller) while losing only
                        // durability — and says so.
                        tracing::warn!(
                            %error,
                            "subagent session could not be opened; running this child session-free"
                        );
                        None
                    }
                }
            }
            None => None,
        };
        if let Some(session) = child_session.as_ref() {
            // The task is the child's first (and, until it answers, only) user
            // message. Committing it up front means the transcript survives a
            // crash and a mid-turn projection has a durable base to fold against.
            //
            // The message must be built exactly as the live window builds it below
            // (`visible_user` with `SubagentTask`), otherwise the first projection
            // would see a window it cannot map onto the transcript.
            let seed = vec![crate::runtime::message::visible_user(
                nuo_protocol::InjectionKind::SubagentTask,
                prompt,
            )];
            if let Err(error) = session
                .commit_turn(nuo_persistence::thread::CommitTurn {
                    messages: &seed,
                    basis: nuo_persistence::thread::CommitBasis::Appended,
                    round_counter: Some(1),
                    usage_records: &[],
                    retry_point: None,
                    round_interrupt: None,
                    operation_id: None,
                    expected_revision: None,
                })
                .await
            {
                tracing::warn!(%error, "could not admit the subagent session; continuing session-free");
            }
        }

        // Track the subagent's own ReAct position as `ModelRequestStarted`
        // events arrive so the streamed `StreamStart` / `ToolCall` events can
        // carry it (mirroring the main session's `(round, turn)` stamping).
        let mut position: (u64, usize) = (1, 0);
        // Transient-provider-retry loop. The top-level interactive round
        // retries `HarnessError::Retryable` in `orchestration::execute_round`
        // (config: `[resilience] retry_max_attempts`), but a subagent runs through
        // `run_streaming_with_events` directly and had *no* retry at all —
        // one flaky long SSE generation (the GLM `xhigh`-effort stream that
        // gets cut mid-body) killed the whole sub-task after minutes of
        // work. Mirror the top-level contract here, bounded and simpler:
        // reuse the same round state across attempts so completed turns are
        // not replayed, back off exponentially, and never retry an
        // interruption, a hard terminal error, or a non-retryable one.
        let retry_config = *self.retry_config.lock().unwrap_or_else(|e| e.into_inner());
        let retry_limit = retry_config.max_attempts.clamp(1, 60);
        let short_id = call_id
            .map(|id| {
                let clean = id.trim_start_matches("call_");
                &clean[..clean.len().min(6)]
            })
            .unwrap_or("subagent");
        let origin_label = format!("subagent #{} | {}", short_id, profile.name);

        // Give the child its own relief valve.
        //
        // `project_context_if_needed` runs between every ReAct turn and asks the
        // agent's gate to project. A subagent previously had neither the threshold
        // nor the gate, so the check always returned early — and since a subagent
        // runs one round to completion, there is no round boundary where
        // `execute_round` could prune or compact for it. This is the only place
        // context relief can reach a child.
        if let Some(session) = child_session.as_ref() {
            // What relief a child gets is the composition root's decision (the gate
            // and its model-relative settings live above this layer). When nothing
            // was bound, the child runs session-free rather than silently getting
            // the *wrong* thresholds.
            let relief = self
                .subagent_relief
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(relief) = relief {
                relief(&subagent, std::sync::Arc::clone(session));
            }
            // Persist every completed turn. This is the child's mid-round save
            // point: a crash keeps the work, and the gate has a durable transcript
            // to fold against instead of projecting a window the session has never
            // seen (which it must refuse — a projection is an append-only fact, not
            // a rewritten history).
            let persist_session = std::sync::Arc::clone(session);
            let persist_agent = std::sync::Arc::clone(&subagent);
            subagent.set_turn_persist(std::sync::Arc::new(
                move |messages: &[nuo_protocol::Message]| {
                    let session = std::sync::Arc::clone(&persist_session);
                    let agent = std::sync::Arc::clone(&persist_agent);
                    let snapshot = messages.to_vec();
                    Box::pin(async move {
                        let basis = if agent.take_history_reprojected() {
                            nuo_persistence::thread::CommitBasis::Reprojected
                        } else {
                            nuo_persistence::thread::CommitBasis::Appended
                        };
                        session
                            .commit_turn(nuo_persistence::thread::CommitTurn {
                                messages: &snapshot,
                                basis,
                                round_counter: None,
                                usage_records: &[],
                                retry_point: None,
                                round_interrupt: None,
                                operation_id: None,
                                expected_revision: None,
                            })
                            .await
                            .map(|_| ())
                    })
                },
            ));
        }

        let mut round = subagent.begin_streaming_round();
        let mut attempt: usize = 0;
        let result = loop {
            attempt += 1;
            let run = subagent
                .resume_streaming_with_events(&mut messages, &child_cancel, &mut round, |event| {
                    if let nuo_protocol::AgentEvent::ModelRequestStarted { round, turn, .. } =
                        &event
                    {
                        position = (*round, *turn);
                    }
                    Self::forward_event(event, position, Some(&origin_label), &mut on_event)
                })
                .await;
            match run {
                Ok(outcome) => break Ok(outcome),
                Err(nuo_protocol::HarnessError::Provider(provider_err))
                    if {
                        matches!(
                            provider_err.retry_disposition(),
                            nuo_protocol::RetryDisposition::Retry { .. }
                        )
                    } && attempt < retry_limit =>
                {
                    let retry_after_ms = match provider_err.retry_disposition() {
                        nuo_protocol::RetryDisposition::Retry { retry_after_ms } => retry_after_ms,
                        _ => unreachable!(),
                    };
                    let message = provider_err.message();
                    let base_ms = crate::runtime::retry::retry_delay_ms(
                        attempt,
                        retry_after_ms,
                        retry_config.base_ms,
                        retry_config.max_ms,
                    );
                    let delay_ms = crate::runtime::retry::apply_jitter_ms(base_ms, |_| {
                        fastrand::u64(0..base_ms)
                    });
                    tracing::warn!(
                        attempt,
                        max_attempts = retry_limit,
                        delay_ms,
                        error = %message,
                        "subagent hit a transient provider error; retrying"
                    );
                    // The child's setback is typed state, not prose: the
                    // parent renders `retry N in Xs` from these numbers, phrased
                    // by the same owner as the principal round's clause.
                    on_event(nuo_protocol::SubagentEvent::RetryScheduled {
                        attempt,
                        max_attempts: retry_limit,
                        delay_ms,
                        message: message.to_string(),
                    });
                    tokio::select! {
                        _ = child_cancel.cancelled() => {
                            break Err(nuo_protocol::HarnessError::Interrupted)
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                    }
                }
                Err(error) => break Err(error),
            }
        };
        // Drop the registry entry for this call regardless of outcome so it
        // never holds a dead handle. The child `Arc` is also dropped here
        // (the last strong ref besides the registry's `Weak`), so any late
        // reply via the handle degrades to a no-op. The cancellation entry is
        // dropped alongside, so a late `request_cancel` finds nothing to
        // cancel and the harness falls back to dropping a finished call.
        if let Some(id) = call_id {
            self.registry.remove(id);
            self.active_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
        }
        match result {
            Ok(result) => {
                let final_content = result.message.content.clone();
                Ok(SubagentOutcome {
                    messages,
                    token_usage: result.token_usage,
                    final_content,
                    failed: false,
                    interrupted: false,
                    generation_ms: result.generation_ms,
                })
            }
            Err(error) => {
                // Interruption (the parent cancelled the turn): preserve the
                // partial transcript as an *interrupted* outcome — not an
                // error. The model must understand the sub-task was stopped by
                // the user (resumable work), not that it failed. On a genuine
                // failure we surface the partial transcript too — both so the
                // parent's tool-result message carries the subagent's
                // work-in-progress `children` and so the real token cost
                // reaches the parent round's accounting; the `final_content`
                // is prefixed `Error: …` so the failure classifier and the
                // TUI's Failed badge both trigger.
                if matches!(error, nuo_protocol::HarnessError::Interrupted) {
                    let tool_calls = messages
                        .iter()
                        .filter(|m| m.role == nuo_protocol::Role::Tool)
                        .count();
                    let partial = messages.iter().rev().find_map(|m| {
                        (m.role == nuo_protocol::Role::Assistant && !m.content.trim().is_empty())
                            .then(|| m.content.trim().to_string())
                    });
                    let final_content = match partial {
                        Some(text) => format!(
                            "Interrupted: the subagent was stopped by the user before completing. \
                             It ran {tool_calls} tool call(s) and produced the following partial \
                             findings:\n{text}"
                        ),
                        None => format!(
                            "Interrupted: the subagent was stopped by the user before producing any \
                             findings (it ran {tool_calls} tool call(s))."
                        ),
                    };
                    tracing::info!(
                        tool_calls,
                        "subagent interrupted by parent; preserving partial transcript"
                    );
                    return Ok(SubagentOutcome {
                        messages,
                        token_usage: nuo_protocol::TokenUsage::default(),
                        final_content,
                        failed: false,
                        interrupted: true,
                        generation_ms: 0,
                    });
                }
                let error_string = error.to_string();
                tracing::warn!(error = %error_string, "subagent failed; preserving partial transcript");
                Ok(SubagentOutcome {
                    messages,
                    token_usage: nuo_protocol::TokenUsage::default(),
                    final_content: format!("Error: {error_string}"),
                    failed: true,
                    interrupted: false,
                    generation_ms: 0,
                })
            }
        }
    }

    pub(super) async fn run_subagent<'a>(
        &self,
        call_id: Option<&str>,
        arguments: &str,
        on_event: Box<dyn FnMut(nuo_protocol::SubagentEvent) + Send + 'a>,
    ) -> Result<String, String> {
        let outcome = self
            .run_subagent_outcome(call_id, arguments, on_event)
            .await?;
        let content = outcome.final_content.trim().to_string();
        if content.is_empty() {
            Ok("(subagent returned no answer)".to_string())
        } else {
            Ok(content)
        }
    }

    fn forward_event(
        event: nuo_protocol::AgentEvent,
        position: (u64, usize),
        origin: Option<&str>,
        on_event: &mut dyn FnMut(nuo_protocol::SubagentEvent),
    ) {
        match event {
            nuo_protocol::AgentEvent::Notice(notice) => {
                on_event(nuo_protocol::SubagentEvent::Notice(notice));
            }
            nuo_protocol::AgentEvent::ModelRequestStarted { turn, .. } => {
                let status = if turn == 0 {
                    "waiting for model".to_string()
                } else {
                    format!("waiting for model (turn {})", turn + 1)
                };
                on_event(nuo_protocol::SubagentEvent::Activity(status));
            }
            nuo_protocol::AgentEvent::AssistantDelta { delta, start } => {
                if start {
                    on_event(nuo_protocol::SubagentEvent::StreamStart {
                        round: position.0,
                        turn: position.1,
                    });
                }
                on_event(nuo_protocol::SubagentEvent::StreamDelta(delta));
            }
            nuo_protocol::AgentEvent::AssistantEnd(content) => {
                on_event(nuo_protocol::SubagentEvent::StreamEnd(content));
            }
            // The subagent's reasoning chain, streamed live instead of surfacing
            // only after a session reload.
            nuo_protocol::AgentEvent::ReasoningDelta { delta, start } => {
                if start {
                    on_event(nuo_protocol::SubagentEvent::StreamReasoningStart {
                        round: position.0,
                        turn: position.1,
                    });
                }
                on_event(nuo_protocol::SubagentEvent::StreamReasoningDelta(delta));
            }
            nuo_protocol::AgentEvent::ReasoningEnd(content) => {
                on_event(nuo_protocol::SubagentEvent::StreamReasoningEnd(content));
            }
            nuo_protocol::AgentEvent::ToolCall {
                id,
                name,
                arguments,
            } => {
                on_event(nuo_protocol::SubagentEvent::ToolCall {
                    id,
                    name,
                    arguments,
                    round: position.0,
                    turn: position.1,
                });
            }
            nuo_protocol::AgentEvent::ToolResult {
                id,
                name,
                output,
                duration_ms,
                ..
            } => {
                on_event(nuo_protocol::SubagentEvent::ToolResult {
                    id,
                    name,
                    output,
                    duration_ms,
                });
            }
            // Full-duplex (ADR-0029): a permission broker request from the
            // child travels *up* stamped with the child's short hash and profile origin.
            nuo_protocol::AgentEvent::PermissionRequest(mut request) => {
                if request.origin.is_none() {
                    request.origin = origin.map(str::to_string);
                }
                on_event(nuo_protocol::SubagentEvent::PermissionRequest(request));
            }
            // Same full-duplex contract as the permission arm above.
            nuo_protocol::AgentEvent::UserQuestionRequest(mut request) => {
                if request.origin.is_none() {
                    request.origin = origin.map(str::to_string);
                }
                on_event(nuo_protocol::SubagentEvent::UserQuestionRequest(request));
            }
            // An interactive `bash` inside the subagent needs operator
            // stdin; forward the request up so the parent harness can surface
            // it, with the reply routed back down via `reply_input`.
            nuo_protocol::AgentEvent::StdinRequest(request) => {
                on_event(nuo_protocol::SubagentEvent::StdinRequest(request));
            }
            _ => {}
        }
    }
}
