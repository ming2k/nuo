//! `/btw` aside machinery (ADR-0017, extended by ADR-0103): the live aside
//! registry, the primary-status watcher that streams coarse updates to the
//! aside banner, and the active-round router that directs a prompt at
//! whichever session the user is currently composing into. Extracted verbatim
//! from `main.rs`.
//!
//! The primary round machinery is intentionally untouched here — an aside
//! peers the primary's per-round state with its own `Agent` + store +
//! [`RoundLifecycle`], so an aside round runs concurrently with the primary
//! round without disturbing the primary's round lifecycle. Where ADR-0017
//! kept at most one live side and tore it down on exit, ADR-0103 lifts the
//! registry to a map and makes leaving a view a non-destructive *detach*: the
//! aside keeps running until it is explicitly closed or discarded pristine.

use std::collections::HashMap;

use crate::session_driver::send_harness_state_for_session;
use nuo_harness::orchestration::{
    ContextProjectionSettings, InteractiveRoundContext, ProxyProvider, RoundInput, round_response,
    start_interactive_round,
};
use nuo_harness::{Agent, AgentIdentity, NoProvider, RoundLifecycle};
use nuo_wire::{AgentResponse, BtwAsideSummary, LoopStatus, ParentStatus, Provider, Tool};

use nuo_persistence::config::Config;
use nuo_persistence::session::SessionStore;
use nuo_harness::skills::SkillRegistry;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::agent_setup::active_context_window;

/// A live `/btw` aside (ADR-0017/0103). Peers the primary session's loose
/// per-round state with its own [`Agent`], [`SessionStore`], and
/// [`RoundLifecycle`], so an aside round runs concurrently with the primary
/// round without disturbing the primary's round lifecycle. The aside store is
/// the single source of truth for the aside's message list (ADR-0048); it is
/// pinned to a self-contained file written by [`SessionStore::fork_to_side`],
/// and only that file is mutated by aside rounds.
pub struct SideSession {
    pub id: String,
    pub agent: Arc<Agent>,
    pub store: Arc<SessionStore>,
    pub lifecycle: Arc<RoundLifecycle>,
    /// `true` once a round has ever been started in this aside. A pristine
    /// aside (no round) is discarded — registry entry and session files —
    /// when the user detaches from its view (ADR-0103 §4), so an opened-then-
    /// abandoned `/btw` never litters the asides list or `/sessions`.
    pub has_round: bool,
    /// How many user prompts the fork inherited from the parent (ADR-0103
    /// §6): the aside's own first prompt is the one *after* this baseline,
    /// so the asides list never shows the parent's prompt as an aside title.
    inherited_user_prompts: usize,
    /// The epoch-seconds timestamp of the aside's most recent open/re-entry,
    /// used to order the asides list newest-first.
    pub touched_at: u64,
}

impl SideSession {
    /// Fork the primary into a self-contained side file and construct a fresh
    /// [`Agent`] + store bound to it. The primary's active pointer and
    /// in-flight round are left untouched. Returns [`None`] when the fork or
    /// side-store open fails; the caller surfaces the error.
    pub async fn build(
        primary: &SessionStore,
        base_tools: &[Arc<dyn Tool>],
        provider_holder: &Arc<RwLock<Arc<dyn Provider>>>,
        skills: SkillRegistry,
        project_root: Option<&std::path::Path>,
        identity: AgentIdentity,
        workspace_security: Arc<std::sync::Mutex<nuo_wire::WorkspaceSecuritySnapshot>>,
    ) -> Result<Self, String> {
        let (side_id, _parent_id) = primary.fork_to_side().await?;
        // Snapshot the inherited user-prompt count BEFORE any aside round can
        // run: the aside's own first prompt is the one past this baseline.
        let inherited_user_prompts = primary
            .model_window()
            .await
            .iter()
            .filter(|message| message.role == nuo_wire::Role::User)
            .count();
        let store = Arc::new(primary.open_side(&side_id).await?);

        // Fresh side agent. The provider is shared through the same
        // `ProxyProvider` holder as the primary, which clones the inner
        // `Arc<dyn Provider>` per call and is safe under concurrency
        // (ADR-0017 §2). Tools come from the cached static snapshot (no
        // `SubagentTool` and no session-scoped dynamic connector sources), so a
        // side chat neither recurses nor implicitly acquires the master's
        // external connections. Dynamic capability propagation must be an
        // explicit policy decision (ADR-0060).
        let side_provider: Arc<dyn Provider> =
            Arc::new(ProxyProvider::new(provider_holder.clone()));
        let mut agent = Agent::builder(side_provider, base_tools.to_vec(), identity)
            .with_skills(skills)
            .build();
        agent.bind_workspace_security_handle(workspace_security);
        let agent = Arc::new(agent);
        agent.set_thread_id(&side_id);
        agent.set_project_root(project_root.map(std::path::Path::to_path_buf));
        // An aside is a quick aside; run it unattended — without human
        // intervention — so it never raises a permission modal whose reply
        // could not be routed back to the side `Agent` through the shared
        // permission channel. This mirrors the subagent policy (`subagent_tool.rs`
        // sets `unattended: true`).
        agent.set_unattended(true);

        Ok(Self {
            id: side_id,
            agent,
            store,
            lifecycle: Arc::new(RoundLifecycle::new()),
            has_round: false,
            inherited_user_prompts,
            touched_at: now_epoch_seconds(),
        })
    }
}

