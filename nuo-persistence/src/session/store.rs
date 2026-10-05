//! Construction, load/persist, snapshot persistence, the list/detail/active views, and the offline corruption scan tools of [`SessionStore`].

use super::*;

/// Retry a `save_session` across the supervisor's respawn window
/// (ADR-0196 D3). Only [`crate::db::PersistenceError::WriterDown`] is
/// retried — it is the one transient variant (the actor is being
/// respawned); engine rejections and encode failures are deterministic and
/// surface immediately.
/// Five attempts over ~3.1 s covers the supervisor's `Recovering` budget.
async fn save_retrying(
    writer: &crate::db::PersistenceHandle,
    data: crate::session::SessionData,
    full: bool,
    usage_upserts: Vec<nuo_wire::RequestUsageRecord>,
    mut guard: crate::db::CommitGuard,
) -> Result<u64, crate::db::PersistenceError> {
    const MAX_ATTEMPTS: u32 = 5;
    const BASE_DELAY: Duration = Duration::from_millis(100);

    // ADR-0236 D3: every logical save carries an idempotency identity, so a
    // retry after a commit whose acknowledgement was lost resolves to the
    // original receipt instead of re-applying. A caller-supplied identity is
    // preserved across the attempts; otherwise one is minted once here.
    if guard.operation_id.is_none() {
        guard.operation_id = Some(uuid::Uuid::new_v4().to_string());
    }

    let mut delay = BASE_DELAY;
    for attempt in 1..=MAX_ATTEMPTS {
        // The save is idempotent by watermark (ADR-0187) and now by operation
        // receipt (ADR-0236 D3), so re-sending a possibly-delivered delta is
        // safe — that license is what makes the retry honest.
        match writer
            .save_session(data.clone(), full, usage_upserts.clone(), guard.clone())
            .await
        {
            Ok(revision) => return Ok(revision),
            Err(crate::db::PersistenceError::WriterDown) if attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("retry loop returns on the last attempt")
}

impl SessionStore {
    /// Open a store pinned to the **workspace**.
    ///
    /// Under ADR-0168 all session state is stored authoritatively in SQLite (`nuo.db`).
    pub fn load_for_project(project_root: PathBuf) -> Self {
        let project_root = project_root.canonicalize().unwrap_or(project_root);
        let workspace = Some(nuo_wire::WorkspaceBinding::new(project_root));
        Self::for_workspace(workspace, None)
    }

    /// Backwards-compatible alias for [`Self::load_for_project`] using the
    /// current process cwd.
    pub fn load() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_for_project(project_root)
    }

    /// Open a `SessionStore` pinned to an optional workspace binding (ADR-0226).
    /// `None` is the workspace-free (unbound) set. `persona` is the staffing
    /// persona recorded on a fresh session, if any.
    pub fn for_workspace(
        workspace: Option<nuo_wire::WorkspaceBinding>,
        role: Option<String>,
    ) -> Self {
        let dirs = paths::get();
        let sessions_dir = match &workspace {
            Some(binding) => dirs.project_sessions_dir(&binding.root),
            None => dirs.bucket_sessions_dir("workspace-free"),
        };
        if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
            tracing::warn!(error = %e, "could not create sessions dir");
        }
        let db_path = dirs.db_file();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let blob_store = BlobStore::new(dirs.blobs_dir());
        Self::pin_fresh(workspace, role, sessions_dir, db_path, blob_store)
    }

    /// Open a `SessionStore` pinned to an explicit snapshot `path`.
    /// In the unified SQLite architecture, `sessions_dir` hosts `nuo.db`.
    pub fn for_path(path: PathBuf) -> Self {
        let sessions_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let workspace = Some(nuo_wire::WorkspaceBinding::new(sessions_dir.clone()));
        let db_path = sessions_dir.join("nuo.db");
        let blob_store = BlobStore::new(sessions_dir.join("blobs"));
        let writer = if db_path == paths::get().db_file() {
            crate::db::get_persistence_handle()
        } else {
            crate::db::PersistenceHandle::spawn(db_path.clone(), Some(blob_store.clone()))
        };
        let id_stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("default");
        let data = load_or_seed(
            writer.reader().ok().as_ref(),
            Some(&writer),
            id_stem,
            &blob_store,
            workspace.as_ref(),
            None,
            Some(&path),
        );
        let defer_persist = !path.exists() && data.is_user_facing_empty();
        let role = data.role.clone();
        Self {
            workspace: std::sync::RwLock::new(workspace),
            role: std::sync::RwLock::new(role),
            sessions_dir,
            db_path,
            blob_store,
            writer,
            state: Mutex::new(SessionState::new(path, data, defer_persist)),
            persist_gate: Mutex::new(()),
        }
    }

    /// Construct a store pinned to a brand-new, empty session file in
    /// `sessions_dir`. The session is **not** written until the session gains
    /// real content, so a `nuo` that starts and exits without a round
    /// leaves no empty-file litter behind.
    fn pin_fresh(
        workspace: Option<nuo_wire::WorkspaceBinding>,
        role: Option<String>,
        sessions_dir: PathBuf,
        db_path: PathBuf,
        blob_store: BlobStore,
    ) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let path = sessions_dir.join(format!("{id}.json"));
        let role = role.or_else(|| Some("developer".to_string()));
        let role_manifest = Some(crate::roles::resolve_role_manifest(
            workspace.as_ref().map(|w| w.root.as_path()),
            role.as_deref(),
        ));
        let data = SessionData {
            id,
            workspace: workspace.clone(),
            role: role.clone(),
            role_manifest,
            ..Default::default()
        };
        let writer = if db_path == paths::get().db_file() {
            crate::db::get_persistence_handle()
        } else {
            crate::db::PersistenceHandle::spawn(db_path.clone(), Some(blob_store.clone()))
        };
        Self {
            workspace: std::sync::RwLock::new(workspace),
            role: std::sync::RwLock::new(role),
            sessions_dir,
            db_path,
            blob_store,
            writer,
            state: Mutex::new(SessionState::new(path, data, true)),
            persist_gate: Mutex::new(()),
        }
    }

    /// The optional workspace binding this store carries.
    pub fn workspace(&self) -> Option<nuo_wire::WorkspaceBinding> {
        self.workspace
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The history filter for this store's sessions: its workspace path, or
    /// unbound when there is no workspace.
    pub fn workspace_filter(&self) -> nuo_wire::WorkspaceFilter {
        nuo_wire::WorkspaceFilter::from_binding(
            self.workspace
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref(),
        )
    }

    /// Canonical domain partition for this session (ADR-0250).
    pub fn partition(&self) -> nuo_wire::SessionPartition {
        nuo_wire::SessionPartition::from_binding(
            self.workspace
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref(),
            self.role
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_deref(),
        )
    }

    /// The staffing role recorded on fresh sessions, if any.
    pub fn role(&self) -> Option<String> {
        self.role.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The workspace root, when one is bound. `None` for the unbound set.
    pub fn workspace_root(&self) -> Option<PathBuf> {
        self.workspace
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|w| w.root.clone())
    }

    pub async fn id(&self) -> String {
        self.state.lock().await.data.id.clone()
    }

    /// The content-addressed blob store for this session.
    pub fn blob_store(&self) -> &crate::blobs::BlobStore {
        &self.blob_store
    }

    /// Test-only: lock the session state (same crate sibling module access).
    #[cfg(test)]
    pub(crate) async fn state_lock_for_test(&self) -> tokio::sync::MutexGuard<'_, SessionState> {
        self.state.lock().await
    }

    /// `true` while this session has never been persisted **and** still holds
    /// no user-facing content in memory (see
    /// `SessionData::is_user_facing_empty`). Such a session is "deferred":
    /// it exists only in memory so that opening one and exiting without any
    /// real interaction leaves no record behind (ADR-0018). The transport
    /// layer's idle reaper uses this probe to reclaim never-persisted hosted
    /// sessions; a persisted session (even one whose messages were later
    /// replaced with an empty window) is never reported empty here.
    pub async fn is_empty_unpersisted(&self) -> bool {
        let state = self.state.lock().await;
        Self::should_skip_persist(&state)
    }

    /// Lock-held core of [`Self::is_empty_unpersisted`] and of every guarded
    /// setter: the post-mutation state is checked against the on-disk marker
    /// (`path.exists()`) plus the user-facing-emptiness rule. Callers must hold
    /// the session lock and pass the just-mutated state, so the decision and
    /// the event-log append it gates are atomic. The check applies only to a
    /// deferred (fresh primary) session — an explicitly pinned store
    /// (`defer_persist == false`) always persists.
    pub(crate) fn should_skip_persist(state: &SessionState) -> bool {
        state.defer_persist && state.data.is_user_facing_empty()
    }

    /// Start a brand-new session and repoint this store at it, inheriting
    /// the current session's workspace and role (ADR-0244, ADR-0245).
    pub async fn reset(&self) -> Result<String, String> {
        let mut state = self.state.lock().await;
        let workspace = state.data.workspace.clone();
        let role = state
            .data
            .role
            .clone()
            .or_else(|| Some("developer".to_string()));
        let role_manifest = state.data.role_manifest.clone().or_else(|| {
            Some(crate::roles::resolve_role_manifest(
                workspace.as_ref().map(|w| w.root.as_path()),
                role.as_deref(),
            ))
        });
        let sessions_dir = self.sessions_dir.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let path = sessions_dir.join(format!("{id}.json"));
        let data = SessionData {
            id: id.clone(),
            workspace: workspace.clone(),
            role: role.clone(),
            role_manifest,
            ..Default::default()
        };
        *self.workspace.write().unwrap_or_else(|e| e.into_inner()) = workspace;
        *self.role.write().unwrap_or_else(|e| e.into_inner()) = role;
        state.path = path;
        state.data = data;
        // Same staleness hazard as `open`: a fresh session must not inherit
        // the previous session's projected window, or `append_turn` would
        // silently drop the first durable delta (length-based delta check).
        state.invalidate_projection_cache();
        state.defer_persist = true;
        Ok(id)
    }

    /// Start a brand-new session with explicit workspace and role (ADR-0244, ADR-0245).
    pub async fn reset_with(
        &self,
        workspace: Option<nuo_wire::WorkspaceBinding>,
        role: Option<String>,
    ) -> Result<String, String> {
        let role = role.or_else(|| Some("developer".to_string()));
        let role_manifest = Some(crate::roles::resolve_role_manifest(
            workspace.as_ref().map(|w| w.root.as_path()),
            role.as_deref(),
        ));
        let mut state = self.state.lock().await;
        let sessions_dir = self.sessions_dir.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let path = sessions_dir.join(format!("{id}.json"));
        let data = SessionData {
            id: id.clone(),
            workspace: workspace.clone(),
            role: role.clone(),
            role_manifest,
            ..Default::default()
        };
        *self.workspace.write().unwrap_or_else(|e| e.into_inner()) = workspace;
        *self.role.write().unwrap_or_else(|e| e.into_inner()) = role;
        state.path = path;
        state.data = data;
        state.invalidate_projection_cache();
        state.defer_persist = true;
        Ok(id)
    }

    /// The immutable role manifest snapshot captured for this session, if any (ADR-0245).
    pub async fn role_manifest(&self) -> Option<nuo_wire::SessionRoleManifest> {
        self.state.lock().await.data.role_manifest.clone()
    }

    pub async fn resume(&self, id: Option<&str>) -> Result<String, String> {
        let target = match id {
            Some(id) => id.to_string(),
            None => self
                .list()
                .await?
                .into_iter()
                .find(|session| !session.active && session.message_count > 0)
                .map(|session| session.id)
                .ok_or_else(|| "No previous session is available to resume.".to_string())?,
        };
        self.open(&target).await?;
        Ok(self.state.lock().await.data.id.clone())
    }

    /// Switch this store to an existing session by id (or 4+-char hex prefix).
    pub async fn open(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let (resolved, path) = self.resolve_session(id, &state)?;
        // No-op when the caller asks for the session we already hold.
        if state.data.id == resolved {
            return Ok(());
        }
        let workspace = self
            .workspace
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let role = self.role.read().unwrap_or_else(|e| e.into_inner()).clone();
        let blob_store = self.blob_store.clone();
        let writer = self.writer.clone();
        let load_path = path.clone();
        let resolved_id = resolved.clone();
        let data = tokio::task::spawn_blocking(move || {
            load_or_seed(
                writer.reader().ok().as_ref(),
                Some(&writer),
                &resolved_id,
                &blob_store,
                workspace.as_ref(),
                role.as_deref(),
                Some(&load_path),
            )
        })
        .await
        .map_err(|e| format!("session open task failed: {e}"))?;
        *self.workspace.write().unwrap_or_else(|e| e.into_inner()) = data.workspace.clone();
        *self.role.write().unwrap_or_else(|e| e.into_inner()) = data.role.clone();
        state.path = path;
        state.data = data;
        // ADR-0189: the projection cache belongs to the session being left.
        // A stale cache made `model_window` return the *previous* session's
        // window after a switch, and the resume path (`restore_session_runtime`)
        // then `replace_messages`-ed it over the newly opened session — wiping
        // the resumed transcript (the `/sessions <id>` restore rendered an
        // empty view).
        state.invalidate_projection_cache();
        state.defer_persist = false;
        Ok(())
    }

    /// Delete a session by id or short id prefix.
    pub async fn delete(&self, id: &str) -> Result<String, String> {
        let (resolved, snapshot, is_active) = {
            let state = self.state.lock().await;
            let (resolved, path) = self.resolve_session(id, &state)?;
            (resolved.clone(), path, state.data.id == resolved)
        };

        let _db_deleted = self
            .writer
            .delete_session(resolved.clone())
            .await
            .map_err(|e| e.to_string())?;

        let log = snapshot.with_extension("jsonl");
        let _ = fs::remove_file(&snapshot);
        let _ = fs::remove_file(&log);

        // Repoint at a fresh session so the store stays usable after the
        // active session is removed (even if the active session was deferred/empty
        // and never written to disk or SQLite).
        if is_active {
            self.reset().await?;
        }
        Ok(resolved)
    }

    /// Set (or clear) a session's manual title by id or short id prefix.
    pub async fn rename(&self, id: &str, title: Option<String>) -> Result<(), String> {
        let manual = title.is_some();
        let (resolved, is_active) = {
            let state = self.state.lock().await;
            let (resolved, _) = self.resolve_session(id, &state)?;
            (resolved.clone(), state.data.id == resolved)
        };
        if is_active {
            return self.set_title(title, manual).await;
        }
        self.writer
            .rename_session(resolved, title, manual)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The picker-style summary of the pinned session, synthesized from
    /// in-memory state.
    pub async fn active_summary(&self) -> SessionSummary {
        let state = self.state.lock().await;
        let data = &state.data;
        let overview = match data.title.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(title) => truncate_preview(title, 64),
            None => data
                .transcript
                .project()
                .into_iter()
                .rev()
                .find(|(_, m)| m.role == nuo_wire::Role::User && !m.hidden)
                .map(|(_, m)| truncate_preview(&m.content, 64))
                .unwrap_or_else(|| "(empty session)".to_string()),
        };
        SessionSummary {
            id: data.id.clone(),
            parent_id: data.parent_id.clone(),
            fork_kind: data.fork_kind,
            message_count: data.transcript.entries.len(),
            updated_at: data.updated_at,
            created_at: data.created_at,
            overview,
            active: true,
            digest: data.digest.clone(),
        }
    }

    pub async fn list(&self) -> Result<Vec<SessionSummary>, String> {
        let active_id = self.state.lock().await.data.id.clone();
        let writer = self.writer.clone();
        let partition = self.partition();
        tokio::task::spawn_blocking(move || {
            let reader = writer.reader().map_err(|e| e.to_string())?;
            reader
                .list_session_summaries_in_partition(&partition, &active_id)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("session list task failed: {e}"))?
    }

    /// Full detail for one session, requested on demand by the session-info sub-view.
    pub async fn detail(&self, id: &str) -> Result<SessionDetail, String> {
        let active_id = self.state.lock().await.data.id.clone();
        let (resolved, _) = {
            let state = self.state.lock().await;
            self.resolve_session(id, &state)?
        };
        let writer = self.writer.clone();
        tokio::task::spawn_blocking(move || {
            let reader = writer.reader().map_err(|e| e.to_string())?;
            reader
                .get_session_detail(&resolved, &active_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Session '{resolved}' not found."))
        })
        .await
        .map_err(|e| format!("session detail task failed: {e}"))?
    }

    /// Run the persistence off the async runtime (ADR-0187): append the
    /// transcript delta above the durable watermark and upsert the session
    /// row. The engine escalates to a full rewrite whenever `data`'s
    /// transcript generation differs from the store's, so no caller-side
    /// mode flag is needed.
    pub(crate) async fn persist_off_runtime(
        &self,
        _path: PathBuf,
        data: SessionData,
        _blob_store: BlobStore,
    ) -> Result<(), String> {
        self.persist_with_usage(_path, data, Vec::new()).await
    }

    /// Persist a transcript delta plus the usage-record upserts a commit
    /// produced (ADR-0187): the usage ledger is key-addressed, so only the
    /// changed attempts are written.
    ///
    /// The delta save is idempotent by watermark (a replayed save escalates
    /// by generation, ADR-0187), so a `WriterDown` failure is retried across
    /// the supervisor's respawn window before the round is failed
    /// (ADR-0196 D3). Engine-level rejections are not retried — they are
    /// deterministic.
    pub(crate) async fn persist_with_usage(
        &self,
        path: PathBuf,
        data: SessionData,
        usage_upserts: Vec<nuo_wire::RequestUsageRecord>,
    ) -> Result<(), String> {
        self.persist_with_usage_guarded(
            path,
            data,
            usage_upserts,
            crate::db::CommitGuard::default(),
        )
        .await
        .map(|_| ())
    }

    /// [`Self::persist_with_usage`] that carries an ADR-0236 D3 commit guard
    /// and returns the committed session revision.
    pub(crate) async fn persist_with_usage_guarded(
        &self,
        _path: PathBuf,
        mut data: SessionData,
        usage_upserts: Vec<nuo_wire::RequestUsageRecord>,
        guard: crate::db::CommitGuard,
    ) -> Result<u64, String> {
        let started = std::time::Instant::now();
        let session_id = data.id.clone();
        let _persist_gate = self.persist_gate.lock().await;
        data.checksum = Some(compute_checksum(&data)?);
        let result = save_retrying(&self.writer, data, false, usage_upserts, guard)
            .await
            .map_err(|e| format!("session persist task failed: {e}"));
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_millis(250) {
            tracing::warn!(session = %session_id, duration_ms = elapsed.as_millis() as u64,
                "slow local session persistence");
        }
        result
    }

    /// Persist a full rewrite (rare: wholesale working-state replacement).
    ///
    /// A full rewrite is idempotent (it replaces every projection row), so
    /// the same `WriterDown` retry policy applies (ADR-0196 D3).
    pub(crate) async fn persist_full_rewrite(&self, data: SessionData) -> Result<(), String> {
        let _persist_gate = self.persist_gate.lock().await;
        let mut data = data;
        data.checksum = Some(compute_checksum(&data)?);
        save_retrying(
            &self.writer,
            data,
            true,
            Vec::new(),
            crate::db::CommitGuard::default(),
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("session persist task failed: {e}"))
    }

    /// Resolve `input` (a 4+ char hex id or prefix) to the full session id
    /// and the path that identifies it.
    pub(crate) fn resolve_session(
        &self,
        input: &str,
        active: &SessionState,
    ) -> Result<(String, PathBuf), String> {
        if input.len() < 4
            || !input
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return Err(format!(
                "Invalid session id prefix '{}'. Use at least 4 hexadecimal characters.",
                input
            ));
        }
        let mut matches: Vec<(String, PathBuf)> = Vec::new();
        if active.data.id.starts_with(input) {
            matches.push((active.data.id.clone(), active.path.clone()));
        }

        // Query the durable store through this store's reader (ADR-0231).
        if let Ok(reader) = self.writer.reader() {
            let filter = self.workspace_filter();
            if let Ok(found) = reader.resolve_session_prefix(input, Some(&filter)) {
                for id in found {
                    if !matches.iter().any(|(m_id, _)| m_id == &id) {
                        let path = self.sessions_dir.join(format!("{id}.json"));
                        matches.push((id, path));
                    }
                }
            }
            // If no match was found with the project_root filter and input is a full UUID,
            // check globally across the database in case of symlink or canonicalization variance.
            if matches.is_empty()
                && input.len() >= 32
                && let Ok(found) = reader.resolve_session_prefix(input, None)
            {
                for id in found {
                    if !matches.iter().any(|(m_id, _)| m_id == &id) {
                        let path = self.sessions_dir.join(format!("{id}.json"));
                        matches.push((id, path));
                    }
                }
            }
        }

        match matches.as_slice() {
            [(id, path)] => Ok((id.clone(), path.clone())),
            [] => {
                // If the input is a full 36-char canonical UUID format that doesn't match
                // anything, still resolve to its expected path so delete/cleanup operations
                // can treat already-deleted/absent sessions idempotently.
                if input.len() == 36 && input.chars().filter(|c| *c == '-').count() == 4 {
                    Ok((
                        input.to_string(),
                        self.sessions_dir.join(format!("{input}.json")),
                    ))
                } else {
                    Err(format!("No session matches '{}'.", input))
                }
            }
            _ => Err(format!(
                "Session prefix '{}' is ambiguous ({} matches).",
                input,
                matches.len()
            )),
        }
    }
}
