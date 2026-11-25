//! The `AgentRequest::SlashCommand` router.
//!
//! Parsing and routing only. Each built-in delegates to a focused async fn in
//! [`super::commands`] so no single frame is large enough to overflow a Tokio
//! worker stack (a monolithic dispatcher reached ~1.6 MiB in debug builds).
//!
//! # The completeness net (ADR-0140 `[INV-CMD-07]`)
//!
//! The reply funnel ([`super::record`]) decides whether a *produced* reply is
//! durable; it cannot force a handler to produce one. A future handler that
//! mutates and records nothing would leave the ledger silently blind — the audit
//! analogue of an orphan contract. So every dispatched **act** is bracketed by a
//! ledger observation: if the handler appended no record, one invocation-only row is
//! written. A spurious row is merely noisy; a missing one is unrecoverable.
//!
//! A topology change (`CommandEffect::SessionControl`) records into the ledger it
//! *creates* — `/new` mints a session and then writes this command into that session's
//! ledger. The mark is therefore taken again on the new identity before comparing, which
//! is what retired the old "a session reset counts as recorded" exemption: the class now
//! says so, instead of the net special-casing one handler's shape.
//!
//! The four surface-owned commands are outside that bracket by construction: they
//! call no handler, so no act happened here and nothing may be recorded for them.

use super::SlashEnv;
use super::commands;
use super::record;
use crate::composition::startup::{BuiltinCmd, effect_for};

/// What the router reached. Distinguishes "a handler ran here" (bracket it) from
/// "the client owns this surface" (answer, record nothing).
enum Routed {
    /// A built-in handler (or the extension/unknown path) executed in-process.
    Handler,
    /// The client performs the act; the server only answers.
    Surface,
}