/// The live `/btw` aside registry (ADR-0103). ADR-0017 kept a single
/// `Option<SideSession>`; the background-aside redesign lifts it to a map so
/// several asides can be alive at once, with MRU ordering for the list modal
/// and an explicit "which aside is the composer targeting" pointer replacing
/// the old `AtomicBool` view flag.
#[derive(Default)]
pub struct SideRegistry {
    /// Live asides keyed by their session id.
    sides: HashMap<String, SideSession>,
    /// MRU order of the keys in `sides` (index 0 = most recent). Kept in lockstep:
    /// every insert pushes to the front, every remove drops its entry.
    order: Vec<String>,
    /// The aside the frontend is currently viewing/composing into, if any.
    /// `None` means the primary session.
    active_side: Option<String>,
}

impl SideRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly built aside and make it the active view. Returns
    /// its id.
    pub fn open(&mut self, side: SideSession) -> String {
        let id = side.id.clone();
        self.order.retain(|entry| entry != &id);
        self.order.insert(0, id.clone());
        self.sides.insert(id.clone(), side);
        self.active_side = Some(id.clone());
        id
    }

    /// Look up a live aside by id.
    pub fn get(&self, id: &str) -> Option<&SideSession> {
        self.sides.get(id)
    }

    /// Clone one live aside's `Arc`s out of the registry (see
    /// [`SideHandle`]) for out-of-lock operations.
    pub fn handle(&self, id: &str) -> Option<SideHandle> {
        self.sides.get(id).map(|side| side.handle())
    }

    /// Mutably look up a live aside by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut SideSession> {
        self.sides.get_mut(id)
    }

    /// Remove and return a live aside. Also clears `active_side` when it
    /// pointed at the removed aside (the caller decides whether to emit
    /// `SideViewClosed`).
    pub fn remove(&mut self, id: &str) -> Option<SideSession> {
        self.order.retain(|entry| entry != id);
        if self.active_side.as_deref() == Some(id) {
            self.active_side = None;
        }
        self.sides.remove(id)
    }

    /// The aside the composer is targeting, if any.
    pub fn active(&self) -> Option<&SideSession> {
        self.active_side
            .as_deref()
            .and_then(|id| self.sides.get(id))
    }

    /// Point the active view at one live aside. Returns `false` when the id
    /// is not registered (stale focus request).
    pub fn focus(&mut self, id: &str) -> bool {
        if !self.sides.contains_key(id) {
            return false;
        }
        self.order.retain(|entry| entry != id);
        self.order.insert(0, id.to_string());
        self.active_side = Some(id.to_string());
        self.touch(id);
        true
    }

    /// Detach from the active aside view back to the primary. The aside
    /// itself stays registered (ADR-0103 §1). Returns the detached aside's id
    /// when a view was active.
    pub fn detach(&mut self) -> Option<String> {
        self.active_side.take()
    }

    /// Refresh an aside's MRU timestamp (re-entry bumps it in the list).
    fn touch(&mut self, id: &str) {
        if let Some(side) = self.sides.get_mut(id) {
            side.touched_at = now_epoch_seconds();
        }
    }

    /// Whether any live aside has an in-flight round.
    pub fn any_running(&self) -> bool {
        self.sides.values().any(|side| side.lifecycle_has_round())
    }

    /// Build the asides list summary, newest (most recently touched) first
    /// (ADR-0103 §5). Titles are resolved by the caller
    /// ([`publish_btw_list`]) because the first-prompt read is async.
    pub fn summary(&self) -> Vec<BtwAsideSummary> {
        self.order
            .iter()
            .filter_map(|id| self.sides.get(id))
            .map(|side| BtwAsideSummary {
                id: side.id.clone(),
                title: String::new(),
                running: side.lifecycle_has_round(),
                updated_at: side.touched_at,
            })
            .collect()
    }

    /// Every live aside, in MRU order.
    pub fn iter(&self) -> impl Iterator<Item = &SideSession> {
        self.order.iter().filter_map(|id| self.sides.get(id))
    }
}

