//! End-to-end server spawn coverage owned by the CLI package so Cargo supplies
//! the exact freshly-built `nuo` binary through `CARGO_BIN_EXE_nuo`.
//! This must never discover an incidental or stale `target/debug/nuo`.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct ServerCleanup {
    cli: PathBuf,
    root: PathBuf,
    pid: u32,
    reaper: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
}

impl ServerCleanup {
    fn stop(&mut self) -> Output {
        let output = Command::new(&self.cli)
            .args(["stop"])
            .env("NUO_HOME", &self.root)
            .stdin(Stdio::null())
            .output()
            .expect("run server stop in the sandbox");
        if !output.status.success() {
            self.kill_process_group();
        }
        self.join_reaper();
        output
    }

    fn kill_process_group(&self) {
        // SAFETY: this test spawned `pid` through the production `setsid`
        // helper, so `-pid` targets only the sandboxed server's process group.
        let _ = unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
    }

    fn join_reaper(&mut self) {
        if let Some(reaper) = self.reaper.take() {
            reaper
                .join()
                .expect("reap thread did not panic")
                .expect("wait for sandboxed server");
        }
    }
}

impl Drop for ServerCleanup {
    fn drop(&mut self) {
        if self.reaper.is_some() {
            let _ = Command::new(&self.cli)
                .args(["stop"])
                .env("NUO_HOME", &self.root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            self.kill_process_group();
            self.join_reaper();
        }
    }
}

fn discovery_path(root: &Path) -> PathBuf {
    root.join("nuo").join("instance").join("server.json")
}

/// ADR-0121's inheritance invariant plus ADR-0129's detachment invariant:
/// a real client binary carrying `NUO_HOME` starts this exact build in a
/// fresh Unix session and keeps every server artifact inside the sandbox.
#[tokio::test]
async fn spawned_server_inherits_the_nuo_home_sandbox() {
    let own = tempfile::tempdir().unwrap();
    let own_root = own.path().to_path_buf();
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_nuo"));

    let mut command = Command::new(&cli);
    command.args(["start", "--fg"]);
    command
        .env("NUO_HOME", &own_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/");
    nuo_client::configure_server_detachment(&mut command);
    let child = command.spawn().expect("spawn the sandboxed server");
    let server_pid = child.id();

    // Reap continuously: `server stop` checks liveness, and an unreaped child
    // would remain a zombie that still answers the process-existence probe.
    let mut reaper_child = child;
    let reaper = std::thread::spawn(move || reaper_child.wait());
    let mut cleanup = ServerCleanup {
        cli,
        root: own_root.clone(),
        pid: server_pid,
        reaper: Some(reaper),
    };

    let own_record = discovery_path(&own_root);
    let deadline = Instant::now() + Duration::from_secs(15);
    let record = loop {
        if let Ok(bytes) = std::fs::read(&own_record)
            && let Ok(record) =
                serde_json::from_slice::<nuo::serve_discovery::Discovery>(&bytes)
        {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "server never advertised inside its sandbox"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(record.pid, server_pid, "sandbox record must name the child");
    assert_eq!(
        record.version.as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
        "Cargo must execute the freshly-built CLI, never a stale target artifact"
    );

    // `setsid(2)` makes the server both session and process-group leader.
    // SAFETY: `server_pid` names the live child owned by this test.
    assert_eq!(
        unsafe { libc::getsid(server_pid as libc::pid_t) },
        server_pid as libc::pid_t
    );
    // SAFETY: `server_pid` names the live child owned by this test.
    assert_eq!(
        unsafe { libc::getpgid(server_pid as libc::pid_t) },
        server_pid as libc::pid_t
    );

    let stop_output = cleanup.stop();
    assert!(
        stop_output.status.success(),
        "server stop must succeed: {} (stderr: {})",
        stop_output.status,
        String::from_utf8_lossy(&stop_output.stderr)
    );
    assert!(!own_record.exists(), "drained server must remove discovery");
}
