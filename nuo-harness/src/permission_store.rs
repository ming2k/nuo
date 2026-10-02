//! Permission allowlist + pending-request registry, extracted from the
//! `Agent` god-object.
//!
//! Owns the "always allow" rule set (optionally persisted to disk per
//! project), the map of pending permission requests awaiting a user reply,
//! and the project root used for persistence. The [`crate::Agent`] owns a
//! single `PermissionStore` and delegates its permission-related public
//! methods here.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::sync::poison_lock;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PermissionRule {
    pub tool: String,
    pub scope: String,
}

/// On-disk shape of the persisted "always allow" allowlist, versioned for
/// future schema evolution. Readers reject unknown future versions rather
/// than guessing, so a downgrade silently ignores the file.
///
/// Version 2 adds `revoked`: the rules a user has explicitly revoked, kept so a
/// declarative `[permissions]` seed cannot resurrect them (#3). A v1 file (no
/// `revoked`) loads with an empty revoked set via `#[serde(default)]`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedPermissions {
    version: u32,
    rules: Vec<PermissionRule>,
    #[serde(default)]
    revoked: Vec<PermissionRule>,
}

impl PersistedPermissions {
    /// The on-disk version this build writes and accepts.
    const CURRENT_VERSION: u32 = 2;
    /// The version this build still reads for back-compat (older files lacking
    /// the `revoked` list). `CURRENT_VERSION` is preferred on write.
    const V1_VERSION: u32 = 1;
}

#[derive(Default)]
struct PermissionState {
    always: HashSet<PermissionRule>,
    /// Rules granted for the current session only (in-memory, not persisted).
    session: HashSet<PermissionRule>,
    /// Rules the user has **explicitly revoked** (via `revoke_allowed` /
    /// `clear_allowed`), remembered so a later `seed_from_config` cannot
    /// silently re-grant them. Without this, a rule sourced from `[permissions]`
    /// config would resurrect on every restart after the user revoked it — the
    /// declarative seed re-applying past an interactive decision. The set is
    /// persisted (see `PersistedPermissions`) and is the complement of `always`:
    /// `add_always` removes from it, `revoke_allowed` adds to it. See #3.
    revoked: HashSet<PermissionRule>,
}

/// In-memory permission state: the "always allow" allowlist, the pending
/// request channels, and the optional project root for on-disk persistence.
pub struct PermissionStore {
    state: Mutex<PermissionState>,
    persistence: Mutex<Option<PermissionPersistence>>,
    /// When true, the agent runs in unattended mode — all tool permissions are auto-approved.
    unattended: Mutex<bool>,
}

