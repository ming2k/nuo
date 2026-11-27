//! Relational permission-rule persistence inside `nuo.db` (ADR-0212).
//!
//! All SQLite reads and writes for `permission_rules` live here behind the
//! `nuo_persistence::db` door (`one_door` invariant).

use rusqlite::{Connection, Result, params};

/// Scope kind for a stored permission rule row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionScopeKind {
    Workspace,
    Global,
}

impl PermissionScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Global => "global",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "workspace" => Ok(Self::Workspace),
            "global" => Ok(Self::Global),
            other => Err(rusqlite::Error::InvalidColumnType(
                0,
                format!("unknown scope_kind: {other}"),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

/// Durable effect of a stored permission rule row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionRuleEffect {
    Allow,
    Revoked,
}

impl PermissionRuleEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "allow" => Ok(Self::Allow),
            "revoked" => Ok(Self::Revoked),
            other => Err(rusqlite::Error::InvalidColumnType(
                0,
                format!("unknown permission effect: {other}"),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

/// Normalized `(role_id, scope_kind, workspace_key)` bucket key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PermissionScopeTarget {
    pub role_id: String,
    pub scope_kind: PermissionScopeKind,
    pub workspace_key: String,
}

impl PermissionScopeTarget {
    pub fn from_role_and_workspace(
        role_id: impl Into<String>,
        workspace_root: Option<&std::path::Path>,
    ) -> Self {
        let role_id = role_id.into();
        match workspace_root {
            Some(path) => {
                let normalized =
                    nuo_tool::normalize_access_path(path.to_string_lossy().into_owned());
                if normalized.is_empty() {
                    Self {
                        role_id,
                        scope_kind: PermissionScopeKind::Global,
                        workspace_key: String::new(),
                    }
                } else {
                    Self {
                        role_id,
                        scope_kind: PermissionScopeKind::Workspace,
                        workspace_key: normalized,
                    }
                }
            }
            None => Self {
                role_id,
                scope_kind: PermissionScopeKind::Global,
                workspace_key: String::new(),
            },
        }
    }
}

/// One row read from `permission_rules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPermissionRuleRow {
    pub scope_kind: PermissionScopeKind,
    pub tool: String,
    pub scope: String,
    pub effect: PermissionRuleEffect,
}

/// Load all persisted `allow` and `revoked` rules for `(role_id, workspace)`.
///
/// When `target.scope_kind == Workspace`, loads both `(role_id, 'global', '')`
/// and `(role_id, 'workspace', workspace_key)` — global rules first, then
/// workspace rules — so workspace-level revocations or grants layer cleanly on
/// top of role-wide defaults.
pub(crate) fn load_permission_rules(
    conn: &Connection,
    target: &PermissionScopeTarget,
) -> Result<Vec<StoredPermissionRuleRow>> {
    let mut rows_out = Vec::new();
    let mut stmt = conn.prepare_cached(
        "SELECT scope_kind, tool, scope, effect
           FROM permission_rules
          WHERE role_id = ?1
            AND (
                (scope_kind = 'global' AND workspace_key = '')
                OR (scope_kind = ?2 AND workspace_key = ?3)
            )
          ORDER BY CASE scope_kind WHEN 'global' THEN 0 ELSE 1 END, id ASC",
    )?;
    let rows = stmt.query_map(
        params![
            target.role_id,
            target.scope_kind.as_str(),
            target.workspace_key
        ],
        |row| {
            let kind_raw: String = row.get(0)?;
            let tool: String = row.get(1)?;
            let scope: String = row.get(2)?;
            let effect_raw: String = row.get(3)?;
            Ok((kind_raw, tool, scope, effect_raw))
        },
    )?;
    for row in rows {
        let (kind_raw, tool, scope, effect_raw) = row?;
        rows_out.push(StoredPermissionRuleRow {
            scope_kind: PermissionScopeKind::parse(&kind_raw)?,
            tool,
            scope,
            effect: PermissionRuleEffect::parse(&effect_raw)?,
        });
    }
    Ok(rows_out)
}

/// Upsert one `allow` or `revoked` rule for `target`.
pub(crate) fn upsert_permission_rule(
    conn: &Connection,
    target: &PermissionScopeTarget,
    tool: &str,
    scope: &str,
    effect: PermissionRuleEffect,
    now_ms: i64,
) -> Result<()> {
    let initial_last_used = match effect {
        PermissionRuleEffect::Allow => Some(now_ms),
        PermissionRuleEffect::Revoked => None,
    };
    conn.prepare_cached(
        "INSERT INTO permission_rules (
             role_id, scope_kind, workspace_key, tool, scope, effect,
             created_at_ms, updated_at_ms, last_used_ms
         )
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)
         ON CONFLICT(role_id, scope_kind, workspace_key, tool, scope)
         DO UPDATE SET
             effect = excluded.effect,
             updated_at_ms = excluded.updated_at_ms,
             last_used_ms = COALESCE(excluded.last_used_ms, permission_rules.last_used_ms)",
    )?
    .execute(params![
        target.role_id,
        target.scope_kind.as_str(),
        target.workspace_key,
        tool,
        scope,
        effect.as_str(),
        now_ms,
        initial_last_used,
    ])?;
    Ok(())
}

/// Update `last_used_ms` on an active `allow` rule matching `target` (including
/// inherited `global` rules for the same role). Best-effort fire-and-forget.
pub(crate) fn touch_permission_rule_last_used(
    conn: &Connection,
    target: &PermissionScopeTarget,
    tool: &str,
    scope: &str,
    now_ms: i64,
) -> Result<()> {
    conn.prepare_cached(
        "UPDATE permission_rules
            SET last_used_ms = ?6
          WHERE role_id = ?1
            AND (
                (scope_kind = 'global' AND workspace_key = '')
                OR (scope_kind = ?2 AND workspace_key = ?3)
            )
            AND tool = ?4
            AND scope = ?5
            AND effect = 'allow'",
    )?
    .execute(params![
        target.role_id,
        target.scope_kind.as_str(),
        target.workspace_key,
        tool,
        scope,
        now_ms,
    ])?;
    Ok(())
}

/// Clear all `allow` rules in `target` by marking them `revoked` and ensuring
/// every currently-active rule (including rules seeded from declarative config)
/// is recorded as `revoked` so config seeding cannot resurrect them.
pub(crate) fn clear_permission_rules(
    conn: &Connection,
    target: &PermissionScopeTarget,
    active_rules: &[(String, String)],
    now_ms: i64,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.prepare_cached(
        "UPDATE permission_rules
            SET effect = 'revoked', updated_at_ms = ?4
          WHERE role_id = ?1
            AND scope_kind = ?2
            AND workspace_key = ?3
            AND effect = 'allow'",
    )?
    .execute(params![
        target.role_id,
        target.scope_kind.as_str(),
        target.workspace_key,
        now_ms,
    ])?;
    for (tool, scope) in active_rules {
        upsert_permission_rule(
            &tx,
            target,
            tool,
            scope,
            PermissionRuleEffect::Revoked,
            now_ms,
        )?;
    }
    tx.commit()?;
    Ok(())
}
