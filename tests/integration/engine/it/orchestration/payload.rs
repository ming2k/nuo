#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Request-body (byte) pressure: a limit tokens cannot see, and its relief.
//!
//! The reported failure was a 55 MB body built from 67 screenshots inside a
//! million-token window: every token budget said healthy, the edge said
//! `413 request_too_large`, and the harness answered by withholding **all**
//! images without ever latching the reason. These tests pin the two halves of
//! the fix:
//!
//! - the pre-turn gate offloads the *oldest* attachments before the bytes ever
//!   leave, keeping the newest ones the conversation is about;
//! - a refusal that slips through is classified as payload pressure, relived
//!   proportionally, and retried — it never arms the vision-capability probe.

use super::*;

/// A provider with a hard request-body ceiling, the way every edge in front of
/// a real provider has one.
struct BodyCeilingProvider {
    /// Bytes this endpoint will accept.
    limit: usize,
    /// Attachment count per request, in order.
    images_per_request: Arc<Mutex<Vec<usize>>>,
    /// Measured body size per request, in order.
    bodies: Arc<Mutex<Vec<usize>>>,
}

#[async_trait]
impl Provider for BodyCeilingProvider {
    async fn chat(
        &self,
        _request: &protocol::ModelRequest,
    ) -> Result<protocol::ProviderCompletion, protocol::ProviderError> {
        Err(protocol::ProviderError::new(
            "mock",
            protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, protocol::ProviderError>>,
        protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: &protocol::ModelRequest,
        _telemetry: protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<ProviderStreamEvent, protocol::ProviderError>>,
        protocol::ProviderError,
    > {
        let images = request
            .messages
            .iter()
            .filter_map(|message| message.images.as_ref().map(Vec::len))
            .sum::<usize>();
        let size = protocol::request_wire_size(request);
        self.images_per_request.lock().unwrap().push(images);
        self.bodies.lock().unwrap().push(size.body_bytes);
        if size.body_bytes > self.limit {
            let error = protocol::ProviderError::new(
                "mock",
                protocol::ProviderErrorKind::PayloadTooLarge,
                "OpenAI HTTP 413 Payload Too Large: [code=request_too_large] request body exceeds configured limit",
            )
            .with_status(413);
            return Ok(Box::pin(stream::iter(vec![Err(error)])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderStreamEvent::TextDelta("read it".to_string())),
            Ok(ProviderStreamEvent::Completed(
                protocol::ProviderCompletionMeta::default(),
            )),
        ])))
    }
}

fn attachments(count: usize, bytes: usize) -> Vec<tool::ImagePart> {
    (0..count)
        .map(|_| tool::ImagePart {
            // Valid base64 so the artifact writer persists the pixels; the
            // content is irrelevant, only its size is.
            mime: "image/png".to_string(),
            data: "QUJD".repeat(bytes / 4),
        })
        .collect()
}

fn payload_round_context(
    agent: &Arc<Agent>,
    tx: mpsc::UnboundedSender<AgentResponse>,
    session: &Arc<ThreadSession>,
    session_id: &str,
) -> RoundContext {
    RoundContext {
        agent: Arc::clone(agent),
        tx,
        token: CancellationToken::new(),
        session_id: session_id.to_string(),
        session: Arc::clone(session),
        projection: ContextProjectionSettings {
            request_overhead_tokens: 0,
            budget: protocol::CompactionPolicy::default().resolve(1_000_000),
            preserve_rounds: 6,
            summarize: false,
            prune: false,
            prune_protect_tokens: 0,
        },
        retry_max_attempts: 3,
        stream_restart_max_attempts: 2,
        retry_base_ms: 1,
        retry_max_ms: 10,
        emit_round_completed: false,
    }
}

fn payload_round_input(prompt: &str, images: Vec<tool::ImagePart>) -> RoundInput {
    RoundInput {
        prompt: prompt.to_string(),
        hidden: false,
        display_prompt: None,
        sent_at_ms: None,
        images,
        driver: engine::orchestration::RoundDriver::Fresh,
    }
}

/// Install the mid-turn projection gate the pre-turn payload relief needs.
fn install_gate(agent: &Arc<Agent>, session: &Arc<ThreadSession>, provider: Arc<dyn Provider>) {
    let window = 1_000_000;
    agent.set_context_projection_gate(Some(Arc::new(engine::PruningProjectionGate::new(
        Arc::clone(session),
        Arc::new(move || ContextProjectionSettings::for_window(window)),
        agent.token_weights_handle(),
        Some(provider),
    ))));
}

#[tokio::test]
async fn the_pre_turn_gate_offloads_the_oldest_attachments_before_the_bytes_leave() {
    let directory = std::env::temp_dir().join(format!("nuo-payload-pre-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let provider = Arc::new(BodyCeilingProvider {
        limit: 250_000,
        images_per_request: Arc::new(Mutex::new(Vec::new())),
        bodies: Arc::new(Mutex::new(Vec::new())),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        engine::AgentIdentity::default(),
    ));
    agent.set_request_body_limit(250_000);
    install_gate(&agent, &session, provider.clone());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        payload_round_context(&agent, tx, &session, &session_id),
        payload_round_input("what changed on this screen?", attachments(6, 100_000)),
    )
    .await
    .expect("the gate must relieve the body before the provider sees it");

