//! Permission allowlist + role/workspace-scoped backend bridge.
//!
//! Owns the in-memory `session` and `always` allow sets, the `revoked`
//! suppression set, declarative `deny` rules, and the `(role_id, workspace)`
//! scope binding backed by SQLite via [`PermissionBackend`] (ADR-0212).
//!
//! `guardrails` depends on contracts only (ADR-0188), never directly on
//! `persistence`; `engine` / `server` inject the concrete SQLite backend.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::command_parse::{
    CommandSegment, is_segment_safe_readonly, parse_shell_commands, segment_matches_prefix,
};
use crate::poison_lock;

const LAST_USED_TOUCH_DEBOUNCE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    pub scope: String,
}

/// Composite durability scope for permission rules: `role_id` + optional
/// normalized `workspace_root` (`None` for workspace-free roles like `ops`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PermissionScopeKey {
    pub role_id: String,
    pub workspace_root: Option<PathBuf>,
}

/// Rules hydrated from durable storage for one `(role_id, workspace_root)` scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedPermissionRules {
    pub allow: Vec<PermissionRule>,
    pub revoked: Vec<PermissionRule>,
}

/// Storage port implemented by the host persistence layer (`nuo.db`).
pub trait PermissionBackend: Send + Sync {
    fn load(&self, key: &PermissionScopeKey) -> Result<LoadedPermissionRules, String>;
    fn upsert_allow(&self, key: &PermissionScopeKey, rule: &PermissionRule) -> Result<(), String>;
    fn revoke(&self, key: &PermissionScopeKey, rule: &PermissionRule) -> Result<(), String>;
    fn clear_all(
        &self,
        key: &PermissionScopeKey,
        active_rules: &[PermissionRule],
    ) -> Result<(), String>;
    /// Non-blocking hint to bump `last_used_ms` when a persisted `Always` rule
    /// is matched at runtime.
    fn touch_last_used(&self, _key: &PermissionScopeKey, _rule: &PermissionRule) {}
}

#[derive(Default)]
struct PermissionState {
    always: HashSet<PermissionRule>,
    /// Rules granted for the current session only (in-memory, not persisted).
    session: HashSet<PermissionRule>,
    /// Rules the user has **explicitly revoked** (via `revoke_allowed` /
    /// `clear_allowed`), remembered so a later `seed_from_config` cannot
    /// silently re-grant them.
    revoked: HashSet<PermissionRule>,
    /// Declarative `[permissions].allow` rules remembered so rebinding scopes
    /// re-applies config defaults against the new scope's `revoked` set.
    config_allow: Vec<PermissionRule>,
    /// Hard denials from declarative config. Never persisted: they are a
    /// deployment policy, not an interactive grant.
    deny: HashSet<PermissionRule>,
}

