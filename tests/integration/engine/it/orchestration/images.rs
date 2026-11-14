//! Route image refusal: learn, probe, and continue without attachments.

use super::*;

// ─────────────────────────────────────────────────────────────────────────────
// Route image refusal
// ─────────────────────────────────────────────────────────────────────────────

/// A provider that refuses the first N requests the way a relay that cannot take
/// attachments does, then serves every later request — while recording how many
/// attachments each request carried.
///
/// `rejection` is the vendor's error text. The harness never reads it: the
/// recovery is decided by the *outcome* of retrying without the attachments
/// so these tests can use prose the harness has no knowledge of.
struct ImageRejectingProvider {
    /// Attachment count seen per request, in order. Its length is also the
    /// attempt counter.
    images_per_request: Arc<Mutex<Vec<usize>>>,
    rejection: &'static str,
    /// How many requests to refuse; `usize::MAX` for a route that never works.
    rejections: usize,
}

#[async_trait]
impl Provider for ImageRejectingProvider {
    async fn chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        Err(nuo_protocol::ProviderError::new(
            "mock",
            nuo_protocol::ProviderErrorKind::Other,
            "non-streaming path should not be used",
        ))
    }

    async fn stream_chat(
        &self,
        _request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        Ok(Box::pin(stream::empty()))
    }

    async fn stream_chat_events(
        &self,
        request: &nuo_protocol::ModelRequest,
        _telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            Result<ProviderStreamEvent, nuo_protocol::ProviderError>,
        >,
        nuo_protocol::ProviderError,
    > {
        let images = request
            .messages
            .iter()
            .filter_map(|message| message.images.as_ref().map(Vec::len))
            .sum::<usize>();
        let attempt = {
            let mut log = self
                .images_per_request
                .lock()
                .expect("image log lock poisoned");
            log.push(images);
            log.len()
        };
        if attempt <= self.rejections {
            let error = nuo_protocol::ProviderError::new(
                "mock",
                nuo_protocol::ProviderErrorKind::InvalidRequest,
                self.rejection,
            )
            .with_status(400);
            return Ok(Box::pin(stream::iter(vec![Err(error)])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderStreamEvent::TextDelta("read it".to_string())),
            Ok(ProviderStreamEvent::Completed(
                nuo_protocol::ProviderCompletionMeta::default(),
            )),
        ])))
    }
}

/// Switching to a model that cannot see images must not brick the session
///
/// The durable transcript keeps the image the user pasted while a multimodal
/// model was active — history is append-only, so no projection may
/// rewrite it. What changes is the *request*: the refusal teaches the harness
/// that this route cannot take attachments (because withholding them made the
/// identical turn succeed), and later requests stop carrying them.
#[tokio::test]
async fn image_refusal_is_learned_and_the_round_continues_without_images() {
    let directory = std::env::temp_dir().join(format!("nuo-image-learn-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let images_per_request = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(ImageRejectingProvider {
            images_per_request: Arc::clone(&images_per_request),
            rejection: "mock HTTP 400: unknown variant `image_url`, expected `text`",
            rejections: 1,
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;
    let images = vec![nuo_protocol::ImagePart {
        mime: "image/png".to_string(),
        data: "aGk=".to_string(),
    }];

    execute_round(
        image_round_context(&agent, tx.clone(), &session, &session_id),
        image_round_input("what is in this picture?", images.clone()),
    )
    .await
    .expect("the round must recover, not fail");

    // Attempt 1 carried the image; the learned retry carried none.
    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0],
        "attempt 1 carries the attachment, the probed retry carries none"
    );

    // The image is still in the durable window: withholding is a projection,
    // never a history rewrite.
    let window = session.model_window().await;
    assert!(
        window.iter().any(|message| message
            .images
            .as_ref()
            .is_some_and(|images| !images.is_empty())),
        "the transcript must keep the pasted image: {window:?}"
    );

    // The user is told what happened, and how to override it.
    let notices = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|response| match response {
            AgentResponse::Round { scope, .. } => match scope.into_event() {
                RoundEvent::Notice(notice) => Some(notice),
                _ => None,
            },
            _ => None,
        })
        .filter(|notice| notice.kind == nuo_protocol::NoticeKind::ImageInputWithheld)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1, "exactly one withholding notice");
    assert!(
        notices[0]
            .body
            .as_deref()
            .is_some_and(|body| body.contains("history") && body.contains("model editor")),
        "the notice must explain that history keeps the image and name the override: {:?}",
        notices[0].body
    );
    assert!(agent.images_withheld_from_current_route());

    // A second round on the same route must not walk into the wall again.
    execute_round(
        image_round_context(&agent, tx, &session, &session_id),
        image_round_input("and now, in words?", Vec::new()),
    )
    .await
    .expect("a latched route must complete without another refusal");

    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0, 0],
        "the second round sends one request, already without the history's image"
    );
}

