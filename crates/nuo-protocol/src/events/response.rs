//! Response-side wire types: agent responses and context-token snapshots.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentResponse {
    /// A per-round event tagged with the thread it belongs to and, when the
    /// event mutated a turn's transcript state, with that turn's context.
    ///
    /// The TUI keys its transcript buffers by `thread_id` and routes the event
    /// to the matching one without clobbering each other's transcript.
    ///
    /// Global (non-thread-scoped) responses — command replies, modal
    /// snapshots, provider switches — stay as dedicated top-level variants so
    /// they are handled once, regardless of which view is focused.
    Round {
        thread_id: String,
        scope: RoundScope,
    },
    /// Backend-owned completion result for [`AgentRequest::CompleteComposer`].
    /// The echoed text/cursor make stale-response rejection explicit even for
    /// clients that restart their local request counter after reconnecting.
    #[serde(alias = "InputCompletions")]
    ComposerCompletions {
        request_id: u64,
        #[serde(alias = "input")]
        text: String,
        cursor: usize,
        items: Vec<crate::ComposerCompletion>,
    },
    /// The persisted prompt input history, in stored order — reply to
    /// [`AgentRequest::QueryInputHistory`].
    InputHistory(Vec<crate::HistoryEntry>),
    /// Cross-project thread search hits — reply to
    /// [`AgentRequest::SearchHistory`]. Each hit anchors a transcript snippet
    /// to its thread so a dashboard/picker can jump straight to it.
    HistorySearch(Vec<crate::HistorySearchHit>),
    /// The stored capability overrides for one provider/model route — reply
    /// to [`AgentRequest::QueryRouteSettings`].
    RouteSettings {
        provider_id: String,
        model: String,
        overrides: Option<crate::model::CapabilityOverrides>,
    },
    PermissionsCleared,
    /// Lowercase provider name → whether a usable API key is configured.
    ProviderKeys(Vec<(String, bool)>),
    /// Full provider-picker state (default id + one row per provider) for the
    /// provider picker. Supersedes `ProviderKeys` for the picker's needs;
    /// `ProviderKeys` is retained for the header key-readiness summary.
    ProviderPicker(ProviderPickerSnapshot),
    /// Copy text to the client's system clipboard (used by `/export` in server mode).
    CopyToClipboard {
        text: String,
    },
    /// Blank the visible transcript and zero the round counter: the harness
    /// switched to a brand-new empty thread (`/new`, `/thread new`). The
    /// previous thread is untouched on disk — nothing was deleted.
    ///
    /// **Both ids are required** (ADR-0123 `[INV-TAB-13]`). `previous_id` is the
    /// thread the store held *before* the reset; `thread_id` is the freshly
    /// minted one. A frontend rebases the tab that represents `previous_id` —
    /// never "the active tab". Without `previous_id` a client cannot tell which
    /// tab the switch belongs to, and a positional guess rewrites the wrong one
    /// (the ghost-tab race this ADR closes). `thread_id` is the post-switch id,
    /// mirroring [`AgentResponse::ConversationReplaced`], so thread-scoped state
    /// (inline ↑/↓ prompt recall, on-demand queries) follows the switch.
    ConversationCleared {
        /// The thread being left. A tab carrying this id is the one to rebase.
        previous_id: String,
        /// The freshly minted thread id.
        thread_id: String,
    },
    /// Replace the visible transcript (dialogue messages) AND the command
    /// ledger with another thread's state, after `/thread open`,
    /// `/resume`, `/thread resume`. The frontend rebuilds the whole document
    /// from these two sources: pure dialogue from `messages`, command rows
    /// from `commands`. `thread_id` identifies the thread that produced the
    /// replacement (the harness emits this only as a thread switch, so it is
    /// the post-switch id); attached frontends track it to keep on-demand
    /// queries (e.g. [`AgentRequest::QueryTokenUsage`]) thread-correct.
    ConversationReplaced {
        /// The thread being left (ADR-0123 `[INV-TAB-13]`). Same contract as
        /// [`AgentResponse::ConversationCleared::previous_id`]: a frontend rebases
        /// the tab that represents the thread it is *leaving*, never "the active
        /// tab" — the two differ whenever a thread switch raced the frame.
        previous_id: String,
        thread_id: String,
        messages: Vec<Message>,
        #[serde(default)]
        commands: Vec<crate::command::CommandRecord>,
        /// Round-interrupt records (C11): re-projected into the transcript at
        /// their timestamp seams so the resumed thread shows which rounds
        /// were stopped, why, and when.
        #[serde(default)]
        round_interrupts: Vec<RoundInterrupt>,
    },
    /// Replace the threads picker contents. Data responses never navigate;
    /// slash-command presentation is signalled separately.
    ThreadsOverview(Vec<ThreadOverview>),
    /// Presentation signal for bare `/threads` / `/thread list`.
    OpenThreadsPanel,
    /// Replace the thread-tree view's data without changing navigation. The
    /// thread id lets a frontend reject a reply that raced a thread switch.
    ThreadTreeSnapshot {
        thread_id: String,
        tree: crate::ThreadTree,
    },
    /// Presentation signal for `/tree`.
    OpenTreePanel,
    /// Reply to [`AgentRequest::QueryThreadDetail`]: full detail for one
    /// thread (complete last prompt, title, timestamps). Consumed by the
    /// thread-info sub-view.
    ThreadDetail(ThreadDetail),
    /// Reply to [`AgentRequest::QueryConnectionDetail`]: full detail and usage
    /// for one connection (identity, endpoint, auth, models, usage/balance).
    ConnectionDetail(crate::ConnectionDetail),
    /// Reply to [`AgentRequest::QueryProviderQuotas`] (ADR-0036): aggregated
    /// provider quota inspection overview across configured connections.
    ProviderQuotas(crate::ProviderQuotaSnapshot),
    /// Reply to [`AgentRequest::QueryTokenUsage`]: the server-side token-source
    /// report for one thread (per-round request usage, reported vs.
    /// estimated). Attached frontends hold no local ledger, so the
    /// context-usage modal renders this snapshot; the thread id lets the
    /// frontend discard a reply that raced a thread switch.
    TokenUsageReport {
        thread_id: String,
        report: crate::token_ledger::TokenSourceReport,
    },
    /// Reply to [`AgentRequest::QueryUsageStats`]: the cross-thread usage
    /// report (per-day / per-model totals + recent event log) aggregated from
    /// the durable usage store. Unlike [`Self::TokenUsageReport`] this data is
    /// thread-independent — it outlives thread deletion by design
    /// so no thread id accompanies it.
    UsageStatsReport {
        report: crate::usage_stats::UsageStatsReport,
    },
    Error(String),
    Exit,
    ProviderSwitched {
        provider: String,
        model: String,
    },
    /// Progress of an OAuth connect/authorize flow (xAI SuperGrok).
    ConnectStatus(ConnectStatus),
    /// Full thread-context snapshot (model + tools + permissions + skills +
    /// mcp) for the thread modal. Sent in reply to [`AgentRequest::QueryThreadContext`]
    /// and re-sent after any mutation handled by the harness
    /// ([`AgentRequest::RevokePermission`] / [`AgentRequest::ToggleTool`]).
    ThreadContext(ThreadContextSnapshot),
    /// The transcript layout preference was updated (from the `/config` modal
    /// via [`AgentRequest::UpdateTuiLayout`]). Carries the persisted config
    /// string so the modal re-renders from the authoritative state — the TOML
    /// write is the source of truth, not the TUI's optimistic local edit.
    TuiLayoutUpdated(String),
    /// The TUI color scheme and custom palette were persisted successfully.
    TuiColorSchemeUpdated {
        name: String,
        custom: serde_json::Value,
    },
    /// Authoritative singleton web configuration; never contains secret text.
    WebSearchConfigSnapshot(WebConfigView),
    /// Validated, persisted and hot-applied authoritative state.
    WebSearchConfigUpdated(WebConfigView),
    /// Response with current server endpoints status (ADR-0045).
    ServerEndpointsStatus(crate::ServerEndpointsReport),
    /// Full authoritative declarative thread snapshot upon attachment (ADR-0049).
    ThreadSnapshot(Box<ThreadSnapshot>),
    /// Readiness transition for the attached thread (ADR-0133
    /// `[INV-ATTACH-04]`). Sent after Welcome; `Ready` always follows the
    /// authoritative snapshot on the same connection.
    ThreadPhase(ThreadPhaseUpdate),
    /// Incremental state delta update (ADR-0049).
    StateDelta(StateDelta),
}

