//! End-to-end daemon spawn coverage owned by the CLI package so Cargo supplies
//! the exact freshly-built `muta` binary through `CARGO_BIN_EXE_nuo`.
//! This must never discover an incidental or stale `target/debug/muta`.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct DaemonCleanup {
    cli: PathBuf,
    root: PathBuf,
    pid: u32,
    reaper: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
}

impl DaemonCleanup {
    fn stop(&mut self) -> Output {
        let output = Command::new(&self.cli)
            .args(["stop"])
            .env("NUO_HOME", &self.root)
            .stdin(Stdio::null())
            .output()
            .expect("run daemon stop in the sandbox");
        if !output.status.success() {
            self.kill_process_group();
        }
        self.join_reaper();
        output
    }

    fn kill_process_group(&self) {
        // SAFETY: this test spawned `pid` through the production `setsid`
        // helper, so `-pid` targets only the sandboxed daemon's process group.
        let _ = unsafe { libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL) };
    }

    fn join_reaper(&mut self) {
        if let Some(reaper) = self.reaper.take() {
            reaper
                .join()
                .expect("reap thread did not panic")
                .expect("wait for sandboxed daemon");
        }
    }
}

impl Drop for DaemonCleanup {
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
    root.join("nuo").join("instance").join("daemon.json")
}

/// ADR-0121's inheritance invariant plus ADR-0129's detachment invariant:
/// a real client binary carrying `MUTA_HOME` starts this exact build in a
/// fresh Unix session and keeps every daemon artifact inside the sandbox.
#[tokio::test]
async fn spawned_daemon_inherits_the_muta_home_sandbox() {
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
    nuo_client::configure_daemon_detachment(&mut command);
    let child = command.spawn().expect("spawn the sandboxed daemon");
    let daemon_pid = child.id();

    // Reap continuously: `daemon stop` checks liveness, and an unreaped child
    // would remain a zombie that still answers the process-existence probe.
    let mut reaper_child = child;
    let reaper = std::thread::spawn(move || reaper_child.wait());
    let mut cleanup = DaemonCleanup {
        cli,
        root: own_root.clone(),
        pid: daemon_pid,
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
            "daemon never advertised inside its sandbox"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(record.pid, daemon_pid, "sandbox record must name the child");
    assert_eq!(
        record.version.as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
        "Cargo must execute the freshly-built CLI, never a stale target artifact"
    );

    // `setsid(2)` makes the daemon both session and process-group leader.
    // SAFETY: `daemon_pid` names the live child owned by this test.
    assert_eq!(
        unsafe { libc::getsid(daemon_pid as libc::pid_t) },
        daemon_pid as libc::pid_t
    );
    // SAFETY: `daemon_pid` names the live child owned by this test.
    assert_eq!(
        unsafe { libc::getpgid(daemon_pid as libc::pid_t) },
        daemon_pid as libc::pid_t
    );

    let stop_output = cleanup.stop();
    assert!(
        stop_output.status.success(),
        "daemon stop must succeed: {} (stderr: {})",
        stop_output.status,
        String::from_utf8_lossy(&stop_output.stderr)
    );
    assert!(!own_record.exists(), "drained daemon must remove discovery");
}
