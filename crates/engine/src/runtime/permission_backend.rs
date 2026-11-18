//! Bridge from [`guardrails::PermissionBackend`] to [`persistence::db::PersistenceHandle`].
//!
//! Keeps `guardrails` free of a direct `persistence` dependency (ADR-0188) while
//! routing all durable permission rules through `nuo.db` (ADR-0212).
use std::collections::HashMap;

use guardrails::{LoadedPermissionRules, PermissionBackend, PermissionRule, PermissionScopeKey};
use persistence::db::PersistenceHandle;
use persistence::db::permissions::{PermissionRuleEffect, PermissionScopeTarget};

/// SQLite-backed permission backend operating through a [`PersistenceHandle`].
#[derive(Clone)]
pub struct SqlitePermissionBackend {
    handle: PersistenceHandle,
}

impl SqlitePermissionBackend {
    pub fn new(handle: PersistenceHandle) -> Self {
        Self { handle }
    }

    fn target(key: &PermissionScopeKey) -> PermissionScopeTarget {
        PermissionScopeTarget::from_role_and_workspace(
            key.role_id.clone(),
            key.workspace_root.as_deref(),
        )
    }
}

impl PermissionBackend for SqlitePermissionBackend {
    fn load(&self, key: &PermissionScopeKey) -> Result<LoadedPermissionRules, String> {
        let target = Self::target(key);
        let rows = self
            .handle
            .load_permission_rules_blocking(&target)
            .map_err(|err| err.to_string())?;
        // Rows arrive ordered `global` first, then `workspace`, so a workspace
        // row naturally overrides a global row for the same `(tool, scope)`.
        let mut resolved: HashMap<PermissionRule, PermissionRuleEffect> = HashMap::new();
        for row in rows {
            resolved.insert(
                PermissionRule {
                    tool: row.tool,
                    scope: row.scope,
                },
                row.effect,
            );
        }
        let mut loaded = LoadedPermissionRules::default();
        for (rule, effect) in resolved {
            match effect {
                PermissionRuleEffect::Allow => loaded.allow.push(rule),
                PermissionRuleEffect::Revoked => loaded.revoked.push(rule),
            }
        }
        Ok(loaded)
    }

    fn upsert_allow(&self, key: &PermissionScopeKey, rule: &PermissionRule) -> Result<(), String> {
        let target = Self::target(key);
        self.handle
            .upsert_permission_rule_blocking(
                target,
                rule.tool.clone(),
                rule.scope.clone(),
                PermissionRuleEffect::Allow,
            )
            .map_err(|err| err.to_string())
    }

    fn revoke(&self, key: &PermissionScopeKey, rule: &PermissionRule) -> Result<(), String> {
        let target = Self::target(key);
        self.handle
            .upsert_permission_rule_blocking(
                target,
                rule.tool.clone(),
                rule.scope.clone(),
                PermissionRuleEffect::Revoked,
            )
            .map_err(|err| err.to_string())
    }

    fn clear_all(
        &self,
        key: &PermissionScopeKey,
        active_rules: &[PermissionRule],
    ) -> Result<(), String> {
        let target = Self::target(key);
        let pairs: Vec<(String, String)> = active_rules
            .iter()
            .map(|r| (r.tool.clone(), r.scope.clone()))
            .collect();
        self.handle
            .clear_permission_rules_blocking(target, pairs)
            .map_err(|err| err.to_string())
    }

    fn touch_last_used(&self, key: &PermissionScopeKey, rule: &PermissionRule) {
        let target = Self::target(key);
        self.handle.try_touch_permission_rule_last_used(
            target,
            rule.tool.clone(),
            rule.scope.clone(),
        );
    }
}