/// In-memory permission state plus optional `(role_id, workspace)` persistence
/// backend.
pub struct PermissionStore {
    state: Mutex<PermissionState>,
    scope: Mutex<Option<PermissionScopeKey>>,
    backend: Mutex<Option<Arc<dyn PermissionBackend>>>,
    /// Debounces `touch_last_used` notifications so repeated tool hits do not
    /// spam SQLite writes.
    last_touched: Mutex<HashMap<(PermissionScopeKey, PermissionRule), Instant>>,
    /// When true (`auto-approve` mode), non-denied tool permissions are
    /// auto-approved in-flight without persisting rules.
    unattended: Mutex<bool>,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PermissionState::default()),
            scope: Mutex::new(None),
            backend: Mutex::new(None),
            last_touched: Mutex::new(HashMap::new()),
            unattended: Mutex::new(false),
        }
    }

    pub fn with_backend(backend: Arc<dyn PermissionBackend>) -> Self {
        Self {
            state: Mutex::new(PermissionState::default()),
            scope: Mutex::new(None),
            backend: Mutex::new(Some(backend)),
            last_touched: Mutex::new(HashMap::new()),
            unattended: Mutex::new(false),
        }
    }

    // auto-approve / unattended posture

    pub fn unattended(&self) -> bool {
        *poison_lock(&self.unattended)
    }

    pub fn set_unattended(&self, value: bool) {
        *poison_lock(&self.unattended) = value;
    }

    // backend & scope binding

    /// Install or replace the persistence backend. If a `(role_id, workspace)`
    /// scope is already bound, reloads durable rules for that scope.
    pub fn set_backend(&self, backend: Option<Arc<dyn PermissionBackend>>) {
        *poison_lock(&self.backend) = backend;
        let current_scope = poison_lock(&self.scope).clone();
        if let Some(key) = current_scope {
            self.reload_scope_from_backend(&key);
        }
    }

    /// Bind the `(role_id, workspace_root)` bucket for durable `Always` /
    /// `Revoked` rules and reload rules from the backend (if installed).
    pub fn set_scope(&self, role_id: impl Into<String>, workspace_root: Option<PathBuf>) {
        let normalized_root = workspace_root.map(|p| {
            let norm = tool::normalize_access_path(p.to_string_lossy().into_owned());
            PathBuf::from(norm)
        });
        let new_key = PermissionScopeKey {
            role_id: role_id.into(),
            workspace_root: normalized_root,
        };
        let changed = {
            let mut scope_guard = poison_lock(&self.scope);
            let changed = scope_guard.as_ref() != Some(&new_key);
            *scope_guard = Some(new_key.clone());
            changed
        };
        if changed {
            self.reload_scope_from_backend(&new_key);
        }
    }

    pub fn scope(&self) -> Option<PermissionScopeKey> {
        poison_lock(&self.scope).clone()
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        poison_lock(&self.scope)
            .as_ref()
            .and_then(|s| s.workspace_root.clone())
    }

    fn reload_scope_from_backend(&self, key: &PermissionScopeKey) {
        let backend = poison_lock(&self.backend).clone();
        let loaded = backend.and_then(|b| match b.load(key) {
            Ok(loaded) => Some(loaded),
            Err(err) => {
                tracing::warn!(
                    role = %key.role_id,
                    workspace = ?key.workspace_root,
                    error = %err,
                    "failed to load persisted permission rules from backend"
                );
                None
            }
        });

        let mut state = poison_lock(&self.state);
        state.always.clear();
        state.revoked.clear();
        if let Some(loaded) = loaded {
            for rule in loaded.revoked {
                state.revoked.insert(rule);
            }
            for rule in loaded.allow {
                state.always.insert(rule);
            }
        }
        let config_rules = state.config_allow.clone();
        for rule in config_rules {
            if !state.revoked.contains(&rule) {
                state.always.insert(rule);
            }
        }
    }

    // allowlist & evaluation

    /// Check whether a rule is in the "always allow" set.
    pub fn is_always_allowed(&self, rule: &PermissionRule) -> bool {
        let state = poison_lock(&self.state);
        rules_match_concrete(&state.always, rule)
    }

    /// Whether a declarative deny rule matches this call. Denies are evaluated
    /// before every allow path, including saved "always" grants.
    pub fn is_denied(&self, rule: &PermissionRule) -> bool {
        let state = poison_lock(&self.state);
        if rule.tool == "execute_command" {
            return command_matches_any_deny(&state.deny, &rule.scope);
        }
        state.deny.iter().any(|stored| rule_matches(stored, rule))
    }

    /// Check whether a rule is in the session allow set (granted for this session only).
    pub fn is_session_allowed(&self, rule: &PermissionRule) -> bool {
        let state = poison_lock(&self.state);
        rules_match_concrete(&state.session, rule)
    }

    /// Check whether a rule is allowed either permanently or for the current session.
    ///
    /// For compound `execute_command` invocations (`cmd1 && cmd2`), every
    /// segment is evaluated across the union of `session` and `always` so a
    /// session grant on `cmd1` and a persistent grant on `cmd2` compose cleanly.
    pub fn is_allowed(&self, rule: &PermissionRule) -> bool {
        let (allowed, hits) = self.evaluate_rule_with_hits(rule, false);
        if allowed {
            self.record_always_hits(hits);
        }
        allowed
    }

    /// Evaluate an `execute_command` invocation against the union of `session`,
    /// `always`, and provably read-only workspace inspection segments.
    ///
    /// Returns `true` when every parsed segment is either covered by `session` /
    /// `always` or classified as safe read-only (`is_segment_safe_readonly`).
    /// Denies (`is_denied`) must still be checked before calling this method.
    pub fn is_command_admitted(&self, command: &str) -> bool {
        let rule = PermissionRule {
            tool: "execute_command".to_string(),
            scope: command.to_string(),
        };
        let (allowed, hits) = self.evaluate_rule_with_hits(&rule, true);
        if allowed {
            self.record_always_hits(hits);
        }
        allowed
    }

    fn evaluate_rule_with_hits(
        &self,
        rule: &PermissionRule,
        allow_readonly_segments: bool,
    ) -> (bool, Vec<PermissionRule>) {
        let state = poison_lock(&self.state);
        if rule.tool != "execute_command" {
            if state.session.iter().any(|s| rule_matches(s, rule)) {
                return (true, Vec::new());
            }
            if let Some(matched) = state.always.iter().find(|s| rule_matches(s, rule)) {
                return (true, vec![matched.clone()]);
            }
            return (false, Vec::new());
        }
        let mut hits = Vec::new();
        let allowed = command_matches_with_hits(
            &state.session,
            &state.always,
            &rule.scope,
            allow_readonly_segments,
            &mut hits,
        );
        (allowed, hits)
    }

    fn record_always_hits(&self, hits: Vec<PermissionRule>) {
        if hits.is_empty() {
            return;
        }
        let (Some(key), Some(backend)) = (
            poison_lock(&self.scope).clone(),
            poison_lock(&self.backend).clone(),
        ) else {
            return;
        };
        let now = Instant::now();
        let to_touch: Vec<PermissionRule> = {
            let mut touched = poison_lock(&self.last_touched);
            let mut out = Vec::new();
            for rule in hits {
                let cache_key = (key.clone(), rule.clone());
                let should_touch = touched.get(&cache_key).is_none_or(|prev| {
                    now.saturating_duration_since(*prev) >= LAST_USED_TOUCH_DEBOUNCE
                });
                if should_touch {
                    touched.insert(cache_key, now);
                    out.push(rule);
                }
            }
            out
        };
        for rule in to_touch {
            backend.touch_last_used(&key, &rule);
        }
    }

    /// Add a rule to the session-scoped allow set (in-memory only, not written to disk).
    pub fn add_session(&self, rule: PermissionRule) {
        let mut state = poison_lock(&self.state);
        state.session.insert(rule);
    }

    /// Clear all session-scoped grants.
    #[cfg(test)]
    pub fn clear_session(&self) {
        let mut state = poison_lock(&self.state);
        state.session.clear();
    }

    /// Add a rule to the "always allow" set and persist to the bound backend.
    pub fn add_always(&self, rule: PermissionRule) {
        {
            let mut state = poison_lock(&self.state);
            state.always.insert(rule.clone());
            state.revoked.remove(&rule);
        }
        if let (Some(key), Some(backend)) = (
            poison_lock(&self.scope).clone(),
            poison_lock(&self.backend).clone(),
        ) && let Err(err) = backend.upsert_allow(&key, &rule)
        {
            tracing::warn!(
                role = %key.role_id,
                tool = %rule.tool,
                scope = %rule.scope,
                error = %err,
                "failed to persist Always permission rule"
            );
        }
    }

    pub fn allowed_tools(&self) -> Vec<String> {
        let mut tools = poison_lock(&self.state)
            .always
            .iter()
            .map(|rule| format!("{} {}", rule.tool, rule.scope))
            .collect::<Vec<_>>();
        tools.sort();
        tools
    }

    pub fn allowed_tools_structured(&self) -> Vec<protocol::PermissionRuleInfo> {
        let mut rules: Vec<protocol::PermissionRuleInfo> = poison_lock(&self.state)
            .always
            .iter()
            .map(|rule| protocol::PermissionRuleInfo {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            })
            .collect();
        rules.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
        rules
    }

    /// Clear the entire "always allow" set. Every cleared rule is added to the
    /// `revoked` set so a `[permissions]` config seed cannot silently resurrect
    /// it.
    pub fn clear_allowed(&self) {
        let drained = {
            let mut state = poison_lock(&self.state);
            let drained: Vec<PermissionRule> = state.always.drain().collect();
            for rule in &drained {
                state.revoked.insert(rule.clone());
            }
            drained
        };
        if let (Some(key), Some(backend)) = (
            poison_lock(&self.scope).clone(),
            poison_lock(&self.backend).clone(),
        ) && let Err(err) = backend.clear_all(&key, &drained)
        {
            tracing::warn!(
                role = %key.role_id,
                error = %err,
                "failed to persist cleared permission rules"
            );
        }
    }

    /// Revoke one rule. Returns whether it was present.
    pub fn revoke_allowed(&self, tool: &str, scope: &str) -> bool {
        let rule = PermissionRule {
            tool: tool.to_string(),
            scope: scope.to_string(),
        };
        let removed = {
            let mut state = poison_lock(&self.state);
            let removed = state.always.remove(&rule);
            if removed {
                state.revoked.insert(rule.clone());
            }
            removed
        };
        if removed
            && let (Some(key), Some(backend)) = (
                poison_lock(&self.scope).clone(),
                poison_lock(&self.backend).clone(),
            )
            && let Err(err) = backend.revoke(&key, &rule)
        {
            tracing::warn!(
                role = %key.role_id,
                tool = %rule.tool,
                scope = %rule.scope,
                error = %err,
                "failed to persist permission rule revocation"
            );
        }
        removed
    }

    // config seeding

    /// Seed the allowlist from declarative `[permissions]` config rules.
    pub fn seed_from_config(&self, rules: &[protocol::policy_config::PermissionRuleConfig]) {
        let mut state = poison_lock(&self.state);
        let mut added = 0;
        let mut skipped_revoked = 0;
        for rule in rules {
            let permission_rule = PermissionRule {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            };
            if !state.config_allow.contains(&permission_rule) {
                state.config_allow.push(permission_rule.clone());
            }
            if state.revoked.contains(&permission_rule) {
                skipped_revoked += 1;
                continue;
            }
            if state.always.insert(permission_rule) {
                added += 1;
            }
        }
        drop(state);
        if added > 0 {
            tracing::info!(added, "seeded {} permission rules from config", added);
        }
        if skipped_revoked > 0 {
            tracing::info!(
                skipped_revoked,
                "skipped {} config permission rules previously revoked by the user",
                skipped_revoked,
            );
        }
    }

    /// Install declarative hard denials. Unlike `allow`, these are never
    /// revoked or persisted: they are deployment policy and always win.
    pub fn seed_denies_from_config(&self, rules: &[protocol::policy_config::PermissionRuleConfig]) {
        let mut state = poison_lock(&self.state);
        for rule in rules {
            state.deny.insert(PermissionRule {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            });
        }
    }
}

