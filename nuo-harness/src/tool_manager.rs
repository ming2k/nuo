//! Unified, two-bucket view of every tool the agent can dispatch.
//!
//! Tools are classified at runtime into two sources — `builtin` (collected
//! from the registry + agent-owned instances like todo) and `mcp` (dynamic
//! tools published through [`nuo_contracts::DynamicToolSink`], today only MCP
//! servers). There is deliberately no third, config-driven source; see
//! ADR-0221 and ADR-0222.
//!
//! ### Why this exists
//!
//! Before this module, the harness computed the live tool set in three
//! separate places that had to be kept consistent by hand:
//!
//! - [`Agent::installed_tools`](crate::Agent) — schema source (resolved ∪ dynamic);
//! - [`Agent::visible_tools`](crate::Agent) — per-turn schema (installed − disabled − delegated-mode `ask_user`);
//! - the lookup inside [`Agent::execute_tool`](crate::Agent) — dispatch (resolved, then dynamic fallback).
//!
//! Each recomputed "static ∪ dynamic − mask" with its own code, so the three
//! could drift (a tool in the schema but un-dispatchable, or vice versa). This
//! struct is the **single authority**: all three choke points ask the same
//! [`ToolManager`] for the schema list and for tool lookup, and the
//! classification (`builtin`/`mcp`) is derived once, here.
//!
//! ### What it is *not*
//!
//! Not a replacement for [`nuo_contracts::ToolSet`] (the capability-pool resolver) or
//! [`DynamicToolRegistry`] (the sink). Those remain the storage; this is a
//! read-side view over them. The storage layers keep their existing
//! invariants (static > dynamic on name clash; dynamic source-keyed groups;
//! disable is name-level and uniform across sources).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use nuo_contracts::Tool;

use crate::dynamic_tools::DynamicToolRegistry;

/// Which bucket a tool lives in. The classification is *runtime*, not a crate
/// boundary: an MCP tool's transport lives in `muta-mcp`, but once published
/// through the sink it is an `mcp` tool *here* in the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    /// Collected from the registry (`collect_toolset`) plus agent-owned
    /// instances (todo, subagent). Resolved per active model/variant.
    Builtin,
    /// Published through [`nuo_contracts::DynamicToolSink`] — today only MCP servers. Named
    /// `mcp__<server>__<tool>` by convention (enforced at the publisher, not
    /// here).
    Mcp,
}

/// One tool paired with its source bucket.
#[derive(Clone)]
pub struct SourcedTool {
    pub source: ToolSource,
    pub tool: Arc<dyn Tool>,
}

/// The unified two-bucket tool view.
///
/// Holds references to the existing storage (`resolved_tools`, `dynamic_tools`,
/// the disable masks). Constructed once per agent and borrowed for the lifetime
/// of dispatch.
pub(crate) struct ToolManager {
    /// The per-model resolved static tools (read from the agent's
    /// `resolved_tools`). Wrapped here so all reads funnel through one place.
    resolved: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
    /// Dynamic tools (MCP today). Wrapped so `find` / `loop_tools` share one
    /// snapshot per call.
    dynamic: Arc<DynamicToolRegistry>,
    /// Persisted user disable mask (session-level). Name-level, uniform
    /// across all sources.
    disabled: Arc<Mutex<HashSet<String>>>,
    /// In-memory hook-scoped disable mask (per RestorePoint).
    scoped_disabled: Arc<Mutex<crate::agent::ScopedToolDisable>>,
    /// Role-level admitted MCP patterns (ADR-0242).
    admit_mcp: Arc<RwLock<Vec<String>>>,
}

impl ToolManager {
    /// Wire the manager to the agent's existing storage. Does not own or move
    /// anything — it borrows shared handles so the agent keeps direct access
    /// for its existing mutators (resolve, disable toggle, etc.).
    pub(crate) fn new(
        resolved: Arc<RwLock<Vec<Arc<dyn Tool>>>>,
        dynamic: Arc<DynamicToolRegistry>,
        disabled: Arc<Mutex<HashSet<String>>>,
        scoped_disabled: Arc<Mutex<crate::agent::ScopedToolDisable>>,
        admit_mcp: Arc<RwLock<Vec<String>>>,
    ) -> Self {
        Self {
            resolved,
            dynamic,
            disabled,
            scoped_disabled,
            admit_mcp,
        }
    }

