//! Execution context passed to every tool invocation (ADR-0008).
//!
//! Runtime capabilities reach a tool **only** through this context
//! (`[INV-TOOL-10]`): conversation coordinates, cooperative cancellation, an
//! incremental output-stream sink, workspace roots, and an opaque type-keyed
//! service map. There are deliberately no `*_with_events` / `request_cancel`
//! trait methods — a tool that streams reads its sink from the context; a tool
//! that needs harness-specific state (e.g. the subagent event channel) looks it
//! up in the service map by type.

use crate::error::{Result, ToolError};
use crate::stream::ToolStream;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A sink for incremental output a long-running tool emits before its final
/// result lands (e.g. a shell command's stdout as it arrives). Implemented by
/// the harness and supplied through [`ToolContext`]; kept as a trait so the
/// tool leaf stays free of any transport or event-enum coupling.
pub trait ToolStreamSink: Send + Sync {
    /// Emit one incremental output chunk.
    fn emit(&self, stream: ToolStream);
}

/// Everything a tool needs for one invocation beyond its own state: the call
/// identity, the raw arguments, the input-execution contract, and the runtime
/// input supervisor (when the dispatch layer supplied one). Bundled so the
/// trait method stays stable as per-call context grows.
pub struct ToolInvocation<'a> {
    /// The dispatch-generated call id (keys live streams and subagent views).
    pub call_id: &'a str,
    /// Raw JSON tool arguments exactly as the model emitted them.
    pub arguments: &'a str,
    /// How the child's input channels are provisioned (command tool only;
    /// other tools ignore it).
    pub input: crate::stream::InputContract,
    /// Runtime input supervisor for a supervised child. The dispatch layer
    /// builds it per invocation (it captures the live event channel), so it is
    /// borrowed rather than owned for the tool's lifetime.
    pub input_handler: Option<&'a dyn crate::stream::InputHandler>,
}

/// Execution context passed to every tool invocation.
///
/// Encapsulates conversation coordinates, cooperative cancellation tokens,
/// an output-stream sink, workspace roots, a type-keyed service map, tracing
/// metadata, and correlation handles.
#[derive(Clone)]
pub struct ToolContext {
    pub session_id: Option<String>,
    pub call_id: String,
    pub cancel_token: CancellationToken,
    pub correlation_id: Option<Uuid>,
    pub metadata: HashMap<String, String>,
    /// Workspace roots a session-scoped tool resolves relative paths against
    /// and starts shells in. `None` means "use the process cwd" (the historical
    /// behaviour, correct where one process serves one project).
    pub workspace_roots: Option<Vec<PathBuf>>,
    /// Incremental output sink, when the dispatch layer supplied one.
    pub stream_sink: Option<Arc<dyn ToolStreamSink>>,
    /// Opaque, type-keyed bag of services for capability-specific state (config
    /// blobs, shared registries, harness event channels). The one seam that lets
    /// a tool reach a concrete dependency without the leaf depending on it.
    services: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            session_id: None,
            call_id: Uuid::new_v4().to_string(),
            cancel_token: CancellationToken::new(),
            correlation_id: None,
            metadata: HashMap::new(),
            workspace_roots: None,
            stream_sink: None,
            services: Arc::new(HashMap::new()),
        }
    }
}

