//! Shared progress-aware wait for `ServerPhase::Initializing -> Ready`
//! (ADR-0260 `[INV-SRVPHASE-03]`, ADR-0266 `[INV-SRVPHASE-07]`).
//!
//! Replaces the duplicated wait loops previously kept in `attach.rs` and
//! `control.rs`. Every incoming `MonitorEvent::ServerPhaseChanged` resets the
//! stall timer, and heavy disk I/O steps (`StorageInitDetail::*` — `VACUUM INTO`
//! backups, migrations, integrity checks) receive an extended stall window so
//! large legitimate upgrades never look like dead peers.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use futures::StreamExt;
use protocol::{MonitorEvent, ServerPhase, StorageInitDetail, Wire, WireFrame};

use super::{AttachError, BoxWireStream};

/// Stall deadline when initializing without heavy disk I/O.
pub(super) const INIT_NORMAL_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Extended stall deadline while `StorageInitDetail` indicates heavy disk I/O
/// (`BackingUp`, `Migrating`, `Verifying`).
pub(super) const INIT_HEAVY_IO_STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Absolute ceiling across all initialization steps.
pub(super) const INIT_HARD_CAP: Duration = Duration::from_secs(300);
/// Delay before printing CLI stderr progress so fast warm boots stay silent.
const CLI_PROGRESS_THRESHOLD: Duration = Duration::from_millis(300);

/// Concise human-readable description of a `ServerPhase::Initializing` state.
pub fn format_init_phase(phase: &ServerPhase) -> Option<String> {
    let ServerPhase::Initializing { stage, detail, .. } = phase else {
        return None;
    };
    Some(match detail {
        Some(StorageInitDetail::BackingUp { target_version }) => {
            format!("backing up database before v{target_version}")
        }
        Some(StorageInitDetail::Migrating { step, total, name }) => {
            format!("migrating database ({step}/{total} · {name})")
        }
        Some(StorageInitDetail::Verifying) => "verifying database integrity".to_string(),
        None => stage.as_str().to_string(),
    })
}

fn stall_budget_for(phase: &ServerPhase) -> Duration {
    match phase {
        ServerPhase::Initializing {
            detail: Some(_), ..
        } => INIT_HEAVY_IO_STALL_TIMEOUT,
        _ => INIT_NORMAL_STALL_TIMEOUT,
    }
}

/// Wait on `wire_source` until `phase` leaves `ServerPhase::Initializing`,
/// buffering any non-terminal `MonitorEvent`s in `buffered_monitor`.
///
/// Resets its stall timer on every `ServerPhaseChanged` frame and prints
/// single-line progress to `stderr` if initialization exceeds 300 ms.
pub(super) async fn await_server_ready(
    wire_source: &mut BoxWireStream,
    initial_phase: ServerPhase,
    buffered_monitor: Option<&mut Vec<MonitorEvent>>,
    hard_cap: Duration,
) -> Result<ServerPhase, AttachError> {
    await_server_ready_with(
        wire_source,
        initial_phase,
        buffered_monitor,
        hard_cap,
        |phase| stall_budget_for(phase).min(hard_cap),
        true,
    )
    .await
}

struct CliProgressPrinter {
    enabled: bool,
    is_tty: bool,
    started: Instant,
    printed: bool,
    last_printed: Option<String>,
}

impl CliProgressPrinter {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            is_tty: std::io::stderr().is_terminal(),
            started: Instant::now(),
            printed: false,
            last_printed: None,
        }
    }

    fn next_reveal_deadline(&self) -> Option<tokio::time::Instant> {
        if self.enabled && !self.printed {
            let remaining = CLI_PROGRESS_THRESHOLD.saturating_sub(self.started.elapsed());
            Some(tokio::time::Instant::now() + remaining)
        } else {
            None
        }
    }

    fn report(&mut self, phase: &ServerPhase, force: bool) {
        if !self.enabled {
            return;
        }
        if !force && self.started.elapsed() < CLI_PROGRESS_THRESHOLD {
            return;
        }
        let Some(desc) = format_init_phase(phase) else {
            return;
        };
        if self.last_printed.as_deref() == Some(desc.as_str()) {
            return;
        }
        self.last_printed = Some(desc.clone());
        self.printed = true;
        let mut err = std::io::stderr();
        if self.is_tty {
            let _ = write!(err, "\r\x1b[2Knuo: initializing server ({desc})…");
            let _ = err.flush();
        } else {
            let _ = writeln!(err, "nuo: initializing server ({desc})…");
        }
    }

    fn finish(self) {
        if self.printed && self.is_tty {
            let mut err = std::io::stderr();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }
}

