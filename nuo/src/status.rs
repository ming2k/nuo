//! `nuo status` (ADR-0093): the first control-plane client of the daemon
//! monitor protocol. One-shot by default (`nuo status`), a live table with
//! `--watch`, machine-readable frames with `--json`.
//!
//! Unlike `nuo attach`, status never spawns a daemon: observing is only
//! meaningful when a host is already running, so a missing/stale discovery
//! record is a clean "no daemon" report, not an excuse to start one.
//!
//! This module is presentation only: the monitor-protocol client
//! ([`nuo_client::monitor_stream`]) and the stream-folding helper
//! ([`nuo_client::upsert_session_row`]) live with the wire protocol
//! in `nuo-client`; what remains here is the terminal rendering of the
//! snapshot.

use std::path::Path;

use nuo_wire::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession, SessionHosting, SessionStatus,
};
use nuo_client::{self as client, DaemonDiagnostics, upsert_session_row, upsert_task_row};

/// How `nuo status` renders its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusOptions {
    pub watch: bool,
    pub json: bool,
    pub include_idle: bool,
    pub diagnostic: bool,
}

pub async fn run(
    project_root: &Path,
    opts: StatusOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    if opts.diagnostic {
        let diag = client::diagnose_daemon();
        render_diagnostics(&diag, opts.json);
        if opts.watch {
            return Err("cannot watch static diagnostic output".into());
        }
        if client::discover(project_root).is_none() {
            return Ok(());
        }
        println!();
    }

    let Some(info) = client::discover(project_root) else {
        let diag = client::diagnose_daemon();
        render_diagnostics(&diag, opts.json);
        return Ok(());
    };
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info).into());
    }
    let action = MonitorAction {
        watch: opts.watch,
        include_idle: opts.include_idle,
    };
    let mut rx = client::monitor_stream(&info, action).await?;
    // The first frame is always the snapshot; from then on the stream is
    // maintained client-side by folding diffs, so `--watch` renders one
    // coherent table instead of a raw event log.
    let mut state = match rx.recv().await {
        Some(MonitorEvent::Snapshot(snapshot)) => snapshot,
        Some(_) => return Err("monitor stream opened without a snapshot".into()),
        None => return Err("daemon closed the monitor stream".into()),
    };
    render(&state, opts);
    if !opts.watch {
        return Ok(());
    }
    while let Some(event) = rx.recv().await {
        match event {
            MonitorEvent::Snapshot(snapshot) => state = snapshot,
            MonitorEvent::SessionAdded(row) | MonitorEvent::SessionUpdated(row) => {
                upsert_session_row(&mut state.sessions, row);
            }
            MonitorEvent::SessionRemoved { session_id } => {
                state.sessions.retain(|row| row.id != session_id);
            }
            MonitorEvent::TaskUpdated(task) => {
                upsert_task_row(&mut state.tasks, task);
            }
            MonitorEvent::TaskRemoved { task_id } => {
                state.tasks.retain(|row| row.id != task_id);
            }
            MonitorEvent::PersistenceHealth(health) => {
                state.persistence_health = Some(health);
            }
            // The daemon is draining (ADR-0101): the stream ends right
            // after this frame. Print a note and stop watching — the next
            // `nuo status` re-discovers (or reports none running).
            MonitorEvent::DaemonDraining => {
                if !opts.json {
                    eprintln!("muta: daemon is shutting down; watch ended.");
                }
                return Ok(());
            }
        }
        render(&state, opts);
    }
    Ok(())
}

pub fn render_diagnostics(diag: &DaemonDiagnostics, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(diag).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print!("{}", format_diagnostics(diag));
    }
}