    pub(crate) fn set_admit_mcp(&self, patterns: Vec<String>) {
        *self.admit_mcp.write().unwrap_or_else(|e| e.into_inner()) = patterns;
    }

    /// Every installed tool, classified — the schema and dispatch authority.
    ///
    /// Order: `builtin` first (in resolved order), then `mcp` (dynamic
    /// snapshot, source-sorted internally). Name clashes are resolved
    /// **builtin > mcp**: a static tool named `x` shadows an mcp tool named
    /// `x`. This preserves the harness's existing "static > dynamic" invariant.
    pub(crate) fn installed(&self) -> Vec<SourcedTool> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<SourcedTool> = Vec::new();

        // 1. builtin (resolved static).
        for tool in self
            .resolved
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
        {
            if seen.insert(tool.name().to_string()) {
                out.push(SourcedTool {
                    source: ToolSource::Builtin,
                    tool,
                });
            }
        }

        // 2. mcp (dynamic snapshot, filtered by role admission patterns, ADR-0242).
        let admit_patterns = self
            .admit_mcp
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for entry in self.dynamic.snapshot() {
            let name = entry.tool.name();
            let server = name
                .strip_prefix("mcp__")
                .and_then(|s| s.split("__").next())
                .unwrap_or("");
            if is_mcp_admitted(&admit_patterns, server) && seen.insert(name.to_string()) {
                out.push(SourcedTool {
                    source: ToolSource::Mcp,
                    tool: entry.tool,
                });
            }
        }

        out
    }

    /// The live tool set the model may see this turn: `installed` minus the
    /// disabled mask (user + hook-scoped).
    ///
    /// Preserves a stable, deterministic tool schema across delegated-posture transitions
    /// (ADR-0137) to prevent KV-cache invalidation. Delegated-mode restrictions on `ask_user`
    /// are enforced at execution time via PermissionPolicy Gate 6 (Runtime Gating).
    pub(crate) fn loop_tools(&self, _delegated: bool) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = self
            .installed()
            .into_iter()
            .filter(|s| s.tool.is_available())
            .filter(|s| !self.is_name_disabled(s.tool.name()))
            .map(|s| s.tool)
            .collect();
        tools.sort_by(|a, b| a.name().cmp(b.name()));
        tools
    }

    /// Look up a tool by name for dispatch. Returns the tool and its source.
    /// Mirrors the historical "resolved first, dynamic fallback" lookup
    /// (builtin > mcp).
    pub(crate) fn find(&self, name: &str) -> Option<SourcedTool> {
        self.installed()
            .into_iter()
            .filter(|s| s.tool.is_available())
            .find(|s| s.tool.matches_name(name))
    }

    /// Is `name` disabled by *either* mask? Name-level and uniform across all
    /// sources — disabling `mcp__foo__bar` works the same as disabling `bash`.
    fn is_name_disabled(&self, name: &str) -> bool {
        let user_disabled = self
            .disabled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name);
        if user_disabled {
            return true;
        }
        self.scoped_disabled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(name)
    }
}

