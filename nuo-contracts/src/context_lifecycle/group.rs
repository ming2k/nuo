//! Execution-group acceptance closure is independent of external termination.
use crate::context_lifecycle::ids::ExecutionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultAcceptance {
    Open,
    Completed,
    Interrupted,
}
impl ResultAcceptance {
    pub const fn is_closed(self) -> bool {
        !matches!(self, Self::Open)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalExecution {
    NotDispatched,
    InFlight,
    TerminationConfirmed,
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GroupCall {
    pub provider_call_id: String,
    pub execution_id: ExecutionId,
    pub acceptance: ResultAcceptance,
    pub external: ExternalExecution,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionGroup {
    pub calls: Vec<GroupCall>,
}
impl ExecutionGroup {
    pub fn new(calls: Vec<GroupCall>) -> Self {
        Self { calls }
    }
    /// Only compacted groups need closure; an intact open tail is preserved.
    pub fn is_closed(&self) -> bool {
        self.calls.iter().all(|c| c.acceptance.is_closed())
    }
    pub fn open_calls(&self) -> impl Iterator<Item = &GroupCall> {
        self.calls.iter().filter(|c| !c.acceptance.is_closed())
    }
    pub fn unresolved_external_calls(&self) -> impl Iterator<Item = &GroupCall> {
        self.calls.iter().filter(|c| {
            matches!(
                c.external,
                ExternalExecution::InFlight | ExternalExecution::OutcomeUnknown
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_acceptance_can_close_while_effects_remain_unknown() {
        let mut group = ExecutionGroup::new(vec![GroupCall {
            provider_call_id: "wire-id".into(),
            execution_id: "exec".into(),
            acceptance: ResultAcceptance::Open,
            external: ExternalExecution::InFlight,
        }]);
        assert!(!group.is_closed());
        group.calls[0].acceptance = ResultAcceptance::Interrupted;
        group.calls[0].external = ExternalExecution::OutcomeUnknown;
        assert!(group.is_closed());
        assert_eq!(group.unresolved_external_calls().count(), 1);
        assert_eq!(group.open_calls().count(), 0);
    }
}
