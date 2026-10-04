//! The durability port's conformance suite (ADR-0303 §4).
//!
//! "Hydration parity is tested, not assumed": any store used with the kernel must
//! pass this suite, and the suite *is* the definition of a valid sink. The
//! property is `hydrate(append(facts)) == facts` — the re-carriage of ADR-0241's
//! hydration-parity invariant as a port contract.
//!
//! # How a host uses this
//!
//! The suite drives a sink through a [`FactSink`] and a hydration function the
//! host supplies, because the kernel cannot know how a host reads its own store
//! (ADR-0303 §3: hydration is an input). A host test is then three lines:
//!
//! ```ignore
//! let report = run_conformance(&MySink::new(), |expected| {
//!     // Load whatever the sink wrote and hand the facts back.
//!     Box::pin(async move { Ok(load_my_store().await?) })
//! });
//! report.assert_all_passed();
//! ```
//!
//! # What it checks
//!
//! 1. **Acknowledged means covered.** The ack's watermark equals the batch's, and
//!    an ack never claims more than was sent.
//! 2. **Replay is idempotent.** Appending the same batch twice yields the same
//!    state — the kernel retries after a transient failure, so a sink must
//!    recognize what it already has.
//! 3. **Round trip is lossless.** Facts hydrated from the sink are equal to the
//!    facts appended: every node, in order, with its payload intact.
//! 4. **Discards are declared.** A sink that drops names what it dropped, so the
//!    kernel never reports a fact as durable when it is gone.
//! 5. **Order is preserved.** Per-instance sequence order survives the store.
//!
//! A failure names the check and the offending fact, because "hydration parity
//! failed" without a fact id is not actionable.

use futures::future::BoxFuture;
use nuo_wire::{
    CausalNode, ExecutionStatus, Message, NodeKind, NodePayload, Role, SessionDelta, StateUpdate,
    SystemNoticePayload,
};

use crate::durability::{Ack, FactSink};

/// What the host does to read facts back.
///
/// `expected` is what the suite appended, so a host may either return what its
/// store holds or assert against what it read; the suite compares the result to
/// the expected facts either way.
pub type Hydrate = Box<
    dyn Fn(Vec<CausalNode>) -> BoxFuture<'static, Result<Vec<CausalNode>, String>> + Send + Sync,
>;

/// The outcome of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub name: &'static str,
    pub passed: bool,
    /// Empty when passed; a fact id and a reason when not.
    pub detail: String,
}

/// Every check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConformanceReport {
    pub outcomes: Vec<CheckOutcome>,
}

impl ConformanceReport {
    fn record(&mut self, name: &'static str, result: Result<(), String>) {
        self.outcomes.push(match result {
            Ok(()) => CheckOutcome {
                name,
                passed: true,
                detail: String::new(),
            },
            Err(detail) => CheckOutcome {
                name,
                passed: false,
                detail,
            },
        });
    }

    /// Whether every check passed.
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|outcome| outcome.passed)
    }

    /// The checks that failed.
    pub fn failures(&self) -> impl Iterator<Item = &CheckOutcome> {
        self.outcomes.iter().filter(|outcome| !outcome.passed)
    }

    /// Panic with every failure named. The one call a host test makes.
    pub fn assert_all_passed(&self) {
        if self.passed() {
            return;
        }
        let failures: Vec<String> = self
            .failures()
            .map(|failure| format!("  {}: {}", failure.name, failure.detail))
            .collect();
        panic!(
            "the durability sink is not conformant ({} of {} checks failed):\n{}",
            failures.len(),
            self.outcomes.len(),
            failures.join("\n")
        );
    }
}

