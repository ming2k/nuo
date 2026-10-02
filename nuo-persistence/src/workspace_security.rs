//! Durable workspace trust for project-supplied assets and configurations.
//!
//! Governs whether project-authored skills, MCP servers, hooks, and rules are
//! trusted to load for a given workspace.
//!
//! Trust is strictly content-bound via per-domain SHA-256 digests.
//! If project assets change (e.g. via git pull/checkout), trust drops back to
//! Quarantined until explicitly reviewed again.

use crate::paths;
use nuo_contracts::{TrustDomain, WorkspaceSecuritySnapshot, WorkspaceTrustState};
use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 2;

const MCP_PATHS: &[&str] = &[".nuo/mcp.json"];

const SKILLS_PATHS: &[&str] = &[".nuo/skills", "skills"];

const HOOK_PATHS: &[&str] = &[".nuo/hooks"];

const INSTRUCTION_PATHS: &[&str] = &["AGENTS.md"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WorkspaceRecord {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    domain_digests: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    denied_digests: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    expires_at_s: BTreeMap<String, u64>,
}

impl WorkspaceRecord {
    fn normalize_legacy_keys(&mut self) -> bool {
        let mut changed = false;
        let legacy_maps = [
            ("rules", "instructions"),
            ("roots", "ex-workspace"),
        ];
        for (old_key, new_key) in legacy_maps {
            if let Some(val) = self.domain_digests.remove(old_key) {
                self.domain_digests.entry(new_key.to_string()).or_insert(val);
                changed = true;
            }
            if let Some(val) = self.denied_digests.remove(old_key) {
                self.denied_digests.entry(new_key.to_string()).or_insert(val);
                changed = true;
            }
            if let Some(val) = self.expires_at_s.remove(old_key) {
                self.expires_at_s.entry(new_key.to_string()).or_insert(val);
                changed = true;
            }
        }
        changed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedWorkspaceSecurity {
    version: u32,
    #[serde(default)]
    workspaces: BTreeMap<String, WorkspaceRecord>,
}

impl PersistedWorkspaceSecurity {
    fn normalize_legacy_keys(&mut self) -> bool {
        let mut changed = false;
        for record in self.workspaces.values_mut() {
            if record.normalize_legacy_keys() {
                changed = true;
            }
        }
        changed
    }
}

impl Default for PersistedWorkspaceSecurity {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            workspaces: BTreeMap::new(),
        }
    }
}

/// Durable store for workspace trust decisions.
#[derive(Debug)]
pub struct WorkspaceSecurityStore {
    db_path: PathBuf,
    /// The single-writer actor bound to [`Self::db_path`] (ADR-0231): the
    /// process-wide handle for the unified `nuo.db`, or a store-private one
    /// for a path-pinned (test / tooling) instance. Either way mutations are
    /// serialized and a caller never opens its own connection.
    handle: crate::db::PersistenceHandle,
}

impl WorkspaceSecurityStore {
    pub fn load() -> Self {
        Self::for_path(paths::get().db_file())
    }

    pub fn load_from(path: PathBuf) -> Self {
        let db_path = if path.extension().and_then(|e| e.to_str()) == Some("json") {
            path.with_extension("db")
        } else {
            path
        };
        Self::for_path(db_path)
    }

    fn for_path(db_path: PathBuf) -> Self {
        let handle = if db_path == paths::get().db_file() {
            crate::db::get_persistence_handle()
        } else {
            crate::db::PersistenceHandle::spawn(db_path.clone(), None)
        };
        Self { db_path, handle }
    }

    /// Compute the current, content-aware trust state for a workspace.
    pub fn snapshot(&self, workspace: &Path) -> WorkspaceSecuritySnapshot {
        let root = workspace_identity(workspace);
        let key = canonical_string(&root);
        let state = self.read_state().unwrap_or_else(|error| {
            tracing::warn!(%error, path = %self.db_path.display(), "workspace security state is unreadable; failing closed");
            PersistedWorkspaceSecurity::default()
        });
        let record = state.workspaces.get(&key).cloned().unwrap_or_default();
        let state_for = |domain| match domain_digest(&root, domain) {
            Ok(current) => trust_state(
                current.as_deref(),
                record.domain_digests.get(domain.as_str()),
                record.denied_digests.get(domain.as_str()),
                record.expires_at_s.get(domain.as_str()).copied(),
            ),
            Err(error) => {
                tracing::warn!(
                    %error,
                    workspace = %root.display(),
                    domain = domain.as_str(),
                    "cannot attest project asset domain; quarantining it"
                );
                if record.domain_digests.contains_key(domain.as_str())
                    || record.denied_digests.contains_key(domain.as_str())
                {
                    WorkspaceTrustState::Changed
                } else {
                    WorkspaceTrustState::Quarantined
                }
            }
        };

        WorkspaceSecuritySnapshot {
            root: key,
            mcp: state_for(TrustDomain::Mcp),
            skills: state_for(TrustDomain::Skills),
            hooks: state_for(TrustDomain::Hooks),
            instructions: state_for(TrustDomain::Instructions),
            ex_workspace: state_for(TrustDomain::ExWorkspace),
            user_assets: WorkspaceTrustState::Absent,
        }
    }

    /// Trust one concrete project asset domain.
    pub fn trust_domain(&self, workspace: &Path, domain: TrustDomain) -> Result<bool, String> {
        Ok(!self.trust_domains(workspace, &[domain])?.is_empty())
    }

    /// Atomically trust every present domain in `domains`.
    ///
    /// Digests are computed before the state lock is taken. If any domain
    /// cannot be attested, no grant is persisted.
    pub fn trust_domains(
        &self,
        workspace: &Path,
        domains: &[TrustDomain],
    ) -> Result<Vec<TrustDomain>, String> {
        let root = workspace_identity(workspace);
        let key = canonical_string(&root);
        let mut digests = Vec::new();
        for &domain in domains {
            if let Some(digest) = domain_digest(&root, domain)? {
                digests.push((domain, digest));
            }
        }
        if digests.is_empty() {
            return Ok(Vec::new());
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let expires_at = now.saturating_add(crate::asset_attestation::ATTESTATION_LEASE_TTL_SECS);

        let mut state = self.read_state_for_update()?;
        let record = state.workspaces.entry(key).or_default();
        for (domain, digest) in &digests {
            record.denied_digests.remove(domain.as_str());
            record
                .domain_digests
                .insert(domain.as_str().to_string(), digest.clone());
            record
                .expires_at_s
                .insert(domain.as_str().to_string(), expires_at);
        }
        self.persist(&state)?;
        Ok(digests.into_iter().map(|(domain, _)| domain).collect())
    }

    /// Explicitly deny and record the human rejection for project asset domains (ADR-0253).
    /// Prevents repeated trust gate nagging unless the content changes on disk.
    pub fn deny_domains(
        &self,
        workspace: &Path,
        domains: &[TrustDomain],
    ) -> Result<Vec<TrustDomain>, String> {
        let root = workspace_identity(workspace);
        let key = canonical_string(&root);
        let mut digests = Vec::new();
        for &domain in domains {
            if let Some(digest) = domain_digest(&root, domain)? {
                digests.push((domain, digest));
            }
        }
        if digests.is_empty() {
            return Ok(Vec::new());
        }

        let mut state = self.read_state_for_update()?;
        let record = state.workspaces.entry(key).or_default();
        for (domain, digest) in &digests {
            record.domain_digests.remove(domain.as_str());
            record.expires_at_s.remove(domain.as_str());
            record
                .denied_digests
                .insert(domain.as_str().to_string(), digest.clone());
        }
        self.persist(&state)?;
        Ok(digests.into_iter().map(|(domain, _)| domain).collect())
    }

    /// Revoke every project asset grant for a workspace.
    pub fn revoke_workspace(&self, workspace: &Path) -> Result<bool, String> {
        let key = canonical_string(&workspace_identity(workspace));
        let mut state = self.read_state_for_update()?;
        let changed = state.workspaces.remove(&key).is_some();
        if changed {
            self.persist(&state)?;
        }
        Ok(changed)
    }

    fn read_state(&self) -> Result<PersistedWorkspaceSecurity, String> {
        let reader = self
            .handle
            .reader()
            .map_err(|e| format!("cannot open sqlite db '{}': {e}", self.db_path.display()))?;

        if let Ok(Some(mut state)) =
            reader.get_json::<PersistedWorkspaceSecurity>("state:workspace_security")
        {
            if state.version != CURRENT_VERSION {
                return Err(format!(
                    "workspace security state in '{}' has unsupported version {}; expected {}",
                    self.db_path.display(),
                    state.version,
                    CURRENT_VERSION
                ));
            }
            if state.normalize_legacy_keys() {
                let _ = self.persist(&state);
            }
            return Ok(state);
        }

        // Check for legacy JSON file to migrate once and purge
        let legacy_json = self.db_path.with_extension("json");
        if legacy_json.exists() {
            if let Ok(text) = std::fs::read_to_string(&legacy_json)
                && let Ok(state) = serde_json::from_str::<PersistedWorkspaceSecurity>(&text)
                && state.version == CURRENT_VERSION
            {
                let _ = self
                    .handle
                    .set_json_blocking("state:workspace_security", &state);
                let _ = std::fs::remove_file(&legacy_json);
                let _ = std::fs::remove_file(legacy_json.with_extension("json.lock"));
                return Ok(state);
            }
            let _ = std::fs::remove_file(&legacy_json);
            let _ = std::fs::remove_file(legacy_json.with_extension("json.lock"));
        }

        Ok(PersistedWorkspaceSecurity::default())
    }

    /// Read state for an explicit new grant/revocation. Version 1 carried the
    /// retired aggregate `extensions_digest`; it cannot be translated into
    /// independent domain authority, so an explicit mutation securely replaces
    /// it with an empty version-2 store.
    fn read_state_for_update(&self) -> Result<PersistedWorkspaceSecurity, String> {
        self.read_state()
    }

    fn persist(&self, state: &PersistedWorkspaceSecurity) -> Result<(), String> {
        self.handle
            .set_json_blocking("state:workspace_security", state)
            .map_err(|e| format!("cannot persist workspace security state to sqlite: {e}"))
    }
}

fn workspace_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn canonical_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn trust_state(
    current: Option<&str>,
    trusted: Option<&String>,
    denied: Option<&String>,
    expires_at: Option<u64>,
) -> WorkspaceTrustState {
    match (
        current,
        trusted.map(String::as_str),
        denied.map(String::as_str),
    ) {
        (None, _, _) => WorkspaceTrustState::Absent,
        (Some(curr), Some(saved), _) if curr == saved => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Some(exp) = expires_at
                && exp > 0
                && now > exp
            {
                WorkspaceTrustState::Expired
            } else {
                WorkspaceTrustState::Trusted
            }
        }
        (Some(curr), _, Some(denied_hash)) if curr == denied_hash => WorkspaceTrustState::Denied,
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => WorkspaceTrustState::Changed,
        (Some(_), None, None) => WorkspaceTrustState::Quarantined,
    }
}

fn domain_paths(domain: TrustDomain) -> &'static [&'static str] {
    match domain {
        TrustDomain::Mcp => MCP_PATHS,
        TrustDomain::Skills => SKILLS_PATHS,
        TrustDomain::Hooks => HOOK_PATHS,
        TrustDomain::Instructions => INSTRUCTION_PATHS,
        TrustDomain::ExWorkspace | TrustDomain::UserAssets => &[],
    }
}

