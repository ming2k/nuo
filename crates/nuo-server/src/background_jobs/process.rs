//! Process and timer spawning.

use super::*;

impl BackgroundJobManager {
    /// Spawn a deterministic shell command in the background.
    pub async fn spawn_process(
        &self,
        command: String,
        opts: ProcessSpawnOptions<'_>,
    ) -> Result<BackgroundJobInfo, String> {
        let ProcessSpawnOptions {
            label,
            cwd,
            workspace_root,
            additional_roots,
            detached,
            timeout,
            owner_session,
        } = opts;
        let job_id = JobId::new("job");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let mut cmd = nuo_host::workspace_sandbox::shell_with_roots(
            &command,
            workspace_root,
            additional_roots,
            nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
            nuo_host::workspace_sandbox::NetworkAccess::Enabled,
        )?;

        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        #[cfg(unix)]
        {
            // Set process group so we can cleanly kill subprocess trees if needed
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn background command: {e}"))?;
        let pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd,
                detached,
                task_kind: JobKind::default(),
                readiness: None,
                restart: None,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();

        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    settled: None,
                    #[cfg(unix)]
                    pid,
                },
            );
        }

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        // Spawn async collector and supervisor task
        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();

        tokio::spawn(async move {
            let start_time = Instant::now();
            let mut log_writer = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_file_path)
                .await
                .ok();

            // Bounded line pump (D5): the fabric never accumulates
            // unbounded progress. Cap 1024 lines in flight; under pressure
            // the *progress broadcast* coalesces (oldest dropped via the
            // send failure path) while the ring buffer + disk log keep full
            // fidelity — a slow UI never stalls the child, and a flood
            // never balloons memory.
            let (line_tx, mut line_rx) = mpsc::channel::<String>(1024);

            // Read stdout stream
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }

            // Read stderr stream
            if let Some(err) = stderr {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            drop(line_tx);

            let timeout_duration = timeout.unwrap_or(Duration::from_secs(3600));
            let timeout_sleep = tokio::time::sleep(timeout_duration);
            tokio::pin!(timeout_sleep);

            let mut was_cancelled = false;
            let mut was_timed_out = false;

            let exit_status = loop {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        mgr.append_line(&jid, &line);
                        if let Some(ref mut w) = log_writer {
                            use tokio::io::AsyncWriteExt;
                            let _ = w.write_all(line.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Progress {
                            job_id: jid.clone(),
                            line,
                        });
                    }
                    status = child.wait() => {
                        // Drain remaining lines
                        while let Ok(line) = line_rx.try_recv() {
                            mgr.append_line(&jid, &line);
                            if let Some(ref mut w) = log_writer {
                                use tokio::io::AsyncWriteExt;
                                let _ = w.write_all(line.as_bytes()).await;
                                let _ = w.write_all(b"\n").await;
                            }
                        }
                        break status.ok();
                    }
                    _ = &mut cancel_rx => {
                        was_cancelled = true;
                        let _ = child.kill().await;
                        break None;
                    }
                    _ = &mut timeout_sleep => {
                        was_timed_out = true;
                        let _ = child.kill().await;
                        break None;
                    }
                }
            };

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let final_state = if was_cancelled {
                JobState::Killed { duration_ms }
            } else if was_timed_out {
                JobState::TimedOut { duration_ms }
            } else if let Some(status) = exit_status {
                let code = status
                    .code()
                    .unwrap_or(if status.success() { 0 } else { 1 });
                if status.success() {
                    JobState::Succeeded {
                        duration_ms,
                        exit_code: code,
                    }
                } else {
                    JobState::Failed {
                        duration_ms,
                        exit_code: code,
                        error: format!("Process exited with status code {code}"),
                    }
                }
            } else {
                JobState::Failed {
                    duration_ms,
                    exit_code: -1,
                    error: "Process terminated unexpectedly".to_string(),
                }
            };

            mgr.finish_job(jid, spec, final_state, Some(log_file_path));
        });

        Ok(info)
    }

    /// Spawn with full control (kind / readiness / restart).
    ///
    /// `Interactive` delegates to [`Self::spawn_process`]. `Service` reports
    /// `Ready` when the readiness condition is met, never settles while
    /// running, wakes the session with `Failed` on unsolicited death, and
    /// honors an optional [`nuo_protocol::RestartPolicy`].
    pub async fn spawn_process_ex(
        &self,
        command: String,
        opts: ProcessSpawnOptions<'_>,
        kind: nuo_protocol::JobKind,
        readiness: Option<nuo_protocol::Readiness>,
        restart: Option<nuo_protocol::RestartPolicy>,
    ) -> Result<BackgroundJobInfo, String> {
        use nuo_protocol::{JobKind, Readiness};
        if kind == JobKind::Interactive {
            return self.spawn_process(command, opts).await;
        }
        let ProcessSpawnOptions {
            label,
            cwd,
            workspace_root,
            additional_roots,
            detached,
            timeout,
            owner_session,
        } = opts;
        let _ = timeout; // service lifetime is not wall-bounded; stop is explicit

        let job_id = JobId::new("svc");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let mut cmd = nuo_host::workspace_sandbox::shell_with_roots(
            &command,
            workspace_root,
            additional_roots,
            nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
            nuo_host::workspace_sandbox::NetworkAccess::Enabled,
        )?;
        if let Some(ref dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn service task: {e}"))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd,
                detached,
                task_kind: JobKind::Service,
                readiness,
                restart,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    settled: None,
                    #[cfg(unix)]
                    pid,
                },
            );
        }
        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        let readiness = readiness.unwrap_or_default();

        tokio::spawn(async move {
            let start_time = Instant::now();
            let mut log_writer = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_file_path)
                .await
                .ok();

            let (line_tx, mut line_rx) = mpsc::channel::<String>(1024);
            if let Some(out) = stdout {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(out).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            if let Some(err) = stderr {
                let tx = line_tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(err).lines();
                    while let Ok(Some(line)) = reader.next_line().await {
                        if tx.send(line).await.is_err() {
                            break;
                        }
                    }
                });
            }
            drop(line_tx);

            // Readiness grace timer for AfterMs; armed lazily below.
            let mut grace: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = match readiness {
                Readiness::AfterMs(ms) => Some(Box::pin(tokio::time::sleep(
                    Duration::from_millis(ms.max(1)),
                ))),
                _ => None,
            };

            let mut ready = false;
            let mut unsolicited = true;
            let exit_status = loop {
                tokio::select! {
                    Some(line) = line_rx.recv() => {
                        if !ready && matches!(readiness, Readiness::FirstOutput) {
                            ready = true;
                            mgr.mark_service_ready(&jid);
                            let _ = mgr.event_tx.send(BackgroundJobEvent::Ready { job_id: jid.clone() });
                        }
                        mgr.append_line(&jid, &line);
                        if let Some(ref mut w) = log_writer {
                            let _ = w.write_all(line.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                        }
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Progress {
                            job_id: jid.clone(),
                            line,
                        });
                    }
                    _ = async {
                        // The grace timer is armed only for `AfterMs` and only
                        // while the service is not yet ready; else pend forever
                        // (the branch guard re-checks the same conditions).
                        // `grace: Pin<Box<Sleep>>` — `as_mut` yields the
                        // pinned projection awaited directly.
                        let armed = if !ready { grace.as_mut() } else { None };
                        match armed {
                            Some(g) => g.await,
                            None => std::future::pending().await,
                        }
                    }, if !ready && matches!(readiness, Readiness::AfterMs(_)) => {
                        ready = true;
                        mgr.mark_service_ready(&jid);
                        let _ = mgr.event_tx.send(BackgroundJobEvent::Ready { job_id: jid.clone() });
                    }
                    status = child.wait() => {
                        // exited on its own — `unsolicited` stays `true`
                        break status.ok();
                    }
                    _ = &mut cancel_rx => {
                        let _ = child.kill().await;
                        unsolicited = false;
                        break None;
                    }
                }
            };

            let duration_ms = start_time.elapsed().as_millis() as u64;
            let final_state = if !unsolicited {
                JobState::Killed { duration_ms }
            } else if let Some(status) = exit_status {
                let code = status
                    .code()
                    .unwrap_or(if status.success() { 0 } else { 1 });
                if status.success() {
                    JobState::Succeeded {
                        duration_ms,
                        exit_code: code,
                    }
                } else {
                    JobState::Failed {
                        duration_ms,
                        exit_code: code,
                        error: format!("Service exited with status code {code}"),
                    }
                }
            } else {
                JobState::Failed {
                    duration_ms,
                    exit_code: -1,
                    error: "Service terminated unexpectedly".to_string(),
                }
            };
            mgr.finish_job(jid, spec, final_state, Some(log_file_path));
        });

        Ok(info)
    }

    /// Spawn a Timer task: sleep until `fire_at_ms`, then emit a
    /// completed outcome whose digest is the caller's prompt — the mailbox
    /// wakes the session with it. Recurring timers re-arm with `interval_ms`
    /// after each fire (deadline = previous deadline + interval) and keep
    /// the same task id; cancellation (`kill_job`) stops the loop.
    pub fn spawn_timer(
        &self,
        label: Option<String>,
        fire_at_ms: u64,
        interval_ms: Option<u64>,
        prompt: String,
        owner_session: Option<String>,
    ) -> Result<BackgroundJobInfo, String> {
        let job_id = JobId::new("timer");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Timer {
                label,
                fire_at_ms,
                interval_ms,
                prompt: prompt.clone(),
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid: None,
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = self
                .inner
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.insert(
                job_id.clone(),
                JobEntry {
                    info: info.clone(),
                    ring_buffer: VecDeque::with_capacity(DEFAULT_RING_BUFFER_CAPACITY),
                    owner_session: owner_session.clone(),
                    cancel_tx: Some(cancel_tx),
                    settled: None,
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }
        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        tokio::spawn(async move {
            let mut deadline = fire_at_ms;
            let mut cancel = cancel_rx;
            let mut cancelled = false;
            loop {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let wait = deadline.saturating_sub(now);
                tokio::select! {
                    _ = &mut cancel => {
                        cancelled = true;
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
                }
                match interval_ms {
                    // Re-arming timer: the fire publishes its
                    // digest, and the task stays armed — and cancellable — for
                    // the next tick. Only a one-shot timer settles its entry.
                    Some(interval) => {
                        mgr.publish_timer_fire(&jid, &spec, prompt.clone());
                        deadline += interval;
                    }
                    None => {
                        mgr.finish_job_with_summary(
                            jid.clone(),
                            spec.clone(),
                            JobState::Succeeded {
                                duration_ms: 0,
                                exit_code: 0,
                            },
                            None,
                            prompt.clone(),
                        );
                        break;
                    }
                }
            }
            if cancelled {
                mgr.settle_entry_silently(&jid, JobState::Killed { duration_ms: 0 });
            }
        });

        Ok(info)
    }
}
