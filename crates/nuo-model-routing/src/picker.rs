//! The provider picker snapshot: the presentation-layer view of the catalog
//! (rows, per-model effort/thinking info, auth badges) rendered by the
//! TUI's Connections/Models modals.

use super::derive::derive_entries;
use super::{Stores, effective_default_connection_id};
use nuo_persistence::config::Config;
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_protocol::{ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot};
use nuo_provider::catalog::{Channel, ProviderEntry, Transport};

/// Whether a model id matches any pattern in `hidden_patterns` (case-insensitive glob or exact match).
pub fn model_is_hidden(model: &str, hidden_patterns: &[String]) -> bool {
    let model_lower = model.to_ascii_lowercase();
    hidden_patterns.iter().any(|pattern| {
        let pat = pattern.trim().to_ascii_lowercase();
        if pat.is_empty() {
            return false;
        }
        if pat.contains('*') || pat.contains('?') {
            glob::Pattern::new(&pat)
                .map(|p| p.matches(&model_lower))
                .unwrap_or(false)
        } else {
            pat == model_lower
        }
    })
}

pub(super) fn active_model_id_for_entry(
    config: &Config,
    entry: &ProviderEntry,
    usage: &ConnectionUsage,
) -> Option<String> {
    config
        .default_model
        .as_deref()
        .filter(|m| entry.offers_model(m))
        .map(|m| m.to_string())
        .or_else(|| {
            usage
                .last_model_for(&entry.id)
                .filter(|m| entry.offers_model(m))
                .filter(|m| !model_is_hidden(m, &config.hidden_models))
                .map(|m| m.to_string())
        })
        .or_else(|| {
            entry
                .channels
                .iter()
                .find(|c| !model_is_hidden(&c.model, &config.hidden_models))
                .map(|channel| channel.model.clone())
        })
        .or_else(|| entry.default_channel().map(|channel| channel.model.clone()))
}

pub fn build_picker_state(config: &Config, usage: &ConnectionUsage) -> ProviderPickerSnapshot {
    build_picker_state_with_rules(config, usage, &std::collections::HashMap::new())
}

