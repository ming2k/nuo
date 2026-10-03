//! Tool administration on [`Agent`]: cached permission rules,
//! enable/disable, declared tool snapshots, and the skills catalog view.

use super::*;

impl Agent {
    /// Structured view of the cached "always allow" rules, for the session
    /// modal's Permissions pane. Unlike [`Agent::allowed_tools`] (which collapses
    /// each rule to a single formatted string), this keeps the tool/scope pair
    /// intact so the modal can target an individual rule for revocation.
    pub fn allowed_tools_structured(&self) -> Vec<nuo_wire::PermissionRuleInfo> {
        self.permissions.allowed_tools_structured()
    }

    /// Designate the project whose bucket backs the persistent "always"
    /// allowlist, and load any rules already on disk into the in-memory set.
    /// Pass `None` to disable persistence (subagents and most tests do this).
    ///
    /// Loading is best-effort: a missing, unreadable, or unsupported file is
    /// silently ignored — the agent simply starts with an empty allowlist and
    /// re-prompts the user. This is the cross-session hook: a fresh session in
    /// the same project inherits prior `Always` approvals without re-asking.
    pub fn set_project_root(&self, root: Option<std::path::PathBuf>) {
        self.permissions.set_project_root(root);
    }

    /// Test seam for binding project persistence to an explicit directory
    /// capability without mutating process-wide path state.
    #[cfg(test)]
    pub(crate) fn set_project_root_with_dirs(
        &self,
        root: Option<std::path::PathBuf>,
        dirs: &nuo_persistence::paths::Dirs,
    ) {
        self.permissions.set_project_root_with_dirs(root, dirs);
    }

    /// Seed declarative permission rules from `[permissions]` config. Delegates
    /// to `PermissionStore::seed_from_config`.
    pub fn seed_permissions_from_config(
        &self,
        rules: &[nuo_persistence::config::PermissionRuleConfig],
    ) {
        self.permissions.seed_from_config(rules);
    }

    /// Check an exact runtime permission scope without prompting. Used by
    /// lifecycle hooks, which execute inside agent control flow and therefore
    /// must fail closed rather than recursively opening a permission prompt.
    pub fn is_permission_allowed(&self, tool: &str, scope: &str) -> bool {
        self.permissions
            .is_allowed(&crate::permission_store::PermissionRule {
                tool: tool.to_string(),
                scope: scope.to_string(),
            })
    }

    /// Replace the complete tool snapshot published by one dynamic source.
    pub fn replace_dynamic_tools(&self, source: &str, tools: Vec<Arc<dyn Tool>>) {
        self.dynamic_tools.replace(source, tools);
    }

    /// Remove one dynamic source and every tool it published.
    pub fn remove_dynamic_tools(&self, source: &str) {
        self.dynamic_tools.remove(source);
    }

    /// The connector-facing publication port. It deliberately exposes no
    /// agent-owned lock or protocol-specific state.
    pub fn dynamic_tool_sink(&self) -> Arc<dyn nuo_wire::DynamicToolSink> {
        self.dynamic_tools.clone()
    }

    /// Update the admitted MCP server patterns for the active role (ADR-0242).
    pub fn set_admit_mcp(&self, patterns: Vec<String>) {
        self.tool_manager.set_admit_mcp(patterns);
    }

    /// Set the session-level enabled flag for a tool. No-op when the name is
    /// unknown (so a stale toggle from the modal cannot poison the dispatch
    /// table). Returns whether the flag actually changed.
    pub fn set_tool_enabled(&self, name: &str, enabled: bool) -> bool {
        let known = self.toolset.variants_of(name).is_some() || self.dynamic_tools.contains(name);
        if !known {
            return false;
        }
        let mut guard = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let currently_enabled = !guard.contains(name);
        if enabled == currently_enabled {
            return false;
        }
        if enabled {
            guard.remove(name);
        } else {
            guard.insert(name.to_string());
        }
        true
    }

    /// Whether `name` is currently enabled (i.e. visible to the model and
    /// dispatchable). Unknown tools report `false`.
    pub fn is_tool_enabled(&self, name: &str) -> bool {
        if self.toolset.variants_of(name).is_none() && !self.dynamic_tools.contains(name) {
            return false;
        }
        let guard = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        !guard.contains(name)
    }

    /// Apply hook-fired [`HookOutcome::ScopeTools`] disables: record each name
    /// (only known tools, matching the user-mask contract) under its restore
    /// point. Idempotent across repeated fires via refcounting.
    pub(super) fn apply_scoped_disables(
        &self,
        disables: &[(String, nuo_wire::RestorePoint)],
    ) {
        if disables.is_empty() {
            return;
        }
        let mut scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (name, restore) in disables {
            // Only known tools: a stale/typo'd name from a hook cannot poison
            // the mask (mirrors `set_tool_enabled`'s known-tool guard).
            if self.toolset.variants_of(name).is_some() {
                scoped.disable(name, *restore);
            }
        }
    }

