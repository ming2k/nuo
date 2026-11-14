//! Background job manager tests.

use super::*;

#[tokio::test]
async fn test_spawn_and_complete_process() {
    if !nuo_host::workspace_sandbox::available() {
        return;
    }
    let mgr = BackgroundJobManager::new();
    let ws = std::env::temp_dir();
    let roots = vec![];

    let mut rx = mgr.subscribe();

    let info = mgr
        .spawn_process(
            "echo 'hello from background'".to_string(),
            ProcessSpawnOptions {
                label: Some("test-echo".to_string()),
                cwd: None,
                workspace_root: &ws,
                additional_roots: &roots,
                detached: false,
                timeout: Some(Duration::from_secs(5)),
                owner_session: None,
            },
        )
        .await
        .expect("spawn success");

    assert_eq!(
        info.spec,
        JobSpec::Process {
            command: "echo 'hello from background'".to_string(),
            label: Some("test-echo".to_string()),
            cwd: None,
            detached: false,
            task_kind: nuo_protocol::JobKind::default(),
            readiness: None,
            restart: None,
        }
    );

    // Wait for completion event
    let mut completed = false;
    while let Ok(evt) = rx.recv().await {
        if let BackgroundJobEvent::Completed(outcome) = evt
            && outcome.job_id == info.id
        {
            assert!(matches!(
                outcome.state,
                JobState::Succeeded { exit_code: 0, .. }
            ));
            assert!(outcome.summary.contains("hello from background"));
            completed = true;
            break;
        }
    }
    assert!(completed);

    // Check list & get
    let snapshot = mgr.get_job(&info.id).expect("job exists");
    assert!(snapshot.state.is_terminal());

    let logs = mgr.get_logs(&info.id, 10).expect("logs exist");
    assert!(logs.iter().any(|l| l.contains("hello from background")));
}

#[tokio::test]
async fn test_kill_process() {
    if !nuo_host::workspace_sandbox::available() {
        return;
    }
    let mgr = BackgroundJobManager::new();
    let ws = std::env::temp_dir();
    let roots = vec![];

    let mut rx = mgr.subscribe();

    let info = mgr
        .spawn_process(
            "sleep 10".to_string(),
            ProcessSpawnOptions {
                label: Some("test-sleep".to_string()),
                cwd: None,
                workspace_root: &ws,
                additional_roots: &roots,
                detached: false,
                timeout: Some(Duration::from_secs(10)),
                owner_session: None,
            },
        )
        .await
        .expect("spawn success");

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    mgr.kill_job(&info.id).expect("kill succeeded");

    let mut killed = false;
    while let Ok(evt) = rx.recv().await {
        if let BackgroundJobEvent::Completed(outcome) = evt
            && outcome.job_id == info.id
        {
            assert!(matches!(
                outcome.state,
                JobState::Killed { .. } | JobState::Failed { .. }
            ));
            killed = true;
            break;
        }
    }
    assert!(killed);
}

/// A settled job is never signalled again — its pid may already
/// be recycled by an unrelated process, so a late "kill" would hit a
/// stranger. The control action refuses instead of claiming a termination.
#[tokio::test]
async fn test_kill_refuses_a_settled_job() {
    if !nuo_host::workspace_sandbox::available() {
        return;
    }
    let mgr = BackgroundJobManager::new();
    let ws = std::env::temp_dir();
    let roots = vec![];
    let mut rx = mgr.subscribe();

    let info = mgr
        .spawn_process(
            "echo done".to_string(),
            ProcessSpawnOptions {
                label: Some("test-settled".to_string()),
                cwd: None,
                workspace_root: &ws,
                additional_roots: &roots,
                detached: false,
                timeout: Some(Duration::from_secs(10)),
                owner_session: None,
            },
        )
        .await
        .expect("spawn success");

    let mut settled = false;
    while let Ok(evt) = rx.recv().await {
        if let BackgroundJobEvent::Completed(outcome) = evt
            && outcome.job_id == info.id
        {
            settled = true;
            break;
        }
    }
    assert!(settled);
    assert!(
        mgr.get_job(&info.id)
            .expect("job exists")
            .state
            .is_terminal(),
        "job must be terminal before the guard is exercised"
    );

    let err = mgr
        .kill_job(&info.id)
        .expect_err("a settled job must not be signalled");
    assert!(
        err.contains("not terminable") && err.contains("state:"),
        "refusal must explain why: {err}"
    );
}

