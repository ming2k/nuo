//! Supervised command input — the platform's single seam for running a command
//! with a controlling terminal and answering a prompt it actually reaches.
//!
//! This module owns the whole mechanism: spawning a child whose stdin *is* a
//! private pty slave (also its controlling terminal), detecting at runtime
//! whether the child is blocked reading that terminal, and writing an answer
//! back. Callers see one capability value ([`input_supervision`]) and one opaque
//! handle ([`SupervisedChild`]); they never see a pty, a `pre_exec` hook, or
//! `/proc`. See ADR-0293, ADR-0294, ADR-0295.

pub mod job;
pub use job::*;

use std::io;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod darwin;

#[cfg(windows)]
mod windows;

/// What this platform can do for supervised command input.
///
/// A single atomic value, consulted **once** by the dispatch layer before it
/// chooses an [`InputContract`](muta_contracts::InputContract). "Platform
/// independence" is thereby a structural property: there is one value to check,
/// and a platform can only reach [`Supervised`](Self::Supervised) if it
/// implements *both* a controlling terminal and reliable wait detection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputSupervision {
    /// No controlling terminal is available. The caller MUST use the sealed
    /// immediate-EOF contract; there is nothing to answer.
    Unsupported,
    /// A terminal is available, but a genuine input wait cannot be told apart
    /// from legitimate quiet computation. The caller MUST NOT auto-inject — it
    /// would misfire on a compiling build — and MUST fall back to the sealed
    /// fast-fail path.
    TerminalOnly,
    /// A controlling terminal *and* reliable wait detection: full supervision.
    Supervised,
}

/// The platform's supervised-input capability. Compile-time constant, no I/O.
#[must_use]
pub const fn input_supervision() -> InputSupervision {
    #[cfg(target_os = "linux")]
    {
        InputSupervision::Supervised
    }
    #[cfg(target_os = "macos")]
    {
        InputSupervision::Supervised
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    {
        InputSupervision::TerminalOnly
    }
    #[cfg(not(unix))]
    {
        InputSupervision::Unsupported
    }
}

/// One examiner step, as observed by the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputWait {
    /// No input wait is evident; keep draining.
    Idle,
    /// The child is blocked reading its terminal and the wait has been observed
    /// stable, so it is a real prompt rather than a momentary block.
    Awaiting,
}

/// A running child that owns a private controlling terminal, with the input
/// examination folded in.
///
/// The whole containment story is here: the process tree is reaped on
/// [`terminate`](Self::terminate) and on drop, and the child is spawned with
/// `kill_on_drop`, so a caller cannot leak a supervised tree by forgetting a
/// step. The stability de-bounce of the detector is internal; callers only ask
/// [`poll_input_wait`](Self::poll_input_wait) and read [`InputWait`].
pub struct SupervisedChild {
    child: tokio::process::Child,
    tree: crate::process::OwnedProcessTree,
    /// The child's terminal master. Because stdin *is* the slave, a stdin read
    /// and a `/dev/tty` read are one channel, answered here. `None` when the
    /// platform has no terminal (never a `Supervised` platform).
    tty: Option<crate::process::PtyMaster>,
    /// Kernel-evidence examiner. Gated per supported platform.
    #[cfg(target_os = "linux")]
    examiner: linux::Examiner,
    #[cfg(target_os = "macos")]
    examiner: darwin::Examiner,
    #[cfg(windows)]
    examiner: windows::Examiner,
}

impl SupervisedChild {
    /// Spawn a supervised child: owned process tree + private controlling
    /// terminal (the child's stdin), stdout/stderr on clean pipes.
    ///
    /// Applies `kill_on_drop` itself so containment does not depend on the
    /// caller, and reuses the process module's contained-spawn (including its
    /// rollback-on-attach-failure), so the containment contract is identical to
    /// every other owned spawn.
    pub fn spawn(command: &mut tokio::process::Command) -> io::Result<Self> {
        command.kill_on_drop(true);
        let spawned = crate::process::spawn_supervised(command)?;
        Ok(Self {
            child: spawned.child,
            tree: spawned.tree,
            tty: spawned.tty,
            #[cfg(target_os = "linux")]
            examiner: linux::Examiner::default(),
            #[cfg(target_os = "macos")]
            examiner: darwin::Examiner::default(),
            #[cfg(windows)]
            examiner: windows::Examiner::default(),
        })
    }