    /// Restore every `TurnEnd` disable at the ReAct-turn boundary.
    pub(crate) fn restore_scoped_turn_end(&self) {
        let mut scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scoped.restore_turn_end();
    }

    /// Restore every scoped disable (both buckets) at user-round end.
    pub(crate) fn restore_scoped_round_end(&self) {
        let mut scoped = self
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scoped.restore_round_end();
    }

    /// Restore the disabled-tool mask from a persisted set on resume
    /// (ADR-0048 Phase 2). Replaces the in-memory mask wholesale so a user
    /// toggle survives restart. Only known tool names are retained so a stale
    /// toggle (e.g. a tool removed from config) cannot poison the dispatch
    /// table.
    pub fn restore_disabled_tools(&self, tools: std::collections::HashSet<String>) {
        let mut guard = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.clear();
        for name in tools {
            if self.toolset.variants_of(&name).is_some() {
                guard.insert(name);
            }
        }
    }

    /// Snapshot the disabled-tool mask for persistence (ADR-0048 Phase 2).
    pub fn disabled_tools_snapshot(&self) -> std::collections::HashSet<String> {
        self.disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// All installed tools that the model may see this turn: every tool whose
    /// name is not disabled by *either* mask (the persisted user mask or the
    /// in-memory hook-scoped mask). Used at the schema-build choke points so a
    /// disabled tool's definition never reaches the provider.
    pub(crate) fn visible_tools(&self) -> Vec<Arc<dyn Tool>> {
        // Delegate to the ToolManager's schema authority.
        // Under ADR-0137, tool schemas remain deterministic and invariant to maximize
        // KV-cache reuse. Unattended-mode restrictions are enforced at runtime via PermissionPolicy.
        self.tool_manager.loop_tools(self.unattended())
    }

    /// Structured view of every installed tool, for the session modal's Tools
    /// pane. `enabled` reflects the disabled mask; `source` classifies origin
    /// (`builtin`, `subagent`, or the publisher-provided dynamic source id).
    pub fn snapshot_tools(&self) -> Vec<nuo_wire::ToolInfo> {
        // Classification delegates to the ToolManager's two-bucket authority
        // for the builtin bucket (with subagent broken out for display); the
        // mcp bucket keeps the publisher-provided dynamic source id as its
        // label. The source label is display-only; dispatch treats both
        // buckets uniformly via name-clash priority (builtin > mcp).
        let disabled = self
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let mut seen: HashSet<String> = HashSet::new();
        let mut sourced_tools: Vec<(String, Arc<dyn Tool>)> = Vec::new();

        // 1. builtin (with subagent broken out), from the manager.
        for sourced in self.tool_manager.installed() {
            let label = match sourced.source {
                crate::tool_manager::ToolSource::Builtin if sourced.tool.spawns_subagent() => {
                    "subagent"
                }
                crate::tool_manager::ToolSource::Builtin => "builtin",
                // Bucket 2 labels by dynamic source id — handled below.
                crate::tool_manager::ToolSource::Mcp => continue,
            };
            if seen.insert(sourced.tool.name().to_string()) {
                sourced_tools.push((label.to_string(), sourced.tool));
            }
        }

        // 2. mcp (dynamic snapshot), labeled by the publisher's source id.
        for entry in self.dynamic_tools.snapshot() {
            if seen.insert(entry.tool.name().to_string()) {
                sourced_tools.push((entry.source, entry.tool));
            }
        }

        let mut infos: Vec<nuo_wire::ToolInfo> = sourced_tools
            .into_iter()
            .map(|(source, tool)| {
                let name = tool.name();
                nuo_wire::ToolInfo {
                    name: name.to_string(),
                    description: tool.description().to_string(),
                    enabled: !disabled.contains(name),
                    source,
                }
            })
            .collect();
        infos.sort_by(|a, b| a.source.cmp(&b.source).then_with(|| a.name.cmp(&b.name)));
        infos
    }

    /// Structured view of the skills registry, for the session modal's Skills
    /// pane. Mirrors [`skills::RegistryGuard::list`] into the render-friendly
    /// DTO.
    pub fn snapshot_skills(&self) -> Vec<nuo_wire::SkillInfo> {
        let guard = self.skills_registry.lock();
        guard
            .list()
            .into_iter()
            .map(|skill| nuo_wire::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
                version: skill.version.clone(),
                enabled: skill.enabled,
                source: skill.scope.to_string(),
                tags: skill.tags.clone(),
                quarantined: skill.quarantined,
            })
            .collect()
    }
}
