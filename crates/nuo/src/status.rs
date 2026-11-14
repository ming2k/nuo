//! `nuo status`: the first control-plane client of the server
//! monitor protocol. One-shot by default (`nuo status`), a live table with
//! `--watch`, machine-readable frames with `--json`.
//!
//! Unlike `nuo attach`, status never spawns a server: observing is only
//! meaningful when a host is already running, so a missing/stale discovery
//! record is a clean "no server" report, not an excuse to start one.
//!
//! This module is presentation only: the monitor-protocol client
//! ([`client::monitor_stream`]) and the stream-folding helper
//! ([`client::upsert_session_row`]) live with the client transport
//! in `nuo-tui::client`; what remains here is the terminal rendering of the
//! snapshot.
use std::path::Path;

use nuo::transport::{self as client, ServerDiagnostics, upsert_session_row, upsert_task_row};
use nuo_protocol::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredThread, ThreadActivity, ThreadHosting,
};

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
        let diag = client::diagnose_server().await;
        render_diagnostics(&diag, opts.json);
        if opts.watch {
            return Err("cannot watch static diagnostic output".into());
        }
        if client::discover(project_root).await.is_none() {
            return Ok(());
        }
        println!();
    }

    let Some(info) = client::discover(project_root).await else {
        let diag = client::diagnose_server().await;
        render_diagnostics(&diag, opts.json);
        return Ok(());
    };
    if !client::versions_compatible(&info) {
        if opts.diagnostic {
            return Ok(());
        }
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
        None => return Err("server closed the monitor stream".into()),
    };
    render(&info, &state, opts);
    if !opts.watch {
        return Ok(());
    }
    while let Some(event) = rx.recv().await {
        match event {
            MonitorEvent::Snapshot(snapshot) => state = snapshot,
            MonitorEvent::ThreadAdded(row) | MonitorEvent::ThreadUpdated(row) => {
                upsert_session_row(&mut state.threads, row);
            }
            MonitorEvent::ThreadRemoved { thread_id } => {
                state.threads.retain(|row| row.id != thread_id);
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
            // The server is draining: the stream ends right
            // after this frame. Print a note and stop watching — the next
            // `nuo status` re-discovers (or reports none running).
            MonitorEvent::ServerDraining => {
                if !opts.json {
                    eprintln!("nuo: server is shutting down; watch ended.");
                }
                return Ok(());
            }
        }
        render(&info, &state, opts);
    }
    Ok(())
}

