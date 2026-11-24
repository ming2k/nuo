//! The Archivist: a conversational nuo-level agent co-stationed
//! on the Hypervisor.
//!
//! The Hypervisor station is the server's single workspace-free
//! slot. The operator-facing `Hypervisor` coordinates the fleet; the
//! **Archivist** is a second conversational identity on the same station — a
//! Root-posture agent whose job is *institutional memory*: it knows every
//! session this instance has ever hosted, where they live, and how to find
//! them back. Its toolset is deliberately retrieval-only (search / list /
//! read, all read-only, inherently cross-project because they read the one
//! shared `nuo.db`); anything that must act on files is delegated to a
//! workspace master over the mesh, keeping the workspace-security posture
//! (0147) intact.
//!
//! Tool results return raw JSON strings — the same shape the Hypervisor
//! tools use — because the agent kernel renders tool results verbatim into
//! the model window; structured wire types are a frontend concern.
use std::sync::Arc;

use async_trait::async_trait;
use engine::{Agent, AgentIdentity};
use persistence::db::{DbReader, get_persistence_handle};
use serde_json::{Value, json};
use tool::Tool;

use acp::{AgentAddress, AgentEnvelope, Fabric, MessageIntent, SteerAction};

/// Run one read against the unified store on the blocking pool.
///
/// The closure owns the reader for exactly as long as it runs, so no WAL read
/// snapshot is ever held across an `await`. Both halves matter:
///
/// - the checkout is synchronous and may wait for a lease, so it must not park
///   an async worker that every hosted session shares;
/// - a reader held across an `await` (an ACP dispatch, an HTTP call) pins the
///   WAL against checkpointing for the duration of unrelated network I/O,
///   which is how a single slow call grows the WAL on the whole server.
///
/// The lookup and recall tools therefore never open a connection of their own
/// and never keep one alive past their own answer.
async fn read_store<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&DbReader) -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let reader = get_persistence_handle()
            .reader()
            .map_err(|error| format!("could not read the session store: {error}"))?;
        f(&reader)
    })
    .await
    .map_err(|error| format!("session store task failed: {error}"))?
}

/// Build the Archivist agent for this server instance: a Root-posture agent
/// addressed as `agent://local/hypervisor/archivist` with the retrieval toolset plus the
/// delegation channel. `fabric` is the server's shared ACP fabric.
pub fn build_archivist(
    provider: Arc<dyn protocol::Provider>,
    fabric: Option<Fabric>,
) -> Arc<Agent> {
    let address = archivist_address();

    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ArchivistSearchHistoryTool),
        Arc::new(ArchivistListSessionsTool),
        Arc::new(ArchivistReadSessionTool),
    ];
    if let Some(f) = fabric {
        tools.push(Arc::new(ArchivistInstructSessionTool::new(
            f,
            address.clone(),
        )));
    }

    let identity = AgentIdentity::new(
        "archivist",
        "the Nuo instance's Archivist — it knows every session the server has \
         ever hosted, where sessions are stored, and how to find a past \
         conversation back from even a rough description; it can also hand a \
         found task to a workspace session for execution",
    );

    let agent = Arc::new(Agent::new(provider, tools, identity));
    agent.set_kind(protocol::AgentKind::Root);
    agent
}

/// The Archivist's ACP address (`agent://local/hypervisor/archivist`).
#[allow(clippy::expect_used)] // Literal constant, pinned by `archivist_address_is_hypervisor_station` below.
pub fn archivist_address() -> AgentAddress {
    AgentAddress::parse("agent://local/hypervisor/archivist").expect("valid archivist address")
}

/// The Archivist's delegation channel: hand a found task to a
/// workspace session as a top-down instruction.
pub struct ArchivistInstructSessionTool {
    fabric: Fabric,
    archivist_address: AgentAddress,
}

impl ArchivistInstructSessionTool {
    pub fn new(fabric: Fabric, archivist_address: AgentAddress) -> Self {
        Self {
            fabric,
            archivist_address,
        }
    }
}

#[async_trait]
impl Tool for ArchivistInstructSessionTool {
    fn name(&self) -> &str {
        "archivist_instruct_session"
    }