/// Session-scoped events emitted while a user round runs, carried under an
/// [`AgentResponse::Round`] envelope. Splitting these off
/// `AgentResponse` makes "which session does this belong to" a first-class
/// question: every event arrives tagged with its `session_id`, and global/command responses stay top-level.
/// Origin of the current model-context token count shown by frontends.
///
/// This describes the AI-visible request context, never the durable session or
/// rendered transcript size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextTokenSource {
    /// Count reported by the provider for the completed request, plus that
    /// request's completion (which becomes history for the next request).
    Api,
    /// Local estimate of the provider-visible projection of `model_window`.
    Projection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextTokenSnapshot {
    pub tokens: usize,
    pub source: ContextTokenSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overhead_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_tokens: Option<usize>,
    /// Request-local temporary-context tokens (`E_n`), reported separately from
    /// durable input so diagnostics do not conflate the two.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary_context_tokens: Option<usize>,
}

impl ContextTokenSnapshot {
    pub fn new(tokens: usize, source: ContextTokenSource) -> Self {
        Self {
            tokens,
            source,
            overhead_tokens: None,
            history_tokens: None,
            temporary_context_tokens: None,
        }
    }

    pub fn from_estimate(
        estimate: crate::RequestTokenEstimate,
        source: ContextTokenSource,
    ) -> Self {
        Self {
            tokens: estimate.total_tokens,
            source,
            overhead_tokens: Some(estimate.overhead_tokens),
            history_tokens: Some(estimate.history_tokens),
            temporary_context_tokens: Some(estimate.temporary_context_tokens),
        }
    }
}