impl Default for PermissionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn rules_match_concrete(rules: &HashSet<PermissionRule>, concrete: &PermissionRule) -> bool {
    if concrete.tool == "execute_command" {
        return command_matches_rules(rules, &concrete.scope);
    }
    rules.iter().any(|stored| rule_matches(stored, concrete))
}

fn command_matches_with_hits(
    session: &HashSet<PermissionRule>,
    always: &HashSet<PermissionRule>,
    command: &str,
    allow_readonly_segments: bool,
    touched_always: &mut Vec<PermissionRule>,
) -> bool {
    let exact = PermissionRule {
        tool: "execute_command".to_string(),
        scope: command.to_string(),
    };
    if session.iter().any(|s| command_direct_match(s, &exact)) {
        return true;
    }
    if let Some(matched) = always.iter().find(|s| command_direct_match(s, &exact)) {
        touched_always.push(matched.clone());
        return true;
    }
    let parsed = parse_shell_commands(command);
    if parsed.malformed || parsed.segments.is_empty() {
        return false;
    }
    parsed.segments.iter().all(|seg| {
        if session.iter().any(|s| command_segment_allowed(s, seg)) {
            return true;
        }
        if let Some(matched) = always.iter().find(|s| command_segment_allowed(s, seg)) {
            touched_always.push(matched.clone());
            return true;
        }
        allow_readonly_segments && is_segment_safe_readonly(seg)
    })
}