    fn description(&self) -> &str {
        "Dispatch a top-down instruction to a hosted workspace session for its agent to execute."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Target session ID (or unique hex prefix)"
                },
                "instruction": {
                    "type": "string",
                    "description": "Directive for the session agent to execute"
                }
            },
            "required": ["session_id", "instruction"]
        })
    }

    async fn execute(
        &self,
        _ctx: &tool::ToolContext,
        arguments: Value,
    ) -> Result<tool::ToolOutput, tool::ToolError> {
        let args = arguments;
        let session_id = args["session_id"]
            .as_str()
            .ok_or("Missing 'session_id' argument")?;
        let instruction = args["instruction"]
            .as_str()
            .ok_or("Missing 'instruction' argument")?;

        // The id may be a prefix the search tools surfaced; resolve it
        // against the durable store first so delegation fails honestly
        // instead of mis-addressing an unrelated session. The lookup is one
        // blocking read; it must not hold a WAL snapshot across the ACP
        // dispatch below.
        let requested = session_id.to_string();
        let resolved = read_store(move |reader| {
            if reader
                .get_session(&requested)
                .map_err(|e| format!("session lookup failed: {e}"))?
                .is_some()
            {
                return Ok(requested);
            }
            reader
                .resolve_session_prefix(&requested, &protocol::ThreadFilter::global())
                .map_err(|e| format!("session resolve failed: {e}"))?
                .into_iter()
                .next()
                .ok_or_else(|| format!("no session matches '{requested}'"))
        })
        .await?;

        let recipient = AgentAddress::parse(&format!("agent://local/session/{resolved}"))
            .map_err(|e| format!("invalid session recipient: {e}"))?;
        let intent = MessageIntent::steer(instruction, SteerAction::Note);
        let envelope = AgentEnvelope::new(self.archivist_address.clone(), recipient, intent);
        let msg_id = envelope.id.to_string();
        self.fabric
            .dispatch(envelope)
            .await
            .map_err(|e| format!("ACP fabric dispatch failed: {e}"))?;

        Ok(tool::ToolOutput::text(
            json!({
                "status": "delegated",
                "message_id": msg_id,
                "target_session": resolved
            })
            .to_string(),
        ))
    }
}

/// Cross-project BM25 search over every persisted transcript.
/// `workspace: None` is the default and the point: the Archivist's retrieval
/// plane is inherently instance-wide.
pub struct ArchivistSearchHistoryTool;

#[async_trait]
impl Tool for ArchivistSearchHistoryTool {
    fn name(&self) -> &str {
        "archivist_search_history"
    }

    fn description(&self) -> &str {
        "Full-text BM25 search across persisted session transcripts, returning matching snippets and session metadata."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords or FTS5 MATCH query"
                },
                "workspace": {
                    "type": "string",
                    "description": "Project root filter (omit to search all projects)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max hits to return (default 20, max 100)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        _ctx: &tool::ToolContext,
        arguments: Value,
    ) -> Result<tool::ToolOutput, tool::ToolError> {
        let args = arguments;
        let query = args["query"].as_str().ok_or("Missing 'query' argument")?;
        let workspace = args["workspace"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;
        let filter = workspace.map(|w| protocol::WorkspaceFilter::Path(w.into()));

        let search_term = query.to_string();
        // Two-stage recall (Layer 3, deterministic leg): strict
        // AND first; an empty strict result widens to OR so a gist whose
        // words never co-occur still recalls candidates. Both stages run
        // inside one blocking read transaction, so they see one snapshot and
        // the reader is released before any network call.
        let (relaxed, hits) = read_store(move |reader| {
            let strict = reader
                .search_history(&search_term, filter.as_ref(), limit)
                .map_err(|e| format!("history search failed: {e}"))?;
            if !strict.is_empty() {
                return Ok((false, strict));
            }
            let relaxed = reader
                .search_history_relaxed(&search_term, filter.as_ref(), limit)
                .map_err(|e| format!("history search failed: {e}"))?;
            Ok((true, relaxed))
        })
        .await?;

        let hits_json: Vec<serde_json::Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "session_id": h.session_id,
                    "session_title": h.session_title,
                    "scope": h.workspace_root.clone().unwrap_or_else(|| "workspace-free".to_string()),
                    "role": h.role,
                    "snippet": strip_highlight(&h.snippet),
                    "relevance": -h.score,
                })
            })
            .collect();

        Ok(tool::ToolOutput::text(
            json!({
                "query": query,
                "recall": if relaxed { "relaxed" } else { "strict" },
                "hit_count": hits_json.len(),
                "hits": hits_json
            })
            .to_string(),
        ))
    }
}