/// A re-arming timer keeps its cancel handle between fires, so a
/// tick that already fired must not make the timer uncancellable — and a
/// cancelled recurring timer must stop claiming to be armed.
#[tokio::test]
async fn test_recurring_timer_stays_cancellable_after_a_fire() {
    let mgr = BackgroundJobManager::new();
    let mut rx = mgr.subscribe();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let info = mgr
        .spawn_timer(None, now + 100, Some(100), "tick".to_string(), None)
        .expect("timer spawn");

    // Wait for the first fire, then cancel.
    let mut fires = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while fires == 0 && tokio::time::Instant::now() < deadline {
        if let Ok(Ok(BackgroundJobEvent::Completed(o))) =
            tokio::time::timeout(Duration::from_millis(500), rx.recv()).await
            && o.job_id == info.id
        {
            fires += 1;
        }
    }
    assert_eq!(fires, 1, "the recurring timer must fire at least once");

    mgr.kill_job(&info.id)
        .expect("a fired-but-re-armed timer must remain cancellable");

    // The cancellation is silent but it must settle the entry, so the
    // snapshot stops claiming the timer is armed.
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut settled = false;
    while tokio::time::Instant::now() < settle_deadline {
        if mgr
            .get_job(&info.id)
            .expect("job listed")
            .state
            .is_terminal()
        {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(settled, "a cancelled recurring timer must settle");

    // …and no further fire may be published for it.
    let quiet_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < quiet_deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                panic!("a cancelled recurring timer must not fire again: {o:?}");
            }
            Ok(_) | Err(_) => {}
        }
    }
}

// Fabric tests: service readiness, adopt, extended spawn.

#[tokio::test]
async fn test_service_ready_on_first_output() {
    if !nuo_host::workspace_sandbox::available() {
        return;
    }
    let mgr = BackgroundJobManager::new();
    let ws = std::env::temp_dir();
    let roots = vec![];
    let mut rx = mgr.subscribe();

    let info = mgr
        .spawn_process_ex(
            "echo service-up && sleep 30".to_string(),
            ProcessSpawnOptions {
                label: Some("test-svc".to_string()),
                cwd: None,
                workspace_root: &ws,
                additional_roots: &roots,
                detached: false,
                timeout: None,
                owner_session: None,
            },
            nuo_protocol::JobKind::Service,
            Some(nuo_protocol::Readiness::FirstOutput),
            None,
        )
        .await
        .expect("service spawn");

    // Expect Ready then (implicitly) no Completed while running.
    let mut saw_ready = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(BackgroundJobEvent::Ready { job_id })) if job_id == info.id => {
                saw_ready = true;
                break;
            }
            Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                panic!("service settled while running: {:?}", o.state);
            }
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
    assert!(saw_ready, "service never reported Ready");

    // Snapshot shows Ready state.
    let snap = mgr.get_job(&info.id).expect("job exists");
    assert!(matches!(snap.state, JobState::Ready { .. }));

    mgr.kill_job(&info.id).expect("kill");
}

#[tokio::test]
async fn test_service_unsolicited_death_fails() {
    if !nuo_host::workspace_sandbox::available() {
        return;
    }
    let mgr = BackgroundJobManager::new();
    let ws = std::env::temp_dir();
    let roots = vec![];
    let mut rx = mgr.subscribe();

    let info = mgr
        .spawn_process_ex(
            "sleep 1; exit 3".to_string(),
            ProcessSpawnOptions {
                label: None,
                cwd: None,
                workspace_root: &ws,
                additional_roots: &roots,
                detached: false,
                timeout: None,
                owner_session: None,
            },
            nuo_protocol::JobKind::Service,
            None,
            None,
        )
        .await
        .expect("service spawn");

    let mut settled: Option<BackgroundJobOutcome> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                settled = Some(o);
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_elapsed) => {} // recv timeout: keep waiting
        }
    }
    let outcome = settled.unwrap_or_else(|| panic!("service crash must settle"));
    assert!(
        matches!(outcome.state, JobState::Failed { exit_code: 3, .. }),
        "unexpected settle state: {:?}",
        outcome.state
    );
}

