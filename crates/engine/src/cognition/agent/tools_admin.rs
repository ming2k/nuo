//! Tool administration on [`Agent`]: cached permission rules,
//! enable/disable, declared tool snapshots, and the skills catalog view.
use super::*;

impl Agent {
    /// Install one harness-owned tool into the live resolved view.
    ///
    /// Used for meta-tools that need a handle to the agent itself (today
    /// `tool_search`); role-scoped capability tools still flow through
    /// [`protocol::ToolSet::resolve_for`]. Replacing a same-named tool keeps
    /// the install idempotent.
    pub fn install_tool(&self, tool: Arc<dyn Tool>) {
        {
            let mut harness = self
                .tools
                .harness_tools
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(existing) = harness
                .iter_mut()
                .find(|existing| existing.name() == tool.name())
            {
                *existing = Arc::clone(&tool);
            } else {
                harness.push(Arc::clone(&tool));
            }
        }
        if let Ok(mut pool) = self.tools.pool.write() {
            pool.upsert(Arc::clone(&tool));
        }
        let mut resolved = self
            .tools
            .resolved_tools
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = resolved
            .iter_mut()
            .find(|existing| existing.name() == tool.name())
        {
            *existing = tool;
        } else {
            resolved.push(tool);
        }
    }

    /// Rank and activate deferred tools for one `tool_search` call.
    pub(crate) fn search_deferred_tools(
        &self,
        query: &str,
        limit: usize,
    ) -> Vec<crate::tools::DeferredHit> {
        let entries: Vec<(String, String, Vec<String>, Vec<String>)> = self
            .tools
            .tool_manager
            .installed()
            .into_iter()
            .filter(|sourced| {
                sourced.tool.is_available()
                    && sourced.tool.exposure() == tool::ToolExposure::Deferred
                    && self.tools.tool_manager.is_enabled(sourced.tool.name())
            })
            .map(|sourced| {
                let properties = sourced
                    .tool
                    .parameters_schema()
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                    .map(|properties| properties.keys().cloned().collect())
                    .unwrap_or_default();
                (
                    sourced.tool.name().to_string(),
                    sourced.tool.description().to_string(),
                    sourced
                        .tool
                        .aliases()
                        .iter()
                        .map(|alias| alias.to_string())
                        .collect(),
                    properties,
                )
            })
            .collect();
        let hits = crate::tools::rank_deferred(&entries, query, limit);
        self.tools
            .tool_activation
            .activate(hits.iter().map(|hit| hit.name.clone()));
        hits
    }

    /// Structured view of the cached "always allow" rules, for the session
    /// modal's Permissions pane. Unlike [`Agent::allowed_tools`] (which collapses
    /// each rule to a single formatted string), this keeps the tool/scope pair
    /// intact so the modal can target an individual rule for revocation.
    pub fn allowed_tools_structured(&self) -> Vec<protocol::PermissionRuleInfo> {
        self.context.permissions.allowed_tools_structured()
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
        self.context.permissions.set_project_root(root);
    }

    /// Test seam for binding project persistence to an explicit directory
    /// capability without mutating process-wide path state.
    #[cfg(test)]
    pub(crate) fn set_project_root_with_dirs(
        &self,
        root: Option<std::path::PathBuf>,
        dirs: &host::paths::Dirs,
    ) {
        self.context
            .permissions
            .set_project_root_with_dirs(root, dirs);
    }

    /// Seed declarative permission rules from `[permissions]` config. Delegates
    /// to `PermissionStore::seed_from_config`.
    pub fn seed_permissions_from_config(
        &self,
        allow: &[persistence::config::PermissionRuleConfig],
        deny: &[persistence::config::PermissionRuleConfig],
    ) {
        self.context.permissions.seed_from_config(allow);
        self.context.permissions.seed_denies_from_config(deny);
    }