fn domain_digest(workspace: &Path, domain: TrustDomain) -> Result<Option<String>, String> {
    if domain == TrustDomain::UserAssets {
        return Ok(None);
    }
    let mut files = Vec::new();
    for relative in domain_paths(domain) {
        let entry_path = workspace.join(relative);
        collect_asset_files(workspace, &entry_path, &mut files)?;
    }

    let config_projection = match domain {
        TrustDomain::Mcp => project_config_projection(workspace, "mcp")?,
        TrustDomain::Hooks => project_config_projection(workspace, "hooks")?,
        TrustDomain::ExWorkspace => project_config_projection(workspace, "workspace")?,
        TrustDomain::Skills | TrustDomain::Instructions | TrustDomain::UserAssets => None,
    };

    if files.is_empty() && config_projection.is_none() {
        return Ok(None);
    }
    compute_files_digest(files, config_projection)
}

fn compute_files_digest(
    mut files: Vec<(String, PathBuf)>,
    config_projection: Option<Vec<u8>>,
) -> Result<Option<String>, String> {
    files.sort();
    let mut hasher = Sha256::new();
    for (rel, abs) in files {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        let meta = std::fs::symlink_metadata(&abs).map_err(|error| {
            format!(
                "cannot inspect project asset path '{}': {error}",
                abs.display()
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "workspace asset path '{}' is a symlink; symlinked asset content cannot be trusted",
                abs.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            // Normalize permissions using git convention: only the executable bit matters (100755 vs 100644).
            // This prevents spurious trust invalidation from local umask differences or editor atomic renames.
            let is_exec = (mode & 0o111) != 0;
            let normalized_mode: u32 = if is_exec { 0o100755 } else { 0o100644 };
            hasher.update(normalized_mode.to_le_bytes());
            hasher.update([0]);
        }
        let bytes = std::fs::read(&abs).map_err(|error| {
            format!(
                "cannot read project asset file content '{}': {error}",
                abs.display()
            )
        })?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0]);
        hasher.update(&bytes);
        hasher.update([0xff]);
    }
    if let Some(bytes) = config_projection {
        hasher.update(b".nuo/config.toml#projection");
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0xff]);
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