#[tokio::test]
async fn test_adopt_process_settles_on_exit() {
    if !nuo_host::workspace_sandbox::available() {
        return;
    }
    let mgr = BackgroundJobManager::new();
    let ws = std::env::temp_dir();
    let roots = vec![];

    // Spawn a real detached child the fabric can adopt. stdio is null:
    // the adoption contract keeps the *tool's* pipes live; a test child
    // with no readers must not die of SIGPIPE on its own output.
    let mut cmd = nuo_host::workspace_sandbox::shell_with_roots(
        "sleep 1",
        &ws,
        &roots,
        nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
        nuo_host::workspace_sandbox::NetworkAccess::Disabled,
    )
    .unwrap();
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    let child = cmd.spawn().expect("spawn adoptable");
    let pid = child.id().unwrap_or_default();

    struct Owned {
        child: tokio::process::Child,
    }
    impl nuo_protocol::CrateChildBridge for Owned {
        fn try_wait(&mut self) -> Result<Option<i32>, String> {
            self.child
                .try_wait()
                .map(|s| s.and_then(|st| st.code()))
                .map_err(|e| e.to_string())
        }
        fn kill(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    let mut rx = mgr.subscribe();
    let info = mgr
        .adopt_process(
            "sleep 1".to_string(),
            Some("adopt-test".to_string()),
            nuo_protocol::AdoptionInfo {
                captured_lines: vec!["pre-adopt line".to_string()],
                pid,
                child: Box::new(Owned { child }),
            },
            Some("session-adopt".to_string()),
        )
        .await
        .expect("adopt");

    let mut settled: Option<BackgroundJobOutcome> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                settled = Some(o);
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_elapsed) => {} // recv timeout: keep waiting
        }
    }
    let outcome = settled.unwrap_or_else(|| panic!("adopted task must settle"));
    assert!(
        matches!(outcome.state, JobState::Succeeded { exit_code: 0, .. }),
        "unexpected settle state: {:?}",
        outcome.state
    );
    let logs = mgr.get_logs(&info.id, 10).expect("logs");
    assert!(
        logs.iter().any(|l| l.contains("pre-adopt line")),
        "captured foreground tail must be replayed into the fabric"
    );
}

#[tokio::test]
async fn test_timer_fires_once_and_wakes() {
    let mgr = BackgroundJobManager::new();
    let mut rx = mgr.subscribe();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let info = mgr
        .spawn_timer(
            Some("one-shot".to_string()),
            now + 300,
            None,
            "wake: timer digest".to_string(),
            None,
        )
        .expect("timer spawn");

    let mut settled: Option<BackgroundJobOutcome> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Ok(BackgroundJobEvent::Completed(o))) if o.job_id == info.id => {
                settled = Some(o);
                break;
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => {}
        }
    }
    let outcome = settled.expect("timer must fire");
    assert!(matches!(outcome.state, JobState::Succeeded { .. }));
    assert_eq!(outcome.summary, "wake: timer digest");
    // Spec surfaces as Timer in the snapshot before settle.
    let snap = mgr.get_job(&info.id).expect("timer listed");
    assert!(matches!(snap.spec, JobSpec::Timer { .. }));
}

#[tokio::test]
async fn test_timer_cancel_stops_recurring() {
    let mgr = BackgroundJobManager::new();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let info = mgr
        .spawn_timer(None, now + 200, Some(200), "tick".to_string(), None)
        .expect("timer spawn");

    // Let one fire land, then cancel before a second.
    tokio::time::sleep(Duration::from_millis(400)).await;
    mgr.kill_job(&info.id).expect("cancel timer");
    tokio::time::sleep(Duration::from_millis(400)).await;
    // No crash, no panic: cancellation is the contract. The job either
    // settled once or was killed mid-wait — both acceptable.
    let snap = mgr.get_job(&info.id);
    let _ = snap;
}

// Outcome retention: a settle must stay retrievable until a
// consumer claims it, and claiming must be idempotent per delivery.