    /// Check an exact runtime permission scope without prompting. Used by
    /// lifecycle hooks, which execute inside agent control flow and therefore
    /// must fail closed rather than recursively opening a permission prompt.
    pub fn is_permission_allowed(&self, tool: &str, scope: &str) -> bool {
        self.context
            .permissions
            .is_allowed(&guardrails::permission_store::PermissionRule {
                tool: tool.to_string(),
                scope: scope.to_string(),
            })
    }

    /// Replace the complete tool snapshot published by one dynamic source.
    pub fn replace_dynamic_tools(&self, source: &str, tools: Vec<Arc<dyn Tool>>) {
        self.tools.dynamic_tools.replace(source, tools);
    }

    /// Remove one dynamic source and every tool it published.
    pub fn remove_dynamic_tools(&self, source: &str) {
        self.tools.dynamic_tools.remove(source);
    }

    /// The connector-facing publication port. It deliberately exposes no
    /// agent-owned lock or protocol-specific state.
    pub fn dynamic_tool_sink(&self) -> Arc<dyn protocol::DynamicToolSink> {
        self.tools.dynamic_tools.clone()
    }

    /// Update the admitted MCP server patterns for the active role.
    pub fn set_admit_mcp(&self, patterns: Vec<String>) {
        self.tools.tool_manager.set_admit_mcp(patterns);
    }

    /// Set the session-level enabled flag for a tool. No-op when the name is
    /// unknown (so a stale toggle from the modal cannot poison the dispatch
    /// table). Returns whether the flag actually changed.
    pub fn set_tool_enabled(&self, name: &str, enabled: bool) -> bool {
        let known = self.tools.toolset.variants_of(name).is_some()
            || self.tools.dynamic_tools.contains(name);
        if !known {
            return false;
        }
        let mut guard = self
            .tools
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
        if self.tools.toolset.variants_of(name).is_none()
            && !self.tools.dynamic_tools.contains(name)
        {
            return false;
        }
        let guard = self
            .tools
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        !guard.contains(name)
    }

    /// Apply hook-fired [`HookOutcome::ScopeTools`] disables: record each name
    /// (only known tools, matching the user-mask contract) under its restore
    /// point. Idempotent across repeated fires via refcounting.
    pub(super) fn apply_scoped_disables(&self, disables: &[(String, protocol::RestorePoint)]) {
        if disables.is_empty() {
            return;
        }
        let mut scoped = self
            .tools
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (name, restore) in disables {
            // Only known tools: a stale/typo'd name from a hook cannot poison
            // the mask (mirrors `set_tool_enabled`'s known-tool guard).
            if self.tools.toolset.variants_of(name).is_some() {
                scoped.disable(name, *restore);
            }
        }
    }

    /// Restore every `TurnEnd` disable at the ReAct-turn boundary.
    pub(crate) fn restore_scoped_turn_end(&self) {
        let mut scoped = self
            .tools
            .scoped_disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        scoped.restore_turn_end();
    }

    /// Restore every scoped disable (both buckets) at user-round end.
    pub(crate) fn restore_scoped_round_end(&self) {
        let mut scoped = self
            .tools
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
            .tools
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.clear();
        for name in tools {
            if self.tools.toolset.variants_of(&name).is_some() {
                guard.insert(name);
            }
        }
    }

    /// Snapshot the disabled-tool mask for persistence (ADR-0048 Phase 2).
    pub fn disabled_tools_snapshot(&self) -> std::collections::HashSet<String> {
        self.tools
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// All installed tools that the model may see this turn: every tool whose
    /// name is not disabled by *either* mask (the persisted user mask or the
    /// in-memory hook-scoped mask). Used at the schema-build choke points so a
    /// disabled tool's definition never reaches the provider.
    pub(crate) fn visible_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.refresh_tool_snapshot().tools()
    }