/// Recognition must not depend on any vendor's error format.
///
/// This provider refuses with prose the harness has no knowledge of — the shape
/// of every relay whose wording nobody anticipated. The recovery rests entirely
/// on the differential probe: retry the identical turn without the attachments
/// and observe the outcome. Recognition by *experiment*, not by reading the
/// vendor's envelope.
#[tokio::test]
async fn an_unrecognized_refusal_is_probed_and_confirmed_by_outcome() {
    let directory = std::env::temp_dir().join(format!("nuo-image-probe-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let images_per_request = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(ImageRejectingProvider {
            images_per_request: Arc::clone(&images_per_request),
            rejection: "mock HTTP 400: unsupported content type for the second message part",
            rejections: 1,
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    execute_round(
        image_round_context(&agent, tx, &session, &session_id),
        image_round_input(
            "describe it",
            vec![nuo_protocol::ImagePart {
                mime: "image/png".to_string(),
                data: "aGk=".to_string(),
            }],
        ),
    )
    .await
    .expect("the probe must recover a refusal whose wording means nothing to us");

    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0],
        "the probe re-sent the same turn without the attachments"
    );
    assert!(
        agent.images_withheld_from_current_route(),
        "a successful probe is the evidence that latches the route"
    );

    // The notice reports the empirical finding, not a claim about what the
    // provider said: the harness never read the provider's wording.
    let notices = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|response| match response {
            AgentResponse::Round { scope, .. } => match scope.into_event() {
                RoundEvent::Notice(notice) => Some(notice),
                _ => None,
            },
            _ => None,
        })
        .filter(|notice| notice.kind == nuo_protocol::NoticeKind::ImageInputWithheld)
        .collect::<Vec<_>>();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0]
            .body
            .as_deref()
            .is_some_and(|body| body.contains("retrying it without the attachments succeeded")),
        "the notice must describe the empirical finding: {:?}",
        notices[0].body
    );
}

/// When the probe **disproves** the hypothesis, nothing may be latched and the
/// user must see the refusal of the request they actually composed.
///
/// The refusal here is unrelated to images — it recurs with the attachments
/// withheld — and its text even *mentions* `image_url`, exactly like a relay
/// that echoes part of the payload it rejected. That is deliberate: it proves no
/// amount of vendor prose can latch anything, because only the probe's outcome
/// is evidence (INV-VISION-6). Were text allowed to decide, this route would
/// silently lose every future image.
#[tokio::test]
async fn an_unrelated_refusal_is_not_blamed_on_images() {
    let directory =
        std::env::temp_dir().join(format!("nuo-image-disprove-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let session = Arc::new(ThreadSession::for_path(directory.join("session.json")));
    let images_per_request = Arc::new(Mutex::new(Vec::new()));
    let agent = Arc::new(Agent::new(
        Arc::new(ImageRejectingProvider {
            images_per_request: Arc::clone(&images_per_request),
            rejection: "mock HTTP 400: invalid tool schema for 'write'; the payload echoes \
                        image_url parts",
            rejections: usize::MAX,
        }),
        Vec::new(),
        nuo_harness::AgentIdentity::default(),
    ));
    let (tx, _rx) = mpsc::unbounded_channel();
    let session_id = session.id().await;

    let error = execute_round(
        image_round_context(&agent, tx, &session, &session_id),
        image_round_input(
            "describe it",
            vec![nuo_protocol::ImagePart {
                mime: "image/png".to_string(),
                data: "aGk=".to_string(),
            }],
        ),
    )
    .await
    .expect_err("an unrelated refusal must remain terminal");

    assert!(
        error
            .to_string()
            .contains("invalid tool schema for 'write'"),
        "the original refusal must surface, got: {error}"
    );
    assert!(
        !agent.images_withheld_from_current_route(),
        "a disproved hypothesis must not latch anything"
    );
    assert_eq!(
        *images_per_request.lock().expect("image log lock poisoned"),
        vec![1, 0],
        "exactly one probe, and no further retries"
    );
}

/// The `RoundContext` every test runs with: a tight retry budget so a
/// probe is unmistakable in the request log.
fn image_round_context(
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
            budget: nuo_protocol::CompactionPolicy::default().resolve(100_000),
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

/// The `RoundInput` every test runs with: a fresh round carrying
/// `images`.
fn image_round_input(prompt: &str, images: Vec<nuo_protocol::ImagePart>) -> RoundInput {
    RoundInput {
        prompt: prompt.to_string(),
        hidden: false,
        display_prompt: None,
        sent_at_ms: None,
        images,
        driver: nuo_harness::orchestration::RoundDriver::Fresh,
    }
}
