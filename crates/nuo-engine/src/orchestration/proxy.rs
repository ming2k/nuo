//! The proxy provider and its diagnostic capture plumbing.

use super::*;

pub struct ProxyProvider {
    pub holder: Arc<RwLock<Arc<dyn Provider>>>,
    /// Whether `/debug trace` is armed. Read on every call so the
    /// toggle takes effect for the very next round-trip.
    debug_enabled: Arc<AtomicBool>,
    /// Dump directory while capture is on; `None` when off.
    debug_dir: Arc<std::sync::Mutex<Option<PathBuf>>>,
    /// Monotonic counter for unique filenames within the same millisecond.
    debug_seq: AtomicU64,
}

impl ProxyProvider {
    pub fn new(holder: Arc<RwLock<Arc<dyn Provider>>>) -> Self {
        Self {
            holder,
            debug_enabled: Arc::new(AtomicBool::new(false)),
            debug_dir: Arc::new(std::sync::Mutex::new(None)),
            debug_seq: AtomicU64::new(0),
        }
    }

    /// Resolve a capture record for the upcoming call, or `None` when capture
    /// is off. Clones the request messages once (only when armed) so the call
    /// can still move the originals into the inner provider.
    fn begin_capture(
        &self,
        provider: &str,
        model: &str,
        kind: &'static str,
        request: &ModelRequest,
    ) -> Option<PendingCapture> {
        if !self.debug_enabled.load(Ordering::SeqCst) {
            return None;
        }
        let dir = self
            .debug_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()?;
        Some(PendingCapture {
            provider: provider.to_string(),
            model: model.to_string(),
            kind,
            dir,
            request: request.clone(),
            seq: self.debug_seq.fetch_add(1, Ordering::SeqCst),
        })
    }
}

#[async_trait]
impl Provider for ProxyProvider {
    /// Delegate to the currently active inner provider so attribution tracks
    /// the live provider even after a mid-session `/models` switch.
    fn provider_id(&self) -> String {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .provider_id()
    }

    fn model(&self) -> String {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model()
    }

    fn wire_protocol(&self) -> Option<nuo_protocol::WireProtocol> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .wire_protocol()
    }

    fn effort(&self) -> Option<nuo_protocol::effort::Effort> {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .effort()
    }

    fn model_capabilities(&self) -> nuo_protocol::ModelCapabilities {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .model_capabilities()
    }

    fn prompt_hints(&self) -> nuo_protocol::ProviderPromptHints {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .prompt_hints()
    }

    fn route_fingerprint(&self) -> nuo_protocol::RouteFingerprint {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .route_fingerprint()
    }

    fn continuation_mode(&self) -> nuo_protocol::ContinuationMode {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .continuation_mode()
    }

    fn set_debug_capture(&self, enabled: bool, dir: PathBuf) {
        self.debug_enabled.store(enabled, Ordering::SeqCst);
        *self
            .debug_dir
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = if enabled { Some(dir) } else { None };
    }

    fn debug_capture_enabled(&self) -> bool {
        self.debug_enabled.load(Ordering::SeqCst)
    }

    async fn chat(
        &self,
        request: &ModelRequest,
    ) -> Result<nuo_protocol::ProviderCompletion, nuo_protocol::ProviderError> {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let started = Instant::now();
        let capture = self.begin_capture(&provider_id, &model, "chat", request);
        let result = p.chat(request).await;
        if let Some(capture) = capture {
            let item = match &result {
                Ok(completion) => serde_json::json!({
                    "status": "ok",
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "completion": completion,
                }),
                Err(error) => serde_json::json!({
                    "status": "error",
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "error": error,
                }),
            };
            write_capture(&capture, &[item]);
        }
        result
    }
    async fn stream_chat(
        &self,
        request: &ModelRequest,
        telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let capture = self.begin_capture(&provider_id, &model, "stream_chat", request);
        let stream = p.stream_chat(request, telemetry).await;
        match (capture, stream) {
            (Some(capture), Err(error)) => {
                write_capture(
                    &capture,
                    &[serde_json::json!({ "status": "error", "error": error })],
                );
                Err(error)
            }
            (Some(capture), Ok(stream)) => Ok(CapturedStream {
                inner: stream,
                items: Vec::new(),
                capture,
            }
            .boxed()),
            (None, Ok(stream)) => Ok(stream),
            (None, Err(error)) => Err(error),
        }
    }
    async fn stream_chat_events(
        &self,
        request: &ModelRequest,
        telemetry: nuo_protocol::TransportTelemetry,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, nuo_protocol::ProviderError>>,
        nuo_protocol::ProviderError,
    > {
        let p = self
            .holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let provider_id = p.provider_id();
        let model = p.model();
        let capture = self.begin_capture(&provider_id, &model, "stream_chat_events", request);
        let stream = p.stream_chat_events(request, telemetry).await;
        match (capture, stream) {
            (Some(capture), Err(error)) => {
                write_capture(
                    &capture,
                    &[serde_json::json!({ "status": "error", "error": error })],
                );
                Err(error)
            }
            (Some(capture), Ok(stream)) => Ok(CapturedStream {
                inner: stream,
                items: Vec::new(),
                capture,
            }
            .boxed()),
            (None, Ok(stream)) => Ok(stream),
            (None, Err(error)) => Err(error),
        }
    }

    /// Delegate usage support to the live inner provider so attribution tracks
    /// the active provider even after a mid-session `/models` swap.
    fn usage_supported(&self) -> bool {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .usage_supported()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        self.holder
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .supports_parallel_tool_calls()
    }
}