pub fn build_picker_state_with_rules(
    config: &Config,
    usage: &ConnectionUsage,
    dynamic_rules: &std::collections::HashMap<String, Vec<nuo_protocol::ModelAvailabilityRule>>,
) -> ProviderPickerSnapshot {
    let stores = Stores::load();
    let entries = derive_entries(&stores.connections, &stores.inputs());
    let default_id = effective_default_connection_id(config, &stores);
    let rows = entries
        .iter()
        .map(|entry| {
            let (protocol, base_url) = entry
                .default_channel()
                .map(channel_protocol_and_base_url)
                .unwrap_or_default();
            let model = active_model_id_for_entry(config, entry, usage).unwrap_or_default();
            let visible_channels: Vec<_> = entry
                .channels
                .iter()
                .filter(|c| !model_is_hidden(&c.model, &config.hidden_models))
                .collect();
            let channels_to_show = if visible_channels.is_empty() {
                entry.channels.iter().collect::<Vec<_>>()
            } else {
                visible_channels
            };
            let connection = stores.connections.get(&entry.id);
            let conn_rules = dynamic_rules
                .get(&entry.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let model_info = channels_to_show
                .iter()
                .copied()
                .map(channel_model_info)
                .map(|mut info| {
                    // Favorite is model-level (ADR-0046): a starred
                    // daily-driver model carries its flag into the flat
                    // Models picker wherever it is served.
                    info.favorite = config.favorites.iter().any(|fav| fav == &info.model);
                    // Recency is (connection, model) level:
                    // the flat Models picker's "recent" section is ordered by
                    // it. 0 (never activated on this connection) surfaces as `None`.
                    let recency = usage.model_recency(&entry.id, &info.model);
                    info.last_used_ms = (recency > 0).then_some(recency);
                    // Availability is resolved against the connection's own
                    // scope here — the SAME helper the server gate uses — so
                    // the row can never claim a model is runnable when the
                    // route would be refused, and a sovereign override is
                    // disclosed rather than silently erasing the upstream
                    // declaration ([INV-CATALOG-04]).
                    if let Some(connection) = connection {
                        let remote = channels_to_show
                            .iter()
                            .find(|channel| channel.model == info.model)
                            .and_then(|channel| channel.remote.as_ref());
                        let (effective, overridden) =
                            super::derive::effective_availability_with_rules(
                                connection,
                                &info.model,
                                remote,
                                &stores.providers,
                                conn_rules,
                            );
                        info.availability_overridden = overridden;
                        info.availability = if overridden || !effective.usable {
                            Some(effective)
                        } else {
                            info.availability.take()
                        };
                    }
                    // A verdict whose refresh has since failed is presented as
                    // observed-but-unverified rather than freshly confirmed
                    // (ADR-0068). Only declared verdicts can be stale.
                    info.availability_stale = info.availability.is_some()
                        && stores
                            .cache
                            .model_lists
                            .get(&entry.id)
                            .is_some_and(|state| state.refresh_failed);
                    info
                })
                .collect();
            let provider = connection.map(|p| p.provider.clone()).unwrap_or_default();
            let headers = connection.map(|p| p.headers.clone()).unwrap_or_default();
            let auth = connection.map(|p| p.auth.clone()).unwrap_or_default();
            let recency = usage.recency_of(&entry.id);
            ProviderPickerRow {
                id: entry.id.clone(),
                name: entry.name.clone(),
                model,
                models: channels_to_show.iter().map(|c| c.model.clone()).collect(),
                model_info,
                builtin: entry.builtin,
                protocol,
                base_url,
                key_ready: entry.key_ready(),
                provider,
                headers,
                last_used_ms: if recency == 0 { None } else { Some(recency) },
                auth,
            }
        })
        .collect();
    ProviderPickerSnapshot { default_id, rows }
}

/// Prune model ids and connection entries from `config` (favorites, default_model)
/// and `usage` (recency, last_models) that are no longer served by any known connection.
pub fn prune_stale_models(config: &mut Config, usage: &mut ConnectionUsage) -> bool {
    let stores = Stores::load();
    let valid_connection_ids: std::collections::HashSet<String> = stores
        .connections
        .connections
        .iter()
        .map(|c| c.name.clone())
        .collect();

    let mut connection_models_map: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    let mut all_valid_models: std::collections::HashSet<String> = std::collections::HashSet::new();

    for conn in &stores.connections.connections {
        let models = super::derive::route_models(conn, &stores.cache, &stores.providers);
        for m in &models {
            all_valid_models.insert(m.clone());
        }
        connection_models_map.insert(conn.name.clone(), models.into_iter().collect());
    }

    let mut changed = false;

    // Prune favorites: only retain models currently offered by at least one connection.
    let prev_fav_len = config.favorites.len();
    config
        .favorites
        .retain(|fav| all_valid_models.contains(fav));
    if config.favorites.len() != prev_fav_len {
        changed = true;
        if let Err(error) = config.save_preserving_connection_selection() {
            tracing::warn!(?error, "could not persist pruned favorites");
        }
    }

    // Prune default_model if it is set to a model no longer offered by the default connection (or any connection).
    if let Some(ref dm) = config.default_model {
        let valid_for_default = connection_models_map
            .get(&config.default_connection)
            .map(|set| set.contains(dm))
            .unwrap_or(false);
        if !valid_for_default && !all_valid_models.contains(dm) {
            config.default_model = None;
            changed = true;
            if let Err(error) = config.save_preserving_connection_selection() {
                tracing::warn!(?error, "could not persist pruned default model");
            }
        }
    }

    // Prune connection usage telemetry.
    let usage_changed = usage.prune(
        |conn_id| valid_connection_ids.contains(conn_id),
        |conn_id, model_id| {
            connection_models_map
                .get(conn_id)
                .is_some_and(|set| set.contains(model_id))
        },
    );
    if usage_changed {
        changed = true;
        if let Err(error) = usage.save_exact() {
            tracing::warn!(?error, "could not persist pruned connection usage");
        }
    }

    changed
}

/// Load on-disk config and usage, prune stale models, and persist if changed.
pub fn prune_stale_models_on_disk() -> bool {
    let mut config = Config::load();
    let mut usage = ConnectionUsage::load();
    prune_stale_models(&mut config, &mut usage)
}

pub(super) fn channel_protocol_and_base_url(channel: &Channel) -> (String, String) {
    match &channel.transport {
        Transport::OpenAiChatCompletions { base_url, .. } => (
            nuo_protocol::WireProtocol::OpenAiChatCompletions
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::OpenAiResponses { base_url, .. } => (
            nuo_protocol::WireProtocol::OpenAiResponses
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::AnthropicMessages { base_url, .. } => (
            nuo_protocol::WireProtocol::AnthropicMessages
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::GoogleGenerateContent { base_url, .. } => (
            nuo_protocol::WireProtocol::GoogleGenerateContent
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::GoogleInteractions { base_url, .. } => (
            nuo_protocol::WireProtocol::GoogleInteractions
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
    }
}

pub fn channel_model_info(channel: &Channel) -> ProviderModelInfo {
    // The route's effective capabilities: the full ADR-0068 resolution
    // (baseline ⊕ remote advertisement ⊕ user overrides). Surfaced so the
    // frontend can gate image affordances, context meters, and telemetry on
    // exactly what this route will actually accept (ADR-0068) — never a
    // client-side re-resolution of the static registry, which cannot see the
    // fitted overlay or per-route overrides.
    let caps = channel.capabilities();
    let vision = caps.vision;
    let context_window = caps.context_window;
    let max_output_tokens = caps.max_output_tokens;
    // The provider-published label for this route, when it advertises one.
    // Bottom-up and presentation-only: it comes straight from the remote
    // catalog, the client never invents or curates it, and an absent label
    // leaves the surfaces on the wire id.
    // The provider-published label for this route, when it advertises one.
    // Bottom-up and presentation-only: it comes straight from the remote
    // catalog, the client never invents or curates it, and an absent label
    // leaves the surfaces on the wire id.
    let name: Option<String> = channel
        .remote
        .as_ref()
        .and_then(|remote| remote.name.clone());
    // The route's tunables, derived from the provider's own declaration. The
    // framework never sniffs the protocol or the family to decide which knobs
    // exist — it asks the resolved capabilities
    // (`ModelCapabilities::tunables`) and renders whatever comes back.
    let effort_levels: Vec<String> = caps
        .effort_levels
        .iter()
        .map(|lvl| lvl.as_str().to_string())
        .collect();
    let effort = channel
        .effective_effort(&caps)
        .map(|effort| effort.as_str().to_string());
    let thinking = channel.thinking_state(&caps);
    // The provider's declared availability for this model, round-tripped from
    // the remote catalog (ADR-0068). This is the *declaration*; the caller
    // applies the user's sovereign override via `derive::effective_availability`
    // and sets `availability_overridden`. `None` is undeclared, never disabled.
    let availability = channel
        .remote
        .as_ref()
        .and_then(|remote| remote.availability.clone());
    // The provider's listing intent, orthogonal to availability: an
    // API-supported model may be deliberately unlisted (Codex `visibility`).
    let advertised = channel.remote.as_ref().and_then(|remote| remote.advertised);
    ProviderModelInfo {
        model: channel.model.clone(),
        name,
        protocol: channel.transport.protocol_label().to_string(),
        effort,
        thinking,
        effort_levels,
        favorite: false,
        last_used_ms: None,
        vision,
        context_window,
        max_output_tokens,
        availability,
        availability_overridden: false,
        advertised,
        availability_stale: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nuo_protocol::PromptCacheCapabilities;

    /// A minimal OpenAI-transport channel for `channel_model_info` tests.
    fn openai_channel(model: &str, remote: Option<nuo_protocol::RemoteModelMetadata>) -> Channel {
        Channel {
            id: "default".to_string(),
            transport: Transport::OpenAiChatCompletions {
                base_url: "https://example.test/v1/chat/completions".to_string(),
                dialect: nuo_protocol::OpenAiChatDialect::Standard,
                user_agent: nuo_provider::NUO_USER_AGENT.to_string(),
                headers: Vec::new(),
            },
            credentials: nuo_protocol::static_credential(String::new()),
            model: model.to_string(),
            remote,
            user_overrides: None,
            tunings: Default::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
            prompt_cache_preference: Default::default(),
        }
    }

    #[test]
    fn channel_model_info_surfaces_route_vision_from_remote_metadata() {
        // A model the static baseline does not know (a fresh relay model).
        // The remote advertisement says it takes images; the info row must
        // carry that so the frontend's image affordances unlock.
        let remote = nuo_protocol::RemoteModelMetadata {
            vision: Some(true),
            ..Default::default()
        };
        let info = channel_model_info(&openai_channel("omen-alpha", Some(remote)));
        assert_eq!(
            info.vision,
            Some(true),
            "remote vision advertisement must surface"
        );
    }

    #[test]
    fn channel_model_info_leaves_vision_undeclared_without_evidence() {
        // No remote metadata and no baseline entry: no layer declares image
        // support, so the row carries `None` — *undeclared*, which a frontend
        // must treat as "try it" rather than as text-only (ADR-0068). The old
        // behavior coerced this to `false`, which silently stripped images for
        // every endpoint that does not advertise a capability field.
        let info = channel_model_info(&openai_channel("unknown-relay-model", None));
        assert_eq!(info.vision, None);
    }

    #[test]
    fn channel_model_info_surfaces_the_providers_lock_declaration() {
        // Qoder catalogs list subscription-locked models with `enable:false`;
        // the row must carry that so pickers render it greyed-out (official
        // `/model` parity) and refuse activation.
        let locked = nuo_protocol::RemoteModelMetadata {
            availability: Some(nuo_protocol::Availability::locked(None)),
            ..Default::default()
        };
        assert_eq!(
            channel_model_info(&openai_channel("gmodel", Some(locked))).availability,
            Some(nuo_protocol::Availability::locked(None))
        );
        // Undeclared stays undeclared — never coerced to usable *or* locked.
        assert_eq!(
            channel_model_info(&openai_channel("qfmodel", None)).availability,
            None
        );
    }

    #[test]
    fn channel_model_info_keeps_listing_intent_apart_from_availability() {
        // Codex's `visibility != "list"` is a *listing* declaration. An
        // API-supported model is usable whether or not it is advertised, so the
        // two fields must move independently (ADR-0068).
        let unlisted_but_usable = nuo_protocol::RemoteModelMetadata {
            availability: Some(nuo_protocol::Availability::usable()),
            advertised: Some(false),
            ..Default::default()
        };
        let info = channel_model_info(&openai_channel("hidden-helper", Some(unlisted_but_usable)));
        assert_eq!(info.advertised, Some(false));
        assert_eq!(
            info.availability,
            Some(nuo_protocol::Availability::usable())
        );
    }

    #[test]
    fn channel_model_info_user_override_beats_remote_vision() {
        // ADR-0068 layer 1: the user's explicit `Some(false)` wins over the
        // provider's advertised vision (e.g. a relay that strips images).
        let remote = nuo_protocol::RemoteModelMetadata {
            vision: Some(true),
            ..Default::default()
        };
        let mut channel = openai_channel("omen-alpha", Some(remote));
        channel.user_overrides = Some(nuo_protocol::CapabilityOverrides {
            vision: Some(false),
            ..Default::default()
        });
        assert_eq!(channel_model_info(&channel).vision, Some(false));

        // And the inverse: a forced-on override over a text-only baseline.
        let mut channel = openai_channel("text-only-model", None);
        channel.user_overrides = Some(nuo_protocol::CapabilityOverrides {
            vision: Some(true),
            ..Default::default()
        });
        assert_eq!(channel_model_info(&channel).vision, Some(true));
    }

    #[test]
    fn channel_model_info_surfaces_the_provider_published_label() {
        // The relay names `deepseek-flash` "DeepSeek V4.1 Flash": the label
        // rides to the frontend as a presentation-only annotation beside the
        // id, which stays the identity.
        let remote = nuo_protocol::RemoteModelMetadata {
            name: Some("DeepSeek V4.1 Flash".to_string()),
            ..Default::default()
        };
        let info = channel_model_info(&openai_channel("deepseek-flash", Some(remote)));
        assert_eq!(info.model, "deepseek-flash");
        assert_eq!(info.name.as_deref(), Some("DeepSeek V4.1 Flash"));
    }

    #[test]
    fn channel_model_info_omits_the_label_when_none_is_advertised() {
        // The common case (stock OpenAI-compatible `/models`): no label at all,
        // so the frontends fall back to the bare wire id.
        let info = channel_model_info(&openai_channel("glm-5.2", None));
        assert_eq!(info.model, "glm-5.2");
        assert_eq!(info.name, None);
    }

    #[test]
    fn channel_model_info_surfaces_route_context_window_from_remote_metadata() {
        // ADR-0068: Discovered models (like glm-5.3 on opencode-go) carry their
        // remote context_window via ADR-0068 resolution into the picker snapshot.
        let remote = nuo_protocol::RemoteModelMetadata {
            context_window: Some(1_000_000),
            max_output_tokens: Some(131_072),
            ..Default::default()
        };
        let info = channel_model_info(&openai_channel("glm-5.3", Some(remote)));
        assert_eq!(info.context_window, 1_000_000);
        assert_eq!(info.max_output_tokens, Some(131_072));
    }

    #[test]
    fn channel_model_info_surfaces_effort_levels_from_remote_metadata() {
        let remote = nuo_protocol::RemoteModelMetadata {
            effort_levels: Some(vec![
                nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Low),
                nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Medium),
                nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::High),
                nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Xhigh),
                nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Max),
                nuo_protocol::EffortLevel::Known(nuo_protocol::Effort::Ultra),
            ]),
            ..Default::default()
        };
        let channel = openai_channel("gpt-6-astra", Some(remote));
        let info = channel_model_info(&channel);
        assert_eq!(
            info.effort_levels,
            vec!["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(info.effort, Some("medium".to_string()));
    }

    #[test]
    fn channel_model_info_user_override_beats_remote_context_window() {
        // ADR-0068 layer 1: user override takes precedence over remote metadata.
        let remote = nuo_protocol::RemoteModelMetadata {
            context_window: Some(1_000_000),
            ..Default::default()
        };
        let mut channel = openai_channel("glm-5.3", Some(remote));
        channel.user_overrides = Some(nuo_protocol::CapabilityOverrides {
            context_window: Some(64_000),
            ..Default::default()
        });
        let info = channel_model_info(&channel);
        assert_eq!(info.context_window, 64_000);
    }

    #[test]
    fn model_is_hidden_matches_exact_and_glob_case_insensitively() {
        let hidden = vec![
            "gemini-3.6-flash*".to_string(),
            "chat_*".to_string(),
            "deprecated-model".to_string(),
        ];

        assert!(model_is_hidden("gemini-3.6-flash-high", &hidden));
        assert!(model_is_hidden("GEMINI-3.6-FLASH-LOW", &hidden));
        assert!(model_is_hidden("gemini-3.6-flash", &hidden));
        assert!(model_is_hidden("chat_20706", &hidden));
        assert!(model_is_hidden("deprecated-model", &hidden));

        assert!(!model_is_hidden("gemini-3.7-flash", &hidden));
        assert!(!model_is_hidden("gemini-3.7-flash-tiered", &hidden));
        assert!(!model_is_hidden("gemini-pro-agent", &hidden));
        assert!(!model_is_hidden("claude-sonnet-4-6", &hidden));
    }
}

#[cfg(test)]
mod display_policy_tests {
    use nuo_provider::DisplayPolicy;

    fn ensure_init() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(nuo_server::provider_registry::init);
    }

    /// The mapping stage is a declared property of the surface, never a guess
    /// from the id shape (ADR-0081 [INV-CATALOG-05]).
    #[test]
    fn native_policy_trusts_a_published_label_verbatim() {
        ensure_init();
        let spec = nuo_provider::model_provider_spec("anthropic").expect("spec");
        assert_eq!(spec.display_policy, DisplayPolicy::Native);
        assert_eq!(
            spec.resolve_display_label(Some("Claude Opus 4.6 (Thinking)"), "claude-opus-4-6"),
            Some("Claude Opus 4.6".to_string())
        );
        // No label advertised -> no label invented.
        assert_eq!(spec.resolve_display_label(None, "claude-opus-4-6"), None);
        // A label identical to the id collapses to none.
        assert_eq!(
            spec.resolve_display_label(Some("claude-opus-4-6"), "claude-opus-4-6"),
            None
        );
    }

    /// A relay publishes bare ids, so its labels are lifted structurally.
    #[test]
    fn structural_policy_lifts_bare_relay_ids_into_typography() {
        ensure_init();
        let spec = nuo_provider::model_provider_spec("opencode-go").expect("spec");
        assert_eq!(spec.display_policy, DisplayPolicy::Structural);
        assert_eq!(
            spec.resolve_display_label(None, "deepseek-v4.1-flash"),
            Some("DeepSeek V4.1 Flash".to_string())
        );
        assert_eq!(
            spec.resolve_display_label(None, "glm-5.3-flash"),
            Some("GLM 5.3 Flash".to_string())
        );
        // A published hyphenated label is folded too.
        assert_eq!(
            spec.resolve_display_label(Some("DeepSeek-V4.1-Flash"), "deepseek-v4.1-flash"),
            Some("DeepSeek V4.1 Flash".to_string())
        );
    }
}
