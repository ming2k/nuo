//! Declarative wire surfaces: the data form of a provider dialect.
//!
//! A [`ProviderDialect`](crate::ProviderDialect) is an *enum key*; a
//! [`DialectSurface`] is the *table* that key points at. Every wire-level
//! difference a dialect has — the inference path and its fixed query, the
//! client identity it must present, the request envelope it wraps the
//! chat-completions body in, the slots that carry the model identity, and the
//! live catalog endpoint it discovers models from — lives here as data.
//!
//! This mirrors [`ClientProfileSpec`](crate::ClientProfileSpec) exactly: a
//! struct of `&'static` scalars and `&'static [(&str, _)]` tables, with small
//! enums where a *kind* must be distinguished. The executor reads the table; it
//! never branches on a provider name. That is the ADR-0260 invariant — the
//! derivation and request paths contain no provider-name or dialect branches.
//!
//! ## Why the surface is data and not code
//!
//! A dialect that is "a boolean plus hand-written code in six files" is legacy
//! burden: every new signed subscription surface re-implements the same six
//! edit sites. A dialect that is a table means a new surface is a new constant
//! (and, for a user-declared provider, a TOML block) — zero new `match` arms in
//! the executor, zero new variants in core enums.
//!
//! ## Identity slots vs. value remapping
//!
//! ADR-0131 forbids *remapping* a model id (the value sent upstream is always
//! the user's choice, verbatim). It does not forbid *declaring where* that value
//! appears on the wire. [`ModelCarrier`] declares the slot; the value is always
//! [`IdentityValue::WireId`] or a sibling fact about the same model. No carrier
//! may rewrite the id.

/// Where a model's identity is written on the wire.
///
/// A dialect lists the carriers it needs; the executor stamps each one from the
/// channel's wire model id (or a sibling fact). Order is irrelevant — carriers
/// are independent slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCarrier {
    /// A top-level body field (the OpenAI `"model"` field).
    BodyField(&'static str),
    /// A JSON pointer into the body (Qoder's `model_config.key`). The pointer
    /// is a `a/b/c` path rooted at the body object.
    BodyPointer(&'static str),
    /// A URL path segment placeholder, written as `{model}` inside
    /// [`InferenceSpec::path`] (Google's `models/{model}:generateContent`).
    PathSegment,
    /// A request header (Qoder's `X-Model-Key`).
    Header(&'static str),
    /// A query parameter.
    QueryParam(&'static str),
}

/// The value a carrier is stamped with. Every variant is a *fact about the
/// selected model*, never a substitution: `WireId` is the user's choice
/// verbatim (ADR-0131).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityValue {
    /// The channel's wire model id, verbatim.
    WireId,
    /// The catalog source the model came from (`system` / `custom`).
    CatalogSource,
    /// The human-readable label the catalog publishes. Presentation only.
    DisplayName,
    /// A constant string.
    Constant(&'static str),
}

/// One model-identity carrier and the fact that fills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelBinding {
    pub carrier: ModelCarrier,
    pub value: IdentityValue,
}

/// The request envelope a dialect wraps the chat-completions body in.
///
/// Most surfaces accept the flat body as-is ([`Envelope::Flat`]). A service
/// that routes through its own agent framework requires a richer envelope
/// ([`Envelope::AgentChat`]) whose scalar slots are declared here and whose
/// structural slots (`messages`, `tools`, `system`, `chat_context`) are
/// projected from the same harness request the flat body is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Envelope {
    /// The plain chat-completions object: `{model, messages, stream, …}`.
    Flat,
    /// Qoder's `agent_chat_generation` envelope. Empirically required: the flat
    /// body is rejected with `400 None flow nodes found for router
    /// agent_router`. The structural slots carry the same messages/tools the
    /// flat body would.
    AgentChat(&'static AgentChatSpec),
}

/// The declared constants of an [`Envelope::AgentChat`] envelope.
///
/// Only the *literal* slots live here. The dynamic slots are fixed by the
/// envelope's shape and filled from the request context:
/// `request_id`/`request_set_id`/`chat_record_id` (fresh UUIDs per request —
/// the server rejects duplicates), `session_id`, `model_config.*` (from the
/// model bindings), `parameters.*` (from the fitted capability view), and
/// `business.*` (from the emulated client identity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentChatSpec {
    /// The task discriminator (`FREE_INPUT`).
    pub chat_task: &'static str,
    /// The routing agent id (`agent_common`).
    pub agent_id: &'static str,
    /// The session type the service expects from this client (`qodercli`).
    pub session_type: &'static str,
    /// The task id (`common`).
    pub task_id: &'static str,
    /// The numeric source discriminator (`1`).
    pub source: u32,
    /// The envelope schema version (`3`).
    pub version: &'static str,
    /// The `business.product` value (`cli`).
    pub business_product: &'static str,
    /// The `business.type` value (`agent`).
    pub business_type: &'static str,
    /// The `business.stage` value (`start`).
    pub business_stage: &'static str,
    /// The envelope's `stream` flag. The service only answers with SSE, so the
    /// reference client states it unconditionally; declaring it keeps the
    /// envelope byte-identical to the client the service fingerprints.
    pub stream: bool,
    /// The envelope's `is_reply` flag — the reference client sets it on every
    /// chat turn.
    pub is_reply: bool,
    /// The envelope's `is_retry` flag — false for a first attempt. The retry
    /// path is a client concern the envelope does not model.
    pub is_retry: bool,
    /// The envelope's `aliyun_user_type` value. The reference client sends an
    /// empty string for a subscription account; it is declared rather than
    /// omitted so the field's presence matches the client.
    pub aliyun_user_type: &'static str,
    /// The `parameters.max_tokens` fallback when the catalog advertises no
    /// output cap for the model.
    ///
    /// This is the reference client's own normalizer default, not a muta
    /// invention: its token-count normalizer returns this value for an absent
    /// or non-positive input, and the surface's catalog publishes no
    /// `max_output_tokens`, so every turn the client sends carries it. Declaring
    /// it keeps the envelope byte-faithful; omitting `max_tokens` entirely would
    /// be the deviation.
    pub default_max_output_tokens: u32,
    /// JSON pointers (rooted at the envelope object) whose values are fresh
    /// UUIDs per request. The server rejects a replayed id with code 103.
    pub fresh_uuid_pointers: &'static [&'static str],
}