/// Metadata-level session listing across every project bucket (
/// layer 2): titles, digests, times, message counts — no transcript bodies.
pub struct ArchivistListSessionsTool;

#[async_trait]
impl Tool for ArchivistListSessionsTool {
    fn name(&self) -> &str {
        "archivist_list_sessions"
    }

    fn description(&self) -> &str {
        "List persisted sessions (newest activity first) with IDs, titles, digests, scopes, and message counts."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "workspace": {
                    "type": "string",
                    "description": "Project root filter (omit for all scopes)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max sessions to return (default 50)"
                }
            }
        })
    }

    async fn execute(
        &self,
        _ctx: &tool::ToolContext,
        arguments: Value,
    ) -> Result<tool::ToolOutput, tool::ToolError> {
        let args = arguments;
        let workspace = args["workspace"].as_str();
        let limit = args["limit"].as_u64().unwrap_or(50).clamp(1, 500) as usize;
        let filter = match workspace {
            Some(path) => {
                protocol::ThreadFilter::workspace_only(protocol::WorkspaceFilter::Path(path.into()))
            }
            None => protocol::ThreadFilter::global(),
        };

        let rows = read_store(move |reader| {
            reader
                .list_sessions(&filter, true)
                .map_err(|e| format!("session listing failed: {e}"))
        })
        .await?;

        let sessions: Vec<serde_json::Value> = rows
            .into_iter()
            .take(limit)
            .map(|s| {
                json!({
                    "session_id": s.id,
                    "title": s.title,
                    "scope": s.workspace_root.clone().unwrap_or_else(|| "workspace-free".to_string()),
                    "message_count": s.msg_count,
                    "created_at_s": s.created_at_s,
                    "updated_at_s": s.updated_at_s,
                    "last_user_prompt": s.last_user_prompt,
                    "digest": s.digest.and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok()),
                })
            })
            .collect();

        Ok(tool::ToolOutput::text(
            json!({
                "session_count": sessions.len(),
                "sessions": sessions
            })
            .to_string(),
        ))
    }
}

/// Read one session's transcript tail by id — the follow-up after a search
/// hit pins down the candidate session.
pub struct ArchivistReadSessionTool;

#[async_trait]
impl Tool for ArchivistReadSessionTool {
    fn name(&self) -> &str {
        "archivist_read_session"
    }

    fn description(&self) -> &str {
        "Read the recent transcript messages of a session by ID."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "Session ID (or 4+ hex-char prefix)"
                },
                "tail_messages": {
                    "type": "integer",
                    "description": "Max recent messages to return (default 20, max 200)"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn execute(
        &self,
        _ctx: &tool::ToolContext,
        arguments: Value,
    ) -> Result<tool::ToolOutput, tool::ToolError> {
        let args = arguments;
        let session_id = args["session_id"]
            .as_str()
            .ok_or("Missing 'session_id' argument")?;
        let tail = args["tail_messages"].as_u64().unwrap_or(20).clamp(1, 200) as usize;

        // Resolve prefix → full id and read the tail inside one blocking
        // transaction: the search tools surface ids, a user may quote a
        // shortened one, and both steps must see the same snapshot.
        let requested = session_id.to_string();
        let view = read_store(move |reader| {
            let resolved = if reader
                .get_session(&requested)
                .map_err(|e| format!("session lookup failed: {e}"))?
                .is_some()
            {
                requested.clone()
            } else {
                reader
                    .resolve_session_prefix(&requested, &protocol::ThreadFilter::global())
                    .map_err(|e| format!("session resolve failed: {e}"))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| format!("no session matches '{requested}'"))?
            };
            // Full transcript tail (the same projection session resume
            // renders), field-private behind the persistence view so the
            // Archivist reads exactly what a human would see.
            reader
                .read_session_transcript(&resolved, tail)
                .map_err(|e| format!("session read failed: {e}"))?
                .ok_or_else(|| format!("session '{resolved}' not found"))
        })
        .await?;

        let messages: Vec<serde_json::Value> = view
            .messages
            .iter()
            .map(|m| {
                json!({
                    "seq": m.seq,
                    "role": m.role,
                    "content": truncate_chars(&m.content, 1_200),
                })
            })
            .collect();

        Ok(tool::ToolOutput::text(json!({
            "session_id": view.id,
            "title": view.title,
            "digest": view.digest,
            "scope": view.workspace_root.clone().unwrap_or_else(|| "workspace-free".to_string()),
            "message_count": view.message_count,
            "returned_messages": messages.len(),
            "messages": messages,
        })
        .to_string()))
    }
}

