use super::*;
use nuo_protocol::ThreadOrigin;

fn row(id: &str, status: ThreadActivity) -> MonitoredThread {
    MonitoredThread {
        id: id.into(),
        title: Some("refactor the parser".into()),
        overview: "refactor the parser".into(),
        created_at: 1,
        updated_at: 100,
        message_count: 12,
        status,
        hosting: ThreadHosting::Hosted,
        round: 3,
        turn: Some(1),
        output_tokens: 1_240,
        elapsed_ms: 83_000,
        current_tool: Some("execute_command".into()),
        activity: Some("waiting for model".into()),
        context_tokens: Some(48_200),
        note: None,
        project_root: "/tmp/project".into(),
        role: "developer".into(),
        parent_id: None,
        origin: ThreadOrigin::default(),
        digest: None,
    }
}

fn snapshot(rows: Vec<MonitoredThread>) -> MonitorSnapshot {
    MonitorSnapshot {
        project_root: "/home/u/proj".into(),
        server_started_at: 50,
        threads: rows,
        tasks: Vec::new(),
        persistence_health: None,
    }
}

#[test]
fn table_renders_server_tasks_section() {
    let mut snap = snapshot(Vec::new());
    snap.tasks = vec![nuo_protocol::MonitoredTask {
        id: "task_abc12345".into(),
        label: "rehost:svc_old".into(),
        spec: "npm run dev".into(),
        state: nuo_protocol::JobState::Ready {
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
    assert!(text.contains("1 server task(s)"), "{text}");
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
    let text = table(&snapshot(vec![row(
        "abcdef123456",
        ThreadActivity::Running,
    )]));
    assert!(text.contains("abcdef12"), "{text}");
    assert!(text.contains("running"), "{text}");
    assert!(text.contains("3 › 1"), "{text}");
    assert!(text.contains("tool execute_command"), "{text}");
    assert!(text.contains("1m23s"), "{text}");
    assert!(text.contains("ctx 48.2k"), "{text}");
}

#[test]
fn blocked_row_shows_its_note() {
    let mut blocked = row("zz", ThreadActivity::NeedsApproval);
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
    let mut rows = vec![row("a", ThreadActivity::Running)];
    let mut newer = row("b", ThreadActivity::Idle);
    newer.updated_at = 200;
    upsert_session_row(&mut rows, newer);
    assert_eq!(rows[0].id, "b");
    let mut updated = row("b", ThreadActivity::Failed);
    updated.updated_at = 300;
    upsert_session_row(&mut rows, updated);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].status, ThreadActivity::Failed);
}

#[test]
fn elapsed_formats_progressively() {
    assert_eq!(fmt_elapsed(0), "–");
    assert_eq!(fmt_elapsed(9_000), "9s");
    assert_eq!(fmt_elapsed(83_000), "1m23s");
    assert_eq!(fmt_elapsed(3_900_000), "1h05m");
}

fn base_diag() -> ServerDiagnostics {
    ServerDiagnostics {
        instance_dir: std::path::PathBuf::from("/run/user/1000/nuo"),
        default_port: 9800,
        discovery_path: std::path::PathBuf::from("/run/user/1000/nuo/server.json"),
        discovery_record: None,
        discovery_valid: true,
        lock_path: std::path::PathBuf::from("/run/user/1000/nuo/server.lock"),
        lock_held: false,
        lock_holder_pid: None,
        lock_holder_alive: false,
        local_endpoint: Some(nuo_host::ipc::LocalEndpoint::UnixSocket(
            std::path::PathBuf::from("/run/user/1000/nuo/server.sock"),
        )),
        local_endpoint_exists: false,
        local_endpoint_connectable: false,
        tcp_port: 9800,
        tcp_listening: false,
        startup_log_path: std::path::PathBuf::from("/tmp/startup.log"),
        last_startup_log: None,
        installed_image: None,
        installed_image_digest: None,
        server_image_digest: None,
        server_image_current: true,
        recent_events: Vec::new(),
    }
}

#[test]
fn diagnostics_flag_rebuilt_binary_drift() {
    // ADR-0021: a live record whose published hash differs from the
    // installed image must be surfaced as rebuilt-binary drift, taking
    // precedence over the generic "healthy" line.
    let diag = ServerDiagnostics {
        discovery_record: Some(client::ServerInfo {
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
        server_image_digest: Some("aa".repeat(32)),
        server_image_current: false,
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
    let diag = ServerDiagnostics {
        discovery_record: Some(client::ServerInfo {
            pid: 12345,
            process_birth_token: None,
            port: 9800,
            token: None,
            project_root: String::new(),
            started_at: 1000,
            uds_path: Some(std::path::PathBuf::from("/run/user/1000/nuo/server.sock")),
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
    assert!(text.contains("Server is running and healthy"), "{text}");
    // The instance scope line leads the report.
    assert!(text.contains("Instance:"), "{text}");
    assert!(text.contains("default port 9800"), "{text}");
}

#[test]
fn diagnostics_formatter_renders_client_bound_zero_tcp_healthy_state() {
    // ADR-0033 & ADR-0045: an interactive client-bound server binds only local UDS (port 0,
    // zero TCP listeners) and must be recognized as healthy rather than unresponsive.
    let diag = ServerDiagnostics {
        discovery_record: Some(client::ServerInfo {
            pid: 12345,
            port: 0,
            started_at: 1000,
            uds_path: Some(std::path::PathBuf::from("/run/user/1000/nuo/server.sock")),
            version: Some("0.25.1".to_string()),
            ..Default::default()
        }),
        discovery_valid: true,
        lock_held: true,
        lock_holder_pid: Some(12345),
        lock_holder_alive: true,
        local_endpoint_exists: true,
        local_endpoint_connectable: true,
        tcp_port: 0,
        tcp_listening: false,
        ..base_diag()
    };
    let text = format_diagnostics(&diag);
    assert!(text.contains("PID 12345"), "{text}");
    assert!(
        text.contains("TCP: disabled (zero-attack-surface client-bound posture)"),
        "{text}"
    );
    assert!(
        text.contains("Server is running and healthy (client-bound local IPC)"),
        "{text}"
    );
}

#[test]
fn diagnostics_formatter_detects_ghost_server() {
    let diag = ServerDiagnostics {
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
    assert!(text.contains("Ghost server detected"), "{text}");
    assert!(text.contains("HELD by PID 9999"), "{text}");
    assert!(text.contains("panic: something went wrong"), "{text}");
}

#[test]
fn diagnostics_formatter_names_the_sandbox_instance() {
    // A sandboxed client must be identifiable at a glance:
    // the report's first data line names the instance dir and the
    // client-resolved default port, so "two servers, one discovered"
    // becomes a one-command diagnosis.
    let mut diag = base_diag();
    diag.instance_dir = std::path::PathBuf::from("/tmp/nuo-dev/nuo/instance");
    diag.default_port = 9801;
    let text = format_diagnostics(&diag);
    assert!(
        text.contains("/tmp/nuo-dev/nuo/instance (default port 9801)"),
        "{text}"
    );
}

#[test]
fn diagnostics_formatter_renders_runtime_events() {
    let diag = ServerDiagnostics {
        recent_events: vec![
            nuo_host::diagnostics::DiagnosticEvent {
                timestamp_ms: 1730419200000,
                pid: 913105,
                process_kind: "server".to_string(),
                event_kind: "takeover".to_string(),
                summary: "terminating existing PID 138253".to_string(),
                details: None,
            },
            nuo_host::diagnostics::DiagnosticEvent {
                timestamp_ms: 1730419201000,
                pid: 913200,
                process_kind: "client".to_string(),
                event_kind: "link_lost".to_string(),
                summary: "outbox channel broken pipe".to_string(),
                details: None,
            },
        ],
        ..base_diag()
    };
    let text = format_diagnostics(&diag);
    assert!(text.contains("Recent Runtime Diagnostics:"), "{text}");
    assert!(
        text.contains("[server:913105] takeover: terminating existing PID 138253"),
        "{text}"
    );
    assert!(
        text.contains("[client:913200] link_lost: outbox channel broken pipe"),
        "{text}"
    );
}
