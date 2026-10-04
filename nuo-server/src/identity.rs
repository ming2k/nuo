//! This CLI's identity + role profile.
//!
//! Lives in the application layer (`muta`), NOT in `muta-runtime`.
//! The server layer stays application-neutral — a future sibling binary
//! brings its own identity/role. The server's `/btw` side
//! session reuses the primary agent's identity via `Agent::identity()`,
//! so it never asks the server to name a product.
//!
//! ## Why the shipped agent has no self-description
//!
//! The engine composes an identity preamble only when the embedding supplies
//! one ([`nuo_wire::AgentIdentity`]); this CLI supplies none. Nothing in the harness
//! reads the model's self-name: no feature parses "I am muta", addressing is
//! user-side (`@role:` / `/role`), and the product name already travels
//! with the binary, the UI chrome, and the config paths. Capabilities are
//! declared by the tool schemas, the environment by the host section, and the
//! work ethos by the persistence policy — so a
//! `"You are muta, an expert AI coding assistant."` opening would spend the
//! prompt's most salient slot on a label that steers no behaviour, and it
//! invites the model to answer identity questions with a product name it has
//! no grounded knowledge of. The baseline prompt therefore opens at the host
//! environment.
//!
//! A `/role` switch is the one place a framing line earns its
//! tokens: the focused roles install an imperative role directive
//! (see [`AgentRoleProfile::from_role`]).

use nuo_wire::AgentRoleProfile;

/// The built-in **coding agent** profile (ADR-0183, ADR-0244): the declarative
/// form of the standard developer role with canonical Base-Tier identity directive.
pub fn agent_code() -> AgentRoleProfile {
    AgentRoleProfile::developer()
}

/// The daemon has no terminal or browser clipboard of its own. Clipboard
/// effects belong to a connected app; until the wire protocol carries that
/// request back to the initiating client, report the boundary explicitly.
pub struct DaemonUiBridge;

#[async_trait::async_trait]
impl nuo::UiBridge for DaemonUiBridge {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<nuo::CopyOutcome, String> {
        Err(
            "clipboard export is a client capability; use the client's local copy action"
                .to_string(),
        )
    }
}

// Built-in roles (`developer`, `philosophist`, `ops`) and the `/role` switching mechanism
// are declared in `muta-contracts` as shared vocabulary (`MainAgentRole`,
// `AgentRoleProfile::from_role`) and applied via `Agent::apply_role`,
// so this binary does not need its own role registry — both frontends share one.
