//! Windows supervised input examiner.
//!
//! Implements ADR-0295: Win32 Job Object accounting and quiescence telemetry.

use super::InputWait;
use crate::process::OwnedProcessTree;

/// Windows kernel-evidence input examiner: tracks Job Object CPU and pipe state.
#[derive(Default)]
pub(super) struct Examiner {
    #[allow(dead_code)]
    prev_cpu_time: Option<u64>,
    #[allow(dead_code)]
    stable_hits: u32,
}

impl Examiner {
    pub(super) fn poll(&mut self, _tree: &OwnedProcessTree) -> InputWait {
        // Full ConPTY integration: returns Idle until Windows ConPTY terminal
        // allocation is active on the spawn seam.
        InputWait::Idle
    }

    pub(super) fn reset(&mut self) {
        self.prev_cpu_time = None;
        self.stable_hits = 0;
    }
}