    /// Build and publish the immutable request snapshot.
    ///
    /// The model request and every dispatch in the resulting round read this
    /// same snapshot: a registry reload mid-request cannot change what an
    /// already-advertised call resolves to.
    pub(crate) fn refresh_tool_snapshot(&self) -> Arc<crate::tool_snapshot::ToolSnapshot> {
        let tools = self.tools.tool_manager.loop_tools(self.unattended());
        let snapshot = Arc::new(crate::tool_snapshot::ToolSnapshot::build(tools));
        tracing::debug!(tools = snapshot.len(), "refreshed tool snapshot");
        *self
            .tools
            .current_snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(Arc::clone(&snapshot));
        snapshot
    }

    /// The snapshot for dispatch: the request's snapshot when present, else a
    /// fresh one (programmatic/test callers that did not assemble a request).
    pub(crate) fn current_tool_snapshot(&self) -> Arc<crate::tool_snapshot::ToolSnapshot> {
        if let Some(snapshot) = self
            .tools
            .current_snapshot
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            return Arc::clone(snapshot);
        }
        self.refresh_tool_snapshot()
    }

    /// Structured view of every installed tool, for the session modal's Tools
    /// pane. `enabled` reflects the disabled mask; `source` classifies origin
    /// (`builtin` or the publisher-provided dynamic source id).
    pub fn snapshot_tools(&self) -> Vec<protocol::ToolInfo> {
        // Classification delegates to the ToolManager's two-bucket authority
        // for the builtin bucket; the
        // mcp bucket keeps the publisher-provided dynamic source id as its
        // label. The source label is display-only; dispatch treats both
        // buckets uniformly via name-clash priority (builtin > mcp).
        let disabled = self
            .tools
            .disabled_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let mut seen: HashSet<String> = HashSet::new();
        let mut sourced_tools: Vec<(String, Arc<dyn Tool>)> = Vec::new();

        // 1. builtin, from the manager.
        for sourced in self.tools.tool_manager.installed() {
            let label = match sourced.source {
                crate::tool_manager::ToolSource::Builtin => "builtin",
                // Bucket 2 labels by dynamic source id — handled below.
                crate::tool_manager::ToolSource::Mcp => continue,
            };
            if seen.insert(sourced.tool.name().to_string()) {
                sourced_tools.push((label.to_string(), sourced.tool));
            }
        }

        // 2. mcp (dynamic snapshot), labeled by the publisher's source id.
        for entry in self.tools.dynamic_tools.snapshot() {
            if seen.insert(entry.tool.name().to_string()) {
                sourced_tools.push((entry.source, entry.tool));
            }
        }

        let mut infos: Vec<protocol::ToolInfo> = sourced_tools
            .into_iter()
            .map(|(source, tool)| {
                let name = tool.name();
                let params = serde_json::to_string_pretty(&tool.parameters_schema()).ok();
                let risk = match tool.risk_profile() {
                    tool::RiskProfile::ReadOnly => "read_only",
                    tool::RiskProfile::IdempotentMutation => "idempotent_mutation",
                    tool::RiskProfile::Destructive => "destructive",
                    tool::RiskProfile::NetworkAccess => "network_access",
                    tool::RiskProfile::ArbitraryExecution => "arbitrary_execution",
                };
                protocol::ToolInfo {
                    name: name.to_string(),
                    description: tool.description().to_string(),
                    enabled: !disabled.contains(name),
                    source,
                    parameters: params,
                    risk: Some(risk.to_string()),
                    requires_user: tool.requires_user(),
                    requires_vision: tool.requires_vision(),
                }
            })
            .collect();
        infos.sort_by_key(|a| a.name.to_lowercase());
        infos
    }

    /// Structured view of the skills registry, for the session modal's Skills
    /// pane. Mirrors [`skills::RegistryGuard::list`] into the render-friendly
    /// DTO.
    pub fn snapshot_skills(&self) -> Vec<protocol::SkillInfo> {
        let guard = self.context.skills_registry.lock();
        guard
            .list()
            .into_iter()
            .map(|skill| protocol::SkillInfo {
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