/// Shorten a 64-char SHA-256 hex digest to its first 12 chars for display.
fn short_hash(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// The human-readable daemon diagnostics output.
pub(crate) fn format_diagnostics(diag: &DaemonDiagnostics) -> String {
    let mut out = String::new();
    out.push_str("nuo status — system status & diagnostics:\n");

    // Instance scope first (ADR-0121): every path below reads differently
    // once the reader knows whether this client resolves the host instance
    // or an isolated `MUTA_HOME` sandbox.
    out.push_str(&format!(
        "  Instance:          {} (default port {})\n",
        diag.instance_dir.display(),
        diag.default_port
    ));

    // Discovery record
    out.push_str("  Discovery Record: ");
    match &diag.discovery_record {
        Some(rec) => {
            let ver = rec.version.as_deref().unwrap_or("unknown");
            let alive_tag = if client::is_process_alive(rec.pid) {
                "alive"
            } else {
                "dead/stale"
            };
            out.push_str(&format!(
                "present (PID {}, {}, v{}, port {})\n",
                rec.pid, alive_tag, ver, rec.port
            ));
            out.push_str(&format!("    • Path: {}\n", diag.discovery_path.display()));
        }
        None => {
            out.push_str(&format!("missing ({})\n", diag.discovery_path.display()));
        }
    }

    // Executable image identity (ADR-0021): the daemon's published hash
    // versus the installed image this client would spawn. This is the
    // comparison a "rebuilt binary under a live daemon" verdict rests on, so
    // it is rendered as first-class evidence rather than left to inference.
    out.push_str("  Core Image:       ");
    match &diag.installed_image {
        Some(path) => {
            out.push_str(&format!("{}\n", path.display()));
            let sha = diag
                .installed_image_digest
                .as_deref()
                .map(short_hash)
                .unwrap_or_else(|| "unreadable".to_string());
            out.push_str(&format!("    • Installed sha256: {sha}\n"));
        }
        None => out.push_str("installed image not found\n"),
    }
    match &diag.daemon_image_digest {
        Some(sha) => out.push_str(&format!(
            "    • Daemon sha256:    {} ({})\n",
            short_hash(sha),
            if diag.daemon_image_current {
                "matches installed — current"
            } else {
                "differs from installed — REBUILT/STALE"
            }
        )),
        None => out.push_str(
            "    • Daemon sha256:    unpublished (pre-ADR-0021 record; inode probe in use)\n",
        ),
    }

    // Instance Lock
    out.push_str("  Instance Lock:    ");
    if diag.lock_held {
        if let Some(pid) = diag.lock_holder_pid {
            let alive_tag = if diag.lock_holder_alive {
                "alive"
            } else {
                "dead"
            };
            out.push_str(&format!("HELD by PID {pid} (process {alive_tag})\n"));
        } else {
            out.push_str("HELD by another process\n");
        }
        out.push_str(&format!("    • Path: {}\n", diag.lock_path.display()));
    } else {
        out.push_str(&format!("free ({})\n", diag.lock_path.display()));
    }

    // Endpoints
    out.push_str("  Control Endpoints:\n");
    if let Some(endpoint) = &diag.local_endpoint {
        let local_status = if diag.local_endpoint_connectable {
            "active (connectable)"
        } else if diag.local_endpoint_exists {
            "unresponsive"
        } else {
            "not created"
        };
        out.push_str(&format!("    • Local: {endpoint} ({local_status})\n"));
    } else {
        out.push_str("    • Local: unavailable (endpoint resolution failed)\n");
    }

    let tcp_status = if diag.tcp_listening {
        "listening"
    } else {
        "closed"
    };
    out.push_str(&format!(
        "    • TCP: ws://127.0.0.1:{} ({tcp_status})\n",
        diag.tcp_port
    ));

    // Startup Log
    if let Some(last_log) = &diag.last_startup_log {
        out.push_str("  Recent Startup Log:\n");
        for line in last_log.lines().take(5) {
            out.push_str(&format!("    | {line}\n"));
        }
    }

    // High level diagnosis
    out.push_str("  Diagnosis:        ");
    if diag.discovery_record.is_some() && !diag.daemon_image_current {
        // ADR-0021: the executable drifted under a live daemon. This takes
        // precedence over the generic "healthy" line: the daemon answers, but
        // it is not the binary the operator thinks they are running.
        out.push_str(
            "Rebuilt-binary drift: the running daemon's executable differs from the installed image.\n",
        );
        out.push_str("                    `nuo` reclaims it automatically when idle; stop it now with `nuo stop`.\n");
    } else if diag.discovery_valid && diag.tcp_listening {
        out.push_str("Daemon is running and healthy. (Observe with `muta status --watch`)\n");
    } else if diag.lock_held && diag.discovery_record.is_none() {
        out.push_str(
            "Ghost daemon detected: Instance lock is held but discovery record is missing.\n",
        );
        out.push_str("                    Run `nuo stop` or kill the locking PID, then start with `nuo start`.\n");
    } else if !diag.lock_held && diag.discovery_record.is_some() {
        out.push_str("Stale discovery record: Process is gone but discovery record remains.\n");
        out.push_str("                    Start a new daemon with `nuo start`.\n");
    } else if !diag.lock_held {
        out.push_str("No session daemon is running.\n");
        out.push_str("                    Start one with `nuo start`.\n");
    } else {
        out.push_str("Daemon state is transitioning or unresponsive.\n");
    }

    out
}

fn render(snapshot: &MonitorSnapshot, opts: StatusOptions) {
    if opts.json {
        println!(
            "{}",
            serde_json::to_string(&MonitorEvent::Snapshot(snapshot.clone()))
                .unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }
    if opts.watch {
        // Cheap in-place refresh: clear the screen and redraw. A full
        // alternate-screen TUI is overkill for a status table.
        print!("\x1b[2J\x1b[H");
    }
    println!("{}", table(snapshot));
}

/// The human-readable table. Extracted (and `pub(crate)`) so tests can pin
/// the layout without a daemon.
pub(crate) fn table(snapshot: &MonitorSnapshot) -> String {
    let mut out = String::new();
    let root = if snapshot.project_root.is_empty() {
        "all projects"
    } else {
        snapshot.project_root.as_str()
    };
    out.push_str(&format!(
        "nuo status — {} — {} session(s) needing attention\n",
        root,
        snapshot.sessions.len()
    ));
    // Durability health (ADR-0196 D4): visible in the operator's first line
    // of view, never log-only.
    if let Some(health) = &snapshot.persistence_health
        && !health.is_healthy()
    {
        let state = match health {
            nuo_wire::monitor::PersistenceHealth::Recovering { .. } => "recovering",
            nuo_wire::monitor::PersistenceHealth::Down { .. } => "down",
            nuo_wire::monitor::PersistenceHealth::Healthy => unreachable!("filtered above"),
        };
        let detail = health.detail().unwrap_or_default();
        out.push_str(&format!(
            "  WARNING: persistence writer is {state}: {detail}\n"
        ));
    }
    if !snapshot.tasks.is_empty() {
        out.push_str(&format!("  {} daemon task(s):\n", snapshot.tasks.len()));
        out.push_str(&format!(
            "    {:<16} {:<12} {:<28} {}\n",
            "TASK", "STATE", "LABEL", "LATEST"
        ));
        for t in &snapshot.tasks {
            let state = match &t.state {
                nuo_wire::JobState::Running { .. } => "running",
                nuo_wire::JobState::Ready { .. } => "ready",
                nuo_wire::JobState::Succeeded { .. } => "done",
                nuo_wire::JobState::Failed { .. } => "failed",
                nuo_wire::JobState::Killed { .. } => "killed",
                nuo_wire::JobState::TimedOut { .. } => "timed out",
                nuo_wire::JobState::Queued => "queued",
            };
            let latest = t.latest_output.as_deref().unwrap_or("");
            let latest = if latest.chars().count() > 30 {
                format!("{}…", latest.chars().take(29).collect::<String>())
            } else {
                latest.to_string()
            };
            out.push_str(&format!(
                "    {:<16} {:<12} {:<28} {}\n",
                short_id(&t.id),
                state,
                truncate_chars(&t.label, 28),
                latest
            ));
        }
    }
    if snapshot.sessions.is_empty() {
        out.push_str("  (all quiet — no running or blocked sessions)\n");
        return out;
    }
    out.push_str(&format!(
        "  {:<10} {:<14} {:<9} {:<7} {:>6} {:<9} {}\n",
        "SESSION", "STATUS", "HOSTING", "ROUND", "OUT", "ELAPSED", "DETAIL"
    ));
    for row in &snapshot.sessions {
        out.push_str(&format!(
            "  {:<10} {:<14} {:<9} {:<7} {:>6} {:<9} {}\n",
            short_id(&row.id),
            row.status.as_str(),
            hosting_cell(row),
            round_turn(row),
            row.output_tokens,
            fmt_elapsed(row.elapsed_ms),
            detail(row),
        ));
    }
    out
}

/// How the row's session is driven. Since ADR-0096 every session is
/// daemon-held, so this is always `hosted`; the column stays so older
/// daemons' rows (which may omit `hosting`) still render.
fn hosting_cell(row: &MonitoredSession) -> String {
    match row.hosting {
        SessionHosting::Hosted => "hosted".to_string(),
    }
}

/// `round 3 › turn 2` while a round runs; `round 3` once it settled; `–`
/// before the first round.
fn round_turn(row: &MonitoredSession) -> String {
    match (row.round, row.turn) {
        (0, _) => "–".to_string(),
        (round, Some(turn)) => format!("{round} › {turn}"),
        (round, None) => format!("{round}"),
    }
}

fn detail(row: &MonitoredSession) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(tool) = &row.current_tool {
        parts.push(format!("tool {tool}"));
    }
    if let Some(note) = &row.note {
        parts.push(note.clone());
    } else if row.status == SessionStatus::Running
        && let Some(activity) = &row.activity
    {
        parts.push(activity.clone());
    }
    if let Some(tokens) = row.context_tokens {
        parts.push(format!("ctx {}", fmt_k(tokens)));
    }
    if parts.is_empty() {
        parts.push(truncate(&row.overview, 60));
    }
    parts.join(" · ")
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn fmt_elapsed(ms: u64) -> String {
    if ms == 0 {
        return "–".to_string();
    }
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn fmt_k(tokens: usize) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_wire::SessionForkKind;

    fn row(id: &str, status: SessionStatus) -> MonitoredSession {
        MonitoredSession {
            id: id.into(),
            overview: "refactor the parser".into(),
            created_at: 1,
            updated_at: 100,
            message_count: 12,
            status,
            hosting: SessionHosting::Hosted,
            round: 3,
            turn: Some(1),
            output_tokens: 1_240,
            elapsed_ms: 83_000,
            current_tool: Some("execute_command".into()),
            activity: Some("waiting for model".into()),
            context_tokens: Some(48_200),
            note: None,
            project_root: "/tmp/project".into(),
            parent_id: None,
            fork_kind: SessionForkKind::default(),
            digest: None,
        }
    }

    fn snapshot(rows: Vec<MonitoredSession>) -> MonitorSnapshot {
        MonitorSnapshot {
            project_root: "/home/u/proj".into(),
            daemon_started_at: 50,
            sessions: rows,
            tasks: Vec::new(),
            persistence_health: None,
        }
    }

    #[test]
    fn table_renders_daemon_tasks_section() {
        let mut snap = snapshot(Vec::new());
        snap.tasks = vec![nuo_wire::MonitoredTask {
            id: "task_abc12345".into(),
            label: "rehost:svc_old".into(),
            spec: "npm run dev".into(),
            state: nuo_wire::JobState::Ready {
                started_at_ms: 0,
                ready_at_ms: 0,
            },
            owner_session: None,
            created_at_ms: 100,
            completed_at_ms: None,
            latest_output: Some("listening on :3000".into()),
            log_path: None,
        }];
        let text = table(&snap);
        assert!(text.contains("1 daemon task(s)"), "{text}");
        assert!(text.contains("ready"), "{text}");
        assert!(text.contains("rehost:svc_old"), "{text}");
        assert!(text.contains("listening on :3000"), "{text}");
    }

    #[test]
    fn empty_snapshot_reports_all_quiet() {
        let text = table(&snapshot(Vec::new()));
        assert!(text.contains("all quiet"), "{text}");
        assert!(text.contains("/home/u/proj"), "{text}");
    }

    #[test]
    fn table_renders_status_round_and_detail() {
        let text = table(&snapshot(vec![row("abcdef123456", SessionStatus::Running)]));
        assert!(text.contains("abcdef12"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("3 › 1"), "{text}");
        assert!(text.contains("tool execute_command"), "{text}");
        assert!(text.contains("1m23s"), "{text}");
        assert!(text.contains("ctx 48.2k"), "{text}");
    }

    #[test]
    fn blocked_row_shows_its_note() {
        let mut blocked = row("zz", SessionStatus::NeedsApproval);
        blocked.current_tool = None;
        blocked.note = Some("permission: write_file".into());
        let text = table(&snapshot(vec![blocked]));
        assert!(text.contains("needs-approval"), "{text}");
        assert!(text.contains("permission: write_file"), "{text}");
        // The note wins over the raw activity string for blocked rows.
        assert!(!text.contains("waiting for model"), "{text}");
    }

    #[test]
    fn upsert_replaces_in_place_and_sorts_by_recency() {
        let mut rows = vec![row("a", SessionStatus::Running)];
        let mut newer = row("b", SessionStatus::Idle);
        newer.updated_at = 200;
        upsert_session_row(&mut rows, newer);
        assert_eq!(rows[0].id, "b");
        let mut updated = row("b", SessionStatus::Failed);
        updated.updated_at = 300;
        upsert_session_row(&mut rows, updated);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, SessionStatus::Failed);
    }

    #[test]
    fn elapsed_formats_progressively() {
        assert_eq!(fmt_elapsed(0), "–");
        assert_eq!(fmt_elapsed(9_000), "9s");
        assert_eq!(fmt_elapsed(83_000), "1m23s");
        assert_eq!(fmt_elapsed(3_900_000), "1h05m");
    }

    fn base_diag() -> DaemonDiagnostics {
        DaemonDiagnostics {
            instance_dir: std::path::PathBuf::from("/run/user/1000/muta"),
            default_port: 9800,
            discovery_path: std::path::PathBuf::from("/run/user/1000/muta/daemon.json"),
            discovery_record: None,
            discovery_valid: true,
            lock_path: std::path::PathBuf::from("/run/user/1000/muta/daemon.lock"),
            lock_held: false,
            lock_holder_pid: None,
            lock_holder_alive: false,
            local_endpoint: Some(nuo_host::ipc::LocalEndpoint::UnixSocket(
                std::path::PathBuf::from("/run/user/1000/muta/daemon.sock"),
            )),
            local_endpoint_exists: false,
            local_endpoint_connectable: false,
            tcp_port: 9800,
            tcp_listening: false,
            startup_log_path: std::path::PathBuf::from("/tmp/startup.log"),
            last_startup_log: None,
            installed_image: None,
            installed_image_digest: None,
            daemon_image_digest: None,
            daemon_image_current: true,
        }
    }

    #[test]
    fn diagnostics_flag_rebuilt_binary_drift() {
        // ADR-0021: a live record whose published hash differs from the
        // installed image must be surfaced as rebuilt-binary drift, taking
        // precedence over the generic "healthy" line.
        let diag = DaemonDiagnostics {
            discovery_record: Some(nuo_client::DaemonInfo {
                pid: 12345,
                version: Some("0.25.1".to_string()),
                protocol: Some(1),
                image_digest: Some("aa".repeat(32)),
                ..Default::default()
            }),
            discovery_valid: true,
            tcp_listening: true,
            lock_held: true,
            lock_holder_pid: Some(12345),
            lock_holder_alive: true,
            installed_image: Some(std::path::PathBuf::from("/usr/bin/nuo")),
            installed_image_digest: Some("bb".repeat(32)),
            daemon_image_digest: Some("aa".repeat(32)),
            daemon_image_current: false,
            ..base_diag()
        };
        let text = format_diagnostics(&diag);
        assert!(text.contains("Rebuilt-binary drift"), "{text}");
        assert!(text.contains("Core Image:"), "{text}");
        assert!(text.contains("REBUILT/STALE"), "{text}");
        assert!(!text.contains("running and healthy"), "{text}");
    }

    #[test]
    fn diagnostics_formatter_renders_healthy_state() {
        let diag = DaemonDiagnostics {
            discovery_record: Some(nuo_client::DaemonInfo {
                pid: 12345,
                process_birth_token: None,
                port: 9800,
                token: None,
                project_root: String::new(),
                started_at: 1000,
                uds_path: Some(std::path::PathBuf::from("/run/user/1000/muta/daemon.sock")),
                local_endpoint: None,
                version: Some("0.25.1".to_string()),
                grace_secs: Some(10),
                protocol: None,
                ..Default::default()
            }),
            discovery_valid: true,
            lock_held: true,
            lock_holder_pid: Some(12345),
            lock_holder_alive: true,
            local_endpoint_exists: true,
            local_endpoint_connectable: true,
            tcp_listening: true,
            ..base_diag()
        };
        let text = format_diagnostics(&diag);
        assert!(text.contains("PID 12345"), "{text}");
        assert!(text.contains("HELD by PID 12345"), "{text}");
        assert!(text.contains("Daemon is running and healthy"), "{text}");
        // The instance scope line leads the report (ADR-0121).
        assert!(text.contains("Instance:"), "{text}");
        assert!(text.contains("default port 9800"), "{text}");
    }

    #[test]
    fn diagnostics_formatter_detects_ghost_daemon() {
        let diag = DaemonDiagnostics {
            discovery_valid: false,
            lock_held: true,
            lock_holder_pid: Some(9999),
            lock_holder_alive: true,
            local_endpoint_exists: true,
            local_endpoint_connectable: false,
            tcp_listening: false,
            last_startup_log: Some("panic: something went wrong".to_string()),
            ..base_diag()
        };
        let text = format_diagnostics(&diag);
        assert!(text.contains("Ghost daemon detected"), "{text}");
        assert!(text.contains("HELD by PID 9999"), "{text}");
        assert!(text.contains("panic: something went wrong"), "{text}");
    }

    #[test]
    fn diagnostics_formatter_names_the_sandbox_instance() {
        // A sandboxed client (ADR-0121) must be identifiable at a glance:
        // the report's first data line names the instance dir and the
        // client-resolved default port, so "two daemons, one discovered"
        // becomes a one-command diagnosis.
        let mut diag = base_diag();
        diag.instance_dir = std::path::PathBuf::from("/tmp/muta-dev/muta/instance");
        diag.default_port = 9801;
        let text = format_diagnostics(&diag);
        assert!(
            text.contains("/tmp/muta-dev/muta/instance (default port 9801)"),
            "{text}"
        );
    }
}

#[cfg(test)]
mod persistence_health_tests {
    use super::*;
    use nuo_wire::monitor::PersistenceHealth;

    #[test]
    fn table_warns_when_the_persistence_writer_is_down() {
        let mut snap = MonitorSnapshot {
            project_root: "/home/u/proj".into(),
            daemon_started_at: 50,
            sessions: Vec::new(),
            tasks: Vec::new(),
            persistence_health: Some(PersistenceHealth::Down {
                attempt: 6,
                since_ms: 1_000,
                error: "engine open failed: database is locked".into(),
            }),
        };
        let rendered = table(&snap);
        assert!(
            rendered.contains("persistence writer is down"),
            "degraded durability must surface in `nuo status`: {rendered:?}"
        );
        assert!(rendered.contains("database is locked"), "cause must render");

        // Healthy (and absence) render no warning line.
        snap.persistence_health = Some(PersistenceHealth::Healthy);
        assert!(!table(&snap).contains("WARNING: persistence"));
        snap.persistence_health = None;
        assert!(!table(&snap).contains("WARNING: persistence"));
    }
}