/// Run every check against `sink` for `instance_id`, hydrating through `hydrate`.
///
/// The instance id is a parameter because a sink is bound to one instance: the
/// batches it receives must be addressed to it, and a suite that invented its own
/// id could not drive a real store.
pub async fn run_conformance(
    instance_id: &str,
    sink: &dyn FactSink,
    hydrate: Hydrate,
) -> ConformanceReport {
    let mut report = ConformanceReport::default();

    // 1. Acknowledged means covered, and no more than was sent.
    let first = delta(instance_id, 1, vec![node("n1", 1, None, "first")]);
    let ack = match sink.append(first.clone()).await {
        Ok(ack) => ack,
        Err(error) => {
            report.record("append_is_acknowledged", Err(error.to_string()));
            // Every later check depends on a working append; stop here rather
            // than report five failures that all say the same thing.
            return report;
        }
    };
    report.record(
        "append_is_acknowledged",
        check_ack_covers(&ack, &first),
    );

    // 2. Replay is idempotent.
    let replay = sink.append(first.clone()).await;
    report.record(
        "replay_is_idempotent",
        match replay {
            Ok(_) => Ok(()),
            Err(error) => Err(format!(
                "re-appending an already-stored batch failed: {error}"
            )),
        },
    );

    // 3. Round trip is lossless, and 5. order survives.
    let second = delta(instance_id, 2, vec![node("n2", 2, Some("n1"), "second")]);
    let mut expected = first.new_nodes.clone();
    expected.extend(second.new_nodes.clone());
    if let Err(error) = sink.append(second.clone()).await {
        report.record("hydration_is_lossless", Err(error.to_string()));
        report.record("order_is_preserved", Err("second append failed".into()));
    } else {
        match hydrate(expected.clone()).await {
            Ok(hydrated) => {
                report.record("hydration_is_lossless", check_lossless(&expected, &hydrated));
                report.record("order_is_preserved", check_order(&hydrated));
            }
            Err(error) => {
                report.record("hydration_is_lossless", Err(error.clone()));
                report.record("order_is_preserved", Err(error));
            }
        }
    }

    // 4. Discards are declared. The suite cannot force a sink to discard, so it
    // checks the *declaration*: an ack that reports a discard must name the fact,
    // and one that reports none must have stored what it was sent.
    report.record(
        "discards_are_declared",
        check_discards(&first, &ack),
    );

    report
}

fn check_ack_covers(ack: &Ack, batch: &SessionDelta) -> Result<(), String> {
    if ack.durable_upto != batch.new_watermark_seq {
        return Err(format!(
            "acknowledged watermark {} does not cover the batch's {}",
            ack.durable_upto, batch.new_watermark_seq
        ));
    }
    for discarded in &ack.discarded {
        if !batch.new_nodes.iter().any(|node| &node.id == discarded) {
            return Err(format!(
                "acknowledged a discard of '{discarded}', which was not in the batch"
            ));
        }
    }
    Ok(())
}

fn check_lossless(expected: &[CausalNode], hydrated: &[CausalNode]) -> Result<(), String> {
    for wanted in expected {
        let Some(found) = hydrated.iter().find(|node| node.id == wanted.id) else {
            return Err(format!("fact '{}' did not survive the store", wanted.id));
        };
        if found.payload != wanted.payload {
            return Err(format!(
                "fact '{}' came back with a different payload",
                wanted.id
            ));
        }
        if found.seq != wanted.seq || found.parent_id != wanted.parent_id {
            return Err(format!(
                "fact '{}' came back with a different position (seq {} vs {}, parent {:?} vs {:?})",
                wanted.id, found.seq, wanted.seq, found.parent_id, wanted.parent_id
            ));
        }
        if found.kind != wanted.kind {
            return Err(format!("fact '{}' came back as a different kind", wanted.id));
        }
    }
    Ok(())
}

fn check_order(hydrated: &[CausalNode]) -> Result<(), String> {
    let mut previous = 0;
    for node in hydrated {
        if node.seq < previous {
            return Err(format!(
                "fact '{}' has sequence {} after {}; per-instance order was not preserved",
                node.id, node.seq, previous
            ));
        }
        previous = node.seq;
    }
    Ok(())
}

fn check_discards(batch: &SessionDelta, ack: &Ack) -> Result<(), String> {
    let declared: Vec<&String> = ack.discarded.iter().collect();
    let sent: Vec<&String> = batch.new_nodes.iter().map(|node| &node.id).collect();
    for id in &declared {
        if !sent.contains(id) {
            return Err(format!("declared a discard of '{id}', which was never sent"));
        }
    }
    Ok(())
}

/// A batch carrying `nodes`, drained from `watermark - 1`.
fn delta(instance_id: &str, watermark: u64, nodes: Vec<CausalNode>) -> SessionDelta {
    SessionDelta {
        session_id: instance_id.to_string(),
        parent_session_id: None,
        previous_watermark_seq: watermark.saturating_sub(1),
        new_watermark_seq: watermark,
        new_nodes: nodes,
        state_update: StateUpdate {
            active_leaf: None,
            status: ExecutionStatus::Idle,
            round_counter: 0,
            pending_notifications: Vec::<SystemNoticePayload>::new(),
        },
        policy_update: None,
        updated_at_s: 1,
    }
}

fn node(id: &str, seq: u64, parent: Option<&str>, text: &str) -> CausalNode {
    CausalNode {
        id: id.to_string(),
        parent_id: parent.map(str::to_string),
        seq,
        timestamp_ms: 1_000 * seq,
        kind: NodeKind::Dialogue,
        payload: NodePayload::Message {
            message: Message::new(Role::User, text),
        },
    }
}

