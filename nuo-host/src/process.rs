//! Process-lifecycle primitives. A daemon and an owned subprocess tree have
//! intentionally different lifetime policies.

use std::io;
use tokio::process::{Child, Command};

/// Configure a long-lived daemon to detach from the invoking terminal.
///
/// This never attaches the daemon to a kill-on-close Windows Job Object.
pub fn configure_daemon(command: &mut Command) {
    native::configure_daemon(command);
}

/// Synchronous-command counterpart used before a runtime exists.
pub fn configure_daemon_std(command: &mut std::process::Command) {
    native::configure_daemon_std(command);
}

/// Spawn a subprocess whose complete descendant tree is owned by the returned
/// guard. Configuration, spawn, and containment are one operation so callers
/// cannot accidentally run an uncontained (or Windows-suspended) child.
pub fn spawn_owned(command: &mut Command) -> io::Result<(Child, OwnedProcessTree)> {
    native::configure_owned(command);
    let child = command.spawn()?;
    match OwnedProcessTree::attach(&child) {
        Ok(tree) => Ok((child, tree)),
        Err(error) => {
            native::rollback_failed_attach(&child);
            Err(error)
        }
    }
}

/// The result of spawning a supervised child: the owned child, its process-tree
/// guard, and its terminal master. Internal to the `supervised` seam (ADR-0293);
/// callers use [`crate::supervised::SupervisedChild`], never these raw parts.
pub(crate) struct SupervisedSpawn {
    pub(crate) child: Child,
    pub(crate) tree: OwnedProcessTree,
    /// The master side of the child's terminal. The child's stdin *is* the
    /// slave side, so stdin reads and `/dev/tty` reads are the same channel and
    /// both are answered here. `None` when the platform cannot provide a
    /// terminal.
    pub(crate) tty: Option<PtyMaster>,
}

/// Master side of a supervised child's controlling terminal. Internal detail of
/// the `supervised` seam.
pub(crate) struct PtyMaster {
    #[cfg(unix)]
    fd: std::os::fd::RawFd,
}

// SAFETY: a master pty fd is a plain file descriptor; it carries no thread
// affinity and all access goes through `&self` syscalls.
unsafe impl Send for PtyMaster {}
unsafe impl Sync for PtyMaster {}

/// Fork-safe child setup that acquires the control terminal. `Fn` (not
/// `FnOnce`) and captured by reference so it satisfies `pre_exec`'s bounds.
pub(crate) type PtyChildSetup = Box<dyn Fn() -> io::Result<()> + Send + Sync>;

impl PtyMaster {
    /// Write `data` to the terminal, then `\n` — the answer to a prompt the
    /// child is reading from `/dev/tty`.
    pub(crate) fn write_input(&self, data: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            let mut bytes = data.as_bytes().to_vec();
            bytes.push(b'\n');
            let mut written = 0;
            while written < bytes.len() {
                // SAFETY: `fd` is a live master fd owned by this handle; the
                // buffer is a valid slice.
                let n = unsafe {
                    libc::write(
                        self.fd,
                        bytes[written..].as_ptr().cast(),
                        bytes.len() - written,
                    )
                };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                written += n as usize;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = data;
            Err(io::Error::other(
                "controlling-terminal input is unsupported on this platform",
            ))
        }
    }

    /// Check how many unconsumed bytes remain pending in the terminal input queue.
    pub(crate) fn pending_input_bytes(&self) -> io::Result<usize> {
        #[cfg(unix)]
        {
            let mut nbytes: libc::c_int = 0;
            // SAFETY: `fd` is a live master fd owned by this handle.
            let ret = unsafe { libc::ioctl(self.fd, libc::FIONREAD, &mut nbytes) };
            if ret < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(nbytes.max(0) as usize)
            }
        }
        #[cfg(not(unix))]
        {
            Ok(0)
        }
    }
}

impl Drop for PtyMaster {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: `fd` is owned solely by this handle; closed exactly once.
            unsafe {
                libc::close(self.fd);
            }
        }
    }
}