fn command_matches_rules(rules: &HashSet<PermissionRule>, command: &str) -> bool {
    let empty = HashSet::new();
    let mut unused_hits = Vec::new();
    command_matches_with_hits(rules, &empty, command, false, &mut unused_hits)
}

fn command_matches_any_deny(denies: &HashSet<PermissionRule>, command: &str) -> bool {
    let exact = PermissionRule {
        tool: "execute_command".to_string(),
        scope: command.to_string(),
    };
    if denies.iter().any(|stored| rule_matches(stored, &exact)) {
        return true;
    }
    let parsed = parse_shell_commands(command);
    parsed.segments.iter().any(|seg| {
        denies.iter().any(|stored| {
            if !wildcard_matches(&stored.tool, "execute_command") {
                return false;
            }
            if stored.scope == "*" || stored.scope == seg.raw {
                return true;
            }
            if stored.scope.ends_with('*') {
                // For hard denials, match prefix even if the segment has redirections.
                let prefix = stored.scope.trim_end_matches('*').trim_end();
                !prefix.is_empty()
                    && (seg.normalized_command == prefix
                        || seg
                            .normalized_command
                            .strip_prefix(prefix)
                            .is_some_and(|rest| rest.starts_with(' '))
                        || seg.raw.starts_with(stored.scope.trim_end_matches('*')))
            } else {
                false
            }
        })
    })
}

