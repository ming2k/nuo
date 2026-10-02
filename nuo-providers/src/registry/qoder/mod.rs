//! The `qoder` provider template: Alibaba Qoder's subscription coding
//! platform (`api*.qoder.sh`, CN: `*.qoder.com.cn`), COSY-signed SSE wire.

pub mod identity;
pub mod pipeline;
pub mod region;
pub mod surface;
pub mod wire;

pub use identity::{QoderRequestIdentity, QoderStoredIdentity};
pub use pipeline::build_qoder_pipeline;
pub use region::{REGION_ENDPOINTS_URL, elect_infer_endpoint};
pub use wire::*;

use super::{ModelProviderSpec, RemoteCatalogSource};
use nuo_contracts::WireProtocol;
use nuo_contracts::model::Model;
use nuo_contracts::reasoning::ReasoningSupport;
use serde_json::Value;

/// The offline seed ids, owned by `nuo_contracts` so the TUI template and the
/// registry cannot disagree about them (the convention every other curated
/// provider follows).
pub use nuo_contracts::model_providers::QODER_MODELS;

/// Baseline capability metadata for the seeded models.
///
/// Qoder's live scene catalog is the authority for what exists and what is
/// runnable; this table only supplies capability fields the catalog does not
/// advertise (`tool_call`, effort ladders) and keeps the provider usable before
/// the first catalog sync.
pub const MODELS: &[Model] = &[
    Model {
        id: "qmodel_38max",
        family: "qwen",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "qfmodel",
        family: "qwen",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(nuo_contracts::model::BaselineModels(MODELS));

pub const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: nuo_contracts::ProviderDialect::Qoder,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("qoder"),
    baselines: MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api3.qoder.sh"),
    user_agent: None,
    protocol: WireProtocol::ChatCompletions,
    catalog_source: RemoteCatalogSource::Endpoint(
        nuo_contracts::provider_surface::CatalogShape::SceneMap,
    ),
    default_client_profile: nuo_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    models: QODER_MODELS,
};

/// Sign a catalog request with the connection's COSY identity.
pub fn sign_catalog_request(
    identity: &QoderRequestIdentity,
    bearer: &str,
    signed_path: &str,
    now_secs: u64,
) -> Result<wire::signer::PreparedCosy, String> {
    let cosy = wire::signer::CosyIdentity::parse(identity.machine_key_hex.expose_secret())
        .ok_or_else(|| "the connection's machine key is malformed".to_string())?;
    let identity_json = identity.identity_payload_json(bearer, "");
    Ok(wire::signer::prepare_request_for_path(
        &cosy,
        &identity_json,
        &identity.uid,
        "",
        signed_path,
        now_secs,
    ))
}

/// Build the Qoder catalog signer from the persisted identity.
pub fn build_catalog_signer(
    store: &dyn crate::oauth::CredentialStore,
    connection_id: &str,
    bearer: &str,
) -> Option<Box<dyn super::super::CatalogSigning>> {
    let identity = crate::oauth::stored_qoder_request_identity(store, connection_id)?;
    Some(Box::new(QoderCatalogSigning::new(
        identity,
        bearer.to_string(),
    )))
}

/// The catalog root for a connection: the stored identity's elected endpoint
/// when one was synced, else `None` (the caller keeps the pinned spec root).
///
/// The catalog and inference share the same COSY-signed host family, and the
/// center region map elects one host for both (§3.1a). A sync failure leaves
/// the stored identity without an election and the pin stays authoritative —
/// the same failure-never-diminishes contract as the election itself.
pub fn catalog_root_for_connection(
    store: &dyn crate::oauth::CredentialStore,
    connection_id: &str,
) -> Option<String> {
    store
        .read(connection_id)
        .ok()??
        .get_json_attr::<QoderStoredIdentity>("qoder")?
        .infer_endpoint
}

/// The Qoder catalog signer.
pub struct QoderCatalogSigning {
    identity: QoderRequestIdentity,
    bearer: String,
}

impl QoderCatalogSigning {
    pub fn new(identity: QoderRequestIdentity, bearer: String) -> Self {
        Self { identity, bearer }
    }
}

impl super::super::CatalogSigning for QoderCatalogSigning {
    fn identity_headers(&self) -> Vec<(String, String)> {
        surface::QODER_SURFACE
            .identity
            .headers_with_version()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    fn identity_subject_headers(&self) -> Vec<(String, String)> {
        if self.identity.uid.is_empty() {
            Vec::new()
        } else {
            vec![("Cosy-User".to_string(), self.identity.uid.clone())]
        }
    }

    fn sign(&self, signed_path: &str) -> Result<super::super::CatalogSignature, String> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let signed = sign_catalog_request(&self.identity, &self.bearer, signed_path, now_secs)?;
        Ok(super::super::CatalogSignature {
            authorization: signed.authorization,
            date: signed.date,
            key: signed.key,
        })
    }
}

/// Parse Qoder's scene-keyed catalog into the requested scene's models.
///
/// Function switches never appear in the result: they are routing modes, not
/// models, so their absence is a membership fact rather than an availability
/// verdict (ADR-0281). Dropping them here — rather than marking them
/// unavailable — keeps the model universe pure: a switch that becomes
/// `enable:true` on a paid plan must not surface as a selectable model, because
/// selecting it hands model choice to the server and invalidates every
/// capability muta fitted for the channel.
pub fn parse_scene_catalog(json: &Value, scene: &str) -> Vec<super::super::DiscoveredModel> {
    let Some(scenes) = json.as_object() else {
        return Vec::new();
    };
    // The owning scene name is part of the switch rule: scene-scoped forms
    // carry it as a hyphen prefix (`quest-auto`, `qwork-advanced`). Read the
    // scene the connection declared, falling back to `assistant` (the CLI's
    // default) when the response does not carry it, and to every scene only
    // when neither is present.
    let scoped = scenes
        .get_key_value(scene)
        .or_else(|| scenes.get_key_value("assistant"))
        .and_then(|(name, entries)| {
            entries
                .as_array()
                .map(|entries| vec![(name.as_str(), entries.as_slice())])
        })
        .unwrap_or_else(|| {
            scenes
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .as_array()
                        .map(|entries| (name.as_str(), entries.as_slice()))
                })
                .collect()
        });
    scoped
        .into_iter()
        .flat_map(|(owning_scene, entries)| {
            entries
                .iter()
                .filter_map(move |entry| discovered_from_scene_entry(entry, owning_scene))
        })
        .collect()
}