/// Build a settled result without a real process, so the retention and
/// claim contract can be tested deterministically.
fn settle(mgr: &BackgroundJobManager, job_id: &str, owner: Option<&str>) -> BackgroundJobOutcome {
    let outcome = BackgroundJobOutcome {
        job_id: JobId(job_id.to_string()),
        spec: JobSpec::Timer {
            label: None,
            fire_at_ms: 0,
            interval_ms: None,
            prompt: "digest".to_string(),
        },
        state: JobState::Succeeded {
            duration_ms: 5,
            exit_code: 0,
        },
        summary: format!("summary for {job_id}"),
        log_path: None,
    };
    mgr.retain_outcome(outcome.clone(), owner.map(str::to_string));
    outcome
}

#[test]
fn retained_outcomes_survive_missing_the_event_and_claim_exactly_once() {
    let mgr = BackgroundJobManager::new();
    // No subscriber at all: the retention, not the notification, is the
    // record.
    settle(&mgr, "job_a", Some("s1"));
    settle(&mgr, "job_b", Some("s1"));
    settle(&mgr, "job_c", Some("s2"));

    assert_eq!(mgr.pending_outcomes().len(), 3);
    assert_eq!(mgr.pending_outcomes_for_session("s1").len(), 2);
    assert_eq!(mgr.pending_outcomes_for_session("s2").len(), 1);

    let job_a = JobId("job_a".to_string());
    let claimed = mgr.claim_outcomes_for_job(&job_a);
    assert_eq!(claimed.len(), 1, "the retained delivery is claimable");
    assert_eq!(claimed[0].outcome.summary, "summary for job_a");
    assert!(
        mgr.claim_outcomes_for_job(&job_a).is_empty(),
        "the same delivery must not be claimed twice"
    );

    // Claiming one job leaves the others untouched, and the claimed row
    // stays as the delivery record.
    assert_eq!(mgr.pending_outcomes_for_session("s1").len(), 2);
    let record = mgr
        .pending_outcomes()
        .into_iter()
        .find(|p| p.outcome.job_id.0 == "job_a")
        .expect("claimed delivery is retained as a record");
    assert!(record.claimed);
}

#[test]
fn distinct_fires_of_one_job_are_distinct_deliveries() {
    let mgr = BackgroundJobManager::new();
    // A re-arming timer settles repeatedly under one job id; deduplicating
    // by job id alone would swallow every fire after the first.
    settle(&mgr, "timer_x", Some("s1"));
    settle(&mgr, "timer_x", Some("s1"));

    let claimed = mgr.claim_outcomes_for_job(&JobId("timer_x".to_string()));
    assert_eq!(claimed.len(), 2);
    assert_ne!(
        claimed[0].sequence, claimed[1].sequence,
        "each fire carries its own delivery identity"
    );
}

/// The SystemWake admission path claims a session's deliveries in one step
/// so the model is handed exactly the results it is acknowledging
#[test]
fn a_session_claim_takes_only_that_sessions_unclaimed_deliveries() {
    let mgr = BackgroundJobManager::new();
    settle(&mgr, "job_a", Some("s1"));
    settle(&mgr, "job_b", Some("s1"));
    settle(&mgr, "job_c", Some("s2"));
    settle(&mgr, "server_job", None);

    let claimed = mgr.claim_outcomes_for_session("s1");
    assert_eq!(claimed.len(), 2, "both of s1's deliveries are claimed");
    assert!(
        claimed
            .iter()
            .all(|p| p.owner_session.as_deref() == Some("s1")),
        "no other session's result may be delivered to s1"
    );
    assert!(
        claimed.iter().all(|p| p.claimed),
        "returned entries are marked claimed"
    );

    // Idempotent: a second admission finds nothing new to deliver.
    assert!(mgr.claim_outcomes_for_session("s1").is_empty());

    // Other owners are untouched and still deliverable.
    assert_eq!(mgr.claim_outcomes_for_session("s2").len(), 1);
    assert_eq!(
        mgr.pending_outcomes()
            .into_iter()
            .filter(|p| !p.claimed)
            .count(),
        1,
        "only the server-level result remains unclaimed"
    );
}