// /debug trace

/// A queued capture record awaiting its response. Held across the inner call
/// (for `chat`) or inside a [`CapturedStream`] (for the streaming paths) and
/// flushed once the round-trip is complete.
struct PendingCapture {
    provider: String,
    model: String,
    kind: &'static str,
    dir: PathBuf,
    request: ModelRequest,
    seq: u64,
}

/// Stream wrapper that tees every item into a buffer and flushes a single
/// capture file on drop — so one streaming round-trip yields one complete JSON
/// file, whether the stream ran to completion, errored, or was cancelled
/// mid-stream (a cancelled stream simply writes whatever was collected).
///
/// The wrapper is `Unpin`: its only pinned field (`inner: BoxStream`) is a
/// `Pin<Box<…>>`, which is itself `Unpin`, so `Pin::new(&mut self.inner)` is
/// sound. This keeps `poll_next` free of unsafe.
struct CapturedStream<S> {
    inner: S,
    items: Vec<serde_json::Value>,
    capture: PendingCapture,
}

impl<S> Stream for CapturedStream<S>
where
    S: Stream + Unpin,
    S::Item: Serialize,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => {
                this.items
                    .push(serde_json::to_value(&item).unwrap_or(serde_json::Value::Null));
                Poll::Ready(Some(item))
            }
            other => other,
        }
    }
}

impl<S> Drop for CapturedStream<S> {
    fn drop(&mut self) {
        write_capture(&self.capture, &self.items);
    }
}

/// Serialize one capture record and write it atomically to the dump directory.
/// Failures are logged and swallowed: debug capture must never break a real
/// turn. Files are owner-only (`0o600`) via `atomic_write_bytes` — request
/// messages can carry pasted secrets, the same privacy profile as `/export`.
fn write_capture(capture: &PendingCapture, items: &[serde_json::Value]) {
    let timestamp = chrono::Utc::now();
    let record = serde_json::json!({
        "timestamp": timestamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "provider": capture.provider,
        "model": capture.model,
        "kind": capture.kind,
        "request": {
            "messages": &capture.request.messages,
            "tools": &capture.request.tool_specs,
        },
        "response": { "items": items },
    });
    let bytes = match serde_json::to_vec_pretty(&record) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "network capture serialize failed");
            return;
        }
    };
    let provider_slug = slug(&capture.provider);
    let model_slug = slug(&capture.model);
    let stamp = timestamp.format("%Y%m%d-%H%M%S%.3f");
    let file = capture.dir.join(format!(
        "{stamp}_{seq:04}_{provider_slug}_{model_slug}.json",
        seq = capture.seq,
    ));
    if let Err(error) = nuo_persistence::fsutil::atomic_write_bytes(&file, &bytes) {
        tracing::warn!(%error, file = %file.display(), "network capture write failed");
    }
    prune_capture_dir(&capture.dir);
}

/// How many capture files one directory keeps. A capture is the full request
/// context of one round-trip — on a long session each file is as big as the
/// context itself, so an armed `/debug trace` writing unbounded captures
/// grows the data dir faster than everything else combined. Debug data is
/// disposable by definition; the newest [`MAX_CAPTURE_FILES`] are plenty to
/// diagnose a provider issue.
pub(super) const MAX_CAPTURE_FILES: usize = 50;

/// Delete the oldest captures beyond [`MAX_CAPTURE_FILES`]. Names sort
/// chronologically (timestamp first), so a name sort is an age sort.
pub(super) fn prune_capture_dir(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.ends_with(".json") && e.file_type().is_ok_and(|t| !t.is_dir())).then_some(name)
        })
        .collect();
    if names.len() <= MAX_CAPTURE_FILES {
        return;
    }
    names.sort();
    let excess = names.len() - MAX_CAPTURE_FILES;
    for name in names.into_iter().take(excess) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Lowercase alnum/hyphen filename component, empty -> `"anon"`.
fn slug(value: &str) -> String {
    let mut out: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .map(|character| character.to_ascii_lowercase())
        .collect();
    if out.is_empty() {
        out.push_str("anon");
    }
    out
}