/// Serialize only one project-config table into the domain digest. This keeps
/// a hook-only edit from invalidating an MCP grant (and vice versa) even though
/// both declarations share `.nuo/config.toml`.
fn project_config_projection(workspace: &Path, key: &str) -> Result<Option<Vec<u8>>, String> {
    let path = workspace.join(".nuo/config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot read project configuration '{}': {error}",
                path.display()
            ));
        }
    };
    let parsed = toml::from_str::<toml::Value>(&text).map_err(|error| {
        format!(
            "cannot attest project configuration '{}': {error}",
            path.display()
        )
    })?;
    let Some(value) = parsed.get(key) else {
        return Ok(None);
    };
    serde_json::to_vec(value)
        .map(Some)
        .map_err(|error| format!("cannot serialize project [{key}] contribution: {error}"))
}

fn collect_asset_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(current) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot inspect project asset path '{}': {error}",
                current.display()
            ));
        }
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "workspace asset path '{}' is a symlink; symlinked asset content cannot be trusted",
            current.display()
        ));
    }
    if meta.is_file() {
        let rel = current
            .strip_prefix(root)
            .map_err(|error| {
                format!(
                    "project asset path '{}' escaped root '{}': {error}",
                    current.display(),
                    root.display()
                )
            })?
            .to_string_lossy()
            .to_string();
        out.push((rel, current.to_path_buf()));
        return Ok(());
    }
    if meta.is_dir() {
        if let Some(name) = current.file_name().and_then(|s| s.to_str()) {
            if matches!(
                name,
                ".git"
                    | "node_modules"
                    | ".venv"
                    | "venv"
                    | "target"
                    | "__pycache__"
                    | ".pytest_cache"
            ) {
                return Ok(());
            }
        }
        let entries = std::fs::read_dir(current).map_err(|error| {
            format!(
                "cannot read project asset directory '{}': {error}",
                current.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read project asset directory entry in '{}': {error}",
                    current.display()
                )
            })?;
            collect_asset_files(root, &entry.path(), out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_content_is_quarantined_then_invalidated_by_change() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Absent);

        std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
        std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "one").unwrap();
        assert_eq!(
            store.snapshot(root).skills,
            WorkspaceTrustState::Quarantined
        );
        assert!(store.trust_domain(root, TrustDomain::Skills).unwrap());
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Trusted);

        std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "two").unwrap();
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn executable_mode_change_revokes_trust() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let hook = root.join(".nuo/hooks/check.sh");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&hook, permissions).unwrap();

        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        assert!(store.trust_domain(root, TrustDomain::Hooks).unwrap());
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&hook, permissions).unwrap();
        assert_eq!(store.snapshot(root).hooks, WorkspaceTrustState::Changed);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_domain_content_cannot_be_trusted() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let outside = root.join("outside-skill.md");
        std::fs::write(&outside, "mutable target").unwrap();
        std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".nuo/skills/demo/SKILL.md")).unwrap();

        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        let error = store.trust_domain(root, TrustDomain::Skills).unwrap_err();
        assert!(error.contains("symlink"));
        assert_eq!(
            store.snapshot(root).skills,
            WorkspaceTrustState::Quarantined
        );
    }

    #[test]
    fn domains_are_granted_persisted_and_revoked_independently() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let file = root.join("state/workspace_security.json");
        let store = WorkspaceSecurityStore::load_from(file.clone());

        std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
        std::fs::write(root.join(".nuo/skills/demo/SKILL.md"), "skill body").unwrap();
        std::fs::write(root.join(".nuo/mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        std::fs::write(
            root.join(".nuo/config.toml"),
            "[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo ready\"\n",
        )
        .unwrap();

        let snap = store.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.hooks, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.instructions, WorkspaceTrustState::Absent);

        assert!(store.trust_domain(root, TrustDomain::Mcp).unwrap());
        let snap = store.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
        assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);

        let granted = store.trust_domains(root, &TrustDomain::ALL).unwrap();
        assert_eq!(
            granted,
            vec![TrustDomain::Mcp, TrustDomain::Skills, TrustDomain::Hooks]
        );
        let reloaded = WorkspaceSecurityStore::load_from(file);
        let snap = reloaded.snapshot(root);
        assert_eq!(snap.aggregate(), WorkspaceTrustState::Trusted);
        assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
        assert_eq!(snap.skills, WorkspaceTrustState::Trusted);
        assert_eq!(snap.hooks, WorkspaceTrustState::Trusted);

        assert!(reloaded.revoke_workspace(root).unwrap());
        let snap = reloaded.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.skills, WorkspaceTrustState::Quarantined);
        assert_eq!(snap.hooks, WorkspaceTrustState::Quarantined);
    }

    #[test]
    fn config_projections_do_not_cross_invalidate_domains() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".nuo")).unwrap();
        let config = root.join(".nuo/config.toml");
        std::fs::write(
            &config,
            "[mcp.demo]\ncommand = [\"demo\"]\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo one\"\n",
        )
        .unwrap();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        store
            .trust_domains(root, &[TrustDomain::Mcp, TrustDomain::Hooks])
            .unwrap();

        std::fs::write(
            &config,
            "[mcp.demo]\ncommand = [\"demo\"]\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"echo two\"\n",
        )
        .unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.mcp, WorkspaceTrustState::Trusted);
        assert_eq!(snap.hooks, WorkspaceTrustState::Changed);
    }

    #[test]
    fn ex_workspace_domain_projection_tracks_workspace_table() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join(".nuo")).unwrap();
        let config = root.join(".nuo/config.toml");
        std::fs::write(&config, "[workspace]\nadditional_roots = [\"../optics\"]\n").unwrap();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));
        let snap = store.snapshot(root);
        assert_eq!(snap.ex_workspace, WorkspaceTrustState::Quarantined);

        store.trust_domain(root, TrustDomain::ExWorkspace).unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.ex_workspace, WorkspaceTrustState::Trusted);

        std::fs::write(
            &config,
            "[workspace]\nadditional_roots = [\"../optics\", \"../backend\"]\n",
        )
        .unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.ex_workspace, WorkspaceTrustState::Changed);
    }

    #[test]
    fn denied_domain_lifecycle_and_invalidation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));

        std::fs::create_dir_all(root.join(".nuo/skills/demo")).unwrap();
        let skill_file = root.join(".nuo/skills/demo/SKILL.md");
        std::fs::write(&skill_file, "skill content v1").unwrap();

        // 1. Initially Quarantined
        assert_eq!(
            store.snapshot(root).skills,
            WorkspaceTrustState::Quarantined
        );

        // 2. Deny domain -> becomes Denied (ADR-0253)
        store.deny_domains(root, &[TrustDomain::Skills]).unwrap();
        let snap = store.snapshot(root);
        assert_eq!(snap.skills, WorkspaceTrustState::Denied);
        assert!(snap.skills.is_denied());
        // In aggregate, when only Denied is present, aggregate is Denied (does not gate)
        assert_eq!(snap.aggregate(), WorkspaceTrustState::Denied);

        // 3. Modifying file on disk invalidates Denied -> becomes Changed!
        std::fs::write(&skill_file, "skill content v2 (altered)").unwrap();
        let snap2 = store.snapshot(root);
        assert_eq!(snap2.skills, WorkspaceTrustState::Changed);
        assert_eq!(snap2.aggregate(), WorkspaceTrustState::Changed);

        // 4. User trusts it now -> becomes Trusted
        store.trust_domains(root, &[TrustDomain::Skills]).unwrap();
        assert_eq!(store.snapshot(root).skills, WorkspaceTrustState::Trusted);
    }

    #[test]
    fn legacy_roots_and_rules_normalized_automatically() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let store = WorkspaceSecurityStore::load_from(root.join("state/workspace_security.json"));

        std::fs::write(root.join("AGENTS.md"), "test agents rules").unwrap();
        std::fs::create_dir_all(root.join(".nuo")).unwrap();
        std::fs::write(root.join(".nuo/config.toml"), "[workspace]\nadditional_roots = []\n").unwrap();

        let root_key = canonical_string(&workspace_identity(root));

        // Manually inject legacy state with "rules" and "roots"
        let mut digests = BTreeMap::new();
        let instructions_digest = domain_digest(root, TrustDomain::Instructions).unwrap().unwrap();
        let ex_workspace_digest = domain_digest(root, TrustDomain::ExWorkspace).unwrap().unwrap();
        digests.insert("rules".to_string(), instructions_digest);
        digests.insert("roots".to_string(), ex_workspace_digest);

        let mut workspaces = BTreeMap::new();
        workspaces.insert(root_key, WorkspaceRecord {
            domain_digests: digests,
            denied_digests: BTreeMap::new(),
            expires_at_s: BTreeMap::new(),
        });

        let legacy_state = PersistedWorkspaceSecurity {
            version: CURRENT_VERSION,
            workspaces,
        };

        store.persist(&legacy_state).unwrap();

        // When reading snapshot, it should automatically normalize and recognize them as Trusted!
        let snap = store.snapshot(root);
        assert_eq!(snap.instructions, WorkspaceTrustState::Trusted);
        assert_eq!(snap.ex_workspace, WorkspaceTrustState::Trusted);

        // Verify that the persisted DB state now has the normalized keys "instructions" and "ex-workspace"
        let reloaded = store.read_state().unwrap();
        let record = reloaded.workspaces.values().next().unwrap();
        assert!(record.domain_digests.contains_key("instructions"));
        assert!(record.domain_digests.contains_key("ex-workspace"));
        assert!(!record.domain_digests.contains_key("rules"));
        assert!(!record.domain_digests.contains_key("roots"));
    }
}