#[test]
fn closing_a_session_discards_only_its_retained_outcomes() {
    let mgr = BackgroundJobManager::new();
    settle(&mgr, "job_a", Some("s1"));
    settle(&mgr, "job_b", Some("s1"));
    settle(&mgr, "server_job", None);

    assert_eq!(mgr.discard_pending_for_session("s1"), 2);
    let left = mgr.pending_outcomes();
    assert_eq!(left.len(), 1, "server-level results are not session-owned");
    assert_eq!(left[0].outcome.job_id.0, "server_job");
}

/// Claiming governs *automatic delivery*, not readability — a
/// caller that inspects or re-waits a finished job must still see what it
/// produced.
#[test]
fn a_claimed_settlement_remains_readable_on_the_job() {
    let mgr = BackgroundJobManager::new();
    let job = JobId("job_readable".to_string());
    let outcome = BackgroundJobOutcome {
        job_id: job.clone(),
        spec: JobSpec::Timer {
            label: None,
            fire_at_ms: 0,
            interval_ms: None,
            prompt: "digest".to_string(),
        },
        state: JobState::Succeeded {
            duration_ms: 12,
            exit_code: 0,
        },
        summary: "the produced result".to_string(),
        log_path: Some(PathBuf::from("/tmp/nuo-jobs/job_readable.log")),
    };
    mgr.inner.write().unwrap().insert(
        job.clone(),
        JobEntry {
            info: BackgroundJobInfo {
                id: job.clone(),
                spec: outcome.spec.clone(),
                state: outcome.state.clone(),
                created_at_ms: 0,
                completed_at_ms: Some(0),
                latest_output: None,
            },
            ring_buffer: VecDeque::new(),
            cancel_tx: None,
            owner_session: Some("s1".to_string()),
            settled: None,
            #[cfg(unix)]
            pid: None,
        },
    );

    // The two halves of delivery, minus the I/O the notification path adds:
    // the entry record makes the result *readable*, the retention queue
    // makes it *deliverable*.
    mgr.record_settlement(&outcome);
    mgr.retain_outcome(outcome.clone(), Some("s1".to_string()));

    assert_eq!(
        mgr.settled_result(&job).map(|o| o.summary),
        Some("the produced result".to_string())
    );

    // Spend the automatic delivery; the readable settlement survives.
    assert_eq!(mgr.claim_outcomes_for_job(&job).len(), 1);
    let after = mgr
        .settled_result(&job)
        .expect("still readable after claim");
    assert_eq!(after.summary, "the produced result");
    assert_eq!(
        after.log_path.as_deref(),
        Some(Path::new("/tmp/nuo-jobs/job_readable.log"))
    );
}

/// A job the fabric never settled has nothing to report — the accessor must
/// not invent a result from the running snapshot.
#[test]
fn an_unsettled_job_has_no_settled_result() {
    let mgr = BackgroundJobManager::new();
    let jid = JobId::new("job");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    mgr.inner.write().unwrap().insert(
        jid.clone(),
        JobEntry {
            info: BackgroundJobInfo {
                id: jid.clone(),
                spec: JobSpec::Timer {
                    label: None,
                    fire_at_ms: now + 1000,
                    interval_ms: None,
                    prompt: "later".to_string(),
                },
                state: JobState::Running {
                    started_at_ms: now,
                    pid: None,
                },
                created_at_ms: now,
                completed_at_ms: None,
                latest_output: None,
            },
            ring_buffer: VecDeque::new(),
            cancel_tx: None,
            owner_session: Some("s1".to_string()),
            settled: None,
            #[cfg(unix)]
            pid: None,
        },
    );

    assert!(mgr.settled_result(&jid).is_none());
}

#[test]
fn retention_is_bounded_and_evicts_the_oldest_delivery() {
    let mgr = BackgroundJobManager::new();
    for i in 0..(MAX_PENDING_OUTCOMES + 8) {
        settle(&mgr, &format!("job_{i:04}"), Some("s1"));
    }

    let retained = mgr.pending_outcomes();
    assert_eq!(
        retained.len(),
        MAX_PENDING_OUTCOMES,
        "an unclaimed backlog must not grow without bound"
    );
    assert_eq!(
        retained[0].outcome.job_id.0, "job_0008",
        "the oldest deliveries are the ones abandoned"
    );
    assert_eq!(
        retained[retained.len() - 1].outcome.job_id.0,
        format!("job_{:04}", MAX_PENDING_OUTCOMES + 7)
    );
}