    let bodies = provider.bodies.lock().unwrap().clone();
    assert_eq!(
        bodies.len(),
        1,
        "one request: the gate fired before sending"
    );
    assert!(
        bodies[0] <= 250_000,
        "the request must fit the ceiling, got {} bytes",
        bodies[0]
    );
    let images = provider.images_per_request.lock().unwrap().clone();
    assert!(
        images[0] < 6,
        "older attachments must be offloaded, got {} images",
        images[0]
    );
    assert!(images[0] >= 1, "the newest attachment must survive");

    // The projection persisted what it dropped: the invoice's artifact path
    // must name a file that really holds the pixels, or the model's only
    // recovery is a lie.
    let window = session.model_window().await;
    let invoice = window
        .iter()
        .find(|message| message.content.contains("[Image Offloaded:"))
        .expect("the projection replaced the evicted attachments with an invoice");
    let path = invoice
        .content
        .split("Artifact Path: \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("the invoice names an artifact path");
    let bytes = std::fs::read(path).expect("the artifact the invoice names was written");
    assert!(
        !bytes.is_empty(),
        "the persisted artifact must carry the attachment's bytes"
    );

    // A size verdict is not a capability verdict.
    assert!(
        !agent.images_withheld_from_current_route(),
        "payload pressure must not latch image suppression"
    );

    // The user is told what happened, and where the pixels went.
    let notices = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|response| match response {
            AgentResponse::Round { scope, .. } => match scope.into_event() {
                RoundEvent::Notice(notice) => Some(notice),
                _ => None,
            },
            _ => None,
        })
        .filter(|notice| notice.kind == tool::NoticeKind::PayloadRelief)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "exactly one payload relief notice");
    assert!(
        notices[0]
            .body
            .as_deref()
            .is_some_and(|body| body.contains("artifact path")),
        "the notice must say how to get an offloaded image back: {:?}",
        notices[0].body
    );
}

#[tokio::test]
async fn a_body_too_large_refusal_is_relieved_and_retried_proportionally() {
    let directory = std::env::temp_dir().join(format!("nuo-payload-413-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    // The configured ceiling is *above* what this edge accepts: the first
    // request is therefore attempted and refused, which is exactly the case the
    // 413 recovery exists for.
    let provider = Arc::new(BodyCeilingProvider {
        limit: 250_000,
        images_per_request: Arc::new(Mutex::new(Vec::new())),
        bodies: Arc::new(Mutex::new(Vec::new())),
    });
    let agent = Arc::new(Agent::new(
        provider.clone(),
        Vec::new(),
        engine::AgentIdentity::default(),
    ));
    agent.set_request_body_limit(1_000_000);
    install_gate(&agent, &session, provider.clone());
    let (tx, _rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        payload_round_context(&agent, tx, &session, &session_id),
        payload_round_input("compare these screens", attachments(6, 100_000)),
    )
    .await
    .expect("the 413 must be recovered, not surfaced");

    let bodies = provider.bodies.lock().unwrap().clone();
    assert!(
        bodies.len() >= 2,
        "the refusal must be retried, got {bodies:?}"
    );
    assert!(bodies[0] > 250_000, "the first attempt is the real body");
    assert!(
        *bodies.last().unwrap() <= 250_000,
        "the final attempt must fit the edge, got {bodies:?}"
    );
    assert!(
        bodies.windows(2).all(|pair| pair[1] < pair[0]),
        "each relief pass must be decisive, got {bodies:?}"
    );
    let images = provider.images_per_request.lock().unwrap().clone();
    assert_eq!(images[0], 6, "the first attempt carried every attachment");
    assert_eq!(
        *images.last().unwrap(),
        2,
        "relief keeps the newest attachments that fit"
    );
    assert!(
        images.windows(2).all(|pair| pair[1] <= pair[0]),
        "relief only ever removes attachments, got {images:?}"
    );
    assert!(
        agent.effective_request_body_limit() < 1_000_000,
        "the refusal must lower the route's ceiling for later rounds"
    );
    assert!(
        !agent.images_withheld_from_current_route(),
        "a payload verdict must not disable vision for the route"
    );
}
