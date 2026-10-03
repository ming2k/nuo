use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::tools::execute_command::pipes::{OutputCollector, spawn_stream_readers};

pub fn workspace_sandbox_shell(
    command: &str,
    workspace_root: &std::path::Path,
    additional_roots: &[std::path::PathBuf],
) -> Result<tokio::process::Command, String> {
    nuo_host::workspace_sandbox::shell_with_roots(
        command,
        workspace_root,
        additional_roots,
        nuo_host::workspace_sandbox::WorkspaceAccess::ReadWrite,
        nuo_host::workspace_sandbox::NetworkAccess::Disabled,
    )
}

/// A running command whose output is being drained. Two shapes exist, and the
/// drain loop below is identical for both; only the input channel differs:
///
/// - **Plain** (sealed / prefilled): an owned child with no terminal. There is
///   nothing to answer.
/// - **Supervised**: a [`nuo_host::supervised::SupervisedChild`] that owns
///   a private controlling terminal, examines whether the child is blocked on
///   it, and can write an answer. All of that mechanism lives in the platform
///   seam (ADR-0293); the loop only asks and reacts.
enum Running {
    Plain {
        child: tokio::process::Child,
        tree: nuo_host::process::OwnedProcessTree,
    },
    Supervised(nuo_host::supervised::SupervisedChild),
}

impl Running {
    fn stdout(&mut self) -> Result<tokio::process::ChildStdout, String> {
        match self {
            Running::Plain { child, .. } => child
                .stdout
                .take()
                .ok_or_else(|| "failed to capture child stdout".to_string()),
            Running::Supervised(child) => child
                .stdout()
                .map_err(|e| format!("failed to capture child stdout: {e}")),
        }
    }

    fn stderr(&mut self) -> Result<tokio::process::ChildStderr, String> {
        match self {
            Running::Plain { child, .. } => child
                .stderr
                .take()
                .ok_or_else(|| "failed to capture child stderr".to_string()),
            Running::Supervised(child) => child
                .stderr()
                .map_err(|e| format!("failed to capture child stderr: {e}")),
        }
    }

    async fn wait(&mut self) -> Option<i32> {
        match self {
            Running::Plain { child, .. } => child.wait().await.ok().and_then(|s| s.code()),
            Running::Supervised(child) => child.wait().await,
        }
    }

    /// Terminate the whole process tree and reap the direct child.
    async fn terminate(&mut self) {
        match self {
            Running::Plain { child, tree } => {
                let _ = tree.terminate();
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            }
            Running::Supervised(child) => {
                let _ = child.terminate();
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            }
        }
    }

    /// Advance the input examiner one step. `None` for a plain child (no
    /// terminal to examine).
    fn poll_input_wait(&mut self) -> Option<nuo_host::supervised::InputWait> {
        match self {
            Running::Plain { .. } => None,
            Running::Supervised(child) => Some(child.poll_input_wait()),
        }
    }

    /// Write one line of input into the child's terminal.
    fn answer(&mut self, data: &str) -> std::io::Result<()> {
        match self {
            Running::Supervised(child) => child.answer(data),
            Running::Plain { .. } => Err(std::io::Error::other(
                "no terminal on a non-supervised child",
            )),
        }
    }
}

/// How the drain loop ended.
enum Supervision {
    /// The child exited on its own.
    Exited(Option<i32>),
    /// An input wait was detected and no answer was supplied (no handler, or
    /// the operator declined). The child was killed.
    InputUnanswered,
    /// Wall-clock ceiling reached while still running.
    TimedOut,
    /// No output for the idle budget with no detectable input wait — the
    /// ambiguous quiet case (a compiling build, or a pipe-buffered command).
    IdleBlocked,
    /// Continuous streaming flood (ADR-0257).
    StreamGuarded,
}

/// Policy knobs for one command run: the wall-clock budget, raw-output mode,
/// and the runtime input supervisor. Bundled so the runner's signature does
/// not grow per feature.
pub struct RunPolicy<'a> {
    pub timeout: Duration,
    pub raw: bool,
    pub handler: Option<&'a dyn nuo_wire::InputHandler>,
}