pub fn render_diagnostics(diag: &ServerDiagnostics, json: bool) {
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

/// The human-readable server diagnostics output.
pub(crate) fn format_diagnostics(diag: &ServerDiagnostics) -> String {
    let mut out = String::new();
    out.push_str("nuo status — system status & diagnostics:\n");

    // Instance scope first: every path below reads differently
    // once the reader knows whether this client resolves the host instance
    // or an isolated `NUO_HOME` sandbox.
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

    // Executable image identity (ADR-0021): the server's published hash
    // versus the installed image this client would spawn. This is the
    // comparison a "rebuilt binary under a live server" verdict rests on, so
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
    match &diag.server_image_digest {
        Some(sha) => out.push_str(&format!(
            "    • Server sha256:    {} ({})\n",
            short_hash(sha),
            if diag.server_image_current {
                "matches installed — current"
            } else {
                "differs from installed — REBUILT/STALE"
            }
        )),
        None => out.push_str(
            "    • Server sha256:    unpublished (pre-ADR-0021 record; inode probe in use)\n",
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

    if diag.tcp_port == 0 && diag.discovery_valid {
        out.push_str("    • TCP: disabled (zero-attack-surface client-bound posture)\n");
    } else {
        let tcp_status = if diag.tcp_listening {
            "listening"
        } else {
            "closed"
        };
        out.push_str(&format!(
            "    • TCP: ws://127.0.0.1:{} ({tcp_status})\n",
            diag.tcp_port
        ));
    }

    // Startup Log
    if let Some(last_log) = &diag.last_startup_log {
        out.push_str("  Recent Startup Log:\n");
        for line in last_log.lines().take(5) {
            out.push_str(&format!("    | {line}\n"));
        }
    }

    // Recent Runtime Diagnostics
    if !diag.recent_events.is_empty() {
        out.push_str("  Recent Runtime Diagnostics:\n");
        for ev in &diag.recent_events {
            let time_str = format_timestamp_ms(ev.timestamp_ms);
            out.push_str(&format!(
                "    | [{time_str}] [{}:{}] {}: {}\n",
                ev.process_kind, ev.pid, ev.event_kind, ev.summary
            ));
        }
    }

    // High level diagnosis
    out.push_str("  Diagnosis:        ");
    if diag.discovery_record.is_some() && !diag.server_image_current {
        // ADR-0021: the executable drifted under a live server. This takes
        // precedence over the generic "healthy" line: the server answers, but
        // it is not the binary the operator thinks they are running.
        out.push_str(
            "Rebuilt-binary drift: the running server's executable differs from the installed image.\n",
        );
        out.push_str("                    `nuo` reclaims it automatically when idle; stop it now with `nuo server stop`.\n");
    } else if diag.discovery_valid && diag.tcp_listening {
        out.push_str("Server is running and healthy (hosted standalone posture). (Observe with `nuo status --watch`)\n");
    } else if diag.discovery_valid && diag.local_endpoint_connectable {
        out.push_str("Server is running and healthy (client-bound local IPC). (Observe with `nuo status --watch`)\n");
    } else if diag.lock_held && diag.discovery_record.is_none() {
        out.push_str(
            "Ghost server detected: Instance lock is held but discovery record is missing.\n",
        );
        out.push_str("                    Run `nuo server stop` or kill the locking PID, then start with `nuo server start`.\n");
    } else if !diag.lock_held && diag.discovery_record.is_some() {
        out.push_str("Stale discovery record: Process is gone but discovery record remains.\n");
        out.push_str("                    Start a new server with `nuo server start`.\n");
    } else if !diag.lock_held {
        out.push_str("No session server is running.\n");
        out.push_str("                    Start one with `nuo server start`.\n");
    } else {
        out.push_str("Server state is transitioning or unresponsive.\n");
    }

    out
}

fn render(info: &client::ServerInfo, snapshot: &MonitorSnapshot, opts: StatusOptions) {
    if opts.json {
        let payload = serde_json::json!({
            "server": {
                "pid": info.pid,
                "port": info.port,
                "version": info.version,
                "started_at": info.started_at,
                "uds_path": info.uds_path,
                "transport": if info.port == 0 { "local_ipc_zero_tcp" } else { "tcp" },
            },
            "snapshot": snapshot,
        });
        println!(
            "{}",
            serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }
    if opts.watch {
        // Cheap in-place refresh: clear the screen and redraw. A full
        // alternate-screen TUI is overkill for a status table.
        print!("\x1b[2J\x1b[H");
    }

    let transport = if info.port == 0 {
        "Local IPC (0-TCP secure)".to_string()
    } else {
        format!("ws://127.0.0.1:{} (bearer auth)", info.port)
    };
    let uptime = format_uptime(info.started_at);
    let root = if snapshot.project_root.is_empty() {
        "all projects"
    } else {
        snapshot.project_root.as_str()
    };

    println!(
        "● Nuo [Healthy] | Server PID {} (uptime {})",
        info.pid, uptime
    );
    println!("  Transport: {transport} | Project: {root}");

    let db_path = nuo_host::paths::get().db_file();
    let db_status = if let Some(health) = &snapshot.persistence_health {
        if health.is_healthy() {
            "WAL healthy".to_string()
        } else {
            let detail = health.detail().unwrap_or_default();
            format!("Degraded: {detail}")
        }
    } else {
        "WAL healthy".to_string()
    };
    println!("  Database:  {} ({})", db_path.display(), db_status);
    println!();

    println!("{}", table(snapshot));
}

fn format_timestamp_ms(timestamp_ms: u64) -> String {
    let secs = (timestamp_ms / 1000) as i64;
    let nsecs = ((timestamp_ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nsecs)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp_ms.to_string())
}

fn format_uptime(started_at_secs: u64) -> String {
    if started_at_secs == 0 {
        return "–".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now < started_at_secs {
        return "just started".to_string();
    }
    let elapsed = now - started_at_secs;
    if elapsed < 60 {
        format!("{elapsed}s")
    } else if elapsed < 3600 {
        format!("{}m {}s", elapsed / 60, elapsed % 60)
    } else {
        format!("{}h {}m", elapsed / 3600, (elapsed % 3600) / 60)
    }
}

/// The human-readable table. Extracted (and `pub(crate)`) so tests can pin
/// the layout without a server.
pub(crate) fn table(snapshot: &MonitorSnapshot) -> String {
    let mut out = String::new();
    let root = if snapshot.project_root.is_empty() {
        "all projects"
    } else {
        snapshot.project_root.as_str()
    };
    out.push_str(&format!(
        "nuo status — {} — {} thread(s) needing attention\n",
        root,
        snapshot.threads.len()
    ));
    // Durability health (D4): visible in the operator's first line
    // of view, never log-only.
    if let Some(health) = &snapshot.persistence_health
        && !health.is_healthy()
    {
        let state = match health {
            nuo_protocol::monitor::PersistenceHealth::Recovering { .. } => "recovering",
            nuo_protocol::monitor::PersistenceHealth::Down { .. } => "down",
            nuo_protocol::monitor::PersistenceHealth::Healthy => unreachable!("filtered above"),
        };
        let detail = health.detail().unwrap_or_default();
        out.push_str(&format!(
            "  WARNING: persistence writer is {state}: {detail}\n"
        ));
    }
    if !snapshot.tasks.is_empty() {
        out.push_str(&format!("  {} server task(s):\n", snapshot.tasks.len()));
        out.push_str(&format!(
            "    {:<16} {:<12} {:<28} {}\n",
            "TASK", "STATE", "LABEL", "LATEST"
        ));
        for t in &snapshot.tasks {
            let state = match &t.state {
                nuo_protocol::JobState::Running { .. } => "running",
                nuo_protocol::JobState::Ready { .. } => "ready",
                nuo_protocol::JobState::Succeeded { .. } => "done",
                nuo_protocol::JobState::Failed { .. } => "failed",
                nuo_protocol::JobState::Killed { .. } => "killed",
                nuo_protocol::JobState::TimedOut { .. } => "timed out",
                nuo_protocol::JobState::Queued => "queued",
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
    if snapshot.threads.is_empty() {
        out.push_str("  (all quiet — no running or blocked sessions)\n");
        out.push_str("\nTip: Run `nuo thread ls` for full thread history, or `nuo status --diagnostic` for transport internals.\n");
        return out;
    }
    out.push_str(&format!(
        "  {:<10} {:<14} {:<9} {:<7} {:>6} {:<9} {}\n",
        "THREAD", "STATUS", "HOSTING", "ROUND", "OUT", "ELAPSED", "DETAIL"
    ));
    for row in &snapshot.threads {
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
    out.push_str("\nTip: Run `nuo thread ls` for full thread history, or `nuo status --diagnostic` for transport internals.\n");
    out
}

/// How the row's session is driven. Every session is
/// server-held, so this is always `hosted`; the column stays so older
/// servers' rows (which may omit `hosting`) still render.
fn hosting_cell(row: &MonitoredThread) -> String {
    match row.hosting {
        ThreadHosting::Hosted => "hosted".to_string(),
        ThreadHosting::Dormant => "dormant".to_string(),
    }
}

/// `round 3 › turn 2` while a round runs; `round 3` once it settled; `–`
/// before the first round.
fn round_turn(row: &MonitoredThread) -> String {
    match (row.round, row.turn) {
        (0, _) => "–".to_string(),
        (round, Some(turn)) => format!("{round} › {turn}"),
        (round, None) => format!("{round}"),
    }
}

fn detail(row: &MonitoredThread) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(tool) = &row.current_tool {
        parts.push(format!("tool {tool}"));
    }
    if let Some(note) = &row.note {
        parts.push(note.clone());
    } else if row.status == ThreadActivity::Running
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
    parts.join(" | ")
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
mod tests;

#[cfg(test)]
mod persistence_health_tests {
    use super::*;
    use nuo_protocol::monitor::PersistenceHealth;

    #[test]
    fn table_warns_when_the_persistence_writer_is_down() {
        let mut snap = MonitorSnapshot {
            project_root: "/home/u/proj".into(),
            server_started_at: 50,
            threads: Vec::new(),
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