/// The provider's stated reason an entry is unusable, verbatim.
///
/// Qoder carries it as an opaque i18n key (`strategies[].disabled_message_key`,
/// e.g. `codeSafeModelReason`) rather than as prose. `[INV-AVAIL-02]` permits
/// recording it and nothing more: the key is stored as the provider stated it,
/// and no surface resolves it against the vendor's own text table — that would
/// localize the reason (`[INV-AVAIL-03]`) and would put a vendor copy table in
/// muta's core (ADR-0281).
fn disabled_reason(entry: &Value) -> Option<String> {
    entry
        .get("strategies")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|strategy| strategy.get("disabled_message_key").and_then(Value::as_str))
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
}

fn discovered_from_scene_entry(
    entry: &Value,
    scene: &str,
) -> Option<super::super::DiscoveredModel> {
    let key = entry.get("key").and_then(Value::as_str)?.trim();
    if key.is_empty() || surface::is_function_switch(key, scene) {
        return None;
    }
    // `enable:false` is subscription-locked, not absent: the official CLI's
    // `/model` menu lists such entries greyed-out, so the entry is kept as a
    // declared-unusable model. The verdict carries the provider's own reason
    // when the payload states one (`strategies[].disabled_message_key`) and
    // `None` otherwise — no surface may invent one (ADR-0273).
    let reason = disabled_reason(entry);
    let availability = match entry.get("enable") {
        Some(Value::Bool(true)) => Some(nuo_contracts::Availability::usable()),
        Some(Value::Bool(false)) => Some(nuo_contracts::Availability::locked(reason)),
        Some(Value::Number(number)) => {
            if number.as_i64().unwrap_or(0) != 0 {
                Some(nuo_contracts::Availability::usable())
            } else {
                Some(nuo_contracts::Availability::locked(reason))
            }
        }
        _ => None,
    };
    let context_window = entry
        .get("context_config")
        .and_then(Value::as_object)
        .and_then(|configs| {
            configs
                .values()
                .find(|tier| tier.get("is_default").and_then(Value::as_bool) == Some(true))
                .and_then(|tier| tier.get("token_count"))
                .and_then(Value::as_u64)
        })
        .or_else(|| entry.get("max_input_tokens").and_then(Value::as_u64))
        .and_then(|tokens| usize::try_from(tokens).ok());
    let effort_levels = entry
        .get("thinking_config")
        .and_then(|config| config.get("enabled"))
        .and_then(|enabled| enabled.get("efforts"))
        .and_then(Value::as_object)
        .map(|efforts| efforts.keys().cloned().collect());
    Some(super::super::DiscoveredModel {
        id: key.to_string(),
        availability,
        advertised: None,
        protocol: None,
        endpoint: None,
        family: entry
            .get("family")
            .and_then(Value::as_str)
            .map(str::to_string),
        name: entry
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_window,
        max_output_tokens: None,
        reasoning: entry.get("is_reasoning").and_then(Value::as_bool),
        thinking: entry
            .get("is_reasoning")
            .and_then(Value::as_bool)
            .map(|reasoning| {
                if reasoning {
                    ReasoningSupport::ReasoningContent
                } else {
                    ReasoningSupport::None
                }
            }),
        tool_call: None,
        vision: entry.get("is_vl").and_then(Value::as_bool),
        effort_levels,
        catalog_source: entry
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::MODEL_PROVIDER_SPEC as SPEC;
    use super::{MODELS, QODER_MODELS, parse_scene_catalog};
    use nuo_contracts::WireProtocol;
    use serde_json::json;

    #[test]
    fn spec_resolves_and_serves_the_chat_wire() {
        assert_eq!(SPEC.id, "qoder");
        assert_eq!(SPEC.protocol, WireProtocol::ChatCompletions);
        assert_eq!(SPEC.baselines.len(), 2);
    }

    /// The official CLI's `/model` menu lists subscription-locked entries
    /// greyed-out, so `enable:false` is a kept, marked entry — never a drop.
    /// Mirrors the decrypted server catalog (17 `assistant` entries, 2 enabled).
    #[test]
    fn scene_catalog_keeps_locked_entries_marked() {
        let json = json!({
            "assistant": [
                {
                    "key": "qmodel_38max",
                    "display_name": "Qwen3.8-Max",
                    "enable": true,
                    "is_default": true,
                    "is_reasoning": true,
                    "is_vl": true,
                    "source": "system"
                },
                {
                    "key": "gmodel",
                    "display_name": "GLM-5.3",
                    "enable": false,
                    "is_reasoning": true,
                    "is_vl": true,
                    "source": "system"
                },
                {
                    "key": "kmodel",
                    "display_name": "Kimi-K2.8-Preview",
                    "enable": 0,
                    "is_vl": true,
                    "source": "system",
                    // The vendor's own reason key, carried verbatim.
                    "strategies": [
                        { "tag": "C4", "enabled": false, "disabled_message_key": "codeSafeModelReason" }
                    ]
                },
                {
                    "key": "qmodel_latest",
                    "display_name": "Qwen3.7-Max",
                    "enable": false,
                    "is_vl": true,
                    "source": "system"
                }
            ]
        });
        let models = parse_scene_catalog(&json, "assistant");
        assert_eq!(models.len(), 4, "locked entries must not be dropped");
        let by_id = |id: &str| {
            models
                .iter()
                .find(|model| model.id == id)
                .unwrap_or_else(|| panic!("{id} present"))
        };
        assert_eq!(
            by_id("qmodel_38max").availability,
            Some(nuo_contracts::Availability::usable())
        );
        // A locked entry the payload gives no reason for stays reasonless: no
        // surface may invent one (ADR-0273).
        assert_eq!(
            by_id("gmodel").availability,
            Some(nuo_contracts::Availability::locked(None))
        );
        // A locked entry the payload *does* explain carries the provider's own
        // key verbatim — never resolved against the vendor's text table
        // (`[INV-AVAIL-03]`).
        assert_eq!(
            by_id("kmodel").availability,
            Some(nuo_contracts::Availability::locked(Some(
                "codeSafeModelReason".to_string()
            )))
        );
        // A numeric `enable: 0` is a lock, same as `false`.
        assert_eq!(
            by_id("qmodel_latest")
                .availability
                .as_ref()
                .map(|a| a.usable),
            Some(false)
        );
        assert_eq!(by_id("gmodel").name.as_deref(), Some("GLM-5.3"));
    }

    /// An entry carrying no `enable` at all declares no verdict. `None` is
    /// undeclared, never "locked" — `[INV-AVAIL-04]`.
    #[test]
    fn absent_enable_is_undeclared_not_locked() {
        let json = json!({
            "assistant": [{ "key": "qfmodel", "display_name": "Qwen3.8-Flash" }]
        });
        let models = parse_scene_catalog(&json, "assistant");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].availability, None);
    }

    /// A blank or missing `key` is not a model and cannot become one: the entry
    /// is dropped rather than surfacing an empty id that no route could name.
    #[test]
    fn blank_key_is_dropped() {
        let json = json!({
            "assistant": [
                { "key": "  ", "display_name": "Blank" },
                { "display_name": "No key" },
                { "key": "qfmodel", "display_name": "Qwen3.8-Flash" }
            ]
        });
        let models = parse_scene_catalog(&json, "assistant");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["qfmodel"]);
    }

    /// Function switches are routing modes, not models: they never enter the
    /// model universe, in either their bare or their scene-prefixed form.
    /// Their absence is a membership fact, not an availability verdict — a
    /// switch that becomes `enable:true` on a paid plan must still not surface
    /// as a selectable model (ADR-0281).
    #[test]
    fn scene_catalog_drops_function_switches() {
        let json = json!({
            "assistant": [
                { "key": "auto",         "display_name": "Auto",        "enable": true },
                { "key": "ultimate",     "display_name": "Ultimate",    "enable": true },
                { "key": "performance",  "display_name": "Performance", "enable": false },
                { "key": "efficient",    "display_name": "Efficient",   "enable": true },
                { "key": "advanced",     "display_name": "Advanced",    "enable": true },
                { "key": "qfmodel",      "display_name": "Qwen3.8-Flash", "enable": true }
            ]
        });
        let models = parse_scene_catalog(&json, "assistant");
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["qfmodel"],
            "function switches must not enter the model universe"
        );
    }

    /// A scene-scoped switch carries its owning scene as a hyphen prefix; the
    /// prefix is stripped before the vocabulary test. Model keys use `_`, so the
    /// two namespaces cannot collide.
    #[test]
    fn scene_prefixed_switches_are_dropped_in_their_own_scene() {
        let json = json!({
            "qwork": [
                { "key": "qwork-auto",     "display_name": "Auto",     "enable": true },
                { "key": "qwork-advanced", "display_name": "Advanced", "enable": true },
                { "key": "smodel",         "display_name": "Sonus",    "enable": true }
            ],
            "quest": [
                { "key": "quest-ultimate", "display_name": "Ultimate", "enable": true },
                { "key": "cmodel",         "display_name": "Cantus",   "enable": true }
            ]
        });
        let qwork = parse_scene_catalog(&json, "qwork");
        assert_eq!(
            qwork.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["smodel"]
        );
        // A model key that merely contains a switch word as a substring is not
        // a switch — the test is on the whole (optionally prefixed) key.
        assert!(
            parse_scene_catalog(&json, "quest")
                .iter()
                .any(|m| m.id == "cmodel")
        );
    }

    /// The seed ids and the baseline capability table must describe the *same*
    /// models in the *same* order. They live in two crates because the TUI
    /// cannot depend on `muta-providers`, so the agreement is an invariant this
    /// test owns rather than something the compiler can express — it is exactly
    /// the invariant the old duplicated `qoder3*` seed silently broke.
    #[test]
    fn seed_ids_and_baseline_capabilities_agree() {
        let seed: Vec<&str> = QODER_MODELS.to_vec();
        let baselines: Vec<&str> = MODELS.iter().map(|model| model.id).collect();
        assert_eq!(
            seed, baselines,
            "the contracts seed and the registry baselines drifted apart"
        );
        assert_eq!(seed, ["qmodel_38max", "qfmodel"]);
        assert_eq!(SPEC.models, QODER_MODELS, "the spec seeds from one source");
        assert_eq!(SPEC.baselines.len(), 2);
    }
}
