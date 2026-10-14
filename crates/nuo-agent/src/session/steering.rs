use acp::{AgentAddress, SteerAction};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// Steering instructions waiting for in-flight requests.
///
/// Steering is consumed by the cognitive loop at **round boundaries**, which is
/// the only moment at which it can honestly take effect: a prompt already sent
/// to a provider cannot be amended, so mid-round injection is not physically
/// possible. Collecting instructions here lets the loop apply them at the next
/// boundary without the sender needing to know the loop's internals.
///
/// Keyed by the correlated request id, so guidance for one turn is never applied
/// to another that happens to be running on the same agent.
#[derive(Clone, Default)]
pub struct SteeringHandle {
    pending: Arc<Mutex<HashMap<Uuid, Vec<Instruction>>>>,
}

#[derive(Debug, Clone)]
struct Instruction {
    from: AgentAddress,
    text: String,
    action: SteerAction,
}

/// What the loop should do with the steering it drained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteeringEffect {
    /// Append these lines to the session before the next inference round.
    Notes(Vec<String>),
    /// Discard the round's planned tool calls and re-plan with these lines.
    Redirect(Vec<String>),
    /// Stop the turn and settle with the result so far.
    Cancel {
        /// Lines to record for attribution before settling.
        notes: Vec<String>,
    },
}

impl SteeringHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an instruction for `correlation`.
    ///
    /// `Cancel` clears any notes already queued for the same request, because a
    /// cancellation supersedes guidance whose purpose was to influence work that
    /// is now stopping.
    pub async fn push(
        &self,
        correlation: Uuid,
        from: AgentAddress,
        text: impl Into<String>,
        action: SteerAction,
    ) {
        let mut guard = self.pending.lock().await;
        let entry = guard.entry(correlation).or_default();

        if action.is_terminal() {
            entry.clear();
        } else {
            // A later note supersedes earlier ones: the sender is correcting
            // itself, and applying both would give contradictory guidance.
            entry.clear();
        }

        entry.push(Instruction {
            from,
            text: text.into(),
            action,
        });
    }

    /// Drains guidance for `correlation`, deciding what the loop should do.
    ///
    /// Returns `None` when nothing is pending, so an un-steered turn pays only a
    /// map lookup per round.
    pub async fn drain(&self, correlation: Uuid) -> Option<SteeringEffect> {
        let instructions = {
            let mut guard = self.pending.lock().await;
            guard.remove(&correlation)?
        };

        if instructions.is_empty() {
            return None;
        }

        // A terminal instruction outranks everything else, regardless of order:
        // once a sender cancels, continuing to reason would contradict them.
        if let Some(cancel) = instructions.iter().find(|i| i.action.is_terminal()) {
            return Some(SteeringEffect::Cancel {
                notes: vec![format!(
                    "[steering from {} (cancel)] {}",
                    cancel.from, cancel.text
                )],
            });
        }

        let lines: Vec<String> = instructions
            .iter()
            .map(|i| format!("[steering from {}] {}", i.from, i.text))
            .collect();

        // Redirect re-plans the round; a plain note merely informs it.
        if instructions
            .iter()
            .any(|i| i.action == SteerAction::Redirect)
        {
            Some(SteeringEffect::Redirect(lines))
        } else {
            Some(SteeringEffect::Notes(lines))
        }
    }

    /// Discards guidance for a finished or abandoned request.
    pub async fn clear(&self, correlation: Uuid) {
        self.pending.lock().await.remove(&correlation);
    }

    /// Number of requests with guidance waiting.
    pub async fn pending_requests(&self) -> usize {
        self.pending.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> AgentAddress {
        AgentAddress::parse("agent://local/caller").unwrap()
    }

    #[tokio::test]
    async fn unsteered_request_costs_nothing() {
        let handle = SteeringHandle::new();
        assert!(handle.drain(Uuid::new_v4()).await.is_none());
    }

    #[tokio::test]
    async fn a_note_is_applied_at_the_next_boundary() {
        let handle = SteeringHandle::new();
        let id = Uuid::new_v4();

        handle
            .push(id, addr(), "also handle the edge case", SteerAction::Note)
            .await;

        match handle.drain(id).await.unwrap() {
            SteeringEffect::Notes(lines) => {
                assert_eq!(lines.len(), 1);
                assert!(lines[0].contains("edge case"));
                assert!(lines[0].contains("agent://local/caller"));
            }
            other => panic!("expected Notes, got {other:?}"),
        }

        // Draining consumes it: the note must not be applied twice.
        assert!(handle.drain(id).await.is_none());
    }

    #[tokio::test]
    async fn guidance_is_scoped_to_its_request() {
        let handle = SteeringHandle::new();
        let mine = Uuid::new_v4();
        let other = Uuid::new_v4();

        handle
            .push(other, addr(), "not yours", SteerAction::Note)
            .await;

        // A different in-flight turn must not receive this guidance.
        assert!(handle.drain(mine).await.is_none());
        assert!(handle.drain(other).await.is_some());
    }

    #[tokio::test]
    async fn cancel_outranks_other_guidance() {
        let handle = SteeringHandle::new();
        let id = Uuid::new_v4();

        handle
            .push(id, addr(), "just a note", SteerAction::Note)
            .await;
        handle
            .push(id, addr(), "stop now", SteerAction::Cancel)
            .await;

        assert!(
            matches!(
                handle.drain(id).await.unwrap(),
                SteeringEffect::Cancel { .. }
            ),
            "a cancellation must win over pending notes"
        );
    }

    #[tokio::test]
    async fn newer_note_supersedes_older() {
        let handle = SteeringHandle::new();
        let id = Uuid::new_v4();

        handle
            .push(id, addr(), "wrong guess", SteerAction::Note)
            .await;
        handle
            .push(id, addr(), "corrected", SteerAction::Note)
            .await;

        match handle.drain(id).await.unwrap() {
            SteeringEffect::Notes(lines) => {
                assert_eq!(lines.len(), 1, "stale guidance must not stack");
                assert!(lines[0].contains("corrected"));
            }
            other => panic!("expected Notes, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn redirect_is_distinguished_from_a_note() {
        let handle = SteeringHandle::new();
        let id = Uuid::new_v4();

        handle
            .push(id, addr(), "wrong direction", SteerAction::Redirect)
            .await;

        assert!(matches!(
            handle.drain(id).await.unwrap(),
            SteeringEffect::Redirect(_)
        ));
    }
}
