//! Atomic harness extensions implementation (ADR-0224).
//!
//! `CodeIntelligenceExtension` retains its identity for existing catalogs but
//! never gates mutations. Writing tools return post-write syntax diagnostics.

use nuo_wire::extension::{HookContext, HookOutcome};
use nuo_wire::{Extension, HookPhase};

/// Code-intelligence extension (ADR-0211, revised by ADR-0214 / ADR-0224).
///
/// ADR-0233 removes syntax interception; ADR-0214 prohibits ambient repository
/// maps. Structure remains available through the separate `code_query` tool.
#[derive(Debug, Clone)]
pub struct CodeIntelligenceExtension;

impl Default for CodeIntelligenceExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeIntelligenceExtension {
    pub fn new() -> Self {
        Self
    }

    /// Compatibility constructor for Explore roles; permissions live elsewhere.
    pub fn read_only() -> Self {
        Self
    }

    /// Compatibility entry point: syntax never vetoes mutations (ADR-0233).
    /// The writing tool owns diagnostics after a successful filesystem commit.
    pub fn check_mutation(&self, _path: &std::path::Path, _content: &str) -> Option<String> {
        None
    }
}

impl Extension for CodeIntelligenceExtension {
    fn id(&self) -> &str {
        "code_intelligence"
    }

    fn hooks(&self) -> &'static [HookPhase] {
        &[]
    }

    fn run(&self, _phase: HookPhase, _ctx: &HookContext<'_>) -> HookOutcome {
        HookOutcome::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn extension_allows_broken_syntax() {
        let ext = CodeIntelligenceExtension::new();
        assert!(
            ext.check_mutation(Path::new("main.rs"), "fn broken(")
                .is_none()
        );
        assert!(
            ext.check_mutation(Path::new("main.rs"), "fn ok() {}")
                .is_none()
        );
    }

    #[test]
    fn read_only_extension_skips_interception() {
        let ext = CodeIntelligenceExtension::read_only();
        assert!(
            ext.check_mutation(Path::new("main.rs"), "fn broken(")
                .is_none()
        );
    }

    #[test]
    fn mutation_gate_allows_invalid_config() {
        let ext = CodeIntelligenceExtension::new();
        assert!(
            ext.check_mutation(Path::new("config.json"), r#"{"a":}"#)
                .is_none()
        );
        assert!(
            ext.check_mutation(Path::new("config.json"), r#"{"a":1}"#)
                .is_none()
        );
    }

    #[test]
    fn hook_dispatch_never_blocks_syntax() {
        let ext = CodeIntelligenceExtension::new();
        assert!(matches!(
            ext.run(
                HookPhase::InterceptFileMutation,
                &HookContext::mutation(Path::new("main.rs"), "fn broken(")
            ),
            HookOutcome::None
        ));
        assert_eq!(
            ext.run(
                HookPhase::InterceptFileMutation,
                &HookContext::mutation(Path::new("main.rs"), "fn ok() {}")
            ),
            HookOutcome::None
        );
        assert_eq!(
            ext.run(
                HookPhase::ProjectTemporaryContext,
                &HookContext::temporary_context(Some(Path::new("/tmp")))
            ),
            HookOutcome::None
        );
    }

    #[test]
    fn extension_declares_no_mutation_gate() {
        let ext = CodeIntelligenceExtension::new();
        assert_eq!(ext.id(), "code_intelligence");
        assert!(ext.hooks().is_empty());
    }

    /// ADR-0214: `model_request` must not append an ambient code-structure
    /// outline; the assembled window equals the filtered source window plus
    /// only skill injections, and source messages stay unmutated.
    #[tokio::test]
    async fn model_request_injects_no_temporary_context_and_does_not_mutate_source_messages() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "pub struct AppState { pub id: u64 }\n").unwrap();

        let provider = std::sync::Arc::new(crate::NoProvider);
        let agent = crate::Agent::new(
            provider,
            Vec::new(),
            crate::AgentIdentity::new("dev", "developer"),
        );
        agent.set_project_root(Some(dir.path().to_path_buf()));

        let source_messages = vec![nuo_wire::Message::new(
            nuo_wire::Role::User,
            "Hello",
        )];

        let req = agent.model_request(&source_messages);

        assert_eq!(source_messages.len(), 1);
        let wire_messages = &req.messages;
        assert_eq!(wire_messages.len(), source_messages.len());
        for (wire, source) in wire_messages.iter().zip(source_messages.iter()) {
            assert_eq!(wire.content, source.content);
        }
        assert!(
            wire_messages
                .iter()
                .all(|message| !message.content.contains("codebase-structure-outline"))
        );
        assert!(req.temporary_context().is_empty());
    }
}