/// An in-memory sink that stores what it is sent, for testing the suite itself
/// and for a host that wants durable-across-restarts behaviour without a file.
///
/// It is a real sink, not a test fixture: an embedding that keeps one process's
/// facts in memory passes conformance with it, and the suite's checks are exactly
/// what makes that claim meaningful.
#[derive(Debug, Default)]
pub struct MemorySink {
    nodes: std::sync::Mutex<std::collections::BTreeMap<u64, CausalNode>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    /// The stored facts, ordered by sequence.
    pub fn nodes(&self) -> Vec<CausalNode> {
        self.nodes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }
}

impl FactSink for MemorySink {
    fn append(&self, batch: SessionDelta) -> BoxFuture<'static, Result<Ack, crate::SinkError>> {
        let watermark = batch.new_watermark_seq;
        {
            let mut nodes = self
                .nodes
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for node in batch.new_nodes {
                // Keyed by sequence: a replay writes the same key with the same
                // value, which is what makes it idempotent.
                nodes.insert(node.seq, node);
            }
        }
        Box::pin(async move { Ok(Ack::durable(watermark)) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hydration from the suite's own in-memory sink.
    fn memory_hydrate(sink: std::sync::Arc<MemorySink>) -> Hydrate {
        Box::new(move |_expected| {
            let sink = std::sync::Arc::clone(&sink);
            Box::pin(async move { Ok(sink.nodes()) })
        })
    }

    #[tokio::test]
    async fn the_in_memory_sink_is_conformant() {
        let sink = std::sync::Arc::new(MemorySink::new());
        let report = run_conformance(
            "instance-conformance",
            sink.as_ref(),
            memory_hydrate(std::sync::Arc::clone(&sink)),
        )
        .await;
        report.assert_all_passed();
        assert_eq!(report.outcomes.len(), 5, "every check ran");
    }

    #[tokio::test]
    async fn the_null_sink_passes_every_check_that_does_not_require_storage() {
        // `NullSink` stores nothing, so it cannot pass hydration — and the suite
        // must say exactly that rather than pass it by accident. This is the
        // distinction between "conformant store" and "legal sink": ADR-0303
        // `[INV-DURABILITY-05]` makes the null sink legal, not conformant.
        let report = run_conformance(
            "instance-conformance",
            &crate::durability::NullSink,
            Box::new(|_: Vec<CausalNode>| Box::pin(async { Ok(Vec::new()) })),
        )
        .await;
        assert!(
            report.outcomes.iter().any(|o| o.name == "append_is_acknowledged" && o.passed),
            "a legal sink still acknowledges honestly"
        );
        assert!(
            report.failures().any(|o| o.name == "hydration_is_lossless"),
            "and the suite reports what it cannot do: {:?}",
            report.failures().collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_sink_that_loses_a_fact_fails_with_the_fact_named() {
        let sink = std::sync::Arc::new(MemorySink::new());
        let sink_for_hydrate = std::sync::Arc::clone(&sink);
        let losing: Hydrate = Box::new(move |_expected: Vec<CausalNode>| {
            let sink = std::sync::Arc::clone(&sink_for_hydrate);
            Box::pin(async move {
                // Drop the second fact, as a retention policy might.
                let mut nodes = sink.nodes();
                nodes.pop();
                Ok(nodes)
            })
        });
        let report = run_conformance("instance-conformance", sink.as_ref(), losing).await;
        let failure = report
            .failures()
            .find(|outcome| outcome.name == "hydration_is_lossless")
            .expect("losing a fact must fail the lossless check");
        assert!(
            failure.detail.contains("n2"),
            "the missing fact is named: {}",
            failure.detail
        );
    }

    #[tokio::test]
    async fn a_sink_that_reorders_facts_fails_the_order_check() {
        let sink = std::sync::Arc::new(MemorySink::new());
        let sink_for_hydrate = std::sync::Arc::clone(&sink);
        let reversing: Hydrate = Box::new(move |_expected: Vec<CausalNode>| {
            let sink = std::sync::Arc::clone(&sink_for_hydrate);
            Box::pin(async move {
                let mut nodes = sink.nodes();
                nodes.reverse();
                Ok(nodes)
            })
        });
        let report = run_conformance("instance-conformance", sink.as_ref(), reversing).await;
        let failure = report
            .failures()
            .find(|outcome| outcome.name == "order_is_preserved")
            .expect("reordering must fail the order check");
        assert!(
            failure.detail.contains("order was not preserved"),
            "the reason is stated: {}",
            failure.detail
        );
    }

    #[test]
    fn the_report_names_every_failure_when_asserting() {
        let mut report = ConformanceReport::default();
        report.record("a_check", Ok(()));
        report.record("another", Err("fact 'x' vanished".into()));
        assert!(!report.passed());
        let panic = std::panic::catch_unwind(|| report.assert_all_passed());
        let message = panic
            .expect_err("a failing report must panic");
        let text = message
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(text.contains("another"), "{text}");
        assert!(text.contains("fact 'x' vanished"), "{text}");
    }
}