pub async fn run_episodic_command(
    command: &str,
    isolation: nuo_wire::ShellIsolation,
    env: Arc<dyn nuo_wire::ExecutionEnvironment>,
    input: nuo_wire::InputContract,
    policy: RunPolicy<'_>,
    on_stream: &mut (dyn FnMut(nuo_wire::ToolStream) + Send + '_),
) -> Result<nuo_wire::ToolOutput, String> {
    let mut invocation = match isolation {
        nuo_wire::ShellIsolation::Host => nuo_host::shell::native_shell(command),
        nuo_wire::ShellIsolation::Workspace => {
            let additional_roots = env.additional_roots();
            workspace_sandbox_shell(command, env.workspace_root(), &additional_roots)?
        }
    };
    invocation.current_dir(env.workspace_root());
    nuo_host::shell::configure_headless_env(&mut invocation);
    invocation.kill_on_drop(true);

    let (running, expectation) = match &input {
        nuo_wire::InputContract::Sealed => {
            invocation.stdin(std::process::Stdio::null());
            (spawn_plain(&mut invocation)?, None)
        }
        nuo_wire::InputContract::Prefilled { data } => {
            invocation.stdin(std::process::Stdio::piped());
            let mut running = spawn_plain(&mut invocation)?;
            if let Running::Plain { child, .. } = &mut running
                && let Some(mut stdin) = child.stdin.take()
            {
                let _ = stdin.write_all(data.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }
            (running, None)
        }
        nuo_wire::InputContract::Supervised { expectation } => {
            let child = nuo_host::supervised::SupervisedChild::spawn(&mut invocation)
                .map_err(|e| format!("Failed to execute and contain supervised process tree: {e}"))?;
            (Running::Supervised(child), expectation.clone())
        }
    };

    run_loop(command, running, policy, expectation, on_stream).await
}

/// Spawn an owned child with piped stdout/stderr (stdin already configured by
/// the caller). Only used for the sealed/prefilled contracts.
fn spawn_plain(invocation: &mut tokio::process::Command) -> Result<Running, String> {
    invocation
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (child, tree) = nuo_host::process::spawn_owned(invocation)
        .map_err(|e| format!("Failed to execute and contain process tree: {e}"))?;
    Ok(Running::Plain { child, tree })
}

/// The single drain loop for every input contract. It asks the platform seam
/// whether the child is waiting on its terminal (never inspecting output text):
/// on a detected wait it injects an answer when a handler is present
/// (supervised), and fast-fails otherwise (the unattended contract). Legitimate
/// quiet computation is never mistaken for a prompt because detection rests on
/// kernel evidence the platform owns (ADR-0292, ADR-0293).
async fn run_loop(
    command: &str,
    mut running: Running,
    policy: RunPolicy<'_>,
    expectation: Option<nuo_wire::InputExpectation>,
    on_stream: &mut (dyn FnMut(nuo_wire::ToolStream) + Send + '_),
) -> Result<nuo_wire::ToolOutput, String> {
    let RunPolicy {
        timeout: timeout_duration,
        raw,
        handler,
    } = policy;
    let stdout = running.stdout()?;
    let stderr = running.stderr()?;
    let mut readers = spawn_stream_readers(stdout, stderr);

    let idle_budget = idle_budget_for(timeout_duration);
    let timeout_deadline = tokio::time::Instant::now() + timeout_duration;
    let examiner_floor = Duration::from_secs(5);

    let mut collector = OutputCollector::new();
    let mut last_output_at = tokio::time::Instant::now();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    let outcome = loop {
        tokio::select! {
            biased;
            msg = readers.rx.recv() => {
                match msg {
                    Some((stream, text)) => {
                        collector.push_line(stream, text, on_stream);
                        // The arrival of a line is itself the proof of progress;
                        // the examiner is asked only on the quiet path below,
                        // never per line (a per-line `/proc` scan would slow a
                        // many-line command to a crawl).
                        last_output_at = tokio::time::Instant::now();
                        if collector.is_stream_flooded(raw) {
                            break Supervision::StreamGuarded;
                        }
                    }
                    None => break Supervision::Exited(running.wait().await),
                }
            }
            _ = ticker.tick() => {
                let now = tokio::time::Instant::now();
                if now >= timeout_deadline {
                    break Supervision::TimedOut;
                }
                let quiet_for = now.duration_since(last_output_at);
                // Examine only once the command has been quiet past the floor;
                // a handful of examiner steps per second at worst.
                if quiet_for >= examiner_floor
                    && running.poll_input_wait()
                        == Some(nuo_host::supervised::InputWait::Awaiting)
                {
                    match handler {
                        Some(handler) => {
                            match await_answer(command, expectation.as_ref(), handler).await {
                                Some(data) => {
                                    if let Err(error) = running.answer(&data) {
                                        tracing::warn!(%error, "failed to write operator input");
                                    }
                                    // The wait is understood, not silent: reset the
                                    // idle clock so it cannot also trip IdleBlocked.
                                    last_output_at = tokio::time::Instant::now();
                                }
                                None => break Supervision::InputUnanswered,
                            }
                        }
                        // No supervisor: fast-fail on the detected wait (the
                        // unattended contract).
                        None => break Supervision::InputUnanswered,
                    }
                }
                if quiet_for >= idle_budget {
                    break Supervision::IdleBlocked;
                }
            }
        }
    };

    let termination = match outcome {
        Supervision::Exited(exit) => {
            let _ = readers.stdout_task.await;
            let _ = readers.stderr_task.await;
            collector.flush_stream(on_stream);
            return finish_output(
                command,
                collector,
                exit,
                nuo_wire::tool_output::ShellTermination::Exited,
                raw,
            );
        }
        Supervision::InputUnanswered => {
            nuo_wire::tool_output::ShellTermination::InputUnanswered
        }
        Supervision::TimedOut => nuo_wire::tool_output::ShellTermination::Timeout,
        Supervision::IdleBlocked => nuo_wire::tool_output::ShellTermination::IdleBlocked,
        Supervision::StreamGuarded => nuo_wire::tool_output::ShellTermination::StreamGuard,
    };
    running.terminate().await;
    readers.stdout_task.abort();
    readers.stderr_task.abort();
    collector.drain_remaining_rx(&mut readers.rx);
    collector.flush_stream(on_stream);
    finish_output(command, collector, None, termination, raw)
}

/// Park the runtime input wait for the operator and return the answer, or
/// `None` when no answer is supplied (cancelled, or no reachable human
/// channel).
async fn await_answer(
    command: &str,
    expectation: Option<&nuo_wire::InputExpectation>,
    handler: &dyn nuo_wire::InputHandler,
) -> Option<String> {
    let prompt = nuo_wire::InputPrompt {
        command: command.to_string(),
        prompt: expectation
            .map(|e| e.prompt.clone())
            .unwrap_or_else(|| format!("This command is waiting for input ({command}):")),
        secret: expectation.map(|e| e.secret).unwrap_or(false),
    };
    handler.resolve(prompt).await
}

fn finish_output(
    command: &str,
    collector: OutputCollector,
    exit: Option<i32>,
    termination: nuo_wire::tool_output::ShellTermination,
    raw: bool,
) -> Result<nuo_wire::ToolOutput, String> {
    let (stdout, stderr, lines, truncated) = collector.apply_caps_ex(exit, raw);
    Ok(nuo_wire::ToolOutput::Shell {
        command: command.to_string(),
        stdout,
        stderr,
        lines,
        exit,
        truncated,
        termination,
        detached_job_id: None,
    })
}

/// Idle-watchdog budget derived from the caller's wall-clock `timeout`.
///
/// One third of the timeout, clamped to [5s, 480s]: callers budgeting
/// more room for a legitimately quiet command (long compiles, network
/// waits, `--quiet` builds) get proportionally more idle tolerance, and
/// even the default (1800s) tolerates 8 minutes of silence — which
/// matters because output buffered by a pipe (`… | tail`) is
/// indistinguishable from silence until the pipe closes.
pub fn idle_budget_for(timeout: Duration) -> Duration {
    let third = timeout / 3;
    third.clamp(Duration::from_secs(5), Duration::from_secs(480))
}