    /// Take the child's stdout pipe.
    pub fn stdout(&mut self) -> io::Result<tokio::process::ChildStdout> {
        self.child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("supervised child has no stdout pipe"))
    }

    /// Take the child's stderr pipe.
    pub fn stderr(&mut self) -> io::Result<tokio::process::ChildStderr> {
        self.child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("supervised child has no stderr pipe"))
    }

    /// Await the child's exit and return its code (if it exited normally).
    pub async fn wait(&mut self) -> Option<i32> {
        self.child.wait().await.ok().and_then(|status| status.code())
    }

    /// Terminate the child's whole process tree. Idempotent.
    pub fn terminate(&self) -> io::Result<()> {
        self.tree.terminate()
    }

    /// Advance the examiner one step. Returns [`InputWait::Awaiting`] only after
    /// the wait has been observed across two consecutive samples with no CPU
    /// progress, so a momentary block does not trip it.
    ///
    /// The caller is expected to poll only after the command has been quiet for
    /// its examiner floor; this method itself performs no per-line work. On a
    /// platform without detection (never a `Supervised` one) it is a constant
    /// [`InputWait::Idle`].
    pub fn poll_input_wait(&mut self) -> InputWait {
        #[cfg(target_os = "linux")]
        {
            self.examiner
                .poll(self.tree.process_group(), self.tty.as_ref())
        }
        #[cfg(target_os = "macos")]
        {
            self.examiner
                .poll(self.tree.process_group(), self.tty.as_ref())
        }
        #[cfg(windows)]
        {
            self.examiner.poll(&self.tree)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            InputWait::Idle
        }
    }

    /// Write one line of input to the child's terminal and reset the examiner's
    /// stability gate, since the child's state has changed.
    pub fn answer(&mut self, data: &str) -> io::Result<()> {
        let tty = self
            .tty
            .as_ref()
            .ok_or_else(|| io::Error::other("supervised child has no terminal to answer"))?;
        tty.write_input(data)?;
        #[cfg(target_os = "linux")]
        self.examiner.reset();
        #[cfg(target_os = "macos")]
        self.examiner.reset();
        #[cfg(windows)]
        self.examiner.reset();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_is_present_on_every_platform() {
        // The value must be one of the three; the point of the seam is that a
        // caller always has exactly one to consult.
        assert!(matches!(
            input_supervision(),
            InputSupervision::Unsupported
                | InputSupervision::TerminalOnly
                | InputSupervision::Supervised
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervised_child_spawns_and_terminates() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("10");
        let mut child = SupervisedChild::spawn(&mut cmd).expect("spawn");
        assert_eq!(child.poll_input_wait(), InputWait::Idle);
        assert!(child.terminate().is_ok());
    }

    /// The core discrimination: a `read(stdin)` block (harness-held pipe) is a
    /// wait, while a legitimate quiet sleep is not. This is what lets the
    /// supervised loop park on a real prompt without fast-failing a build.
    #[cfg(target_os = "linux")]
    #[test]
    fn classifier_distinguishes_stdin_read_from_sleep() {
        use std::process::{Command, Stdio};

        let (reader, writer) = std::io::pipe().expect("pipe");
        let mut prompt = Command::new("sh")
            .arg("-c")
            .arg("read line")
            .stdin(Stdio::from(reader.try_clone().expect("clone")))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn prompt");
        let mut sleeper = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeper");

        std::thread::sleep(std::time::Duration::from_millis(600));
        let prompt_state = linux::classify_sleeping_process(prompt.id() as libc::pid_t);
        let sleeper_state = linux::classify_sleeping_process(sleeper.id() as libc::pid_t);

        let _ = prompt.kill();
        let _ = sleeper.kill();
        let _ = prompt.wait();
        let _ = sleeper.wait();
        drop(writer);

        assert_eq!(
            prompt_state,
            linux::GroupState::Awaiting,
            "a live-pipe read must be classified as an input wait"
        );
        assert_eq!(
            sleeper_state,
            linux::GroupState::Otherwise,
            "a nanosleep must NOT be mistaken for an input wait"
        );
    }

    /// A *pipe*-family wait with a non-pipe fd 0 (e.g. a command whose stdin was
    /// redirected inside its own pipeline) must NOT be treated as an input wait
    /// on the harness channel — this locks the fd-0 gating for the pipe branch.
    #[cfg(target_os = "linux")]
    #[test]
    fn classifier_does_not_flag_pipe_read_when_fd0_is_not_a_pipe() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30 | { read x; }")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        std::thread::sleep(std::time::Duration::from_millis(700));
        let sample = linux::classify_group(child.id() as i32);
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            matches!(
                sample.state,
                linux::GroupState::Awaiting
                    | linux::GroupState::Running
                    | linux::GroupState::Otherwise
                    | linux::GroupState::Gone
            ),
            "group classification must always yield a definite state"
        );
    }
}