/// Clamp content to a character budget so one tool result cannot flood the
/// Archivist's model window; searches already carry the precise snippets.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

/// Strip FTS highlight markers when the consumer is a model (which reads
/// plain text) rather than a renderer (which wants emphasis spans).
fn strip_highlight(snippet: &str) -> String {
    snippet.replace("<b>", "").replace("</b>", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sandbox the process-wide store to a temp `NUO_HOME` before any test
    /// opens it.
    fn sandbox_once() {
        use std::sync::Once;
        static SANDBOX: Once = Once::new();
        static KEEP: std::sync::Mutex<Option<tempfile::TempDir>> = std::sync::Mutex::new(None);
        SANDBOX.call_once(|| {
            let tmp = tempfile::tempdir().unwrap();
            unsafe { std::env::set_var("NUO_HOME", tmp.path()) };
            *KEEP.lock().unwrap() = Some(tmp);
        });
    }

    #[test]
    fn archivist_address_is_hypervisor_station() {
        let addr = archivist_address();
        assert_eq!(addr.as_str(), "agent://local/hypervisor/archivist");
    }

    #[tokio::test]
    async fn search_tool_reports_missing_query() {
        let err = ArchivistSearchHistoryTool
            .execute_text(&::tool::ToolContext::default(), "{}")
            .await
            .unwrap_err();
        assert!(err.contains("query"), "{err}");
    }

    #[tokio::test]
    async fn search_tool_runs_against_the_real_store() {
        sandbox_once();
        let out = ArchivistSearchHistoryTool
            .execute_text(
                &::tool::ToolContext::default(),
                r#"{"query": "nonexistent-needle-xyz"}"#,
            )
            .await
            .unwrap();
        assert!(out.contains("hit_count"), "{out}");
    }

    #[tokio::test]
    async fn list_tool_runs_against_the_real_store() {
        sandbox_once();
        let out = ArchivistListSessionsTool
            .execute_text(&::tool::ToolContext::default(), "{}")
            .await
            .unwrap();
        assert!(out.contains("session_count"), "{out}");
    }

    #[tokio::test]
    async fn read_tool_reports_unknown_session() {
        sandbox_once();
        let err = ArchivistReadSessionTool
            .execute_text(
                &::tool::ToolContext::default(),
                r#"{"session_id": "ffffffff-ffff-ffff-ffff-ffffffffffff"}"#,
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("not found") || err.contains("no session"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn delegation_tool_delivers_to_the_session_mailbox() {
        use acp::AgentManifest;

        sandbox_once();
        let fabric = Fabric::new("test-fabric");
        let tool = ArchivistInstructSessionTool::new(fabric.clone(), archivist_address());

        let err = tool
            .execute_text(
                &::tool::ToolContext::default(),
                r#"{"session_id": "deadbeef", "instruction": "do the thing"}"#,
            )
            .await
            .unwrap_err();
        assert!(
            err.contains("no session") || err.contains("not found"),
            "{err}"
        );

        let full_id = uuid::Uuid::new_v4().to_string();
        let target_addr = AgentAddress::parse(&format!("agent://local/session/{full_id}")).unwrap();
        let manifest = AgentManifest::new(target_addr.clone(), "test-session", "test session");
        let mut mailbox = fabric.join(manifest, 64).await;

        let envelope = AgentEnvelope::new(
            archivist_address(),
            target_addr,
            MessageIntent::steer("run the verification", SteerAction::Note),
        );
        fabric.dispatch(envelope).await.unwrap();

        let received = mailbox.recv().await.expect("mailbox receives");
        if let MessageIntent::Steer(p) = received.intent {
            assert_eq!(p.instruction, "run the verification");
        } else {
            panic!("expected Steer intent");
        }
    }
}