fn is_mcp_admitted(patterns: &[String], server: &str) -> bool {
    if patterns.is_empty() {
        return false;
    }
    patterns.iter().any(|pat| {
        if pat == "*" {
            true
        } else if let Some(prefix) = pat.strip_suffix('*') {
            server.starts_with(prefix)
        } else {
            pat == server
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use async_trait::async_trait;
    use nuo_contracts::DynamicToolSink;
    use nuo_contracts::ScopeTarget;
    use nuo_contracts::ToolAccesses;

    /// Minimal tool stub for classification tests.
    struct StubTool {
        name: String,
        available: bool,
    }
    impl StubTool {
        fn new(name: &str) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                available: true,
            })
        }
        fn unavailable(name: &str) -> Arc<Self> {
            Arc::new(Self {
                name: name.to_string(),
                available: false,
            })
        }
    }
    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _args: &str) -> Result<String, String> {
            Ok("ok".to_string())
        }
        fn accesses(&self, _args: &str) -> ToolAccesses {
            ToolAccesses::none()
        }
        fn scope_target(&self, _args: &str) -> ScopeTarget {
            ScopeTarget::Unspecified
        }
    }

    fn manager(
        resolved: Vec<Arc<dyn Tool>>,
        dynamic_tools: Vec<Arc<dyn Tool>>,
        disabled: Vec<&str>,
    ) -> ToolManager {
        let resolved = Arc::new(RwLock::new(resolved));
        let dynamic = Arc::new(DynamicToolRegistry::default());
        // Publish dynamic tools under a synthetic MCP source.
        dynamic.replace("mcp:test", dynamic_tools);
        let disabled = Arc::new(Mutex::new(disabled.iter().map(|s| s.to_string()).collect()));
        let scoped = Arc::new(Mutex::new(crate::agent::ScopedToolDisable::default()));
        let admit_mcp = Arc::new(RwLock::new(vec!["*".to_string()]));
        ToolManager::new(resolved, dynamic, disabled, scoped, admit_mcp)
    }

    #[test]
    fn classifies_two_buckets_in_order() {
        let m = manager(
            vec![StubTool::new("execute_command"), StubTool::new("read_text")],
            vec![StubTool::new("mcp__srv__x")],
            vec![],
        );
        let installed = m.installed();
        let labels: Vec<(ToolSource, &str)> = installed
            .iter()
            .map(|s| (s.source, s.tool.name()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (ToolSource::Builtin, "execute_command"),
                (ToolSource::Builtin, "read_text"),
                (ToolSource::Mcp, "mcp__srv__x"),
            ]
        );
    }

    #[test]
    fn static_shadows_mcp_on_name_clash() {
        // Same name in both buckets — builtin wins.
        let m = manager(
            vec![StubTool::new("dup")],
            vec![StubTool::new("dup")],
            vec![],
        );
        let installed = m.installed();
        assert_eq!(installed.len(), 1, "name clash collapses to one");
        assert_eq!(installed[0].source, ToolSource::Builtin);
    }

    #[test]
    fn loop_tools_filters_disabled_and_preserves_stable_schema() {
        let m = manager(
            vec![StubTool::new("execute_command"), StubTool::new("ask_user")],
            vec![],
            vec!["execute_command"],
        );
        let live = m.loop_tools(false);
        let names: Vec<&str> = live.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["ask_user"], "bash disabled; ask_user kept");

        // Delegated mode keeps ask_user in schema (Runtime Gating via PermissionPolicy Gate 6) for KV-cache stability.
        let live_delegated = m.loop_tools(true);
        let names_ap: Vec<&str> = live_delegated.iter().map(|t| t.name()).collect();
        assert_eq!(
            names_ap,
            vec!["ask_user"],
            "delegated mode preserves schema for prompt cache"
        );
    }

    #[test]
    fn find_returns_source() {
        let m = manager(
            vec![StubTool::new("execute_command")],
            vec![StubTool::new("mcp__s__t")],
            vec![],
        );
        assert_eq!(
            m.find("execute_command").unwrap().source,
            ToolSource::Builtin
        );
        assert_eq!(m.find("mcp__s__t").unwrap().source, ToolSource::Mcp);
        assert!(m.find("nope").is_none());
    }

    #[test]
    fn loop_tools_and_find_filter_unavailable_tools() {
        let m = manager(
            vec![
                StubTool::new("active_builtin"),
                StubTool::unavailable("inactive_builtin"),
            ],
            vec![
                StubTool::new("mcp__srv__active"),
                StubTool::unavailable("mcp__srv__inactive"),
            ],
            vec![],
        );
        let live = m.loop_tools(false);
        let names: Vec<&str> = live.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["active_builtin", "mcp__srv__active"]);

        assert!(m.find("active_builtin").is_some());
        assert!(m.find("inactive_builtin").is_none());
        assert!(m.find("mcp__srv__active").is_some());
        assert!(m.find("mcp__srv__inactive").is_none());
    }

    #[test]
    fn role_filters_mcp_tools_by_admission_patterns() {
        let m = manager(
            vec![],
            vec![
                StubTool::new("mcp__obsidian__search"),
                StubTool::new("mcp__philpapers__query"),
                StubTool::new("mcp__postgres__select"),
            ],
            vec![],
        );
        assert_eq!(m.installed().len(), 3);

        // Filter to philosophist pattern: only obsidian and philpapers
        m.set_admit_mcp(vec!["obsidian".to_string(), "phil*".to_string()]);
        let names: Vec<String> = m
            .installed()
            .iter()
            .map(|s| s.tool.name().to_string())
            .collect();
        assert_eq!(
            names,
            vec!["mcp__obsidian__search", "mcp__philpapers__query"]
        );
    }
}
