//! External process adoption and service readiness.

use super::*;

impl BackgroundJobManager {
    pub(super) fn mark_service_ready(&self, job_id: &JobId) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(job_id)
            && let JobState::Running { started_at_ms, .. } = entry.info.state
        {
            entry.info.state = JobState::Ready {
                started_at_ms,
                ready_at_ms: now_ms,
            };
        }
    }

    /// Adopt an already-running foreground child that hit its sync budget
    /// (detach-on-budget). The fabric takes ownership of the child,
    /// replays the captured foreground tail as progress, and settles with a
    /// normal outcome when the process exits — the caller's session is woken
    /// through the ordinary completion event.
    pub async fn adopt_process(
        &self,
        command: String,
        label: Option<String>,
        adoption: nuo_protocol::AdoptionInfo,
        owner_session: Option<String>,
    ) -> Result<BackgroundJobInfo, String> {
        let job_id = JobId::new("adopted");
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let log_file_path = self.log_dir.join(format!("{}.log", job_id.0));

        let info = BackgroundJobInfo {
            id: job_id.clone(),
            spec: JobSpec::Process {
                command: command.clone(),
                label: label.clone(),
                cwd: None,
                detached: true,
                task_kind: JobKind::Interactive,
                readiness: None,
                restart: None,
            },
            state: JobState::Running {
                started_at_ms: now_ms,
                pid: Some(adoption.pid),
            },
            created_at_ms: now_ms,
            completed_at_ms: None,
            latest_output: None,
        };

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
                    // An adopted child belongs to the session that
                    // detached it — losing the owner here would make the later
                    // settle undeliverable and unrevocable.
                    owner_session: owner_session.clone(),
                    cancel_tx: None,
                    settled: None,
                    #[cfg(unix)]
                    pid: Some(adoption.pid),
                },
            );
        }

        for line in &adoption.captured_lines {
            self.append_line(&job_id, line);
        }
        let mut log_writer = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_file_path)
            .await
            .ok();
        for line in &adoption.captured_lines {
            if let Some(ref mut w) = log_writer {
                let _ = w.write_all(line.as_bytes()).await;
                let _ = w.write_all(b"\n").await;
            }
        }

        let _ = self
            .event_tx
            .send(BackgroundJobEvent::Started(info.clone()));

        let mgr = self.clone();
        let jid = job_id.clone();
        let spec = info.spec.clone();
        let mut poll_child = adoption.child;

        tokio::spawn(async move {
            let start_time = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(200)).await;
                match poll_child.try_wait() {
                    Ok(None) => {}
                    Ok(Some(code)) => {
                        let duration_ms = start_time.elapsed().as_millis() as u64;
                        let summary = String::new();
                        let _ = summary;
                        let state = if code == 0 {
                            JobState::Succeeded {
                                duration_ms,
                                exit_code: 0,
                            }
                        } else {
                            JobState::Failed {
                                duration_ms,
                                exit_code: code,
                                error: format!("Process exited with status code {code}"),
                            }
                        };
                        mgr.finish_job(jid, spec, state, Some(log_file_path.clone()));
                        break;
                    }
                    Err(_) => {
                        let duration_ms = start_time.elapsed().as_millis() as u64;
                        mgr.finish_job(
                            jid,
                            spec,
                            JobState::Failed {
                                duration_ms,
                                exit_code: -1,
                                error: "adopted process could not be polled".to_string(),
                            },
                            Some(log_file_path.clone()),
                        );
                        break;
                    }
                }
            }
        });

        Ok(info)
    }
}