/// Spawn a **supervised** subprocess: owned process tree + a private
/// controlling terminal that is also the child's stdin, with stdout/stderr
/// captured on clean pipes.
///
/// On Unix the child becomes a session leader (`setsid`) and claims the slave
/// pty as its controlling terminal (`TIOCSCTTY`) and stdin (`dup2`), so an
/// explicit `open("/dev/tty")` resolves to that terminal rather than the
/// operator's; writing to the parent-held master feeds it. The child never sees
/// the operator's real terminal. Containment matches [`spawn_owned`], including
/// rollback if the tree guard cannot be established.
pub(crate) fn spawn_supervised(command: &mut Command) -> io::Result<SupervisedSpawn> {
    // The placeholder stdin is replaced (via `dup2` in the child setup) by the
    // pty slave once the terminal is opened; stdout/stderr stay clean pipes.
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    // Order matters: `setsid` must establish the new session before the
    // controlling-terminal acquiring `ioctl` can succeed.
    native::configure_owned(command);
    let (tty, acquire_ctty) = native::open_pty_master()?;
    if let Some(acquire) = acquire_ctty {
        // SAFETY: `pre_exec` runs between fork and exec; the closure performs
        // only async-signal-safe syscalls (open, dup2, ioctl).
        unsafe {
            command.pre_exec(acquire);
        }
    }
    let child = command.spawn()?;
    match OwnedProcessTree::attach(&child) {
        Ok(tree) => Ok(SupervisedSpawn { child, tree, tty }),
        Err(error) => {
            native::rollback_failed_attach(&child);
            Err(error)
        }
    }
}

/// Native lifetime guard for an owned subprocess tree.
///
/// Keep this guard for the intended subprocess lifetime. Explicit
/// [`Self::terminate`] and dropping the guard both kill remaining descendants,
/// including processes which outlive the direct shell child.
pub struct OwnedProcessTree {
    native: native::OwnedProcessTree,
}

impl OwnedProcessTree {
    fn attach(child: &Child) -> io::Result<Self> {
        Ok(Self {
            native: native::OwnedProcessTree::attach(child)?,
        })
    }

    pub fn terminate(&self) -> io::Result<()> {
        self.native.terminate()
    }

    /// The process-group id this guard owns. Used by the `supervised` examiner
    /// to scope its `/proc` scan.
    pub(crate) fn process_group(&self) -> i32 {
        self.native.process_group()
    }
}

/// Stable-enough native process identity used to avoid acting on a recycled
/// PID. The birth token is an OS process creation timestamp/start tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub birth_token: u64,
}

pub fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
    native::process_identity(pid)
}

pub fn process_is_alive(identity: ProcessIdentity) -> bool {
    process_identity(identity.pid).is_ok_and(|current| current == identity)
}

/// Force-terminate exactly the process represented by `identity`.
pub fn force_terminate(identity: ProcessIdentity) -> io::Result<()> {
    if !process_is_alive(identity) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process no longer exists or PID has been reused",
        ));
    }
    native::force_terminate(identity.pid)
}

/// Request the platform's graceful process termination, when one exists.
/// Windows daemons use the control protocol and therefore report
/// `Unsupported` here; callers may then proceed to their force tier.
pub fn request_termination(identity: ProcessIdentity) -> io::Result<()> {
    if !process_is_alive(identity) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process no longer exists or PID has been reused",
        ));
    }
    native::request_termination(identity.pid)
}

/// Options controlling the graceful-to-force escalation budget during takeover.
#[derive(Clone, Copy, Debug)]
pub struct TakeoverOptions {
    pub grace_period: std::time::Duration,
    pub poll_interval: std::time::Duration,
}

impl Default for TakeoverOptions {
    fn default() -> Self {
        Self {
            grace_period: std::time::Duration::from_millis(500),
            poll_interval: std::time::Duration::from_millis(25),
        }
    }
}

