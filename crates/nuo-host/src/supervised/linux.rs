//! Linux kernel-evidence supervised input examiner.
//!
//! Tracks the stability gate across polls and resolves the process group's
//! wait state from `/proc/<pid>/wchan` and `/proc/<pid>/fd/0`.

use super::InputWait;

/// Kernel-evidence input examiner: tracks the stability gate across polls and
/// resolves the process group's wait state from `/proc`.
#[derive(Default)]
pub(super) struct Examiner {
    /// Previous aggregate CPU ticks, for the no-progress half of the gate.
    prev_cpu_ticks: Option<u64>,
    /// Consecutive `Awaiting` samples with no CPU progress.
    stable_hits: u32,
}

impl Examiner {
    pub(super) fn poll(
        &mut self,
        pgid: i32,
        master_tty: Option<&crate::process::PtyMaster>,
    ) -> InputWait {
        if let Some(tty) = master_tty
            && tty.pending_input_bytes().unwrap_or(0) > 0
        {
            self.stable_hits = 0;
            return InputWait::Idle;
        }
        let sample = classify_group(pgid);
        match sample.state {
            GroupState::Awaiting => {
                let progressed = self
                    .prev_cpu_ticks
                    .is_some_and(|prev| sample.total_cpu_ticks > prev);
                self.stable_hits = if progressed { 0 } else { self.stable_hits + 1 };
                self.prev_cpu_ticks = Some(sample.total_cpu_ticks);
                if self.stable_hits >= 2 {
                    InputWait::Awaiting
                } else {
                    InputWait::Idle
                }
            }
            GroupState::Running => {
                self.prev_cpu_ticks = Some(sample.total_cpu_ticks);
                self.stable_hits = 0;
                InputWait::Idle
            }
            GroupState::Otherwise | GroupState::Gone => {
                self.stable_hits = 0;
                InputWait::Idle
            }
        }
    }

    pub(super) fn reset(&mut self) {
        self.prev_cpu_ticks = None;
        self.stable_hits = 0;
    }
}

/// Aggregate group state from kernel scheduling evidence. Output is agnostic to
/// the reason for a stall (a compiling build and a password prompt both emit
/// nothing), so "is this command waiting for input?" is a question about
/// process state, not stream contents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GroupState {
    /// No live process observed in the group.
    Gone,
    /// A process is running or runnable.
    Running,
    /// Blocked, but not on the harness-owned terminal (or unidentifiable
    /// because the kernel restricts `wchan`). Ambiguous: the caller must not
    /// fast-fail on this.
    Otherwise,
    /// Blocked reading the harness-owned terminal — a real input wait.
    Awaiting,
}

#[derive(Clone, Copy)]
pub(crate) struct GroupSample {
    pub(crate) state: GroupState,
    pub(crate) total_cpu_ticks: u64,
}

/// Scan the process group for kernel scheduling evidence, resolving the most
/// informative state in the priority order `Awaiting > Running > Otherwise >
/// Gone`, and summing CPU ticks across the group.
pub(crate) fn classify_group(pgid: i32) -> GroupSample {
    let mut state = GroupState::Gone;
    let mut total_cpu_ticks = 0u64;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return GroupSample {
            state,
            total_cpu_ticks,
        };
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = name.parse::<libc::pid_t>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // The comm field may contain spaces and ')'; split after its last ') '.
        let Some((_, tail)) = stat.rsplit_once(") ") else {
            continue;
        };
        let mut fields = tail.split_whitespace();
        let Some(proc_state) = fields.next() else {
            continue;
        };
        let _ppid = fields.next();
        let Some(proc_pgid) = fields.next().and_then(|s| s.parse::<libc::pid_t>().ok()) else {
            continue;
        };
        if proc_pgid != pgid {
            continue;
        }
        // Fields after pgid: skip 8 to reach utime, then stime.
        let mut rest = fields.skip(8);
        let utime = rest.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let stime = rest.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        total_cpu_ticks += utime + stime;

        let candidate = if proc_state != "S" {
            GroupState::Running
        } else {
            classify_sleeping_process(pid)
        };
        state = match (state, candidate) {
            (GroupState::Awaiting, _) => GroupState::Awaiting,
            (_, GroupState::Awaiting) => GroupState::Awaiting,
            (_, GroupState::Running) => GroupState::Running,
            (GroupState::Gone, other) => other,
            (existing, _) => existing,
        };
    }
    GroupSample {
        state,
        total_cpu_ticks,
    }
}

/// Resolve one sleeping process's wait state from `/proc/<pid>/wchan` (the
/// kernel function it sleeps in) and `/proc/<pid>/fd/0` (the channel fd 0
/// resolves to).
pub(crate) fn classify_sleeping_process(pid: libc::pid_t) -> GroupState {
    let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
    let wchan = wchan.trim();
    if wchan.is_empty() || wchan == "0" {
        return GroupState::Otherwise;
    }
    if matches!(wchan, "n_tty_read" | "wait_woken") {
        return GroupState::Awaiting;
    }
    let fd0 = std::fs::read_link(format!("/proc/{pid}/fd/0"))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stdin_is_pipe = fd0.starts_with("pipe:") || fd0 == "pipe";
    if matches!(
        wchan,
        "pipe_wait_readable" | "anon_pipe_read" | "pipe_read" | "wait_for_partner"
    ) && stdin_is_pipe
    {
        return GroupState::Awaiting;
    }
    GroupState::Otherwise
}