/// The per-aside handles an out-of-lock operation needs: the `Arc`s that
/// outlive the registry read guard. Built from a [`SideSession`] peek so
/// interrupt/close paths can act after the guard drops.
pub struct SideHandle {
    pub id: String,
    pub agent: Arc<Agent>,
    pub store: Arc<SessionStore>,
    pub lifecycle: Arc<RoundLifecycle>,
}

impl SideSession {
    /// Clone the per-aside `Arc`s out of the registry (see [`SideHandle`]).
    pub fn handle(&self) -> SideHandle {
        SideHandle {
            id: self.id.clone(),
            agent: self.agent.clone(),
            store: self.store.clone(),
            lifecycle: self.lifecycle.clone(),
        }
    }

    /// Whether this aside has produced any user content of its own. Only the
    /// first round flips [`Self::has_round`]; the inherited parent transcript
    /// does not count — it was copied at fork time and is not this aside's
    /// work.
    pub fn is_pristine(&self) -> bool {
        !self.has_round
    }

    /// Coarse "does this aside have a round in flight" signal, off the
    /// lifecycle token (the same source the primary-status watcher uses).
    /// Synchronous best-effort peek — see
    /// [`RoundLifecycle::is_running_blocking`].
    fn lifecycle_has_round(&self) -> bool {
        self.lifecycle.is_running_blocking()
    }
}

/// The aside's display title: its first *own* user-authored prompt
/// (truncated), skipping the prompts inherited from the parent at fork time
/// (ADR-0103 §6), or a placeholder for a not-yet-used aside. Async because
/// the store read is a locked clone.
pub async fn aside_title(side: &SideSession, max: usize) -> String {
    let (title, has_title) = side.store.title().await;
    if has_title && let Some(t) = title {
        let t = t.trim();
        if !t.is_empty() {
            if t.chars().count() > max {
                let cut: String = t.chars().take(max.saturating_sub(1)).collect();
                return format!("{cut}…");
            } else {
                return t.to_string();
            }
        }
    }
    let window = side.store.model_window().await;
    let prompt = window
        .iter()
        .filter(|message| message.role == nuo_wire::Role::User)
        .nth(side.inherited_user_prompts)
        .map(|message| message.content.clone());
    match prompt {
        Some(text) => {
            let flat: String = text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .into();
            if flat.chars().count() > max {
                let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
                format!("{cut}…")
            } else {
                flat
            }
        }
        None => "aside".to_string(),
    }
}

fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Coarse primary-session status, derived from the primary's [`RoundLifecycle`]
/// for the `/btw` parent-status watcher (ADR-0017 §5). No live uncancelled
/// round means the primary is idle; a running round means running.
pub async fn primary_status(primary_lifecycle: &Arc<RoundLifecycle>) -> ParentStatus {
    if primary_lifecycle.is_running().await {
        ParentStatus::Running
    } else {
        ParentStatus::Idle
    }
}

/// Watch the primary round while at least one `/btw` aside is live and stream
/// coarse [`ParentStatus`] updates to the TUI (ADR-0017 §5, ADR-0103: the
/// watcher now spans the whole registry, not one slot). Self-terminates once
/// the registry empties. Emits only on change so a long-running primary round
/// does not flood the channel.
pub fn spawn_parent_status_watcher(
    side: Arc<AsyncRwLock<SideRegistry>>,
    primary_lifecycle: Arc<RoundLifecycle>,
    tx: mpsc::UnboundedSender<AgentResponse>,
) {
    tokio::spawn(async move {
        let mut last: Option<ParentStatus> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if side.read().await.iter().next().is_none() {
                break;
            }
            let status = primary_status(&primary_lifecycle).await;
            if last != Some(status) {
                last = Some(status);
                let _ = tx.send(AgentResponse::ParentStatus(status));
            }
        }
    });
}

