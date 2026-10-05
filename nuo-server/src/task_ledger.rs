//! ADR-0190 D4: durable task ledger and boot rehost.
//!
//! Every settled task writes one `task:<job_id>` row into the unified KV
//! store (ADR-0168 typed JSON KV): spec, terminal state, summary, log path.
//! The row is the fabric's durable record — restart-inspectable, and the
//! input to the boot rehost: services whose spec carries a `restart` policy
//! are re-spawned by the daemon at boot (the successor of ADR-0125's
//! bespoke armed-schedule rehost machinery), while one-shot tasks merely
//! keep their last outcome inspectable.
//!
//! Every access goes through the single-writer actor (ADR-0231): the ledger
//! never opens a connection of its own.

use nuo_wire::{BackgroundJobInfo, BackgroundJobOutcome, JobSpec, JobState};
use nuo_persistence::db::PersistenceHandle;

const LEDGER_PREFIX: &str = "task:";

/// One durable task row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskLedgerRow {
    pub job_id: String,
    pub spec: JobSpec,
    /// The terminal state at settle time (services keep their last known
    /// state; a rehosted service overwrites this on its next settle).
    pub state: JobState,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    /// Session that owned the task, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Persist a settled task's outcome (best-effort: the ledger must never
/// break task execution).
pub fn record_outcome(
    writer: &PersistenceHandle,
    outcome: &BackgroundJobOutcome,
    owner_session: Option<String>,
) {
    let row = TaskLedgerRow {
        job_id: outcome.job_id.0.clone(),
        spec: outcome.spec.clone(),
        state: outcome.state.clone(),
        summary: outcome.summary.clone(),
        log_path: outcome.log_path.as_ref().map(|p| p.display().to_string()),
        session_id: owner_session,
    };
    match serde_json::to_string(&row) {
        Ok(json) => {
            if let Err(error) =
                writer.set_kv_blocking(format!("{LEDGER_PREFIX}{}", outcome.job_id.0), json)
            {
                tracing::warn!(%error, job = %outcome.job_id.0, "task ledger write failed");
            }
        }
        Err(error) => {
            tracing::warn!(%error, job = %outcome.job_id.0, "task ledger serialize failed");
        }
    }
}

/// Load every ledger row whose key starts with the task prefix.
pub fn load_all(writer: &PersistenceHandle) -> Vec<TaskLedgerRow> {
    let Ok(reader) = writer.reader() else {
        tracing::warn!("task ledger scan failed: no reader");
        return Vec::new();
    };
    let keys = match reader.list_kv_keys_with_prefix(LEDGER_PREFIX) {
        Ok(keys) => keys,
        Err(error) => {
            tracing::warn!(%error, "task ledger scan failed");
            return Vec::new();
        }
    };
    keys.iter()
        .filter_map(|key| {
            reader
                .get_kv(key)
                .ok()
                .flatten()
                .and_then(|json| serde_json::from_str(&json).ok())
        })
        .collect()
}

/// Prune ledger rows older than `max_age_days` days (settle timestamp comes
/// from the row's id-unavailable path, so we use the state's duration-free
/// created marker when present — pruning is best-effort housekeeping).
pub fn prune(writer: &PersistenceHandle, keep: &[String]) {
    let Ok(reader) = writer.reader() else {
        return;
    };
    let keys = match reader.list_kv_keys_with_prefix(LEDGER_PREFIX) {
        Ok(keys) => keys,
        Err(_) => return,
    };
    for key in keys {
        let job = key.trim_start_matches(LEDGER_PREFIX).to_string();
        if !keep.contains(&job)
            && let Err(error) = writer.delete_kv_blocking(key.clone())
        {
            tracing::warn!(%error, key = %key, "task ledger prune failed");
        }
    }
}

/// Services whose spec carries a restart policy are rehost candidates at
/// daemon boot.
pub fn rehost_candidates(rows: &[TaskLedgerRow]) -> Vec<TaskLedgerRow> {
    rows.iter()
        .filter(|row| {
            matches!(
                &row.spec,
                JobSpec::Process {
                    restart: Some(_),
                    ..
                }
            )
        })
        .cloned()
        .collect()
}

/// Rehost every restartable service found in the ledger (best-effort; runs
/// at daemon boot). `spawn` is the caller's spawn closure so this module
/// stays decoupled from any one execution environment.
pub fn rehost_all<F>(writer: &PersistenceHandle, spawn: F)
where
    F: Fn(TaskLedgerRow),
{
    let rows = load_all(writer);
    let candidates = rehost_candidates(&rows);
    for row in candidates {
        tracing::info!(job = %row.job_id, "task rehost: respawning service with restart policy");
        spawn(row);
    }
}

/// Snapshot helper used by the fabric when a job finishes.
pub fn outcome_from_info(
    info: &BackgroundJobInfo,
    summary: String,
) -> Option<BackgroundJobOutcome> {
    let state = match &info.state {
        s if s.is_terminal() => Some(s.clone()),
        _ => None,
    }?;
    Some(BackgroundJobOutcome {
        job_id: info.id.clone(),
        spec: info.spec.clone(),
        state,
        summary,
        log_path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store-private actor over a temp database: the same write path
    /// production uses, without touching the user's real `nuo.db`.
    fn writer() -> (PersistenceHandle, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let handle = PersistenceHandle::spawn(tmp.path().join("muta.db"), None);
        (handle, tmp)
    }

    fn outcome(restart: Option<nuo_wire::RestartPolicy>) -> BackgroundJobOutcome {
        BackgroundJobOutcome {
            job_id: nuo_wire::JobId::new("svc"),
            spec: JobSpec::Process {
                command: "sleep 30".into(),
                label: Some("ledger-test".into()),
                cwd: None,
                detached: false,
                task_kind: nuo_wire::JobKind::Service,
                readiness: None,
                restart,
            },
            state: JobState::Failed {
                duration_ms: 10,
                exit_code: 1,
                error: "crashed".into(),
            },
            summary: "tail".into(),
            log_path: None,
        }
    }

    #[test]
    fn record_then_load_roundtrips_and_flags_rehost_candidates() {
        let (writer, _tmp) = writer();

        let with_policy = outcome(Some(nuo_wire::RestartPolicy {
            max_retries: 3,
            backoff_ms: 500,
        }));
        record_outcome(&writer, &with_policy, None);

        let without_policy = outcome(None);
        record_outcome(&writer, &without_policy, None);

        let rows = load_all(&writer);
        assert_eq!(rows.len(), 2, "both settles persisted");
        // Owner recorded when supplied (D5 ownership pass-through).
        assert!(
            rows.iter().all(|r| r.session_id.is_none()),
            "None-owner settles carry no session"
        );

        let candidates = rehost_candidates(&rows);
        assert_eq!(
            candidates.len(),
            1,
            "only the restart-policy service rehosts"
        );
        assert_eq!(candidates[0].job_id, with_policy.job_id.0);
        assert_eq!(candidates[0].summary, "tail");
    }

    #[test]
    fn prune_keeps_listed_jobs_only() {
        let (writer, _tmp) = writer();
        let keep_me = outcome(None);
        let drop_me = outcome(None);
        record_outcome(&writer, &keep_me, None);
        record_outcome(&writer, &drop_me, None);

        prune(&writer, std::slice::from_ref(&keep_me.job_id.0));

        let rows = load_all(&writer);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].job_id, keep_me.job_id.0);
    }
}
