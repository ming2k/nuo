//! Qoder's declarative wire surface: Alibaba Qoder's subscription coding
//! platform (`api*.qoder.sh`, CN: `*.qoder.com.cn`), COSY-signed SSE.
//!
//! Everything the executor needs to speak this surface is here as data — the
//! inference path and its fixed query, the emulated client version, the
//! envelope the body is wrapped in, the slots that carry the model identity,
//! and the live catalog endpoint.

use nuo_model_codec::wire_surface::{
    AgentChatSpec, CatalogSpec, DialectSurface, Envelope, IdentitySpec, IdentityValue,
    InferenceSpec, ModelBinding, ModelCarrier,
};

/// Qoder's per-request inference path (the URL pathname minus the base host).
pub const INFERENCE_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";

/// Qoder's fixed inference query, in wire order.
pub const INFERENCE_QUERY: &[(&str, &str)] = &[
    ("FetchKeys", "llm_model_result"),
    ("AgentId", "agent_common"),
    ("Encode", "1"),
];

/// The path form that participates in the COSY signature: the URL pathname
/// minus the `/algo` prefix, no query string.
pub const SIGNED_PATH: &str = "/api/v2/service/pro/sse/agent_chat_generation";

/// The catalog path (the URL pathname minus the base host).
#[allow(dead_code)]
pub const CATALOG_PATH: &str = "/algo/api/v2/model/list";

/// Qoder's *function switches*: catalog entries that select a routing mode
/// rather than a model.
///
/// The vendor's own text table describes them as switches, not models — `auto`
/// is "smartly select the optimal model, balancing performance and cost",
/// `ultimate`/`performance`/`efficient`/`advanced` are quality tiers, and the
/// selector groups them under `modelSelector.functionSwitch.*`. Sending one as
/// `X-Model-Key` delegates model choice to the server, which makes every
/// capability muta fits for the channel (effort, thinking, context window)
/// describe a model nobody selected — `performance` even advertises a
/// `272K` window that no real entry carries.
///
/// This vocabulary is **surface data, not a heuristic**: it is the closed set
/// the vendor published as of [`COSY_VERSION`], and
/// `tests/fixtures/qoder-model-list-*.json` pins it. A new switch name is a
/// contract change, caught by the fixture audit rather than guessed at here.
pub const FUNCTION_SWITCH_KEYS: &[&str] =
    &["advanced", "auto", "efficient", "performance", "ultimate"];

/// Whether a catalog `key` names a function switch in the scene it appeared in.
///
/// Scene-scoped forms carry their owning scene as a hyphen prefix
/// (`quest-auto`, `qwork-advanced`, `experts-ultimate`, `nap-auto`), so the
/// prefix is stripped before the vocabulary test. Model keys never use a
/// hyphen — they use `_` (`qmodel_38max`, `kmodel_latest`) — so the two
/// namespaces cannot collide.
pub fn is_function_switch(key: &str, scene: &str) -> bool {
    let bare = key
        .strip_prefix(scene)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(key);
    FUNCTION_SWITCH_KEYS.contains(&bare)
}

/// The catalog's signed-path form (the URL pathname minus the `/algo` prefix,
/// no query). One signer covers both this and [`SIGNED_PATH`].
#[allow(dead_code)]
pub const CATALOG_SIGNED_PATH: &str = "/api/v2/model/list";

/// The COSY protocol version string this surface emulates.
pub const COSY_VERSION: &str = "1.1.58";

/// The static identity headers Qoder's client always sends.
pub const IDENTITY_HEADERS: &[(&str, &str)] = &[
    ("Accept", "text/event-stream"),
    ("Cache-Control", "no-cache"),
    ("Connection", "keep-alive"),
    ("Cosy-ClientType", "5"),
    ("Cosy-MachineType", "5"),
    ("Cosy-Business-Product", "cli"),
    ("Cosy-Business-Type", "agent"),
    ("Cosy-Scene", "assistant"),
    ("Cosy-Data-Policy", "agree"),
    ("Login-Version", "v2"),
];

/// The header that carries [`COSY_VERSION`].
pub const VERSION_HEADER: &str = "Cosy-Version";

/// The `agent_chat_generation` envelope's literal slots.
pub const AGENT_CHAT: AgentChatSpec = AgentChatSpec {
    chat_task: "FREE_INPUT",
    agent_id: "agent_common",
    session_type: "qodercli",
    task_id: "common",
    source: 1,
    version: "3",
    business_product: "cli",
    business_type: "agent",
    business_stage: "start",
    // The reference client (qodercli 1.1.59, `UT`) states these four on every
    // chat turn regardless of the turn's content. The service answers only with
    // SSE, so `stream` is not a request option here — it is part of the shape
    // the client is fingerprinted on.
    stream: true,
    is_reply: true,
    is_retry: false,
    aliyun_user_type: "",
    // The reference client's token-count normalizer returns 32000 for an absent
    // input, and Qoder's catalog publishes no `max_output_tokens`, so this is
    // the value every turn it sends carries.
    default_max_output_tokens: 32_000,
    fresh_uuid_pointers: &["request_id", "request_set_id", "chat_record_id"],
};

/// The slots that carry the model identity.
pub const MODEL_BINDINGS: &[ModelBinding] = &[
    ModelBinding {
        carrier: ModelCarrier::Header("X-Model-Key"),
        value: IdentityValue::WireId,
    },
    ModelBinding {
        carrier: ModelCarrier::Header("X-Model-Source"),
        value: IdentityValue::CatalogSource,
    },
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/key"),
        value: IdentityValue::WireId,
    },
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/display_name"),
        value: IdentityValue::DisplayName,
    },
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/source"),
        value: IdentityValue::CatalogSource,
    },
    // The catalog entry's `format` field; every entry the surface publishes
    // carries `"openai"`, so it is bound as a constant rather than threaded
    // through `EnvelopeInput`. This belongs *inside* `model_config`, matching
    // the reference client — the envelope has no root-level `model_format`.
    ModelBinding {
        carrier: ModelCarrier::BodyPointer("model_config/format"),
        value: IdentityValue::Constant("openai"),
    },
];

/// The declarative surface for the Qoder subscription dialect.
pub const QODER_SURFACE: DialectSurface = DialectSurface {
    identity: IdentitySpec {
        emulated_version: COSY_VERSION,
        version_header: Some(VERSION_HEADER),
        headers: IDENTITY_HEADERS,
    },
    inference: InferenceSpec {
        path: INFERENCE_PATH,
        query: INFERENCE_QUERY,
        signed_path: Some(SIGNED_PATH),
        envelope: Envelope::AgentChat(&AGENT_CHAT),
        model_bindings: MODEL_BINDINGS,
    },
    catalog: Some(CatalogSpec {
        shape: nuo_model_codec::provider_surface::CatalogShape::SceneMap,
    }),
};