/// Push a fresh asides list to the frontend (ADR-0103 §5). Called on every
/// registry mutation and on explicit `QueryBtwList`. Titles are resolved
/// under a short-lived read lock; the stores' in-memory Mutexes make this
/// cheap.
pub async fn publish_btw_list(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    let mut rows = side.read().await.summary();
    for row in rows.iter_mut() {
        let title = match side.read().await.get(&row.id) {
            Some(side_session) => aside_title(side_session, 64).await,
            None => "aside".to_string(),
        };
        row.title = title;
    }
    let _ = tx.send(AgentResponse::BtwList(rows));
}

/// Start an interactive round against whichever session the user is currently
/// composing into — the primary, or the active `/btw` aside (ADR-0017,
/// ADR-0103). A stale active pointer (aside closed concurrently) falls back
/// to the primary so the prompt is never silently dropped.
/// Compaction/retry knobs are resolved once from the primary agent + config,
/// which is correct because the aside shares the same provider/model.
/// Bundled round-start environment: the side registry, the primary
/// session's agent/store/lifecycle triple, the response channel, and the
/// config. Every turn entry point threads the same six handles; bundling
/// keeps the signatures to (env, input).
/// Copyable by construction (all fields are shared references), so the wake
/// path can clone it into a round boundary without a hand-written helper.
#[derive(Clone, Copy)]
pub(crate) struct SideEnv<'a> {
    pub side: &'a Arc<AsyncRwLock<SideRegistry>>,
    pub agent: &'a Arc<Agent>,
    pub primary_session: &'a Arc<SessionStore>,
    pub primary_lifecycle: &'a Arc<RoundLifecycle>,
    pub tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub config: &'a Config,
}

pub(crate) async fn start_active_turn(env: SideEnv<'_>, input: RoundInput) {
    let SideEnv {
        side,
        agent,
        primary_session,
        primary_lifecycle,
        tx,
        config,
    } = env;
    // Resolve which live session this round belongs to, cloning the per-session
    // Arcs out of the registry under a short-lived read lock. The guard drops
    // at the end of this statement, before the round starts.
    let target = match side.read().await.active() {
        Some(active) => ResolvedTurnTarget {
            agent: active.agent.clone(),
            session: active.store.clone(),
            lifecycle: active.lifecycle.clone(),
            session_id: active.id.clone(),
        },
        None => ResolvedTurnTarget {
            agent: agent.clone(),
            session: primary_session.clone(),
            lifecycle: primary_lifecycle.clone(),
            session_id: primary_session.id().await,
        },
    };
    // Mark the aside as used before the round starts so a detach racing the
    // first stream event still sees a non-pristine aside.
    if let Some(active_id) = side.read().await.active().map(|s| s.id.clone())
        && let Some(s) = side.write().await.get_mut(&active_id)
    {
        s.has_round = true;
    }

    // Refuse up-front when no real provider is configured. The TUI bumps its
    // activity-bar state optimistically (is_responding, activity_status, and
    // running_sessions) before sending `AgentRequest::Chat`; failing here
    // without emitting terminal events would leave that state stuck on
    // "queued". Emit a session-scoped `RoundEvent::Error` (resets the global
    // is_responding/activity cells) followed by an idle `HarnessState`
    // (drives the `OutboxSignal` that removes the session from
    // running_sessions) so the chrome collapses cleanly. Symmetric with
    // `start_session_turn`'s refusal path.
    if refuse_if_no_provider(tx, &target.agent, &target.session, &target.session_id).await {
        return;
    }

    start_resolved_turn(agent, tx, config, target, input).await;
}

/// Resolve a live master or aside agent by its stable session id. Keeping
/// this lookup explicit prevents an outbox action from following a later view
/// switch into the wrong conversation.
pub async fn target_agent(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    agent: &Arc<Agent>,
    primary_session: &Arc<SessionStore>,
    target_session_id: &str,
) -> Option<Arc<Agent>> {
    if primary_session.id().await == target_session_id {
        return Some(agent.clone());
    }
    side.read()
        .await
        .get(target_session_id)
        .map(|session| session.agent.clone())
}

/// Resolve a live master or aside turn target by its stable session id
/// (ADR-0197 M4): the shared resolution behind `start_session_turn` and the
/// driver's follow-up queue authority (busy checks need the *target's* own
/// lifecycle, not the primary's).
pub(crate) async fn resolve_turn_target(
    side: &Arc<AsyncRwLock<SideRegistry>>,
    agent: &Arc<Agent>,
    primary_session: &Arc<SessionStore>,
    primary_lifecycle: &Arc<RoundLifecycle>,
    target_session_id: &str,
) -> Option<ResolvedTurnTarget> {
    let primary_id = primary_session.id().await;
    if primary_id == target_session_id {
        return Some(ResolvedTurnTarget {
            agent: agent.clone(),
            session: primary_session.clone(),
            lifecycle: primary_lifecycle.clone(),
            session_id: primary_id,
        });
    }
    side.read()
        .await
        .get(target_session_id)
        .map(|session| ResolvedTurnTarget {
            agent: session.agent.clone(),
            session: session.store.clone(),
            lifecycle: session.lifecycle.clone(),
            session_id: session.id.clone(),
        })
}

