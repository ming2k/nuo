//! Darwin (macOS) kernel-evidence supervised input examiner.
//!
//! Implements ADR-0294: multi-factor kernel introspection via `libproc` and
//! terminal buffer telemetry.

use super::InputWait;
use crate::process::PtyMaster;

const PROC_PIDLISTTHREADS: libc::c_int = 1;
const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;

const TH_STATE_RUNNING: i32 = 1;
const TH_STATE_WAITING: i32 = 3;

#[repr(C)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: i64,
    fi_type: i32,
    fi_guardflags: u32,
}

#[repr(C)]
struct VnodeFdInfoWithPath {
    #[allow(dead_code)]
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

/// Darwin kernel-evidence input examiner: tracks the stability gate across polls
/// and resolves the process group's wait state from XNU `libproc`.
#[derive(Default)]
pub(super) struct Examiner {
    /// Previous aggregate CPU ticks, for the no-progress half of the gate.
    prev_cpu_ticks: Option<u64>,
    /// Consecutive `Awaiting` samples with no CPU progress.
    stable_hits: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DarwinGroupState {
    /// No live process observed in the group.
    Gone,
    /// A process is running or runnable.
    Running,
    /// Blocked, but not on the harness-owned terminal (e.g. sleep, socket, lock).
    Otherwise,
    /// Blocked reading the harness-owned terminal — a real input wait.
    Awaiting,
}

pub(crate) struct DarwinGroupSample {
    pub(crate) state: DarwinGroupState,
    pub(crate) total_cpu_ticks: u64,
}

impl Examiner {
    pub(super) fn poll(&mut self, pgid: i32, master_tty: Option<&PtyMaster>) -> InputWait {
        let sample = classify_group_darwin(pgid, master_tty);
        match sample.state {
            DarwinGroupState::Awaiting => {
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
            DarwinGroupState::Running => {
                self.prev_cpu_ticks = Some(sample.total_cpu_ticks);
                self.stable_hits = 0;
                InputWait::Idle
            }
            DarwinGroupState::Otherwise | DarwinGroupState::Gone => {
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

/// Enumerate all processes in the process group and classify group state.
pub(crate) fn classify_group_darwin(
    pgid: i32,
    master_tty: Option<&PtyMaster>,
) -> DarwinGroupSample {
    let mut state = DarwinGroupState::Gone;
    let mut total_cpu_ticks = 0u64;

    // First check master tty queue: if there are pending unconsumed input bytes,
    // the child cannot be awaiting harness input.
    if let Some(tty) = master_tty
        && tty.pending_input_bytes().unwrap_or(0) > 0
    {
        return DarwinGroupSample {
            state: DarwinGroupState::Otherwise,
            total_cpu_ticks,
        };
    }

    let mut pids = [0 as libc::pid_t; 256];
    let bytes = unsafe {
        libc::proc_listpgrppids(
            pgid,
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
        )
    };
    if bytes <= 0 {
        return DarwinGroupSample {
            state,
            total_cpu_ticks,
        };
    }
    let count = (bytes as usize) / std::mem::size_of::<libc::pid_t>();

    for &pid in &pids[..count] {
        if pid <= 0 {
            continue;
        }
        let mut taskinfo: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                (&mut taskinfo as *mut libc::proc_taskinfo).cast(),
                std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int,
            )
        };
        if ret <= 0 {
            continue;
        }

        // Ticks converted from nanoseconds (100ns units to avoid overflow).
        let cpu_ticks =
            (taskinfo.pti_total_user.saturating_add(taskinfo.pti_total_system)) / 100_000;
        total_cpu_ticks = total_cpu_ticks.saturating_add(cpu_ticks);

        // If any thread in the task is runnable/running, the task is actively executing.
        let candidate = if taskinfo.pti_numrunning > 0 {
            DarwinGroupState::Running
        } else {
            classify_quiescent_darwin_process(pid)
        };

        state = match (state, candidate) {
            (DarwinGroupState::Awaiting, _) => DarwinGroupState::Awaiting,
            (_, DarwinGroupState::Awaiting) => DarwinGroupState::Awaiting,
            (_, DarwinGroupState::Running) => DarwinGroupState::Running,
            (DarwinGroupState::Gone, other) => other,
            (existing, _) => existing,
        };
    }

    DarwinGroupSample {
        state,
        total_cpu_ticks,
    }
}

/// Inspect a quiescent Darwin process to determine if it is blocked on its terminal slave.
fn classify_quiescent_darwin_process(pid: libc::pid_t) -> DarwinGroupState {
    // 1. Verify descriptor 0 is bound to a terminal vnode (/dev/ttys* or /dev/tty*).
    let mut vnode_fd: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        libc::proc_pidfdinfo(
            pid,
            0,
            PROC_PIDFDVNODEPATHINFO,
            (&mut vnode_fd as *mut VnodeFdInfoWithPath).cast(),
            std::mem::size_of::<VnodeFdInfoWithPath>() as libc::c_int,
        )
    };
    if ret <= 0 {
        return DarwinGroupState::Otherwise;
    }

    let raw_bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(vnode_fd.pvip.vip_path.as_ptr().cast::<u8>(), 1024)
    };
    let nul_pos = raw_bytes.iter().position(|&b| b == 0).unwrap_or(raw_bytes.len());
    let path = match std::str::from_utf8(&raw_bytes[..nul_pos]) {
        Ok(s) => s,
        Err(_) => return DarwinGroupState::Otherwise,
    };

    if !path.starts_with("/dev/ttys") && !path.starts_with("/dev/tty") {
        return DarwinGroupState::Otherwise;
    }

    // 2. Inspect threads to verify that at least one thread is in TH_STATE_WAITING
    // with pth_sleep_time > 0 and no thread is in TH_STATE_RUNNING.
    let mut thread_ids = [0u64; 64];
    let thread_bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            thread_ids.as_mut_ptr().cast(),
            (thread_ids.len() * std::mem::size_of::<u64>()) as libc::c_int,
        )
    };
    if thread_bytes <= 0 {
        return DarwinGroupState::Otherwise;
    }
    let thread_count = (thread_bytes as usize) / std::mem::size_of::<u64>();

    let mut has_waiting_thread = false;
    for &th_id in &thread_ids[..thread_count] {
        let mut th_info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTHREADINFO,
                th_id,
                (&mut th_info as *mut libc::proc_threadinfo).cast(),
                std::mem::size_of::<libc::proc_threadinfo>() as libc::c_int,
            )
        };
        if ret <= 0 {
            continue;
        }
        if th_info.pth_run_state == TH_STATE_RUNNING {
            return DarwinGroupState::Running;
        }
        if th_info.pth_run_state == TH_STATE_WAITING && th_info.pth_sleep_time > 0 {
            has_waiting_thread = true;
        }
    }

    if has_waiting_thread {
        DarwinGroupState::Awaiting
    } else {
        DarwinGroupState::Otherwise
    }
}
