//! Template lookups, labels, and editor field selection.

use super::*;

/// Resolve either a curated template or the standalone `custom` template by
/// its **provider id**.
pub fn connection_definition(provider: &str) -> Option<&'static ConnectionTemplate> {
    if provider == CUSTOM_TEMPLATE.id {
        return Some(&CUSTOM_TEMPLATE);
    }
    PROVIDER_PRESETS.iter().find(|t| t.id == provider)
}

/// The editor header title for a create-mode connection — the label of the
/// template the flow was seeded from, falling back to a generic header. The
/// lookup is by **provider id**, not wire protocol: several providers share the
/// `openai` wire, and a first-match-by-protocol lookup would mislabel the
/// editor (e.g. "ChatGPT Subscription" for the OpenAI Platform flow).
pub fn provider_label_for(provider: Option<&str>) -> String {
    provider
        .and_then(connection_definition)
        .map(|t| t.label.to_string())
        .unwrap_or_else(|| "＋ Add connection".to_string())
}

/// Resolve the provider **type** label for a Connections row from its
/// `provider` id — e.g. `provider = "openai"` → `"OpenAI Platform"`. This is
/// the service surface shown beside the user-given connection name (distinct
/// from the connection name itself). Returns `None` for a provider the local
/// template table does not know, in which case the row renders the connection
/// name alone.
pub fn provider_type_label(provider: &str) -> Option<&'static str> {
    if provider.is_empty() {
        return None;
    }
    connection_definition(provider).map(|t| t.label)
}

/// The ordered editor fields shown when **editing** an existing connection.
/// For the `custom` provider the form offers Name, Base URL, and Token (the
/// Model field is omitted — models, and their per-model reasoning, ADR-0046,
/// are managed in the Models picker). For a curated provider (where endpoint
/// and models are owned by the provider spec), Base URL is fixed by the
/// provider, so only Name and Token are offered. For an OAuth connection
/// (ChatGPT/Codex, xAI, Copilot, Antigravity) only Name is editable: the Base
/// URL and Token are fixed by the auth flow and must not be hand-edited, so a
/// rename is the only safe operation.
pub fn edit_fields(curated: bool, auth: ConnectionAuth) -> Vec<CustomField> {
    if auth.is_negotiated() {
        vec![CustomField::Name, CustomField::Budget]
    } else if curated {
        vec![CustomField::Name, CustomField::Token, CustomField::Budget]
    } else {
        vec![
            CustomField::Name,
            CustomField::BaseUrl,
            CustomField::Token,
            CustomField::Protocol,
            CustomField::Budget,
        ]
    }
}

/// Whether a protocol's model set is *closed*: the candidate list is the full,
/// fixed set and the add-model overlay must NOT offer a free-text fallback.
/// OpenAI and Anthropic relays serve an open, evolving model set, so
/// typing an unlisted id is legitimate; native Google is a closed family — its
/// models are enumerated by Google and forwarded verbatim by relays, so an
/// arbitrary id is almost certainly a typo or hallucination, not a real model.
#[cfg(test)]
pub fn protocol_model_set_closed(protocol_wire: &str) -> bool {
    protocol_wire == WireProtocol::GoogleGenerateContent.as_str()
}

/// The registry model ids matching a wire protocol. Kept as a test helper for
/// registry and template consistency; the custom connection editor
/// intentionally does not use this list.
#[cfg(test)]
pub(crate) fn protocol_model_candidates(protocol_wire: &str) -> Vec<&'static str> {
    let Ok(protocol) = protocol_wire.parse::<WireProtocol>() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    nuo_protocol::baseline_models()
        .filter(|m| m.protocol == protocol)
        .map(|m| m.id)
        // Deduplicate: a model id can appear in multiple provider tables (e.g.
        // gpt-4o-mini in both `openai` and `copilot`), and inventory iteration
        // order is not guaranteed, so without dedup the candidate list — and
        // thus the first-match the picker commits — would be non-deterministic.
        .filter(|id| seen.insert(*id))
        .collect()
}