/// Request graceful termination with automatic escalation to force termination (ADR-0029).
pub async fn takeover_pid(pid: u32, opts: TakeoverOptions) -> io::Result<()> {
    if pid == std::process::id() {
        return Ok(());
    }
    let identity = process_identity(pid)?;
    if !process_is_alive(identity) {
        return Ok(());
    }

    let _ = request_termination(identity);
    let deadline = tokio::time::Instant::now() + opts.grace_period;
    while tokio::time::Instant::now() < deadline && process_is_alive(identity) {
        tokio::time::sleep(opts.poll_interval).await;
    }

    if process_is_alive(identity) {
        tracing::warn!(pid, "process did not terminate gracefully; escalating to force terminate");
        let _ = force_terminate(identity);
        let force_deadline = tokio::time::Instant::now() + opts.grace_period;
        while tokio::time::Instant::now() < force_deadline && process_is_alive(identity) {
            tokio::time::sleep(opts.poll_interval).await;
        }
    }

    if process_is_alive(identity) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("process {pid} is still alive after force terminate"),
        ))
    } else {
        Ok(())
    }
}

/// Synchronous / blocking version of [`takeover_pid`].
pub fn takeover_pid_sync(pid: u32, opts: TakeoverOptions) -> io::Result<()> {
    if pid == std::process::id() {
        return Ok(());
    }
    let identity = process_identity(pid)?;
    if !process_is_alive(identity) {
        return Ok(());
    }

    let _ = request_termination(identity);
    let deadline = std::time::Instant::now() + opts.grace_period;
    while std::time::Instant::now() < deadline && process_is_alive(identity) {
        std::thread::sleep(opts.poll_interval);
    }

    if process_is_alive(identity) {
        tracing::warn!(pid, "process did not terminate gracefully; escalating to force terminate");
        let _ = force_terminate(identity);
        let force_deadline = std::time::Instant::now() + opts.grace_period;
        while std::time::Instant::now() < force_deadline && process_is_alive(identity) {
            std::thread::sleep(opts.poll_interval);
        }
    }

    if process_is_alive(identity) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("process {pid} is still alive after force terminate"),
        ))
    } else {
        Ok(())
    }
}

/// Checks whether a running process's executed binary image matches the on-disk file.
pub fn process_image_matches_path(pid: u32, expected: &std::path::Path) -> bool {
    native::native_process_image_matches_path(pid, expected)
}

/// Bytes sampled per window when digesting an image larger than the window
/// budget; small images are digested whole.
const IMAGE_DIGEST_WINDOW: u64 = 64 * 1024;

/// Number of evenly-spaced windows sampled across a large image. The first
/// (head) and last (tail) are always included: the head carries the ELF
/// header and build-id note, the tail the section headers — both rewritten on
/// relink, so the sampled signal is strong for the drift case.
const IMAGE_DIGEST_WINDOWS: u64 = 8;

/// A **bounded content digest** of an executable image reachable at `path`:
/// its exact byte length plus the lowercase-hex SHA-256 of a positionally
/// sampled content window. `None` when the file cannot be read (missing,
/// permission denied, a directory) — callers treat that as "no evidence",
/// never as drift.
///
/// This is used by a **client** to fingerprint the image it *would spawn*:
/// deliberately by path, because the client's question is "is the daemon
/// running the file that now sits at this path?".
pub fn image_digest_len(path: &std::path::Path) -> Option<(u64, String)> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    Some((len, digest_sampled(&mut file, len)?))
}