/// The client identity a dialect must present, and the version it emulates.
///
/// The emulated version participates in request signing on surfaces that sign
/// (Qoder's `Cosy-Version` is an input to the COSY canonical string), so it has
/// exactly one home here and is read by the header table, the signature
/// payload, and the envelope's `business.version` alike. A second copy is a
/// silent signature mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentitySpec {
    /// The upstream client version this dialect emulates. `""` when the
    /// surface does not advertise one.
    pub emulated_version: &'static str,
    /// The header name that carries [`Self::emulated_version`], when the
    /// surface sends one (Qoder's `Cosy-Version`). Declaring it here means the
    /// header, the signature payload, and the envelope's business version all
    /// read one value — a second copy is a silent signature mismatch.
    pub version_header: Option<&'static str>,
    /// Static headers every request of this dialect carries, beyond auth.
    pub headers: &'static [(&'static str, &'static str)],
}

impl IdentitySpec {
    /// The dialect's identity headers: the static table plus the version
    /// header (when declared). One iterator serves every request path — the
    /// inference builder and the catalog fetcher — so the two can never drift.
    pub fn headers_with_version(&self) -> Vec<(&'static str, String)> {
        let mut headers: Vec<(&'static str, String)> = self
            .headers
            .iter()
            .map(|(name, value)| (*name, (*value).to_string()))
            .collect();
        if let Some(name) = self.version_header {
            headers.push((name, self.emulated_version.to_string()));
        }
        headers
    }
}

/// A dialect's inference endpoint: where requests go and how they are shaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InferenceSpec {
    /// The URL path, appended to the connection's base URL. May contain the
    /// `{model}` placeholder when a [`ModelCarrier::PathSegment`] is declared.
    pub path: &'static str,
    /// Fixed query parameters, in declaration order.
    pub query: &'static [(&'static str, &'static str)],
    /// The path form used as a signature input, when the surface signs.
    /// `None` means the surface does not sign.
    pub signed_path: Option<&'static str>,
    /// The envelope the body is wrapped in.
    pub envelope: Envelope,
    /// The slots that carry the model identity.
    pub model_bindings: &'static [ModelBinding],
}

/// The live catalog endpoint a dialect discovers its models from.
///
/// The *shape* of the response is one of a closed set
/// ([`CatalogShape`](crate::provider_surface::CatalogShape)); the provider is
/// not. The shape carries the catalog's path, query, auth, signed path, and
/// dimensions, and the *root* is the provider spec's `catalog_root_url` — so
/// this descriptor is exactly "which shape the catalog parses as". A provider
/// that reuses a shape costs zero Rust code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSpec {
    /// The response shape to parse (also carries the path, query, auth, signed
    /// path, and dimensions).
    pub shape: crate::provider_surface::CatalogShape,
}

/// The complete declarative description of one wire dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DialectSurface {
    /// Client identity and emulated version.
    pub identity: IdentitySpec,
    /// Inference endpoint, envelope, and model carriers.
    pub inference: InferenceSpec,
    /// Live catalog endpoint, when the dialect has one.
    pub catalog: Option<CatalogSpec>,
}