/// Start a fresh round in one exact live session. Returns `false` when the
/// target aside was closed after the message entered the frontend outbox.
pub(crate) async fn start_session_turn(
    target_session_id: &str,
    env: SideEnv<'_>,
    input: RoundInput,
) -> bool {
    let SideEnv {
        side,
        agent,
        primary_session,
        primary_lifecycle,
        tx,
        config,
    } = env;
    let resolved = resolve_turn_target(
        side,
        agent,
        primary_session,
        primary_lifecycle,
        target_session_id,
    )
    .await;
    let Some(target) = resolved else {
        return false;
    };
    let primary_id = primary_session.id().await;
    if target.session_id != primary_id
        && let Some(s) = side.write().await.get_mut(&target.session_id)
    {
        s.has_round = true;
    }

    // Same refusal contract as `start_active_turn`: a queued outbox item
    // cannot run without a real provider. Emitting the session-scoped error
    // + idle HarnessState here lets the frontend roll back its optimistically-
    // bumped state (running_sessions, the "queued" activity chip, the
    // spinner) and leave the outbox item in a recoverable state. Returning
    // `false` routes the caller through `RoundEvent::UserInputUnavailable`,
    // which promotes the dispatch item back to `Waiting` so the user can
    // recall or replay it once a real provider is configured.
    if refuse_if_no_provider(tx, &target.agent, &target.session, &target.session_id).await {
        return false;
    }

    start_resolved_turn(agent, tx, config, target, input).await;
    true
}

pub(crate) struct ResolvedTurnTarget {
    pub(crate) agent: Arc<Agent>,
    pub(crate) session: Arc<SessionStore>,
    pub(crate) lifecycle: Arc<RoundLifecycle>,
    pub(crate) session_id: String,
}

async fn start_resolved_turn(
    _agent: &Arc<Agent>,
    tx: &mpsc::UnboundedSender<AgentResponse>,
    config: &Config,
    target: ResolvedTurnTarget,
    mut input: RoundInput,
) {
    let ResolvedTurnTarget {
        agent,
        session,
        lifecycle,
        session_id,
    } = target;
    // `/retry` target validation (ADR-0128). The slash handler pre-checks
    // against the *primary* session, but the round may resolve onto the
    // active aside — so the authoritative check happens here, against the
    // session that will actually run it. The viewed session's parked point
    // is the only one that may be resumed; a mismatch (empty aside, or a
    // point left over from a different session) degrades the resume into a
    // refusal instead of silently minting a fresh round on the wrong target.
    if input.is_retry() {
        let pending = session.retry_pending().await;
        let round_counter = session.round_counter().await;
        let Some(pending) = pending.filter(|point| point.round == round_counter) else {
            let _ = tx.send(round_response(
                &session_id,
                RoundEvent::Error(
                    "Nothing to retry — the last round already completed.".to_string(),
                ),
            ));
            send_harness_state_for_session(tx, &session_id, &agent, &session, LoopStatus::Idle)
                .await;
            return;
        };
        // Re-bind the checkpoint to the resolved session's own point: the
        // handler read the primary's, which may differ from an aside target.
        input = RoundInput::resume(pending);
    }
    let projection = ContextProjectionSettings::from_context_policy(&config.context, active_context_window(&agent));
    let retry_max_attempts = config.connection_retry_max_attempts;
    let retry_base_ms = config.connection_retry_base_ms;
    let retry_max_ms = config.connection_retry_max_ms;

        start_interactive_round(
        InteractiveRoundContext {
            agent,
            tx: tx.clone(),
            lifecycle,
            session,
            session_id,
            projection,
            retry_max_attempts,
            retry_base_ms,
            retry_max_ms,
        },
        input,
    )
    .await;
}