/// `AgentRequest::SlashCommand` — parse the command, dispatch to the matching
/// built-in handler, or fall through to the user-defined project-command path.
pub async fn dispatch(cmd: String, env: SlashEnv<'_>) {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }
    let start_instant = std::time::Instant::now();
    let name = parts[0].trim_start_matches('/');
    let args = cmd.strip_prefix(parts[0]).unwrap_or("").trim();
    let effect = effect_for(&parts);
    // Captured before dispatch consumes `env`: the net must run even though every
    // arm moves the environment into its handler.
    let session = std::sync::Arc::clone(env.session);
    // The ledger mark is taken before dispatch and compared after: a handler that
    // recorded anything (including into a freshly minted ledger) moves it.
    // Bracket every command that makes a durable claim. `SessionControl` is included: it
    // changes topology and owes the audit a row — in the ledger it recorded into, which
    // for `/new` is the *new* session's. What it must not do is produce a row in a
    // thread's transcript, which is a placement question the client answers from
    // `occupies_a_row` and not from this bracket.
    let mark = if effect.is_durable() {
        Some(LedgerMark::of(&session, name, args).await)
    } else {
        None
    };

    let routed = match BuiltinCmd::from_slash(parts[0]) {
        Some(BuiltinCmd::Models) => {
            // The model picker is a client surface: the client owns the switch,
            // records the provider ack through its own path, and reports back.
            surface(env, name, args, "The model picker is a client surface — open it in the app that owns the modal (the TUI: `/models`).").await
        }
        Some(BuiltinCmd::Connections) => {
            surface(env, name, args, "Connection management is a client surface — open it in the app that owns the modal (the TUI: `/connections`).").await
        }
        Some(BuiltinCmd::Tools) => {
            // `/tools` opens the tools manager modal locally (intercepted in
            // input.rs as `InputAction::OpenTools`) and is never forwarded here
            // as a SlashCommand by a client that can host it.
            surface(env, name, args, "Tool management is a client surface — open it in the app that owns the modal (the TUI: `/tools`).").await
        }
        Some(BuiltinCmd::Mcp) => {
            // `/mcp` opens the MCP manager modal locally (intercepted as
            // `InputAction::OpenMcp`); its pane reads the live session-context
            // snapshot the harness keeps current via the shared `McpRuntime`.
            surface(env, name, args, "MCP management is a client surface — open it in the app that owns the modal (the TUI: `/mcp`).").await
        }
        Some(BuiltinCmd::Permissions) => {
            commands::permissions(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::AutoApprove) => {
            commands::auto_approve(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Confinement) => {
            commands::confinement(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Role) => {
            commands::role(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Search) => {
            commands::search(env, &cmd, name, args, &parts, start_instant).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Threads) => {
            commands::sessions(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Fork) => {
            commands::fork(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Tree) => {
            commands::tree(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Diff) => {
            commands::diff(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Undo) => {
            commands::undo(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Stats) => {
            commands::stats(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Context) => {
            commands::context(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Trace) => {
            commands::trace(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Usage) => {
            commands::usage(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Quotas) => {
            commands::quota(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Compact) => {
            commands::compact(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Language) => {
            commands::language(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Jobs) => {
            commands::jobs(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Init) => {
            commands::init(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Trust) | Some(BuiltinCmd::Untrust) => {
            commands::trust(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Skills) => {
            commands::skills(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::New) => {
            commands::new_session(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Export) => {
            commands::export(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Retry) => {
            commands::retry(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Help) => {
            commands::help(env, name, args, &parts).await;
            Routed::Handler
        }
        Some(BuiltinCmd::Exit) => {
            commands::exit(env, name, args, &parts).await;
            Routed::Handler
        }
        None => {
            commands::user_command(env, &cmd, name, args, &parts).await;
            Routed::Handler
        }
    };

    if let (Routed::Handler, Some(mark)) = (routed, mark) {
        mark.assert_recorded(&session).await;
    }
}

/// Answer a command whose surface the client owns, without recording anything.
async fn surface(env: SlashEnv<'_>, name: &str, args: &str, guidance: &str) -> Routed {
    record::record_surface_only(env.session, env.resp_tx, name, args, guidance).await;
    Routed::Surface
}

/// One observation of the session's command ledger.
struct LedgerMark<'a> {
    /// The thread the ledger belongs to: a reset or fork mints a new one, and the
    /// handler recorded into *that* ledger, which is a complete answer.
    thread_id: String,
    len: usize,
    /// The invocation the net backfills if the handler recorded nothing.
    name: &'a str,
    args: &'a str,
}

impl<'a> LedgerMark<'a> {
    async fn of(
        session: &persistence::thread::ThreadSession,
        name: &'a str,
        args: &'a str,
    ) -> Self {
        Self {
            thread_id: session.id().await,
            len: session.commands().await.len(),
            name,
            args,
        }
    }

    /// Write an invocation-only row when a dispatched act left no trace at all.
    ///
    /// The identity check that used to sit here ("a session reset counts as recorded") is
    /// gone. It existed only because `CommandEffect` could not say what `/new` is: a reset
    /// minted a fresh ledger, so a length comparison against the *old* ledger could never
    /// see the row the handler had just written into the new one. With
    /// [`CommandEffect::SessionControl`] the class states that a topology change records
    /// into the ledger it creates, so the bracket no longer has to guess — and a control
    /// handler that records nothing is now caught, where before it was silently exempt.
    async fn assert_recorded(self, session: &std::sync::Arc<persistence::thread::ThreadSession>) {
        // A reset or fork repoints the store, so the ledger to compare is a *different*
        // one and a length difference against the old mark is meaningless (`/new` on a
        // thread with three records would look like it lost two).
        if session.id().await != self.thread_id {
            if session.commands().await.is_empty() {
                // A topology change that left its new ledger empty is a defect, but
                // fabricating a row would be worse than reporting it: the row would be an
                // invented history for a thread that has none, and the next reader could not
                // tell it from a real act. So the net discloses and stops; the class
                // (`SessionControl`) is what makes the omission visible here at all, where
                // the old exemption hid it.
                tracing::warn!(
                    name = self.name,
                    "a dispatched topology change recorded nothing into the ledger it created \
                     (ADR-0252 §2); not fabricating a row in a fresh thread"
                );
            }
            return;
        }
        if session.commands().await.len() != self.len {
            return;
        }
        // No reply to attach: the handler produced none, which is exactly the
        // case this net exists to cover. The row is the invocation alone.
        tracing::warn!(
            name = self.name,
            "a dispatched slash command recorded nothing; writing an invocation-only row (ADR-0140)"
        );
        let record = protocol::CommandRecord::new(self.name, self.args);
        if let Err(error) = session.mutate_commands(|ledger| ledger.push(record)).await {
            tracing::warn!(?error, "could not write the completeness-net record");
        }
    }
}

#[cfg(test)]
mod completeness_net_tests {
    //! `[INV-CMD-07]`: an act that ran leaves a trace, whatever its handler did.
    //!
    //! The funnel cannot force a handler to produce a reply, so the net is tested
    //! where it lives: against the ledger mark it takes and the condition it
    //! applies. Every case below is a state a real dispatch can reach.

    use std::sync::Arc;

    use persistence::thread::ThreadSession;

    use super::LedgerMark;

    fn store(dir: &tempfile::TempDir) -> Arc<ThreadSession> {
        Arc::new(ThreadSession::for_path(dir.path().join("session.json")))
    }

    /// The case the net exists for: a mutation handler that recorded nothing.
    #[tokio::test]
    async fn an_unrecorded_act_gains_an_invocation_only_row() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);

        // Take the mark as dispatch does, then do nothing at all.
        let mark = LedgerMark {
            thread_id: session.id().await,
            len: session.commands().await.len(),
            name: "trust",
            args: "deny",
        };
        mark.assert_recorded(&session).await;

        let records = session.commands().await;
        assert_eq!(records.len(), 1, "the act must not vanish from the audit");
        assert_eq!(records[0].name, "trust");
        assert_eq!(records[0].args, "deny");
        assert!(
            records[0].result.is_none(),
            "the net cannot invent a reply — the row is the invocation alone"
        );
    }

    /// A handler that recorded already moves the mark, so the net stays silent.
    #[tokio::test]
    async fn a_recorded_act_is_not_double_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);
        let mark = LedgerMark {
            thread_id: session.id().await,
            len: session.commands().await.len(),
            name: "unattended",
            args: "on",
        };
        session
            .mutate_commands(|ledger| ledger.push(protocol::CommandRecord::new("unattended", "on")))
            .await
            .unwrap();

        mark.assert_recorded(&session).await;
        assert_eq!(
            session.commands().await.len(),
            1,
            "the handler's own record is the complete answer"
        );
    }

    /// A reset or fork mints a ledger; the handler recorded into *that* one, which
    /// is complete. Comparing across identities would fabricate a row in the new
    /// thread for an act the old thread took.
    #[tokio::test]
    async fn a_session_reset_counts_as_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let session = store(&dir);
        let mark = LedgerMark {
            thread_id: session.id().await,
            len: session.commands().await.len(),
            name: "new",
            args: "",
        };
        session.reset().await.unwrap();

        mark.assert_recorded(&session).await;
        assert!(
            session.commands().await.is_empty(),
            "a fresh thread must not inherit a fabricated invocation row"
        );
    }
}