/// The fingerprint of the executable image this process actually **holds in
/// memory** — *not* whatever may now sit at its path.
///
/// This asymmetry matters: a **client** fingerprints by path (the file it
/// would exec), but a **daemon** must fingerprint the very image the kernel
/// loaded into it. On Linux `/proc/self/exe` is a magic link that resolves to
/// the *held inode* (and to `… (deleted)` once the on-disk file has been
/// replaced), which is exactly that image. Re-opening [`current_exe`]'s *path*
/// would instead follow the path to any newly dropped file — the precise
/// dev-rebuild race this feature exists to catch — so we never do that here.
/// (Confirmed: on Linux `current_exe()` readlinks `/proc/self/exe` and returns
/// the *dereferenced path string*, so opening it hits the new file.)
///
/// On platforms with no equivalent handle (macOS, Windows) the held image
/// cannot be read reliably, so this returns `None`: the daemon then publishes
/// no digest and clients keep the pre-ADR-0021 inode probe, rather than
/// trusting a digest that might describe the wrong file. Absence of evidence
/// is not drift.
pub fn current_exe_digest_len() -> Option<(u64, String)> {
    #[cfg(target_os = "linux")]
    {
        let mut file = std::fs::File::open("/proc/self/exe").ok()?;
        let len = file.metadata().ok()?.len();
        Some((len, digest_sampled(&mut file, len)?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The shared, constant-cost sampling core: fold the exact length in first (a
/// size change is definitive drift), then digest up to
/// [`IMAGE_DIGEST_WINDOWS`] evenly-spaced [`IMAGE_DIGEST_WINDOW`] windows (head
/// and tail always included), or the whole image when it fits the window
/// budget. A relink rewrites the ELF head (build-id note) and tail (section
/// headers), so the sampled signal is strong for the case that matters.
fn digest_sampled(file: &mut std::fs::File, len: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(len.to_le_bytes());
    let span = IMAGE_DIGEST_WINDOWS * IMAGE_DIGEST_WINDOW;
    if len <= span {
        let mut whole = Vec::with_capacity(len as usize);
        file.read_to_end(&mut whole).ok()?;
        hasher.update(&whole);
    } else {
        let mut buf = vec![0u8; IMAGE_DIGEST_WINDOW as usize];
        for i in 0..IMAGE_DIGEST_WINDOWS {
            let pos = (len - IMAGE_DIGEST_WINDOW) * i / (IMAGE_DIGEST_WINDOWS - 1);
            if file.seek(SeekFrom::Start(pos)).is_err() {
                return None;
            }
            let mut filled = 0;
            while filled < buf.len() {
                match file.read(&mut buf[filled..]) {
                    Ok(0) => break,
                    Ok(n) => filled += n,
                    Err(_) => return None,
                }
            }
            hasher.update(&buf[..filled]);
        }
    }
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(unix)]
mod native {
    use super::*;

    pub(super) fn native_process_image_matches_path(pid: u32, expected: &std::path::Path) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let exe_link = std::path::Path::new("/proc")
                .join(pid.to_string())
                .join("exe");
            match (std::fs::metadata(&exe_link), std::fs::metadata(expected)) {
                (Ok(daemon), Ok(expected_meta)) => {
                    daemon.dev() == expected_meta.dev() && daemon.ino() == expected_meta.ino()
                }
                _ => true,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (pid, expected);
            true
        }
    }

    pub(super) fn configure_daemon(command: &mut Command) {
        // SAFETY: `setsid` is async-signal-safe and performs no allocation.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub(super) fn configure_daemon_std(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;

        // SAFETY: `setsid` is async-signal-safe and performs no allocation.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    pub(super) fn configure_owned(command: &mut Command) {
        // SAFETY: `setsid` is async-signal-safe, creates a new session, establishes
        // the child as the session and process-group leader (PGID == PID), and
        // completely detaches it from the host controlling terminal (/dev/tty)
        // per ADR-0286.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// Open a pty pair. Returns the parent-held master handle plus the
    /// fork-safe closure that makes the child claim the slave as its
    /// controlling terminal **and** use it as stdin. The child is already a
    /// session leader from [`configure_owned`]; `pre_exec` runs after stdio
    /// setup, so the `dup2` here overrides the placeholder stdin and leaves the
    /// child reading/writing prompts on the terminal while stdout/stderr stay
    /// pipes.
    pub(super) fn open_pty_master() -> io::Result<(Option<super::PtyMaster>, Option<PtyChildSetup>)> {
        // SAFETY: `posix_openpt`/`grantpt`/`unlockpt` operate on a freshly
        // opened fd and perform no allocation.
        let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let handle = super::PtyMaster { fd: master };
        // SAFETY: `master` is a live pty master fd.
        if unsafe { libc::grantpt(master) } != 0 || unsafe { libc::unlockpt(master) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // A zero-sized window makes some programs that query `TIOCGWINSZ`
        // misbehave; give the terminal a conventional default size.
        let winsize = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `master` is a live pty master fd; suppressing the result is
        // fine — a failed size hint only affects cosmetic wrapping.
        unsafe {
            libc::ioctl(master, libc::TIOCSWINSZ, &winsize);
        }
        // SAFETY: `ptsname` returns a static string for the given master fd.
        let slave_ptr = unsafe { libc::ptsname(master) };
        if slave_ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `slave_ptr` is a valid NUL-terminated C string.
        let slave_path = unsafe { std::ffi::CStr::from_ptr(slave_ptr) }.to_owned();
        let acquire: PtyChildSetup = Box::new(move || {
            // SAFETY: async-signal-safe open + dup2 + ioctl between fork and exec.
            let fd = unsafe { libc::open(slave_path.as_ptr(), libc::O_RDWR) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // Make the terminal the child's stdin, so a `read(stdin)` and a
            // `read("/dev/tty")` are the same channel the harness can answer.
            if unsafe { libc::dup2(fd, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { libc::ioctl(0, libc::TIOCSCTTY, 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
        Ok((Some(handle), Some(acquire)))
    }

    pub(super) fn rollback_failed_attach(child: &Child) {
        if let Some(pid) = child.id() {
            // SAFETY: configure_owned made this child the process-group
            // leader. This rollback is reached only when guard creation
            // failed, before ownership can be returned to the caller.
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }

    pub(super) struct OwnedProcessTree {
        pgid: libc::pid_t,
    }

    impl OwnedProcessTree {
        pub(super) fn attach(child: &Child) -> io::Result<Self> {
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("child has no process id"))?;
            Ok(Self {
                pgid: pid as libc::pid_t,
            })
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: a negative pid targets the process group established by
            // `configure_owned`. ESRCH means the tree already exited.
            let rc = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if rc == 0 {
                Ok(())
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }

        pub(super) fn process_group(&self) -> i32 {
            self.pgid
        }
    }

    impl Drop for OwnedProcessTree {
        fn drop(&mut self) {
            // Descendants may still be alive after the direct child exits.
            // Ownership is lexical: dropping the guard closes that lifetime.
            let _ = self.terminate();
        }
    }

    pub(super) fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
        #[cfg(target_os = "linux")]
        {
            // Field 22 in /proc/<pid>/stat is the process start time. The
            // comm field may contain spaces and ')' so split only after its
            // final closing parenthesis.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let tail = stat
                .rsplit_once(") ")
                .map(|(_, tail)| tail)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat"))?;
            let birth_token = tail
                .split_whitespace()
                .nth(19)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing start time"))?
                .parse::<u64>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            Ok(ProcessIdentity { pid, birth_token })
        }

        #[cfg(target_os = "macos")]
        {
            use std::mem::{size_of, zeroed};

            let mut info: libc::proc_bsdinfo = unsafe { zeroed() };
            // SAFETY: `info` is valid writable storage for PROC_PIDTBSDINFO;
            // libproc returns the number of bytes written.
            let written = unsafe {
                libc::proc_pidinfo(
                    pid as libc::c_int,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&raw mut info).cast(),
                    size_of::<libc::proc_bsdinfo>() as libc::c_int,
                )
            };
            if written != size_of::<libc::proc_bsdinfo>() as libc::c_int {
                return Err(io::Error::last_os_error());
            }
            let birth_token = info
                .pbi_start_tvsec
                .checked_mul(1_000_000)
                .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "process start time overflow")
                })?;
            Ok(ProcessIdentity { pid, birth_token })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Portable fallback for Unix targets without a native process
            // creation-time API wired here yet.
            // SAFETY: signal zero performs permission/liveness probing only.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
                Ok(ProcessIdentity {
                    pid,
                    birth_token: 0,
                })
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    pub(super) fn force_terminate(pid: u32) -> io::Result<()> {
        // SAFETY: the caller verified the process identity immediately before
        // this signal.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn request_termination(pid: u32) -> io::Result<()> {
        // SAFETY: the caller verified process identity immediately before
        // requesting SIGTERM.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_has_a_stable_nonzero_birth_token() {
        let first = process_identity(std::process::id()).expect("current process identity");
        let second = process_identity(std::process::id()).expect("current process identity");
        assert_eq!(first, second);
        assert_ne!(first.birth_token, 0);
        assert!(process_is_alive(first));
    }

    #[test]
    fn image_digest_is_deterministic_and_detects_change() {
        // ADR-0021: the bounded content digest must be stable for identical
        // bytes and must change when the image changes, including via a
        // length-preserving edit in the sampled tail region.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"identical image bytes").unwrap();
        std::fs::write(&b, b"identical image bytes").unwrap();
        let da = image_digest_len(&a).unwrap();
        let db = image_digest_len(&b).unwrap();
        assert_eq!(da, db, "equal bytes must digest equal");
        assert_eq!(da.0, 21);
        assert_eq!(image_digest_len(&a).unwrap(), da, "digest is deterministic");

        // A length-preserving change is caught by the content sample.
        std::fs::write(&b, b"identical image byteZ").unwrap();
        assert_eq!(image_digest_len(&b).unwrap().0, 21);
        assert_ne!(image_digest_len(&b).unwrap().1, da.1);

        // Missing file: no evidence, `None`.
        assert!(image_digest_len(&dir.path().join("missing")).is_none());
    }

    /// On Linux the daemon's self-fingerprint must describe the image the
    /// kernel **loaded into this process** (`/proc/self/exe`), not whatever
    /// sits at `current_exe()`'s path — the asymmetry that keeps the daemon's
    /// attestation honest when the on-disk file is later replaced.
    #[cfg(target_os = "linux")]
    #[test]
    fn current_exe_digest_reads_the_held_image() {
        let (len, digest) = current_exe_digest_len().expect("held image readable on linux");
        assert!(len > 0, "held image has a length");
        assert_eq!(digest.len(), 64, "sha256 hex digest");
        // Deterministic across calls.
        assert_eq!(current_exe_digest_len().unwrap(), (len, digest.clone()));
        // It fingerprints the same held image `/proc/self/exe` resolves to.
        let via_proc = image_digest_len(std::path::Path::new("/proc/self/exe"))
            .expect("readable via /proc/self/exe");
        assert_eq!(via_proc, (len, digest));
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::mem::{size_of, zeroed};
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessTimes, OpenProcess,
        OpenThread, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, ResumeThread,
        THREAD_SUSPEND_RESUME, TerminateProcess,
    };

    pub(super) fn configure_daemon(command: &mut Command) {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    pub(super) fn configure_daemon_std(command: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    pub(super) fn configure_owned(command: &mut Command) {
        // Suspend before the first user instruction so `attach` can place the
        // process in its Job Object before it has any opportunity to spawn an
        // uncontained descendant. `attach` resumes the primary thread only
        // after assignment succeeds.
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    /// Windows has no controlling-terminal equivalent here, so a supervised
    /// spawn degrades to a held-open stdin pipe only. The command exporting a
    /// terminal does not skip this: the caller reports `unsupported` rather
    /// than silently misbehaving.
    pub(super) fn open_pty_master(
    ) -> io::Result<(Option<super::PtyMaster>, Option<super::PtyChildSetup>)> {
        Ok((None, None))
    }

    pub(super) struct OwnedProcessTree {
        job: HANDLE,
    }

    unsafe impl Send for OwnedProcessTree {}
    unsafe impl Sync for OwnedProcessTree {}

    impl OwnedProcessTree {
        pub(super) fn attach(child: &Child) -> io::Result<Self> {
            // SAFETY: null attributes/name request an unnamed, non-inheritable
            // job owned solely by this guard.
            let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }

            let tree = Self { job };

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `info` matches the requested information class.
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                let error = io::Error::last_os_error();
                return Err(error);
            }

            let process = child
                .raw_handle()
                .ok_or_else(|| io::Error::other("child has no process handle"))?
                as HANDLE;
            // SAFETY: Tokio owns a live process handle for `child`; the job
            // handle remains owned by this guard.
            if unsafe { AssignProcessToJobObject(tree.job, process) } == 0 {
                let error = io::Error::last_os_error();
                return Err(error);
            }
            if let Err(error) = resume_primary_thread(
                child
                    .id()
                    .ok_or_else(|| io::Error::other("child has no process id"))?,
            ) {
                // Dropping `tree` closes the kill-on-close Job Object, so a
                // process whose primary thread cannot be resumed never leaks.
                return Err(error);
            }
            Ok(tree)
        }

        pub(super) fn terminate(&self) -> io::Result<()> {
            // SAFETY: this guard owns a live job handle.
            if unsafe { TerminateJobObject(self.job, 1) } != 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        pub(super) fn process_group(&self) -> i32 {
            // Windows has no process-group id in the Unix sense; the supervised
            // examiner is never enabled here (`input_supervision()` is
            // `Unsupported`), so this value is unused.
            0
        }
    }

    pub(super) fn rollback_failed_attach(child: &Child) {
        if let Some(process) = child.raw_handle() {
            // A configure_owned child is still suspended here and cannot run
            // cleanup of its own. TerminateProcess is the only safe rollback
            // if Job Object creation/assignment failed.
            unsafe {
                TerminateProcess(process as HANDLE, 1);
            }
        }
    }

    fn resume_primary_thread(pid: u32) -> io::Result<()> {
        // CreateProcess starts this process with exactly one suspended thread.
        // ToolHelp is used because std/Tokio intentionally expose the process
        // handle but not the primary-thread handle returned by CreateProcess.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        struct Snapshot(HANDLE);
        impl Drop for Snapshot {
            fn drop(&mut self) {
                unsafe { CloseHandle(self.0) };
            }
        }
        let snapshot = Snapshot(snapshot);
        let mut entry: THREADENTRY32 = unsafe { zeroed() };
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut found = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let resumed = unsafe { ResumeThread(thread) };
                unsafe { CloseHandle(thread) };
                if resumed == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            found = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "suspended child primary thread was not found",
        ))
    }

    impl Drop for OwnedProcessTree {
        fn drop(&mut self) {
            // KILL_ON_JOB_CLOSE is the final containment guarantee.
            unsafe { CloseHandle(self.job) };
        }
    }

    struct ProcessHandle(HANDLE);

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    fn open(pid: u32, access: u32) -> io::Result<ProcessHandle> {
        // SAFETY: OpenProcess validates pid/access and returns an owned handle.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(ProcessHandle(handle))
        }
    }

    pub(super) fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
        let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let mut created: FILETIME = unsafe { zeroed() };
        let mut exited: FILETIME = unsafe { zeroed() };
        let mut kernel: FILETIME = unsafe { zeroed() };
        let mut user: FILETIME = unsafe { zeroed() };
        // SAFETY: all FILETIME pointers are valid writable outputs.
        if unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        let birth_token = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        Ok(ProcessIdentity { pid, birth_token })
    }

    pub(super) fn force_terminate(pid: u32) -> io::Result<()> {
        let process = open(pid, PROCESS_TERMINATE)?;
        // SAFETY: handle carries PROCESS_TERMINATE access.
        if unsafe { TerminateProcess(process.0, 1) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(super) fn request_termination(_pid: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows daemon shutdown is protocol-driven",
        ))
    }

    pub(super) fn native_process_image_matches_path(
        _pid: u32,
        _expected: &std::path::Path,
    ) -> bool {
        true
    }
}

