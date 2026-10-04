//! Extension point for application-registered slash commands (ADR-0037 §6
//! follow-up).
//!
//! The built-in command vocabulary ([`crate::startup::BuiltinCmd`]) is a
//! closed set compiled into `muta-runtime`: adding a built-in means editing
//! the `define_builtin_commands!` macro *and* a `match` arm, so completion,
//! `/help`, and dispatch can never drift.
//!
//! That closed set is the right default for the shared harness commands
//! (`/models`, `/mcp`, `/pursue`, …) every agent needs. But an application
//! embedding the server (a future `muta-quant` binary) often wants its own
//! commands that run *Rust* logic, not a markdown prompt template (the only
//! other custom-command mechanism, via `.nuo/commands/*.md`). Forcing those
//! into `BuiltinCmd` would mean forking the server crate for each application
//! — exactly the coupling the server layer exists to avoid.
//!
//! [`SlashCommandHandler`] closes that gap: an embedding registers a handler
//! per command name, the harness holds them in an `extra_commands` map, and
//! `handlers_slash::dispatch` consults that map in its `None` (unknown
//! built-in) arm *before* falling back to the markdown-template path. The
//! handler receives the same dispatcher context the built-ins do, minus the
//! parts that are built-in-specific.
//!
//! This is the master-side analogue of how tools self-register via
//! `inventory`: capabilities (tools) and commands (slash handlers) are both
//! supplied by the embedding, never hardcoded in the neutral server.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use nuo_harness::{Agent, RoundLifecycle};
use nuo_wire::{AgentRequest, AgentResponse, Provider, Tool};
use nuo_persistence::{config::Config, connection_usage::ConnectionUsage, session::SessionStore};
use nuo_harness::skills::SkillRegistry;

use tokio::sync::{RwLock as AsyncRwLock, mpsc};

use crate::UiBridge;
use crate::side::SideRegistry;
use crate::startup::SessionStart;

/// The slice of the dispatcher context an extension slash command may touch.
///
/// This mirrors the parameter list of `handlers_slash::dispatch` so a
/// handler has the same reach as a built-in: it can start a turn, mutate the
/// session, emit responses, read config, etc. It is deliberately the full set
/// rather than a narrowed "safe" view — a registered handler is trusted
/// application code, just like a built-in arm.
///
/// Field names match the dispatcher locals so a handler body reads the same as
/// a built-in arm would.
#[allow(clippy::type_complexity)]
pub struct SlashContext<'a> {
    /// The raw command string the user typed, including the leading `/` and
    /// any arguments (e.g. `"/backtest AAPL 1d"`). Split it as needed.
    pub cmd: &'a str,
    /// Whitespace-split parts of [`Self::cmd`]; `parts[0]` is the command name
    /// (with the `/`). Mirrors the dispatcher's `parts`.
    pub parts: &'a [&'a str],
    pub config: &'a Config,
    pub agent: &'a Arc<Agent>,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub session: &'a Arc<SessionStore>,
    /// Primary round lifecycle: begin/supersede/cancel rounds, replacing the
    /// old token-slot + generation-counter pair.
    pub lifecycle: &'a Arc<RoundLifecycle>,
    pub side: &'a Arc<AsyncRwLock<SideRegistry>>,
    pub base_tools: &'a Arc<Vec<Arc<dyn Tool>>>,
    pub provider_holder: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub provider_usage: &'a mut ConnectionUsage,
    pub skills_registry: &'a Arc<SkillRegistry>,
    pub req_tx: &'a mpsc::Sender<AgentRequest>,
    pub project_root: Option<&'a Path>,
    pub startup: &'a SessionStart,
    pub ui: &'a dyn UiBridge,
}

/// A slash command implemented in Rust by the embedding application.
///
/// Register one per command name (without the leading `/`) via
/// [`SlashCommandRegistry::register`]. The harness dispatches an unknown
/// built-in to the matching handler before falling back to the markdown
/// template path.
///
/// The handler owns its argument parsing. Return `Ok(handled)` where
/// `handled = true` means the command was recognized and fully dispatched
/// (the caller stops here); `handled = false` means "not mine — keep going"
/// (the caller falls through to the markdown template / unknown-command
/// error). The latter lets a handler selectively ignore a name it registered
/// but does not apply to in this context.
#[async_trait::async_trait]
pub trait SlashCommandHandler: Send + Sync {
    /// Human-readable one-liner shown in `/help` and tab completion.
    fn description(&self) -> &str;

    /// Dispatch the command. See the trait docs for the return contract.
    async fn handle(&self, ctx: SlashContext<'_>) -> bool;
}

/// A registry of application-supplied slash command handlers, keyed by command
/// name (without the leading `/`). Owned by the harness; consulted by the
/// dispatcher's unknown-built-in arm.
///
/// Thread-safe (`RwLock`) so a handler could be registered at runtime, though
/// the common case is to populate it once at startup before the agent loop
/// starts.
#[derive(Default)]
pub struct SlashCommandRegistry {
    handlers: RwLock<HashMap<String, Arc<dyn SlashCommandHandler>>>,
}

impl SlashCommandRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a handler for `name` (without the leading `/`).
    /// Names colliding with built-ins are ignored at dispatch time — built-ins
    /// always win — but registering one is not an error, so an application can
    /// shadow a built-in in a fork without touching the macro.
    pub fn register(&self, name: impl Into<String>, handler: Arc<dyn SlashCommandHandler>) {
        let _ = self
            .handlers
            .write()
            .map(|mut g| g.insert(name.into(), handler));
    }

    /// Look up the handler for `name` (without the leading `/`).
    pub fn get(&self, name: &str) -> Option<Arc<dyn SlashCommandHandler>> {
        self.handlers.read().ok()?.get(name).cloned()
    }

    /// Snapshot every `(name, description)` pair, sorted by name, for `/help`
    /// and completion.
    pub fn list(&self) -> Vec<(String, String)> {
        let Ok(g) = self.handlers.read() else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = g
            .iter()
            .map(|(name, h)| (name.clone(), h.description().to_string()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Whether the registry is empty (skip the `/help` section if so).
    pub fn is_empty(&self) -> bool {
        self.handlers.read().map(|g| g.is_empty()).unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoHandler {
        desc: &'static str,
    }

    #[async_trait::async_trait]
    impl SlashCommandHandler for EchoHandler {
        fn description(&self) -> &str {
            self.desc
        }
        async fn handle(&self, ctx: SlashContext<'_>) -> bool {
            let _ = ctx
                .resp_tx
                .send(AgentResponse::Error(format!("echo: {}", ctx.cmd)));
            true
        }
    }

    #[test]
    fn register_get_and_list() {
        let reg = SlashCommandRegistry::new();
        assert!(reg.is_empty());
        reg.register(
            "backtest",
            Arc::new(EchoHandler {
                desc: "Run a backtest",
            }),
        );
        reg.register("alpha", Arc::new(EchoHandler { desc: "Sort demo" }));
        assert!(!reg.is_empty());
        assert!(reg.get("backtest").is_some());
        assert!(reg.get("missing").is_none());
        // list() is sorted by name, so alpha precedes backtest.
        let listed = reg.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].0, "alpha");
        assert_eq!(listed[1].0, "backtest");
        assert_eq!(listed[1].1, "Run a backtest");
    }

    #[test]
    fn register_replaces_existing() {
        let reg = SlashCommandRegistry::new();
        reg.register("x", Arc::new(EchoHandler { desc: "first" }));
        reg.register("x", Arc::new(EchoHandler { desc: "second" }));
        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1, "second");
    }
}