/// Direct full-string or `"*"` match (without compound splitting).
fn command_direct_match(stored: &PermissionRule, concrete: &PermissionRule) -> bool {
    if !wildcard_matches(&stored.tool, &concrete.tool) {
        return false;
    }
    stored.scope == "*" || stored.scope == concrete.scope
}

fn command_segment_allowed(stored: &PermissionRule, seg: &CommandSegment) -> bool {
    if !wildcard_matches(&stored.tool, "execute_command") {
        return false;
    }
    if stored.scope == "*" || stored.scope == seg.raw {
        return true;
    }
    if stored.scope.ends_with('*') {
        return segment_matches_prefix(&stored.scope, seg);
    }
    false
}

/// Whether a stored rule matches a concrete call for non-command tools.
///
/// `*` matches anything; a trailing `*` is a prefix; every other pattern is
/// exact.
fn rule_matches(stored: &PermissionRule, concrete: &PermissionRule) -> bool {
    wildcard_matches(&stored.tool, &concrete.tool)
        && wildcard_matches(&stored.scope, &concrete.scope)
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::policy_config::PermissionRuleConfig;
    use std::collections::HashMap;

    #[derive(Default)]
    struct InMemoryBackend {
        rows: Mutex<HashMap<PermissionScopeKey, LoadedPermissionRules>>,
        touches: Mutex<Vec<(PermissionScopeKey, PermissionRule)>>,
    }

    impl PermissionBackend for InMemoryBackend {
        fn load(&self, key: &PermissionScopeKey) -> Result<LoadedPermissionRules, String> {
            let map = poison_lock(&self.rows);
            let mut merged = LoadedPermissionRules::default();
            if key.workspace_root.is_some() {
                let global_key = PermissionScopeKey {
                    role_id: key.role_id.clone(),
                    workspace_root: None,
                };
                if let Some(g) = map.get(&global_key) {
                    merged.allow.extend(g.allow.clone());
                    merged.revoked.extend(g.revoked.clone());
                }
            }
            if let Some(local) = map.get(key) {
                merged.allow.extend(local.allow.clone());
                merged.revoked.extend(local.revoked.clone());
            }
            Ok(merged)
        }

        fn upsert_allow(
            &self,
            key: &PermissionScopeKey,
            rule: &PermissionRule,
        ) -> Result<(), String> {
            let mut map = poison_lock(&self.rows);
            let entry = map.entry(key.clone()).or_default();
            entry.revoked.retain(|r| r != rule);
            if !entry.allow.contains(rule) {
                entry.allow.push(rule.clone());
            }
            Ok(())
        }

        fn revoke(&self, key: &PermissionScopeKey, rule: &PermissionRule) -> Result<(), String> {
            let mut map = poison_lock(&self.rows);
            let entry = map.entry(key.clone()).or_default();
            entry.allow.retain(|r| r != rule);
            if !entry.revoked.contains(rule) {
                entry.revoked.push(rule.clone());
            }
            Ok(())
        }

        fn clear_all(
            &self,
            key: &PermissionScopeKey,
            active_rules: &[PermissionRule],
        ) -> Result<(), String> {
            let mut map = poison_lock(&self.rows);
            let entry = map.entry(key.clone()).or_default();
            let drained = std::mem::take(&mut entry.allow);
            for rule in drained.into_iter().chain(active_rules.iter().cloned()) {
                if !entry.revoked.contains(&rule) {
                    entry.revoked.push(rule);
                }
            }
            Ok(())
        }

        fn touch_last_used(&self, key: &PermissionScopeKey, rule: &PermissionRule) {
            poison_lock(&self.touches).push((key.clone(), rule.clone()));
        }
    }

    #[test]
    fn seed_from_config_adds_rules_to_allowlist() {
        let store = PermissionStore::new();
        let rules = vec![
            PermissionRuleConfig {
                tool: "execute_command".to_string(),
                scope: "cargo test".to_string(),
            },
            PermissionRuleConfig {
                tool: "write_file".to_string(),
                scope: "*".to_string(),
            },
        ];
        store.seed_from_config(&rules);
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "cargo test".to_string(),
        }));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "write_file".to_string(),
            scope: "/tmp/any".to_string(),
        }));
        assert!(!store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "rm -rf /".to_string(),
        }));
    }

    #[test]
    fn command_prefix_blocks_chaining_redirection_and_word_boundary_bypass() {
        let store = PermissionStore::new();
        store.seed_from_config(&[PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "git status *".to_string(),
        }]);

        assert!(store.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        }));
        assert!(store.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status -s".to_string(),
        }));

        for bypass in [
            "git status && curl evil.com",
            "git status; rm -rf .",
            "git status | sh",
            "git status > /tmp/out",
            "git status $(whoami)",
            "git statusx",
            "LD_PRELOAD=/tmp/evil.so git status",
        ] {
            assert!(
                !store.is_allowed(&PermissionRule {
                    tool: "execute_command".to_string(),
                    scope: bypass.to_string(),
                }),
                "prefix rule must NOT admit {bypass:?}"
            );
        }
    }

    #[test]
    fn compound_command_passes_when_every_segment_is_allowed() {
        let store = PermissionStore::new();
        store.add_always(PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status *".to_string(),
        });
        store.add_session(PermissionRule {
            tool: "execute_command".to_string(),
            scope: "cargo test *".to_string(),
        });
        assert!(store.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status -s && cargo test --lib".to_string(),
        }));
        assert!(!store.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status -s && cargo publish".to_string(),
        }));
    }

    #[test]
    fn declarative_deny_wins_across_compound_commands() {
        let store = PermissionStore::new();
        store.add_always(PermissionRule {
            tool: "execute_command".to_string(),
            scope: "*".to_string(),
        });
        store.seed_denies_from_config(&[PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "git push *".to_string(),
        }]);
        assert!(store.is_denied(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status && git push origin main".to_string(),
        }));
    }

    #[test]
    fn workspace_free_role_persists_globally_and_is_isolated_from_other_roles() {
        let backend = Arc::new(InMemoryBackend::default());
        let ops = PermissionStore::with_backend(backend.clone());
        ops.set_scope("ops", None);
        ops.add_always(PermissionRule {
            tool: "execute_command".to_string(),
            scope: "systemctl status *".to_string(),
        });

        let ops_reloaded = PermissionStore::with_backend(backend.clone());
        ops_reloaded.set_scope("ops", None);
        assert!(ops_reloaded.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "systemctl status nginx".to_string(),
        }));

        let dev = PermissionStore::with_backend(backend);
        dev.set_scope("developer", Some(PathBuf::from("/repo/a")));
        assert!(!dev.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "systemctl status nginx".to_string(),
        }));
    }

    #[test]
    fn role_workspace_isolation_and_revocation_survive_reopen() {
        let backend = Arc::new(InMemoryBackend::default());
        let rule = PermissionRule {
            tool: "execute_command".to_string(),
            scope: "cargo test *".to_string(),
        };

        let store = PermissionStore::with_backend(backend.clone());
        store.set_scope("developer", Some(PathBuf::from("/repo/a")));
        store.add_always(rule.clone());

        let same_ws = PermissionStore::with_backend(backend.clone());
        same_ws.set_scope("developer", Some(PathBuf::from("/repo/a")));
        assert!(same_ws.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "cargo test --lib".to_string(),
        }));

        let other_ws = PermissionStore::with_backend(backend.clone());
        other_ws.set_scope("developer", Some(PathBuf::from("/repo/b")));
        assert!(!other_ws.is_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "cargo test --lib".to_string(),
        }));

        assert!(same_ws.revoke_allowed("execute_command", "cargo test *"));
        let after_revoke = PermissionStore::with_backend(backend);
        after_revoke.set_scope("developer", Some(PathBuf::from("/repo/a")));
        after_revoke.seed_from_config(&[PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "cargo test *".to_string(),
        }]);
        assert!(after_revoke.allowed_tools().is_empty());
    }

    #[test]
    fn debounces_last_used_touch_and_skips_failed_compound_commands() {
        let backend = Arc::new(InMemoryBackend::default());
        let store = PermissionStore::with_backend(backend.clone());
        store.set_scope("developer", Some(PathBuf::from("/repo/a")));
        let rule = PermissionRule {
            tool: "execute_command".to_string(),
            scope: "cargo test *".to_string(),
        };
        store.add_always(rule.clone());

        // Compound command where the second segment is denied/unknown must not
        // admit or emit a `touch_last_used`.
        assert!(!store.is_command_admitted("cargo test --lib && rm -rf target"));
        assert!(poison_lock(&backend.touches).is_empty());

        // Compound command combining read-only `git status` and allowed
        // `cargo test --lib` succeeds and records exactly one debounced touch.
        assert!(store.is_command_admitted("git status -s && cargo test --lib"));
        assert!(store.is_command_admitted("cargo test --bins"));
        let touches = poison_lock(&backend.touches);
        assert_eq!(touches.len(), 1);
        assert_eq!(touches[0].1, rule);
    }
}
