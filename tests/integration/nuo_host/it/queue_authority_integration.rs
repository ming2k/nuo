#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end queue authority over the **real driver** (ADR-0144).
//!
//! Every other queue test drives `App` or `FollowUpQueue` in isolation: the
//! defect class this ADR names lives in the *seam* between the client
//! projection and the driver's record, which is exactly where a unit test
//! cannot stand. These tests assemble a real session (`bootstrap::assemble`),
//! run the real `SessionDriver` task, and speak to it over the real in-process
//! `AgentRequest` channel — so a verb that never learned to reach the driver,
//! or an ordering rule that only exists in a comment, fails here.
//!
//! The provider is a local mock: rounds complete deterministically, which is
//! what makes the queue boundary observable at all.

use std::sync::Arc;

use super::sandbox_once;
use nuo::UiBridge;
use nuo::composition::bootstrap::{self, BootstrapParams};
use nuo::composition::startup::ThreadStart;
use nuo_protocol::{AgentRequest, AgentResponse, RoundEvent};
use tokio::sync::mpsc;

struct HeadlessProbe;

#[async_trait::async_trait]
impl UiBridge for HeadlessProbe {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<nuo::CopyOutcome, String> {
        Err("probe: headless".to_string())
    }
}

fn params(project_root: std::path::PathBuf) -> BootstrapParams {
    let identity = nuo_protocol::AgentIdentity::new("probe", "queue probe");
    BootstrapParams {
        human_channel: None,
        identity: identity.clone(),
        preset: nuo_protocol::AgentRoleProfile::with_identity("probe", identity),
        ui: Arc::new(HeadlessProbe),
        startup: ThreadStart::Fresh,
        project_root: Some(project_root),
        role: None,
        unattended: false,
        confined: true,
        teardown_token: None,
        shared_config: None,
        shared_provider_usage: None,
        phase_sink: None,
    }
}

/// A driver task plus the client side of its two channels.
struct Harness {
    session_id: String,
    req_tx: mpsc::Sender<AgentRequest>,
    resp_rx: mpsc::UnboundedReceiver<AgentResponse>,
    driver: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start(project: std::path::PathBuf) -> Self {
        let mut boot = bootstrap::assemble(params(project))
            .await
            .expect("assemble succeeds");
        let session_id = boot.session.id().await;
        let req_tx = boot.req_tx.clone();
        let resp_rx = std::mem::replace(&mut boot.resp_rx, mpsc::unbounded_channel().1);
        let driver = tokio::spawn(boot.driver.run());
        Self {
            session_id,
            req_tx,
            resp_rx,
            driver,
        }
    }

    /// Wait for the next authoritative queue snapshot.
    async fn next_snapshot(&mut self) -> (Vec<nuo_protocol::QueuedMessage>, bool) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout_at(deadline, self.resp_rx.recv()).await {
                Ok(Some(AgentResponse::Round { scope, .. })) => {
                    if let RoundEvent::QueueUpdated { items, paused } = scope.into_event() {
                        return (items, paused);
                    }
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
        panic!("no QueueUpdated within the deadline");
    }
}

#[tokio::test]
async fn queue_snapshot_is_answerable_and_authoritative() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let mut h = Harness::start(tmp.path().join("p")).await;

    // Ask the driver what it holds. Before ADR-0144 §7 there was no such verb,
    // so a client attaching to a live session could only ever render an empty
    // queue and then be surprised by items shipping at the next boundary.
    h.req_tx
        .send(AgentRequest::QueueSnapshot {
            thread_id: h.session_id.clone(),
        })
        .await
        .expect("driver is running");

    let (items, paused) = h.next_snapshot().await;
    assert!(items.is_empty(), "a fresh session has no queued follow-ups");
    assert!(!paused, "and it is not paused");
    h.driver.abort();
}

/// The two verbs that make a client mutation *real* are answered by the driver.
///
/// `QueueRemove` on an unknown id must be a no-op that still re-publishes the
/// snapshot (the client's projection is rebuilt from the answer), and
/// `QueuePaused` must round-trip its flag. A client that sent these into a void
/// — the pre-ADR-0144 state, where they had zero call sites — would pass every
/// unit test and still duplicate prompts in production.
#[tokio::test]
async fn queue_verbs_round_trip_through_the_driver() {
    sandbox_once();
    let tmp = tempfile::tempdir().unwrap();
    let mut h = Harness::start(tmp.path().join("p")).await;

    // Idempotent remove: unknown id is a no-op, not an error.
    h.req_tx
        .send(AgentRequest::QueueRemove {
            thread_id: h.session_id.clone(),
            input_id: "never-queued".to_string(),
        })
        .await
        .expect("driver is running");
    let (items, paused) = h.next_snapshot().await;
    assert!(items.is_empty());
    assert!(!paused);

    // Pause round-trips its flag — and resuming an empty queue is still a
    // faithful answer (the wake rule must not turn it into a silent drop).
    h.req_tx
        .send(AgentRequest::QueuePaused {
            thread_id: h.session_id.clone(),
            paused: true,
        })
        .await
        .expect("driver is running");
    let (_, paused) = h.next_snapshot().await;
    assert!(paused, "the driver records the pause");

    h.req_tx
        .send(AgentRequest::QueuePaused {
            thread_id: h.session_id.clone(),
            paused: false,
        })
        .await
        .expect("driver is running");
    let (_, paused) = h.next_snapshot().await;
    assert!(!paused, "and it records the resume");

    h.driver.abort();
}
