//! Attach-time state synchronization buffer and session snapshot builders.
use nuo_protocol::AgentResponse;

/// Attach-time state sync buffer.
///
/// Rather than an ephemeral FIFO queue that is drained destructively by the
/// first attacher and evicts startup provider/picker state over long sessions,
/// `AttachSyncBuffer` retains latest-value state projections for each
/// attach-time event class (keys readiness, picker state, context tokens, harness state).
///
/// Every attaching client (initial attach, second attacher, or lagged reconnect)
/// receives an idempotent, non-destructive snapshot of the current state.
#[derive(Debug, Clone, Default)]
pub struct AttachSyncBuffer {
    provider_switched: Option<AgentResponse>,
    provider_keys: Option<AgentResponse>,
    provider_picker: Option<AgentResponse>,
    context_tokens: Option<AgentResponse>,
    harness_state: Option<AgentResponse>,
}

impl AttachSyncBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the buffer with an incoming response if it is an attach-sync event.
    pub fn observe(&mut self, response: &AgentResponse) {
        match response {
            AgentResponse::ProviderSwitched { .. } => {
                self.provider_switched = Some(response.clone());
            }
            AgentResponse::ProviderKeys(_) => {
                self.provider_keys = Some(response.clone());
            }
            AgentResponse::ProviderPicker(_) => {
                self.provider_picker = Some(response.clone());
            }
            AgentResponse::Round { scope, .. } => match scope.event() {
                nuo_protocol::RoundEvent::ContextTokens(_) => {
                    self.context_tokens = Some(response.clone());
                }
                nuo_protocol::RoundEvent::HarnessState(_) => {
                    self.harness_state = Some(response.clone());
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Convenience method for pushing/recording an event into the buffer.
    pub fn push(&mut self, response: AgentResponse) {
        self.observe(&response);
    }

    /// Produce a non-destructive snapshot of the buffered startup state.
    /// Order is deterministic: switched -> keys -> picker -> context tokens -> harness state.
    pub fn snapshot(&self) -> Vec<AgentResponse> {
        let mut items = Vec::with_capacity(5);
        if let Some(ref ps) = self.provider_switched {
            items.push(ps.clone());
        }
        if let Some(ref pk) = self.provider_keys {
            items.push(pk.clone());
        }
        if let Some(ref pp) = self.provider_picker {
            items.push(pp.clone());
        }
        if let Some(ref ct) = self.context_tokens {
            items.push(ct.clone());
        }
        if let Some(ref hs) = self.harness_state {
            items.push(hs.clone());
        }
        items
    }

    pub fn is_empty(&self) -> bool {
        self.provider_switched.is_none()
            && self.provider_keys.is_none()
            && self.provider_picker.is_none()
            && self.context_tokens.is_none()
            && self.harness_state.is_none()
    }

    pub fn len(&self) -> usize {
        let mut count = 0;
        if self.provider_switched.is_some() {
            count += 1;
        }
        if self.provider_keys.is_some() {
            count += 1;
        }
        if self.provider_picker.is_some() {
            count += 1;
        }
        if self.context_tokens.is_some() {
            count += 1;
        }
        if self.harness_state.is_some() {
            count += 1;
        }
        count
    }

    /// Produce a declarative SessionSnapshot from the buffered state.
    ///
    /// The attach path uses `build_full_session_snapshot`, which adds the
    /// durable transcript and ledger; this method covers the live state-sync
    /// fields that both share.
    pub fn build_session_snapshot(&self, session_id: &str) -> nuo_protocol::ThreadSnapshot {
        let (provider, model) = match &self.provider_switched {
            Some(AgentResponse::ProviderSwitched { provider, model }) => {
                (provider.clone(), model.clone())
            }
            _ => ("default".to_string(), "default".to_string()),
        };
        let provider_keys = match &self.provider_keys {
            Some(AgentResponse::ProviderKeys(k)) => Some(k.clone()),
            _ => None,
        };
        let provider_picker = match &self.provider_picker {
            Some(AgentResponse::ProviderPicker(p)) => Some(p.clone()),
            _ => None,
        };
        let context_tokens = match &self.context_tokens {
            Some(AgentResponse::Round { scope, .. }) => match scope.event() {
                nuo_protocol::RoundEvent::ContextTokens(ct) => Some(*ct),
                _ => None,
            },
            _ => None,
        };
        let harness_state = match &self.harness_state {
            Some(AgentResponse::Round { scope, .. }) => match scope.event() {
                nuo_protocol::RoundEvent::HarnessState(hs) => Some(hs.clone()),
                _ => None,
            },
            _ => None,
        };

        nuo_protocol::ThreadSnapshot {
            thread_id: session_id.to_string(),
            round_counter: 0,
            messages: Vec::new(),
            provider,
            model,
            todos: Vec::new(),
            context_tokens,
            harness_state,
            activity: None,
            provider_picker,
            provider_keys,
            commands: Vec::new(),
            round_interrupts: Vec::new(),
            command_catalog: None,
            watermark_seq: 0,
        }
    }
}

/// Build the authoritative hydration frame for one hosted session.
///
/// `[INV-ATTACH-04]` (ADR-0133): this is the complete visible state — durable
/// transcript and command ledger included — so a late attacher never needs a
/// second frame to become correct. It is only called *after* Welcome, so its
/// local reads never delay the handshake.
pub(crate) async fn build_full_session_snapshot(
    hosted: &crate::runtime::registry::HostedThread,
) -> nuo_protocol::ThreadSnapshot {
    let session = &hosted.session;
    let session_id = session.id().await;
    let mut snapshot = {
        let buffer = hosted.sync_buffer.lock().await;
        buffer.build_session_snapshot(&session_id)
    };
    snapshot.messages = session.full_transcript().await;
    snapshot.round_counter = session.round_counter().await;
    snapshot.todos = session.todos().await.items;
    snapshot.commands = session.commands().await;
    snapshot.round_interrupts = session.round_interrupts().await;
    if let Some(selection) = session.provider_selection().await {
        snapshot.provider = selection.connection;
        snapshot.model = selection.model.unwrap_or_default();
    } else if snapshot.provider == "default" {
        snapshot.provider = hosted.initial_provider.clone();
        snapshot.model = hosted.initial_model.clone();
    }
    let workspace_security = match hosted.workspace_root.as_deref() {
        Some(root) => hosted.security.snapshot(root),
        None => nuo_protocol::WorkspaceSecuritySnapshot::new("workspace-free"),
    };
    // The trust snapshot MUST ride in the same frame that precedes `Ready`
    // (the pre-view gate is mounted from it, ADR-0127).
    snapshot.harness_state = Some(nuo_protocol::HarnessSnapshot {
        loop_status: snapshot
            .harness_state
            .as_ref()
            .map(|harness| harness.loop_status)
            .unwrap_or(nuo_protocol::LoopStatus::Idle),
        round_counter: snapshot.round_counter,
        unattended: session.unattended().await,
        confined: hosted.shared_confinement.is_confined(),
        workspace_security,
        retry_pending: session.retry_pending().await.is_some(),
        role: session.role(),
        workspace: session
            .workspace()
            .map(|workspace| workspace.root.to_string_lossy().to_string()),
    });
    snapshot.command_catalog = Some(hosted.command_catalog.clone());
    snapshot
}

/// Inform a newly attached client that project-authored configurations are
/// awaiting a human decision.
///
/// This is the **fallback**, not the primary affordance. An interactive client
/// reads the same snapshot out of the attach-sync `HarnessState` and opens the
/// pre-view gate before the composer takes input; only a client that cannot
/// answer (an autonomous attach) needs the passive escalation, because a
/// question pushed to a channel nobody reads would park the security decision
/// forever.
///
/// Trust mutation is intentionally available only through the canonical
/// `/trust` command path, which also reloads every affected consumer
/// atomically.
pub(crate) fn workspace_trust_notice(
    session_id: &str,
    snapshot: &nuo_protocol::WorkspaceSecuritySnapshot,
) -> AgentResponse {
    let round = |event: nuo_protocol::RoundEvent| AgentResponse::Round {
        thread_id: session_id.to_string(),
        scope: nuo_protocol::RoundScope::Session { event },
    };
    let awaiting = snapshot.domains_needing_review();
    let changed_desc = if awaiting.is_empty() {
        "workspace files".to_string()
    } else {
        awaiting
            .iter()
            .map(|domain| domain.label())
            .collect::<Vec<_>>()
            .join(", ")
    };

    round(nuo_protocol::RoundEvent::Notice(
        nuo_tool::AgentNotice::trust_changed("Workspace configuration changed").with_body(format!(
            "Awaiting review: {changed_desc}.\n\
             Run `/trust` to approve and reload, `/trust deny` to ignore until the content \
             changes, or `/untrust` to revoke every grant for this workspace.",
        )),
    ))
}

/// Whether `response` is an attach-time state-sync event: one of the
/// startup emissions a client that attaches *after* the session began can
/// never reconstruct (active context projection, harness snapshot, key
/// readiness, picker state). These are buffered per session and replayed to
/// a new client right after the welcome so the TUI hydrates immediately.
pub(crate) fn is_attach_sync_event(response: &AgentResponse) -> bool {
    match response {
        AgentResponse::ProviderSwitched { .. }
        | AgentResponse::ProviderKeys(_)
        | AgentResponse::ProviderPicker(_) => true,
        AgentResponse::Round { scope, .. } => matches!(
            scope.event(),
            nuo_protocol::RoundEvent::ContextTokens(_) | nuo_protocol::RoundEvent::HarnessState(_)
        ),
        _ => false,
    }
}