/// No-provider gate shared by [`start_active_turn`], [`start_session_turn`],
/// and the plain-chat entry (`handlers_chat::chat`).
///
/// When the resolved agent is parked on the `NoProvider` sentinel, emit the
/// session-scoped events the TUI needs to roll back the optimistic
/// "queued" bump it performed before dispatching the chat/outbox item:
///
/// - [`RoundEvent::Error`] surfaces the user-facing "add a provider" notice
///   and resets the global `is_responding` / `activity_status` cells in the
///   TUI listener.
/// - [`RoundEvent::HarnessState`] with `loop_status: LoopStatus::Idle` drives
///   the `OutboxSignal::HarnessState { idle: true }` path, which removes the
///   session from `running_sessions` so the composer treats the next send as
///   immediate instead of busy-queueing it.
///
/// Returns `true` when the refusal fired (caller returns early without
/// starting a round); `false` when the round should proceed normally.
pub(super) async fn refuse_if_no_provider(
    tx: &mpsc::UnboundedSender<AgentResponse>,
    agent: &Agent,
    session: &SessionStore,
    session_id: &str,
) -> bool {
    if !NoProvider::is(agent.provider.as_ref()) {
        return false;
    }
    let _ = tx.send(round_response(
        session_id,
        RoundEvent::Error(
            "No provider configured. Add one with /connections before sending a message."
                .to_string(),
        ),
    ));
    send_harness_state_for_session(tx, session_id, agent, session, LoopStatus::Idle).await;
    true
}

use nuo_wire::RoundEvent;

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `SideSession` for registry-mechanics tests: no fork, no
    /// provider — the registry only moves these around.
    fn fixture(id: &str) -> SideSession {
        let agent = Arc::new(
            Agent::builder(Arc::new(NoProvider), Vec::new(), AgentIdentity::default()).build(),
        );
        let dir = std::env::temp_dir().join(format!("nuo-side-test-{id}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir fixture");
        let store = SessionStore::for_path(dir.join(format!("{id}.json")));
        SideSession {
            id: id.to_string(),
            agent,
            store: Arc::new(store),
            lifecycle: Arc::new(RoundLifecycle::new()),
            has_round: false,
            inherited_user_prompts: 0,
            touched_at: 0,
        }
    }

    #[tokio::test]
    async fn registry_open_focus_detach_and_remove_roundtrip() {
        let mut registry = SideRegistry::new();
        let a = registry.open(fixture("a"));
        assert_eq!(a, "a");
        assert!(registry.active().is_some_and(|s| s.id == "a"));

        // A second aside: multiple live at once (ADR-0103 §1).
        registry.open(fixture("b"));
        assert!(registry.active().is_some_and(|s| s.id == "b"));
        assert_eq!(registry.summary().len(), 2);

        // Detach is non-destructive: the aside stays registered.
        let detached = registry.detach();
        assert_eq!(detached.as_deref(), Some("b"));
        assert!(registry.active().is_none());
        assert_eq!(registry.summary().len(), 2);

        // Focus re-enters and moves the aside to the MRU front.
        assert!(registry.focus("a"));
        let order: Vec<String> = registry.summary().into_iter().map(|s| s.id).collect();
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
        assert!(registry.active().is_some_and(|s| s.id == "a"));

        // Focusing an unknown id fails (stale focus request).
        assert!(!registry.focus("zzz"));

        // Remove drops the entry and clears a matching active pointer.
        assert!(registry.remove("a").is_some());
        assert!(registry.active().is_none());
        assert_eq!(registry.summary().len(), 1);
    }

    #[tokio::test]
    async fn detach_does_not_remove_but_pristine_discard_does() {
        // The registry itself only reports `is_pristine`; the discard decision
        // lives in `handlers_session::detach_side_view`. Here we pin the
        // invariant the handler relies on: `is_pristine` flips exactly when a
        // round is started, never on open/focus/detach.
        let mut registry = SideRegistry::new();
        registry.open(fixture("a"));
        assert!(registry.get("a").is_some_and(SideSession::is_pristine));

        registry.get_mut("a").unwrap().has_round = true;
        assert!(!registry.get("a").is_some_and(SideSession::is_pristine));

        registry.detach();
        assert!(registry.get("a").is_some_and(|s| !s.is_pristine()));
    }

    #[tokio::test]
    async fn summary_orders_by_mru_and_flags_running() {
        let mut registry = SideRegistry::new();
        registry.open(fixture("old"));
        registry.open(fixture("new"));
        // Mark "old" as running to exercise the summary flag.
        registry.get_mut("old").unwrap().lifecycle.begin().await;
        let rows = registry.summary();
        assert_eq!(rows[0].id, "new");
        assert!(!rows[0].running);
        assert_eq!(rows[1].id, "old");
        assert!(rows[1].running);
    }
}