pub(super) async fn await_server_ready_with<F>(
    wire_source: &mut BoxWireStream,
    mut current_phase: ServerPhase,
    mut buffered_monitor: Option<&mut Vec<MonitorEvent>>,
    hard_cap: Duration,
    stall_for: F,
    emit_stderr_progress: bool,
) -> Result<ServerPhase, AttachError>
where
    F: Fn(&ServerPhase) -> Duration,
{
    if !current_phase.is_initializing() {
        return Ok(current_phase);
    }

    let hard_deadline = tokio::time::Instant::now() + hard_cap;
    let mut printer = CliProgressPrinter::new(emit_stderr_progress);

    let result = loop {
        let stall_deadline = tokio::time::Instant::now() + stall_for(&current_phase);
        let step_deadline = stall_deadline.min(hard_deadline);
        let progress_tick = printer.next_reveal_deadline();

        tokio::select! {
            _ = tokio::time::sleep_until(step_deadline) => {
                let desc = format_init_phase(&current_phase).unwrap_or_else(|| "initializing".into());
                break Err(AttachError::transport(format!(
                    "timed out waiting for server initialization ({desc})"
                )));
            }
            _ = async {
                if let Some(at) = progress_tick {
                    tokio::time::sleep_until(at).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                printer.report(&current_phase, true);
            }
            frame = wire_source.next() => {
                match frame {
                    Some(Ok(WireFrame::Wire(Wire::Monitor { event }))) => {
                        if let MonitorEvent::ServerPhaseChanged(ref next) = event {
                            current_phase = next.clone();
                            if let Some(buf) = buffered_monitor.as_deref_mut() {
                                buf.push(event);
                            }
                            if !current_phase.is_initializing() {
                                break Ok(current_phase);
                            }
                            printer.report(&current_phase, false);
                        } else if let Some(buf) = buffered_monitor.as_deref_mut() {
                            buf.push(event);
                        }
                    }
                    Some(Ok(WireFrame::Wire(Wire::Error { message, code, .. }))) => {
                        break Err(AttachError::server_refusal(code, message));
                    }
                    Some(Ok(WireFrame::Undecodable { envelope, detail })) => {
                        break Err(AttachError::transport(format!(
                            "server sent a frame this client cannot decode{}: {detail}",
                            envelope
                                .map(|tag| format!(" (envelope `{tag}`)"))
                                .unwrap_or_default()
                        )));
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => {
                        break Err(AttachError::transport(format!(
                            "wire recv while waiting for server ready: {error}"
                        )));
                    }
                    None => {
                        break Err(AttachError::transport(
                            "server closed connection while initializing",
                        ));
                    }
                }
            }
        }
    };

    printer.finish();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use protocol::ServerInitStage;

    #[tokio::test]
    async fn progress_events_reset_stall_timer_and_reach_ready() {
        // Stall budget per step is 80ms, total elapsed across 3 steps is ~150ms:
        // would time out without progress-reset, succeeds with progress-reset.
        let s = stream::unfold(0u8, |step| async move {
            tokio::time::sleep(Duration::from_millis(45)).await;
            let phase = match step {
                0 => ServerPhase::initializing_with(
                    ServerInitStage::Storage,
                    Some(StorageInitDetail::BackingUp { target_version: 9 }),
                    None,
                ),
                1 => ServerPhase::initializing_with(
                    ServerInitStage::Storage,
                    Some(StorageInitDetail::Migrating {
                        step: 1,
                        total: 1,
                        name: "0009_pure_catalog".into(),
                    }),
                    None,
                ),
                2 => ServerPhase::Ready,
                _ => return None,
            };
            let frame = Ok(WireFrame::Wire(Wire::Monitor {
                event: MonitorEvent::ServerPhaseChanged(phase),
            }));
            Some((frame, step + 1))
        });
        let mut wire_source: BoxWireStream = Box::pin(s);
        let mut buffered = Vec::new();
        let result = await_server_ready_with(
            &mut wire_source,
            ServerPhase::initializing(ServerInitStage::Storage),
            Some(&mut buffered),
            Duration::from_secs(2),
            |_| Duration::from_millis(80),
            false,
        )
        .await
        .expect("progress updates must reset stall timer");
        assert_eq!(result, ServerPhase::Ready);
        assert_eq!(buffered.len(), 3);
    }

    #[tokio::test]
    async fn stall_timeout_includes_active_migration_detail() {
        let mut wire_source: BoxWireStream = Box::pin(stream::pending());
        let initial = ServerPhase::initializing_with(
            ServerInitStage::Storage,
            Some(StorageInitDetail::BackingUp { target_version: 9 }),
            None,
        );
        let err = await_server_ready_with(
            &mut wire_source,
            initial,
            None,
            Duration::from_secs(1),
            |_| Duration::from_millis(30),
            false,
        )
        .await
        .expect_err("stalled migration must time out");
        assert!(
            err.message.contains("backing up database before v9"),
            "unexpected timeout message: {}",
            err.message
        );
    }
}