impl ToolContext {
    /// Creates a new context with a specific tool call ID.
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            ..Self::default()
        }
    }

    /// Sets the enclosing session ID.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Attaches an existing cooperative cancellation token.
    pub fn with_cancel_token(mut self, cancel_token: CancellationToken) -> Self {
        self.cancel_token = cancel_token;
        self
    }

    /// Sets the correlation UUID.
    pub fn with_correlation_id(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }

    /// Inserts a metadata key-value pair.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Sets the workspace roots a session-scoped tool resolves against.
    pub fn with_workspace_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.workspace_roots = Some(roots);
        self
    }

    /// Attaches an incremental output-stream sink.
    pub fn with_stream_sink(mut self, sink: Arc<dyn ToolStreamSink>) -> Self {
        self.stream_sink = Some(sink);
        self
    }

    /// The primary workspace root (first admitted root), if any.
    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.workspace_roots.as_ref().and_then(|r| r.first()).map(|p| p.as_path())
    }

    /// Emit one incremental output chunk, if a sink is attached.
    pub fn emit_stream(&self, stream: ToolStream) {
        if let Some(sink) = &self.stream_sink {
            sink.emit(stream);
        }
    }

    /// Look up a service by its exact type. `None` if none was provided.
    pub fn service<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }

    /// Look up a shared handle stored as `Arc<T>` and clone the `Arc` out.
    pub fn shared_service<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.service::<Arc<T>>().cloned()
    }

    /// Checks if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel_token.is_cancelled()
    }

    /// Returns `Err(ToolError::Cancelled)` if cooperative cancellation was triggered.
    pub fn check_cancelled(&self, tool_name: &str) -> Result<()> {
        if self.is_cancelled() {
            Err(ToolError::cancelled(tool_name))
        } else {
            Ok(())
        }
    }
}

/// Builder for the type-keyed service map of a [`ToolContext`] (ADR-0008).
///
/// Provide services by concrete type, then [`build`](Self::build) to freeze a
/// [`ServiceMap`] and attach it to a context. Kept separate from
/// [`ToolContext`] construction so the map can be assembled once and reused
/// across many invocations.
#[derive(Default)]
pub struct ServiceMapBuilder {
    services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

/// A frozen, cheaply cloneable type-keyed service map.
#[derive(Clone, Default)]
pub struct ServiceMap {
    services: Arc<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ServiceMapBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Provide a service by its concrete type. Later inserts of the same type
    /// replace earlier ones.
    pub fn provide<T: Any + Send + Sync>(&mut self, value: T) -> &mut Self {
        self.services.insert(TypeId::of::<T>(), Arc::new(value));
        self
    }

    /// Freeze the map.
    pub fn build(self) -> ServiceMap {
        ServiceMap {
            services: Arc::new(self.services),
        }
    }
}

impl ServiceMap {
    /// Look up a value by its exact type.
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.services
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
    }

    /// Look up a shared handle stored as `Arc<T>` and clone the `Arc` out.
    pub fn shared<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.get::<Arc<T>>().cloned()
    }
}

impl ToolContext {
    /// Attach a frozen service map, replacing any existing services.
    pub fn with_services(mut self, map: ServiceMap) -> Self {
        self.services = map.services;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_context_has_no_capabilities() {
        let ctx = ToolContext::default();
        assert!(ctx.workspace_root().is_none());
        assert!(ctx.stream_sink.is_none());
        assert!(ctx.service::<u32>().is_none());
        assert!(!ctx.is_cancelled());
    }

    #[test]
    fn workspace_roots_resolve_primary() {
        let ctx = ToolContext::default().with_workspace_roots(vec![PathBuf::from("/tmp/ws")]);
        assert_eq!(ctx.workspace_root(), Some(std::path::Path::new("/tmp/ws")));
    }

    #[test]
    fn service_map_is_type_keyed() {
        let mut b = ServiceMapBuilder::new();
        b.provide(7u32);
        b.provide("hi".to_string());
        let map = b.build();
        assert_eq!(map.get::<u32>(), Some(&7));
        assert_eq!(map.get::<String>().map(String::as_str), Some("hi"));
        assert!(map.get::<i64>().is_none());

        let ctx = ToolContext::default().with_services(map);
        assert_eq!(ctx.service::<u32>(), Some(&7));
    }

    #[test]
    fn cancellation_is_reported() {
        let ctx = ToolContext::default();
        ctx.cancel_token.cancel();
        assert!(ctx.is_cancelled());
        assert!(ctx.check_cancelled("t").is_err());
    }
}
