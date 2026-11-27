//! Runs the freshly built `nuo` executable with a bounded wall-clock budget.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Child, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::Result;
use crate::sandbox::Sandbox;

/// A CLI invocation bound to one sandbox.
///
/// `Cli` never mutates the test process environment; every invocation receives
/// the sandbox roots through `Command::env`, so nextest can run tests in
/// parallel without cross-test contamination.
pub struct Cli<'a> {
    bin: &'a Path,
    sandbox: &'a Sandbox,
    args: Vec<OsString>,
    timeout: Duration,
}

impl<'a> Cli<'a> {
    /// Bind `bin` (normally [`crate::binary_path`]) to `sandbox`.
    pub fn new(bin: &'a Path, sandbox: &'a Sandbox) -> Self {
        Self {
            bin,
            sandbox,
            args: Vec::new(),
            timeout: Duration::from_secs(60),
        }
    }

    /// Append one argument.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Override the default 60 second budget.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run and capture output, using the configured timeout.
    pub fn output(&self) -> Result<Output> {
        self.output_with_timeout(self.timeout)
    }

    /// Run and capture output. On timeout the child is killed and an error is
    /// returned; stdout/stderr are drained on background threads so a verbose
    /// failure cannot block the child on a full pipe.
    pub fn output_with_timeout(&self, timeout: Duration) -> Result<Output> {
        let mut command = self.sandbox.command(self.bin);
        command
            .args(&self.args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or("failed to capture child stdout")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("failed to capture child stderr")?;
        let stdout_reader = thread::spawn(move || drain(stdout));
        let stderr_reader = thread::spawn(move || drain(stderr));

        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(
                    format!("`{}` did not finish within {timeout:?}", self.bin.display()).into(),
                );
            }
            thread::sleep(Duration::from_millis(10));
        };

        Ok(Output {
            status,
            stdout: join_reader(stdout_reader)?,
            stderr: join_reader(stderr_reader)?,
        })
    }

    /// Run and require success, returning the full output.
    pub fn success(&self) -> Result<Output> {
        let output = self.output()?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(format!(
                "`{}` failed with {}\nstdout:\n{}\nstderr:\n{}",
                self.bin.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into())
        }
    }

    /// Run a JSON-lines command and parse every non-empty stdout line.
    pub fn json_lines(&self) -> Result<Vec<serde_json::Value>> {
        let output = self.success()?;
        let text = String::from_utf8(output.stdout)?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    /// Spawn without waiting. The returned guard kills the child on drop, so a
    /// panicking test cannot leave a server or headless client behind.
    pub fn spawn(&self) -> Result<ChildGuard> {
        let mut command = self.sandbox.command(self.bin);
        command
            .args(&self.args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(ChildGuard::new(command.spawn()?))
    }
}

fn drain(mut reader: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = reader.read_to_end(&mut buffer);
    buffer
}

fn join_reader(handle: thread::JoinHandle<Vec<u8>>) -> Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| "child output reader thread panicked".into())
}

/// Owns a spawned child process and kills it on drop.
pub struct ChildGuard {
    child: Option<Child>,
    pid: u32,
}

impl ChildGuard {
    /// Wrap a spawned child.
    pub fn new(child: Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    /// OS process id of the child.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// `Some(status)` once the child has exited.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match self.child.as_mut() {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }

    /// Wait for the child to exit, reaping it exactly once.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match self.child.take() {
            Some(mut child) => child.wait(),
            None => Err(std::io::Error::other("child already reaped")),
        }
    }

    /// Send `SIGKILL`/`TerminateProcess` without reaping.
    pub fn kill(&mut self) -> std::io::Result<()> {
        match self.child.as_mut() {
            Some(child) => child.kill(),
            None => Ok(()),
        }
    }

    /// Mutable access for tests that need to wait for specific output.
    pub fn child_mut(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