/// Stable on-disk target for one project's permissions.
///
/// Resolve the concrete file once when the project is bound. Re-reading the
/// process-wide path resolver on every mutation would let a later environment
/// or test override silently redirect one store between different files.
#[derive(Clone)]
struct PermissionPersistence {
    project_root: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl PermissionStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(PermissionState::default()),
            persistence: Mutex::new(None),
            unattended: Mutex::new(false),
        }
    }

    // unattended execution posture

    pub fn unattended(&self) -> bool {
        *poison_lock(&self.unattended)
    }

    pub fn set_unattended(&self, value: bool) {
        *poison_lock(&self.unattended) = value;
    }

    // Pending-request parking moved to the human-request broker
    // (ADR-0141, `nuo_harness::human_broker`): one owner for permission /
    // ask_user / interactive-input oneshots, uniform exactly-once
    // settlement, per-kind metrics. The store keeps only rules + the delegated flag.

    // allowlist

    /// Check whether a rule is in the "always allow" set. A stored scope of
    /// `"*"` is a wildcard for that tool (matches any scope). Any other stored
    /// scope must match `rule.scope` **exactly** — there is no prefix or
    /// substring matching, so `bash git` does not allow `bash git status`.
    /// (`PermissionRuleConfig.scope` documents this contract for config authors.)
    pub fn is_always_allowed(&self, rule: &PermissionRule) -> bool {
        let state = poison_lock(&self.state);
        state.always.contains(rule)
            || state.always.contains(&PermissionRule {
                tool: rule.tool.clone(),
                scope: "*".to_string(),
            })
    }

    /// Check whether a rule is in the session allow set (granted for this session only).
    pub fn is_session_allowed(&self, rule: &PermissionRule) -> bool {
        let state = poison_lock(&self.state);
        state.session.contains(rule)
            || state.session.contains(&PermissionRule {
                tool: rule.tool.clone(),
                scope: "*".to_string(),
            })
    }

    /// Check whether a rule is allowed either permanently or for the current session.
    pub fn is_allowed(&self, rule: &PermissionRule) -> bool {
        self.is_session_allowed(rule) || self.is_always_allowed(rule)
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

    /// Add a rule to the "always allow" set and persist. Re-approving a rule
    /// also clears it from the `revoked` set (the user has reversed their
    /// earlier revocation), so a subsequent `seed_from_config` will re-grant
    /// it as expected rather than treating it as still-revoked.
    pub fn add_always(&self, rule: PermissionRule) {
        {
            let mut state = poison_lock(&self.state);
            state.always.insert(rule.clone());
            state.revoked.remove(&rule);
        }
        self.persist();
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

    pub fn allowed_tools_structured(&self) -> Vec<nuo_contracts::PermissionRuleInfo> {
        let mut rules: Vec<nuo_contracts::PermissionRuleInfo> = poison_lock(&self.state)
            .always
            .iter()
            .map(|rule| nuo_contracts::PermissionRuleInfo {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            })
            .collect();
        rules.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
        rules
    }

    /// Clear the entire "always allow" set. Every cleared rule is added to the
    /// `revoked` set so a `[permissions]` config seed cannot silently resurrect
    /// it — the user's blanket revocation must survive a restart. (#3)
    pub fn clear_allowed(&self) {
        {
            let mut state = poison_lock(&self.state);
            // Record each revoked rule so the declarative seed cannot re-grant
            // it on the next start. Collect before inserting into `revoked`:
            // `drain` holds a mutable borrow of `state.always` for its whole
            // duration, so mutating `state.revoked` inside the loop would
            // borrow `state` twice at once.
            let drained: Vec<PermissionRule> = state.always.drain().collect();
            for rule in drained {
                state.revoked.insert(rule);
            }
        }
        self.persist();
    }

    /// Revoke one rule. Returns whether it was present. The rule is added to the
    /// `revoked` set so a `[permissions]` config seed cannot resurrect it on the
    /// next start — an interactive revocation must survive a restart, even for
    /// rules originally sourced from config. (#3)
    pub fn revoke_allowed(&self, tool: &str, scope: &str) -> bool {
        let rule = PermissionRule {
            tool: tool.to_string(),
            scope: scope.to_string(),
        };
        let removed = {
            let mut state = poison_lock(&self.state);
            let removed = state.always.remove(&rule);
            if removed {
                // Remember the revocation so the declarative seed does not
                // re-grant it.
                state.revoked.insert(rule);
            }
            removed
        };
        if removed {
            self.persist();
        }
        removed
    }

    // persistence

    /// Seed the allowlist from declarative `[permissions]` config rules. Called
    /// at startup after `set_project_root` (so persistent rules are already
    /// loaded). Config rules are **not** persisted to `permissions.json` — they
    /// are re-applied on every start from `config.toml`, keeping them
    /// declarative and version-controllable. Rules already present (from disk)
    /// are not duplicated.
    ///
    /// #3 — a rule present in the `revoked` set is **skipped**: the user has
    /// already revoked it interactively (or via `clear_allowed`), and the
    /// declarative seed must not silently reverse that decision. The revocation
    /// is itself persisted, so this holds across restarts. Re-approving the
    /// rule later (`add_always`) clears the revocation and lets the seed apply
    /// again.
    pub fn seed_from_config(&self, rules: &[nuo_persistence::config::PermissionRuleConfig]) {
        let mut state = poison_lock(&self.state);
        let mut added = 0;
        let mut skipped_revoked = 0;
        for rule in rules {
            let permission_rule = PermissionRule {
                tool: rule.tool.clone(),
                scope: rule.scope.clone(),
            };
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

    /// The persisted project root, if any.
    pub fn project_root(&self) -> Option<std::path::PathBuf> {
        poison_lock(&self.persistence)
            .as_ref()
            .map(|target| target.project_root.clone())
    }

    /// Designate the project whose bucket backs the persistent "always"
    /// allowlist, and load any rules already on disk into the in-memory set.
    /// Pass `None` to disable persistence (subagents and most tests do this).
    pub fn set_project_root(&self, root: Option<std::path::PathBuf>) {
        self.set_project_root_with_dirs(root, &nuo_persistence::paths::get());
    }

    /// Bind a project using an explicit path capability.
    ///
    /// Production callers normally use [`Self::set_project_root`], which
    /// snapshots the installed application directories. The explicit form is
    /// also the hermetic test seam: tests can supply isolated directories
    /// without mutating process-wide environment or path overrides.
    pub(crate) fn set_project_root_with_dirs(
        &self,
        root: Option<std::path::PathBuf>,
        dirs: &nuo_persistence::paths::Dirs,
    ) {
        let target = root.map(|project_root| PermissionPersistence {
            file: dirs.project_permissions(&project_root),
            project_root,
        });
        *poison_lock(&self.persistence) = target.clone();
        if let Some(target) = target {
            self.load_persistent(&target.file);
        }
    }

    fn load_persistent(&self, path: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        match serde_json::from_str::<PersistedPermissions>(&text) {
            Ok(persisted)
                if persisted.version == PersistedPermissions::CURRENT_VERSION
                    || persisted.version == PersistedPermissions::V1_VERSION =>
            {
                // v1 files predate the `revoked` list; the `#[serde(default)]`
                // on that field yields an empty vec, so they load cleanly with
                // no revocations remembered (the pre-#3 behaviour).
                let mut perms = poison_lock(&self.state);
                let count = persisted.rules.len();
                for rule in persisted.rules {
                    perms.always.insert(rule);
                }
                let revoked_count = persisted.revoked.len();
                for rule in persisted.revoked {
                    perms.revoked.insert(rule);
                }
                tracing::info!(
                    count,
                    revoked_count,
                    path = %path.display(),
                    "loaded persistent permission rules",
                );
            }
            Ok(other) => {
                tracing::warn!(
                    version = other.version,
                    path = %path.display(),
                    "unsupported persisted permissions version; ignoring file",
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse persistent permissions file; ignoring",
                );
            }
        }
    }

    /// Atomically mirror the current `always` allowlist **and** `revoked` set
    /// into the project bucket. Best-effort: logs on failure and never
    /// propagates the error.
    fn persist(&self) {
        let target = poison_lock(&self.persistence).clone();
        let Some(target) = target else {
            return;
        };
        let path = target.file;
        let snapshot = {
            let perms = poison_lock(&self.state);
            let mut rules: Vec<PermissionRule> = perms.always.iter().cloned().collect();
            rules.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
            let mut revoked: Vec<PermissionRule> = perms.revoked.iter().cloned().collect();
            revoked.sort_by(|a, b| a.tool.cmp(&b.tool).then_with(|| a.scope.cmp(&b.scope)));
            PersistedPermissions {
                version: PersistedPermissions::CURRENT_VERSION,
                rules,
                revoked,
            }
        };
        if let Err(e) = nuo_persistence::fsutil::atomic_write_json(&path, &snapshot) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "could not persist permission rules",
            );
        }
    }
}

impl Default for PermissionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use nuo_persistence::config::PermissionRuleConfig;

    /// A unique temp directory for a test, with no external `tempfile` dep.
    /// Cleaned up (best-effort) on drop.
    struct ScratchDir(std::path::PathBuf);
    impl ScratchDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("muta-permstore-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            ScratchDir(dir)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn seed_from_config_adds_rules_to_allowlist() {
        let store = PermissionStore::new();
        // No project root → no persistence, so seeding is purely in-memory.
        let rules = vec![
            PermissionRuleConfig {
                tool: "execute_command".to_string(),
                scope: "*".to_string(),
            },
            PermissionRuleConfig {
                tool: "read_text".to_string(),
                scope: "*".to_string(),
            },
        ];
        store.seed_from_config(&rules);
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "*".to_string(),
        }));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "read_text".to_string(),
            scope: "*".to_string(),
        }));
    }

    #[test]
    fn wildcard_scope_allows_any_scope_for_same_tool() {
        let store = PermissionStore::new();
        store.seed_from_config(&[PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "*".to_string(),
        }]);
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        }));
        assert!(!store.is_always_allowed(&PermissionRule {
            tool: "edit_text".to_string(),
            scope: "*".to_string(),
        }));
    }

    #[test]
    fn seed_from_config_does_not_duplicate_existing_rules() {
        let store = PermissionStore::new();
        let rule = PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "*".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        store.seed_from_config(std::slice::from_ref(&rule)); // idempotent
        assert_eq!(store.allowed_tools().len(), 1);
    }

    // Parking moved to the human-request broker (ADR-0141); the
    // receiver-and-timestamp behavior now lives there — see
    // `human_broker::tests::park_reply_settles_exactly_once` and
    // `human_broker::tests::cancel_all_settles_every_kind_with_none_or_reject`.
    // The store keeps only rules + the delegated flag.

    // #3: revoked config rules must not resurrect on re-seed

    #[test]
    fn revoke_then_reseed_does_not_resurrect_config_rule() {
        // The core #3 fix: a config-sourced rule the user revoked must NOT come
        // back when seed_from_config runs again (e.g. on restart).
        let store = PermissionStore::new();
        let rule = PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        }));
        // User revokes it.
        assert!(store.revoke_allowed("execute_command", "git status"));
        assert!(!store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        }));
        // Re-seed (simulating a restart): the revoked rule stays revoked.
        store.seed_from_config(std::slice::from_ref(&rule));
        assert!(
            !store.is_always_allowed(&PermissionRule {
                tool: "execute_command".to_string(),
                scope: "git status".to_string(),
            }),
            "a revoked config rule must not resurrect on re-seed"
        );
    }

    #[test]
    fn re_approving_a_revoked_rule_lets_seed_apply_it_again() {
        // add_always clears the revocation, so a later seed re-grants the rule.
        // The user reversed their revocation; the declarative seed wins again.
        let store = PermissionStore::new();
        let rule = PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        store.revoke_allowed("execute_command", "git status");
        // Re-approve interactively.
        store.add_always(PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        });
        // A subsequent seed is idempotent (rule present) but no longer blocked.
        store.seed_from_config(std::slice::from_ref(&rule));
        assert!(store.is_always_allowed(&PermissionRule {
            tool: "execute_command".to_string(),
            scope: "git status".to_string(),
        }));
    }

    #[test]
    fn clear_allowed_revokes_every_rule_against_future_seed() {
        // clear_allowed is a blanket revocation: every rule is added to the
        // revoked set, so a config seed cannot resurrect any of them.
        let store = PermissionStore::new();
        store.seed_from_config(&[
            PermissionRuleConfig {
                tool: "execute_command".to_string(),
                scope: "*".to_string(),
            },
            PermissionRuleConfig {
                tool: "edit_text".to_string(),
                scope: "/src".to_string(),
            },
        ]);
        store.clear_allowed();
        // Re-seed: neither rule comes back.
        store.seed_from_config(&[
            PermissionRuleConfig {
                tool: "execute_command".to_string(),
                scope: "*".to_string(),
            },
            PermissionRuleConfig {
                tool: "edit_text".to_string(),
                scope: "/src".to_string(),
            },
        ]);
        assert_eq!(store.allowed_tools().len(), 0);
    }

    #[test]
    fn revoked_set_persists_across_load() {
        // The revoked set round-trips through the on-disk file: revoke, persist,
        // reload into a fresh store, and the seed still skips the rule.
        let tmp = ScratchDir::new();
        let dirs = nuo_persistence::paths::Dirs {
            config_dir: tmp.path().join("config"),
            data_dir: tmp.path().join("data"),
            state_dir: tmp.path().join("state"),
            cache_dir: tmp.path().join("cache"),
            runtime_dir: None,
        };
        let project_root = tmp.path().to_path_buf();

        let store = PermissionStore::new();
        store.set_project_root_with_dirs(Some(project_root.clone()), &dirs);
        let rule = PermissionRuleConfig {
            tool: "execute_command".to_string(),
            scope: "git push".to_string(),
        };
        store.seed_from_config(std::slice::from_ref(&rule));
        store.revoke_allowed("execute_command", "git push");

        // A fresh store pointed at the same bucket loads the revoked set.
        let reloaded = PermissionStore::new();
        reloaded.set_project_root_with_dirs(Some(project_root), &dirs);
        reloaded.seed_from_config(std::slice::from_ref(&rule));
        assert!(
            !reloaded.is_always_allowed(&PermissionRule {
                tool: "execute_command".to_string(),
                scope: "git push".to_string(),
            }),
            "revocation must survive a restart (persisted revoked set)"
        );
    }
}
