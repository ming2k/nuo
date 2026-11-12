//! Picker filtering, ordering, template, and quota tests.

use super::*;

extern crate nuo_provider_anthropic;
extern crate nuo_provider_google_antigravity;
extern crate nuo_provider_openai;
use nuo_protocol::{ProviderModelInfo, ProviderPickerRow};

fn row(id: &str, name: &str, models: &[&str], builtin: bool) -> ProviderPickerRow {
    ProviderPickerRow {
        id: id.to_string(),
        name: name.to_string(),
        model: models.first().copied().unwrap_or("").to_string(),
        models: models.iter().map(|m| m.to_string()).collect(),
        model_info: Vec::new(),
        builtin,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        provider: String::new(),
        headers: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    }
}

fn sample() -> ProviderPickerSnapshot {
    ProviderPickerSnapshot {
        default_id: "openai".to_string(),
        rows: vec![
            row("kimi-code", "Kimi Code", &["kimi-k2.7-code"], true),
            row("openai", "OpenAI", &["gpt-4o", "gpt-4o-mini"], true),
            row(
                "anthropic",
                "Anthropic",
                &[
                    "claude-fable-5",
                    "claude-sonnet-5",
                    "claude-opus-4-8",
                    "claude-sonnet-4-6",
                ],
                true,
            ),
            row("my-relay", "My Relay", &["glm-5.2", "glm-5.1"], false),
        ],
    }
}

/// Build a `ProviderModelInfo` with everything neutral — no favorite, no
/// recency, no reasoning knobs.
fn info(model: &str) -> ProviderModelInfo {
    ProviderModelInfo {
        model: model.to_string(),
        name: None,
        protocol: String::new(),
        effort: None,
        thinking: None,
        effort_levels: Vec::new(),
        favorite: false,
        last_used_ms: None,
        vision: Some(false),
        context_window: 128_000,
        max_output_tokens: None,
        availability: None,

        availability_overridden: false,

        advertised: None,
        availability_stale: false,
    }
}

/// The sample with one favorited model (`claude-sonnet-5`) and two with
/// usage history — `glm-5.1` used most recently (t=2000), `gpt-4o` older
/// (t=1000) — so all three sections are populated and the RECENT section
/// has a meaningful internal order.
fn sectioned() -> ProviderPickerSnapshot {
    let mut snapshot = sample();
    for prow in &mut snapshot.rows {
        let (id, info) = match prow.id.as_str() {
            "anthropic" => ("anthropic", info("claude-sonnet-5")),
            "openai" => {
                let mut i = info("gpt-4o");
                i.last_used_ms = Some(1_000);
                ("openai", i)
            }
            "my-relay" => {
                let mut i = info("glm-5.1");
                i.last_used_ms = Some(2_000);
                ("my-relay", i)
            }
            _ => continue,
        };
        assert_eq!(id, prow.id);
        prow.model_info = vec![info];
    }
    // The favorite flag lands after the match above (the borrow ends).
    for prow in &mut snapshot.rows {
        if prow.id == "anthropic" {
            prow.model_info[0].favorite = true;
        }
    }
    snapshot
}

#[test]
fn flat_rows_show_the_raw_wire_id() {
    // The id-fallback half of the policy: with no provider label the row
    // leads with the raw wire id, and there is no client-side name mapping
    // to drift. (The label half is
    // `flat_rows_carry_the_provider_label_only_when_advertised`; either way
    // `model` is what gets activated.)
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let glm = rows
        .iter()
        .find(|r| r.model == "glm-5.2")
        .expect("relay pair present");
    assert_eq!(glm.provider_label, "My Relay");
    assert_eq!(glm.name, None, "nothing was advertised to label with");
    // The rendered label is the id itself (verified via the fuzzy match
    // target): matching "glm-5.2" hits positions inside the id.
    assert!(
        fuzzy::fuzzy_match(&glm.model, "glm52").is_some(),
        "the match target is the id"
    );
}

#[test]
fn flat_sections_favorites_then_recent_then_all() {
    // Three-section ordering: Favorites lead, Recent (usage history,
    // most-recent-first) follow, All models contains all candidate models in ASCII order.
    // The current pair no longer pins to the top — it keeps its natural
    // section position in ALL MODELS.
    let snapshot = sectioned();
    let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");

    // Section boundaries: collect the section of each row and assert the
    // sequence is the display order with no interleaving.
    let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
    let mut first_all = sections.len();
    for (i, s) in sections.iter().enumerate() {
        match s {
            ModelSection::Favorites if i < 1 => {}
            ModelSection::Recent if (1..3).contains(&i) => {}
            ModelSection::All => {
                first_all = first_all.min(i);
            }
            _ => panic!("unexpected section {s:?} at index {i}: {sections:?}"),
        }
    }
    assert_eq!(first_all, 3, "ALL MODELS starts after both lead sections");

    // Favorites section: exactly the starred model, first.
    assert!(rows[0].favorite);
    assert_eq!(rows[0].model, "claude-sonnet-5");
    assert_eq!(rows[0].section(), ModelSection::Favorites);

    // Recent section: glm-5.1 (t=2000) before gpt-4o (t=1000).
    assert_eq!(rows[1].model, "glm-5.1", "most recent first");
    assert_eq!(rows[2].model, "gpt-4o");
    assert_eq!(rows[1].last_used_ms, Some(2_000));
    assert_eq!(rows[2].last_used_ms, Some(1_000));
    assert_eq!(rows[1].section(), ModelSection::Recent);
    assert_eq!(rows[2].section(), ModelSection::Recent);

    // All models: plain ASCII, containing ALL 9 models (including favorites, recent, and current pair).
    assert_eq!(rows[first_all..].len(), 9);
    assert!(
        rows[first_all..]
            .iter()
            .all(|r| r.section() == ModelSection::All)
    );
    let rest: Vec<&str> = rows[first_all..].iter().map(|r| r.model.as_str()).collect();
    let mut sorted = rest.clone();
    sorted.sort();
    assert_eq!(rest, sorted);
    assert!(rest.contains(&"claude-sonnet-5"));
    assert!(rest.contains(&"glm-5.1"));
    assert!(rest.contains(&"gpt-4o"));
}

#[test]
fn flat_recent_orders_by_recency_desc_ascii_tiebreak() {
    // Two models with the SAME recency fall back to ASCII id order, so
    // the section stays deterministic when timestamps collide.
    let mut snapshot = sample();
    for prow in &mut snapshot.rows {
        let ids: Vec<String> = prow.models.clone();
        prow.model_info = ids
            .iter()
            .map(|m| {
                let mut i = info(m);
                i.last_used_ms = Some(5_000);
                i
            })
            .collect();
    }
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    // Every model is recent → RECENT section contains all models, and ALL MODELS section contains all models.
    let recent_rows: Vec<&RankedModel> = rows
        .iter()
        .filter(|r| r.section() == ModelSection::Recent)
        .collect();
    let all_rows: Vec<&RankedModel> = rows
        .iter()
        .filter(|r| r.section() == ModelSection::All)
        .collect();
    assert_eq!(recent_rows.len(), 9);
    assert_eq!(all_rows.len(), 9);
    let ids: Vec<&str> = recent_rows.iter().map(|r| r.model.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn flat_recent_isolates_same_model_across_different_connections() {
    // Two connection instances offer the exact same model (e.g. gpt-4o).
    // When only one connection has usage history for it, ONLY that
    // (connection, model) row must appear in RECENT. The other instance
    // must NOT be pulled into RECENT or elevated.
    let mut row_a = row("openai-official", "OpenAI Official", &["gpt-4o"], true);
    let mut info_a = info("gpt-4o");
    info_a.last_used_ms = Some(5_000);
    row_a.model_info = vec![info_a];

    let mut row_b = row("openai-proxy", "OpenAI Proxy", &["gpt-4o"], false);
    let info_b = info("gpt-4o");
    row_b.model_info = vec![info_b];

    let snapshot = ProviderPickerSnapshot {
        default_id: "openai-official".to_string(),
        rows: vec![row_a, row_b],
    };

    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let recent_rows: Vec<&RankedModel> = rows
        .iter()
        .filter(|r| r.section() == ModelSection::Recent)
        .collect();

    assert_eq!(
        recent_rows.len(),
        1,
        "only the activated connection row should be in RECENT"
    );
    assert_eq!(recent_rows[0].provider_id, "openai-official");
    assert_eq!(recent_rows[0].model, "gpt-4o");

    // In ALL MODELS, both instances still exist
    let all_rows: Vec<&RankedModel> = rows
        .iter()
        .filter(|r| r.section() == ModelSection::All)
        .collect();
    assert_eq!(all_rows.len(), 2);
}

#[test]
fn flat_favorite_outranks_recency() {
    // Precedence: a favorite always wins over the recency signal —
    // favorites are pinned user intent, recency is emergent. A starred
    // model with NO usage history still leads a used-but-unstarred one.
    let snapshot = sectioned();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    assert_eq!(
        rows[0].section(),
        ModelSection::Favorites,
        "the unstarred-but-recent glm-5.1 must not lead"
    );
    assert_eq!(rows[0].model, "claude-sonnet-5");
    assert_eq!(rows[1].section(), ModelSection::Recent);
}

#[test]
fn flat_current_pair_keeps_its_section_not_the_top() {
    // The live (provider, model) pair is identified by its ● glyph, not
    // by list position any more: make the never-used glm-5.2 the current
    // pair — it stays in ALL MODELS at its ASCII position while the
    // favorite keeps the lead.
    let snapshot = sectioned();
    let rows = models_flat_filtered_from(&snapshot, "my-relay", "glm-5.2", "");
    assert_eq!(rows[0].model, "claude-sonnet-5", "favorite still leads");
    let current = rows
        .iter()
        .find(|r| r.provider_id == "my-relay" && r.model == "glm-5.2")
        .expect("current pair present");
    assert_eq!(
        current.section(),
        ModelSection::All,
        "current pair is not pinned to the top"
    );
}

#[test]
fn flat_sections_survive_a_fuzzy_query() {
    // Search mode keeps the same grouping: filtered rows stay ordered
    // Favorites → Recent → All.
    let snapshot = sectioned();
    let rows = models_flat_filtered_from(&snapshot, "", "", "g");
    // Matches gpt-4o (recent) and glm-5.1/glm-5.2 (recent/plain).
    let sections: Vec<ModelSection> = rows.iter().map(|r| r.section()).collect();
    let mut ordered = sections.clone();
    ordered.sort();
    assert_eq!(sections, ordered, "sections never regress under a query");
}

#[test]
fn body_lines_interleave_labels_and_rows() {
    // The body geometry: a section label precedes each non-empty section,
    // rows keep their flat index, and empty sections emit nothing.
    let snapshot = sectioned();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let (lines, row_line) = models_body_lines(&rows);

    // One label per non-empty section (all three are populated here).
    let labels: Vec<&str> = lines
        .iter()
        .filter_map(|l| match l {
            ModelBodyLine::Section(s) => Some(s.label()),
            ModelBodyLine::Row(_) => None,
        })
        .collect();
    assert_eq!(labels, vec!["FAVORITES", "RECENT", "ALL MODELS"]);

    // Labels come from the display-ordered section enum.
    assert!(lines[0] == ModelBodyLine::Section(ModelSection::Favorites));

    // Row 0 (the favorite) paints one line below its label; the row map
    // is strictly increasing and within the body.
    assert_eq!(row_line[0], 1);
    assert!(row_line.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        row_line.last().copied(),
        Some(lines.len() - 1),
        "the last row paints the last line"
    );
    // Every Row(i) entry's mapped line actually holds that row.
    for (i, line) in row_line.iter().enumerate() {
        assert_eq!(lines[*line], ModelBodyLine::Row(i));
    }
}

#[test]
fn body_lines_skip_empty_sections() {
    // A snapshot with neither favorites nor usage renders ONE label
    // (ALL MODELS) — no empty FAVORITES/RECENT headers.
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let (lines, _) = models_body_lines(&rows);
    let labels: Vec<&str> = lines
        .iter()
        .filter_map(|l| match l {
            ModelBodyLine::Section(s) => Some(s.label()),
            ModelBodyLine::Row(_) => None,
        })
        .collect();
    assert_eq!(labels, vec!["ALL MODELS"]);
}

#[test]
fn flat_sorts_ascii_with_provider_label_tiebreak() {
    // Full-list invariant inside each section: rows never increase across
    // ASCII model id, then provider label. Run against the plain sample's
    // every adjacent pair.
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let keys: Vec<(ModelSection, String, String)> = rows
        .iter()
        .map(|r| (r.section(), r.model.clone(), r.provider_label.clone()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn flat_fuzzy_filters_by_model_id() {
    // A query matching a model id keeps that pair with highlight
    // positions indexing onto the id's characters.
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "opus");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model, "claude-opus-4-8");
    assert!(
        rows[0].match_id.is_some(),
        "an id match carries the id column's highlight"
    );
}

#[test]
fn protocol_candidates_filter_by_wire_format() {
    let openai = protocol_model_candidates(WireProtocol::OpenAiChatCompletions.as_str());
    assert!(openai.contains(&"gpt-4o"));
    // Anthropic-format models are excluded from the OpenAI candidate list.
    assert!(!openai.contains(&"claude-opus-4-8"));
    let anthropic = protocol_model_candidates(WireProtocol::AnthropicMessages.as_str());
    assert!(anthropic.contains(&"claude-opus-4-8"));
    assert!(!anthropic.contains(&"gpt-4o"));
}

#[test]
fn google_candidate_set_is_the_canonical_family() {
    // The native-Google candidate list mirrors the ids Google plus common
    // relays/中转站 serve — so a Custom Google provider offers real models,
    // not hallucinated preview ids. Image/embedding/video-only models are
    // excluded (an agent only consumes the text generateContent surface).
    let google = protocol_model_candidates(WireProtocol::GoogleGenerateContent.as_str());
    for id in [
        "gemini-4-argon",
        "gemini-3.8-flash",
        "gemini-3.7-flash",
        "gemini-3.5-flash",
        "gemini-3-pro-preview",
        "gemini-3-flash-preview",
        "gemini-3.1-pro-preview",
        "gemini-3.1-flash-lite",
        "gemini-2.5-flash",
        "gemini-2.5-pro",
        "gemini-2.0-flash",
    ] {
        assert!(google.contains(&id), "google candidate set missing {id}");
    }
    // Image-generation variants must NOT be in the text agent's candidate set.
    assert!(
        !google.contains(&"gemini-2.5-flash-image"),
        "image-only model leaked into google candidates"
    );
}

#[test]
fn google_candidate_set_includes_antigravity_relay_models() {
    // The Antigravity (sub2api) relay ids are registered as native-Google
    // baselines, so the add-model overlay for a Google provider offers
    // them (the closed-set policy has real candidates to pick from).
    let google = protocol_model_candidates(WireProtocol::GoogleGenerateContent.as_str());
    for id in [
        "gemini-3.1-pro-high",
        "gemini-3.1-pro-low",
        "gemini-3-flash",
    ] {
        assert!(
            google.contains(&id),
            "antigravity relay model {id} missing from google candidates"
        );
    }
}

#[test]
fn antigravity_template_is_offered_with_prefilled_url_and_seeded_models() {
    let tmpl = PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "google-antigravity")
        .expect("antigravity template offered in the chooser");
    assert_eq!(tmpl.label, "Google Antigravity");
    assert_eq!(tmpl.protocol, WireProtocol::GoogleGenerateContent);
    assert_eq!(
        tmpl.models,
        nuo_protocol::model_providers::ANTIGRAVITY_OAUTH_MODELS
    );
    assert_eq!(
        tmpl.default_url,
        Some("https://daily-cloudcode-pa.googleapis.com")
    );
    assert!(!tmpl.needs_url, "OAuth template hides Base URL field");
    assert!(
        !tmpl.needs_model,
        "no free-text Model field — models are seeded"
    );
    assert_eq!(tmpl.fields(), vec![CustomField::Name]);
}

#[test]
fn openai_template_seeds_openai_text_models() {
    let tmpl = PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "openai")
        .expect("openai template offered in the chooser");
    assert_eq!(tmpl.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(
        tmpl.models,
        nuo_protocol::model_providers::OPENAI_BUILTIN_MODELS
    );
    assert!(
        !tmpl.needs_url,
        "official endpoint URL is prefilled and hidden"
    );
    assert!(
        !tmpl.needs_model,
        "model list is seeded; add-model handles custom ids"
    );
    assert_eq!(tmpl.fields(), vec![CustomField::Name, CustomField::Token]);
    for id in ["gpt-5.5", "gpt-5.4", "gpt-5.6-sol"] {
        assert!(
            protocol_model_candidates(WireProtocol::OpenAiChatCompletions.as_str()).contains(&id),
            "OpenAI candidate set missing {id}"
        );
    }
}

#[test]
fn builtin_templates_prefill_official_urls_generic_relays_do_not() {
    let builtin_labels = [
        "OpenAI Platform",
        "Anthropic",
        "Google AI Studio",
        "CommandCode GOAT",
        "DeepSeek",
        "xAI",
        "ChatGPT Plan",
        "GitHub Copilot",
        "Google Antigravity",
        "Kimi Code",
        "GLM Coding Plan CN",
        "OpenCode",
        "OpenCode Go",
        "OpenCode Zen",
        "OpenDesign",
        "OpenRouter",
        "QianwenAI Token Plan",
        "Qoder",
        "Tencent CodeBuddy CN",
        "Tencent CodeBuddy Global",
    ];
    for t in PROVIDER_PRESETS {
        if builtin_labels.contains(&t.label) {
            assert!(t.default_url.is_some(), "{:?} should pre-fill", t.label);
        } else {
            assert!(
                t.default_url.is_none(),
                "{:?} generic relay must not pre-fill",
                t.label
            );
        }
    }
}

#[test]
fn opencode_zen_template_is_key_authenticated_on_the_zen_relay() {
    let tmpl = PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "opencode-zen")
        .expect("opencode-zen template offered in the chooser");
    assert_eq!(tmpl.label, "OpenCode Zen");
    assert_eq!(tmpl.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(
        tmpl.models,
        nuo_protocol::model_providers::OPENCODE_ZEN_MODELS
    );
    assert_eq!(
        tmpl.default_url,
        Some("https://opencode.ai/zen/v1/chat/completions")
    );
    assert_eq!(tmpl.auth, ConnectionAuth::Static);
    assert!(
        !tmpl.oauth_first(),
        "the Zen relay signs in with an API key"
    );
    assert_eq!(tmpl.fields(), vec![CustomField::Name, CustomField::Token]);
}

#[test]
fn google_model_set_is_closed_others_open() {
    // A closed set means the add-model overlay offers no free-text fallback:
    // the candidate list is the complete family, so an unmatched id is a
    // typo. OpenAI/Anthropic relays serve an open, evolving set, so typing
    // an unlisted id stays legitimate there.
    assert!(
        protocol_model_set_closed(WireProtocol::GoogleGenerateContent.as_str()),
        "native Google must be a closed model set"
    );
    assert!(
        !protocol_model_set_closed(WireProtocol::OpenAiChatCompletions.as_str()),
        "OpenAI relays keep an open model set"
    );
    assert!(
        !protocol_model_set_closed(WireProtocol::AnthropicMessages.as_str()),
        "Anthropic relays keep an open model set"
    );
}

#[test]
fn connections_lists_one_row_per_provider_including_custom() {
    let snapshot = sample();
    let rows = providers_filtered_from(&snapshot, "");
    assert_eq!(rows.len(), snapshot.rows.len());
    // The user-defined provider shows up like any built-in.
    assert!(rows.iter().any(|r| r.id == "my-relay"));
}

#[test]
fn connections_fuzzy_filters_by_provider_name() {
    let snapshot = sample();
    let rows = providers_filtered_from(&snapshot, "anthro");
    assert!(rows.iter().all(|r| r.id == "anthropic"));
    assert_eq!(rows.len(), 1);
}

#[test]
fn connections_orders_by_last_used_then_name_no_favorite() {
    // Favorite is model-level now (ADR-0046), so the Connections list no
    // longer sorts by it — only last-used desc, then instance name.
    let mut snapshot = sample();
    // Give kimi-code a recent activation so it leads.
    for r in &mut snapshot.rows {
        r.last_used_ms = (r.id == "kimi-code").then_some(1_000);
    }
    let rows = providers_filtered_from(&snapshot, "");
    assert_eq!(rows[0].id, "kimi-code");
    // The rest fall back to name order.
    let rest: Vec<&str> = rows[1..].iter().map(|r| r.id.as_str()).collect();
    assert_eq!(rest, vec!["anthropic", "my-relay", "openai"]);
}

#[test]
fn connections_no_longer_groups_builtins_before_custom() {
    let snapshot = sample();
    let rows = providers_filtered_from(&snapshot, "");
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["anthropic", "kimi-code", "my-relay", "openai"]);
}

#[test]
fn is_multi_model_tracks_model_count() {
    let snapshot = sample();
    let rows = providers_filtered_from(&snapshot, "");
    let kimi = rows.iter().find(|r| r.id == "kimi-code").unwrap();
    assert!(!kimi.is_multi_model());
    let openai = rows.iter().find(|r| r.id == "openai").unwrap();
    assert!(openai.is_multi_model());
}

#[test]
fn flat_lists_every_provider_model_pair() {
    // The flat Models picker has one row per (provider, model) pair across
    // ALL snapshot rows — no drilling, no per-provider scoping.
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let pair_count: usize = snapshot.rows.iter().map(|r| r.models.len()).sum();
    assert_eq!(rows.len(), pair_count);
    // Pairs from different providers coexist, each carrying its provider
    // id AND the provider display name for the `| <provider>` row suffix.
    let openai = rows
        .iter()
        .find(|r| r.provider_id == "openai" && r.model == "gpt-4o-mini")
        .expect("openai pair present");
    assert_eq!(openai.provider_label, "OpenAI");
    assert!(rows.iter().any(|r| r.provider_id == "my-relay"));
    assert!(
        rows.iter()
            .any(|r| r.provider_id == "anthropic" && r.model == "claude-opus-4-8")
    );
}

#[test]
fn flat_sorts_favorite_model_first_then_ascii() {
    // Favorite is model-level (ADR-0046): a starred model sorts into the
    // leading section of the flat list wherever it is served. Give one
    // anthropic model recency and favorite another: the favorited model
    // leads in FAVORITES, the used model is in RECENT, and ALL MODELS contains all 8 models.
    let mut snapshot = sample();
    let anthropic = snapshot
        .rows
        .iter_mut()
        .find(|r| r.id == "anthropic")
        .unwrap();
    let mut starred = info("claude-sonnet-5");
    starred.favorite = true;
    let mut used = info("claude-fable-5");
    used.last_used_ms = Some(100);
    anthropic.model_info = vec![starred, used];
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    // The favorited model leads the FAVORITES section.
    assert!(rows[0].favorite);
    assert_eq!(rows[0].model, "claude-sonnet-5");
    assert_eq!(rows[0].section(), ModelSection::Favorites);
    // The used model is in the RECENT section.
    assert_eq!(rows[1].model, "claude-fable-5");
    assert_eq!(rows[1].section(), ModelSection::Recent);
    // Everything from the third row on is the ALL MODELS section (all 9 models).
    assert_eq!(rows[2..].len(), 9);
    assert!(rows[2..].iter().all(|r| r.section() == ModelSection::All));
    let rest: Vec<&str> = rows[2..].iter().map(|r| r.model.as_str()).collect();
    let mut sorted = rest.clone();
    sorted.sort();
    assert_eq!(rest, sorted);
}

#[test]
fn flat_fuzzy_by_provider_name_includes_its_models_unhighlighted() {
    // "relay" matches no model id but DOES match the "My Relay"
    // provider name: that provider's models are included with no model
    // match at all (rendered without highlight), while other providers
    // drop out.
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "relay");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.provider_id == "my-relay"));
    assert!(
        rows.iter()
            .all(|r| r.match_id.is_none() && r.match_name.is_none()),
        "provider-name fallback rows are unhighlighted"
    );
}

#[test]
fn flat_fuzzy_matches_the_name_label_and_keeps_the_id_as_an_alias() {
    // A relay's wire id (`deepseek-flash`) says nothing about "V4.1", and a
    // name-first list leads with the name — so a name query highlights the
    // label column. The id stays reachable as an alias, and because the id
    // is drawn too (its own column), an id query highlights the ID column
    // rather than leaving the row unhighlighted.
    let mut snapshot = sample();
    for prow in &mut snapshot.rows {
        if prow.id == "my-relay" {
            let mut i = info("deepseek-flash");
            i.name = Some("DeepSeek V4.1 Flash".to_string());
            prow.models = vec!["deepseek-flash".to_string()];
            prow.model_info = vec![i];
        }
    }

    // The marketed name matches the leading label column → highlighted.
    let rows = models_flat_filtered_from(&snapshot, "", "", "v4.1");
    assert_eq!(rows.len(), 1, "only the labelled model matches: {rows:?}");
    assert_eq!(rows[0].model, "deepseek-flash", "the id stays the identity");
    assert_eq!(rows[0].name.as_deref(), Some("DeepSeek V4.1 Flash"));
    assert!(
        rows[0].match_name.is_some() && rows[0].match_id.is_none(),
        "a name query highlights the label column only"
    );

    // The wire id finds the row and highlights the ID column.
    let rows = models_flat_filtered_from(&snapshot, "", "", "deepseek-flash");
    assert_eq!(rows.len(), 1, "the id matches: {rows:?}");
    assert!(
        rows[0].match_id.is_some() && rows[0].match_name.is_none(),
        "an id query highlights the id column only"
    );

    let rows = models_flat_filtered_from(&snapshot, "", "", "v9.9");
    assert!(rows.is_empty(), "no row matches: {rows:?}");
}

#[test]
fn flat_rows_carry_the_provider_label_only_when_advertised() {
    // `name` is optional by construction: a model with no advertised label
    // surfaces `None` (the renderer then leads with the bare id), one with a
    // label surfaces it verbatim.
    let mut snapshot = sample();
    for prow in &mut snapshot.rows {
        let ids: Vec<String> = prow.models.clone();
        prow.model_info = ids.iter().map(|m| info(m)).collect();
    }
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    assert!(rows.iter().all(|r| r.name.is_none()));

    if let Some(prow) = snapshot.rows.iter_mut().find(|p| p.id == "my-relay") {
        let mut i = info("glm-5.2");
        i.name = Some("GLM-5.2 (Agentic)".to_string());
        prow.model_info = vec![i];
    }
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let glm = rows.iter().find(|r| r.model == "glm-5.2").expect("row");
    assert_eq!(glm.name.as_deref(), Some("GLM-5.2 (Agentic)"));
}

#[test]
fn flat_rows_order_ascii_without_current_or_favorite() {
    // With no favorites and no usage history, the whole list is the ALL
    // MODELS section in pure ASCII order (provider label as the
    // tiebreak) — deterministic regardless of provider order.
    let snapshot = sample();
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    let ids: Vec<&str> = rows.iter().map(|r| r.model.as_str()).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
}

#[test]
fn each_template_models_reference_the_shared_constants() {
    // The template `id` IS the model provider id, persisted on the created
    // connection as `provider`. The server's provider specs and this UI
    // table must share the *same* model-list constant (single source of
    // truth in `nuo_protocol::model_providers`) — otherwise the catalog's
    // reconciliation could not re-seed a connection from its provider. This
    // test catches a UI table that inlined a drifted copy of the list.
    for t in PROVIDER_PRESETS {
        let referenced = match t.id {
            "anthropic" => Some(nuo_protocol::model_providers::ANTHROPIC_BUILTIN_MODELS),
            "chatgpt-plan" | "openai-subscription" => {
                Some(nuo_protocol::model_providers::CHATGPT_BUILTIN_MODELS)
            }
            "commandcode-goat" => Some(nuo_protocol::model_providers::COMMANDCODE_BUILTIN_MODELS),
            "deepseek" => Some(nuo_protocol::model_providers::DEEPSEEK_BUILTIN_MODELS),
            "github-copilot" => Some(nuo_protocol::model_providers::COPILOT_SEED_MODELS),
            "google" | "google-ai-studio" | "ai-studio" | "gemini" => {
                Some(nuo_protocol::model_providers::GOOGLE_BUILTIN_MODELS)
            }
            "google-antigravity" => Some(nuo_protocol::model_providers::ANTIGRAVITY_OAUTH_MODELS),
            "kimi-code" => Some(nuo_protocol::model_providers::KIMI_CODE_MODELS),
            "codebuddy-cn" => Some(nuo_protocol::model_providers::CODEBUDDY_CN_BUILTIN_MODELS),
            "codebuddy-intl" => Some(nuo_protocol::model_providers::CODEBUDDY_INTL_BUILTIN_MODELS),
            "openai" => Some(nuo_protocol::model_providers::OPENAI_BUILTIN_MODELS),
            "openrouter" => Some(nuo_protocol::model_providers::OPENROUTER_BUILTIN_MODELS),
            "opencode-go" => Some(nuo_protocol::model_providers::OPENCODE_GO_MODELS),
            "opencode-zen" => Some(nuo_protocol::model_providers::OPENCODE_ZEN_MODELS),
            "opendesign" => Some(nuo_protocol::model_providers::OPENDESIGN_BUILTIN_MODELS),
            "glm-cn" => Some(nuo_protocol::model_providers::GLM_CN_MODELS),
            "xai" => Some(nuo_protocol::model_providers::XAI_BUILTIN_MODELS),
            _ => None,
        };
        if let Some(expected) = referenced {
            assert_eq!(
                t.models, expected,
                "template {} model list diverged from the shared constant",
                t.id
            );
        }
    }
}

#[test]
fn template_ids_are_unique() {
    let mut ids: Vec<&str> = PROVIDER_PRESETS
        .iter()
        .chain(std::iter::once(&CUSTOM_TEMPLATE))
        .map(|t| t.id)
        .collect();
    ids.sort_unstable();
    let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
    assert!(dups.is_empty(), "duplicate template ids: {dups:?}");
}

#[test]
fn openai_platform_template_is_labeled_to_distinguish_chatgpt() {
    // The `openai` template is the platform/API-key billing plan, distinct
    // from the ChatGPT Subscription template that shares its wire protocol.
    // The label must say so — a bare "OpenAI" reads as the company and
    // matches the subscription plan users actually have.
    let openai = PROVIDER_PRESETS.iter().find(|t| t.id == "openai").unwrap();
    assert_eq!(openai.label, "OpenAI Platform");
    let chatgpt = PROVIDER_PRESETS
        .iter()
        .find(|t| t.id == "chatgpt-plan")
        .unwrap();
    assert_eq!(openai.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(chatgpt.protocol, WireProtocol::OpenAiResponses);
    assert!(
        chatgpt.models.is_empty(),
        "the subscription template must not hardcode a seed — Codex /backend-api/codex/models is authoritative"
    );
}

#[test]
fn commandcode_template_uses_provider_route_and_seeds() {
    let cmd = PROVIDER_PRESETS
        .iter()
        .find(|template| template.id == "commandcode-goat")
        .unwrap();
    assert_eq!(cmd.label, "CommandCode GOAT");
    assert_eq!(cmd.protocol, WireProtocol::OpenAiResponses);
    assert_eq!(
        cmd.default_url,
        Some("https://api.commandcode.ai/provider/v1/responses")
    );
    assert_eq!(
        cmd.models,
        nuo_protocol::model_providers::COMMANDCODE_BUILTIN_MODELS
    );
}

#[test]
fn opencode_go_template_uses_go_relay_and_seeds() {
    let go = PROVIDER_PRESETS
        .iter()
        .find(|template| template.id == "opencode-go")
        .unwrap();
    assert_eq!(go.label, "OpenCode Go");
    assert_eq!(go.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(
        go.default_url,
        Some("https://opencode.ai/zen/go/v1/chat/completions")
    );
    assert_eq!(go.models, nuo_protocol::model_providers::OPENCODE_GO_MODELS);
}

#[test]
fn opendesign_template_uses_the_cloud_gateway_and_seeds() {
    let template = PROVIDER_PRESETS
        .iter()
        .find(|template| template.id == "opendesign")
        .unwrap();
    assert_eq!(template.label, "OpenDesign");
    assert_eq!(template.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(
        template.default_url,
        Some("https://amr-link.open-design.ai/v1/chat/completions")
    );
    assert_eq!(
        template.models,
        nuo_protocol::model_providers::OPENDESIGN_BUILTIN_MODELS
    );
    assert_eq!(template.auth, nuo_protocol::ConnectionAuth::Static);
}

#[test]
fn openrouter_template_uses_gateway_route_and_nex_seed() {
    let openrouter = PROVIDER_PRESETS
        .iter()
        .find(|template| template.id == "openrouter")
        .unwrap();
    assert_eq!(openrouter.label, "OpenRouter");
    assert_eq!(openrouter.protocol, WireProtocol::OpenAiChatCompletions);
    assert_eq!(
        openrouter.default_url,
        Some("https://openrouter.ai/api/v1/chat/completions")
    );
    assert_eq!(
        openrouter.models,
        nuo_protocol::model_providers::OPENROUTER_BUILTIN_MODELS
    );
}

#[test]
fn editor_title_resolves_by_provider_id_not_protocol() {
    // Several providers share the `openai` wire (openai-subscription is
    // declared first). A create-mode editor title must resolve from the
    // seeded provider id, otherwise every openai-wire flow would be
    // headed "ChatGPT Subscription".
    assert_eq!(provider_label_for(Some("openai")), "OpenAI Platform");
    assert_eq!(provider_label_for(Some("chatgpt-plan")), "ChatGPT Plan");
    assert_eq!(provider_label_for(Some("custom")), "Custom connection");
    assert_eq!(provider_label_for(Some("deepseek")), "DeepSeek");
    // Unknown / unseeded ids fall back to the generic header.
    assert_eq!(provider_label_for(None), "＋ Add connection");
    assert_eq!(
        provider_label_for(Some("no-such-provider")),
        "＋ Add connection"
    );
}

#[test]
fn custom_template_is_not_a_chooser_row() {
    assert!(
        PROVIDER_PRESETS.iter().all(|t| t.id != "custom"),
        "custom connections have their own Connections-level branch"
    );
    assert_eq!(
        connection_definition("custom").map(|definition| definition.id),
        Some("custom")
    );
}

#[test]
fn edit_fields_api_key_shows_transport_and_identity_for_custom_only() {
    // A pure-custom API-key provider also exposes its transport and
    // request identity.
    let custom_fields = edit_fields(false, ConnectionAuth::Static);
    assert_eq!(
        custom_fields,
        vec![
            CustomField::Name,
            CustomField::BaseUrl,
            CustomField::Token,
            CustomField::Protocol,
            CustomField::Budget,
        ]
    );

    // A curated API-key provider derives its Base URL from the provider
    // spec, so it exposes Name, Token, and Budget.
    let curated_fields = edit_fields(true, ConnectionAuth::Static);
    assert_eq!(
        curated_fields,
        vec![CustomField::Name, CustomField::Token, CustomField::Budget]
    );
}

#[test]
fn edit_fields_oauth_shows_name_only() {
    // An OAuth connection's endpoint and bearer are owned by the auth flow
    // (xAI `https://api.x.ai/...`, ChatGPT
    // `https://chatgpt.com/backend-api/codex/...`). The editor exposes Name and Budget.
    let xai = edit_fields(true, ConnectionAuth::negotiated("xai"));
    assert_eq!(xai, vec![CustomField::Name, CustomField::Budget]);

    let chatgpt = edit_fields(true, ConnectionAuth::negotiated("chatgpt"));
    assert_eq!(chatgpt, vec![CustomField::Name, CustomField::Budget]);
}

#[test]
fn flat_rows_exclude_unready_providers() {
    let mut snapshot = sample();
    // Mark openai as not key_ready
    snapshot.rows[1].key_ready = false;
    let rows = models_flat_filtered_from(&snapshot, "", "", "");
    assert!(!rows.iter().any(|r| r.provider_id == "openai"));
    assert!(rows.iter().any(|r| r.provider_id == "kimi-code"));
}

/// The flat picker row must carry the route's effort ladder straight from
/// the snapshot. `RankedModel.effort_levels` was hardcoded empty, so the
/// editor opened from a Models row always fell back to the value-only
/// control — the node slider never appeared. The server is the only
/// authority for the ladder (it owns the provider baseline tables, which
/// this client does not link), so the field must be a pass-through.
#[test]
fn flat_rows_carry_the_snapshot_effort_ladder() {
    let mut snapshot = sample();
    for prow in &mut snapshot.rows {
        if prow.id == "kimi-code" {
            let mut i = info("kimi-k2.7-code");
            i.effort = Some("high".to_string());
            i.effort_levels = vec!["low".to_string(), "high".to_string(), "max".to_string()];
            prow.model_info = vec![i];
        }
    }
    let rows = models_flat_filtered_from(&snapshot, "", "", "kimi-k2.7-code");
    let row = rows
        .iter()
        .find(|r| r.model == "kimi-k2.7-code")
        .expect("row");
    assert_eq!(
        row.effort_levels,
        vec!["low".to_string(), "high".to_string(), "max".to_string()],
        "the row must forward the snapshot ladder, not drop it"
    );
}

#[test]
fn flat_multi_token_hybrid_search_cross_field_coverage_and_order_independence() {
    // [INV-MODEL-SEARCH-01, INV-MODEL-SEARCH-02, INV-MODEL-SEARCH-03, INV-MODEL-SEARCH-04]
    let mut snapshot = sample();
    // Setup distinct providers and overlapping models:
    // Provider 1: "Azure Enterprise" serving "gpt-4o"
    // Provider 2: "OpenAI Official" serving "gpt-4o" and "o1"
    snapshot.rows[0].id = "azure-ent".to_string();
    snapshot.rows[0].name = "Azure Enterprise".to_string();
    snapshot.rows[0].models = vec!["gpt-4o".to_string()];
    snapshot.rows[0].model_info = vec![info("gpt-4o")];

    snapshot.rows[1].id = "openai-off".to_string();
    snapshot.rows[1].name = "OpenAI Official".to_string();
    snapshot.rows[1].models = vec!["gpt-4o".to_string(), "o1".to_string()];
    snapshot.rows[1].model_info = vec![info("gpt-4o"), info("o1")];

    // 1. Single model search returns gpt-4o from BOTH providers
    let all_gpt = models_flat_filtered_from(&snapshot, "", "", "gpt-4o");
    assert_eq!(all_gpt.len(), 2);

    // 2. Cross-field hybrid search with whitespace: "azure gpt"
    let azure_gpt = models_flat_filtered_from(&snapshot, "", "", "azure gpt");
    assert_eq!(azure_gpt.len(), 1, "only Azure's gpt-4o should match");
    assert_eq!(azure_gpt[0].provider_id, "azure-ent");
    assert_eq!(azure_gpt[0].model, "gpt-4o");
    // [INV-MODEL-SEARCH-03] Column-isolated highlights:
    assert!(
        azure_gpt[0].match_connection.is_some(),
        "connection highlighted"
    );
    assert!(azure_gpt[0].match_id.is_some(), "model id highlighted");
    // [INV-MODEL-SEARCH-04] Cross-field bonus awarded:
    assert!(azure_gpt[0].relevance_score >= BONUS_CROSS_FIELD);

    // 3. [INV-MODEL-SEARCH-02] Order-independence: "gpt azure" yields identical row
    let gpt_azure = models_flat_filtered_from(&snapshot, "", "", "gpt azure");
    assert_eq!(gpt_azure.len(), 1);
    assert_eq!(gpt_azure[0].provider_id, "azure-ent");
    assert_eq!(gpt_azure[0].model, "gpt-4o");
    assert_eq!(gpt_azure[0].relevance_score, azure_gpt[0].relevance_score);

    // 4. Path and tag delimiters: "azure/gpt", "azure:gpt", "gpt@azure", "azure|gpt"
    for q in &["azure/gpt", "azure:gpt", "gpt@azure", "azure|gpt"] {
        let res = models_flat_filtered_from(&snapshot, "", "", q);
        assert_eq!(
            res.len(),
            1,
            "query '{q}' must resolve exactly to Azure's gpt-4o"
        );
        assert_eq!(res[0].provider_id, "azure-ent");
    }

    // 5. Targeting the other provider: "openai gpt"
    let openai_gpt = models_flat_filtered_from(&snapshot, "", "", "openai gpt");
    assert_eq!(openai_gpt.len(), 1);
    assert_eq!(openai_gpt[0].provider_id, "openai-off");
    assert_eq!(openai_gpt[0].model, "gpt-4o");

    // 6. [INV-MODEL-SEARCH-01] Conjunctive coverage: if any token is unmatched, candidate rejected
    let rejected = models_flat_filtered_from(&snapshot, "", "", "azure nonexistent");
    assert!(
        rejected.is_empty(),
        "unmatched token must disqualify candidate"
    );

    // 7. Normalized scoring: redundant marketing name matching wire id does not artificially double score
    let mut row_a = row("prov-a", "Prov A", &["m-test"], true);
    row_a.model_info = vec![info("m-test")]; // name: None
    let mut row_b = row("prov-b", "Prov B", &["m-test"], true);
    let mut info_b = info("m-test");
    info_b.name = Some("m-test".to_string()); // redundant name
    row_b.model_info = vec![info_b];

    let snap_norm = ProviderPickerSnapshot {
        default_id: "prov-a".to_string(),
        rows: vec![row_a, row_b],
    };
    let res = models_flat_filtered_from(&snap_norm, "", "", "m-test");
    assert_eq!(res.len(), 2);
    assert_eq!(
        res[0].relevance_score, res[1].relevance_score,
        "redundant display name identical to wire id must not artificially inflate score"
    );
}

#[test]
fn flat_multi_token_intra_model_and_relevance_ranking_bonus() {
    // Verifies multi-word intra-model search and that dual-dimension match outranks single-dimension
    let mut sonnet_info = info("claude-3-5-sonnet");
    sonnet_info.name = Some("Claude 3.5 Sonnet".to_string());

    let mut row = row(
        "anthropic",
        "Anthropic",
        &["claude-3-5-sonnet", "claude-3-haiku"],
        true,
    );
    row.model_info = vec![sonnet_info, info("claude-3-haiku")];

    let snapshot = ProviderPickerSnapshot {
        default_id: "anthropic".to_string(),
        rows: vec![row],
    };

    // Multi-word intra-model query: "3.5 sonnet"
    let sonnet_rows = models_flat_filtered_from(&snapshot, "", "", "3.5 sonnet");
    assert_eq!(sonnet_rows.len(), 1);
    assert_eq!(sonnet_rows[0].model, "claude-3-5-sonnet");
    assert!(sonnet_rows[0].match_name.is_some());
    // Highlight positions should cover characters from both tokens:
    let positions = &sonnet_rows[0].match_name.as_ref().unwrap().positions;
    assert!(positions.len() >= 5);

    // Query "anthropic claude":
    // "claude-3-5-sonnet" matches connection ("anthropic") AND model name ("claude") -> receives BONUS_CROSS_FIELD
    // "claude-3-haiku" matches connection ("anthropic") AND model id ("claude") -> receives BONUS_CROSS_FIELD
    let anthropic_claude = models_flat_filtered_from(&snapshot, "", "", "anthropic claude");
    assert_eq!(anthropic_claude.len(), 2);
    assert!(
        anthropic_claude
            .iter()
            .all(|r| r.relevance_score >= BONUS_CROSS_FIELD)
    );
}

#[test]
fn format_context_tokens_formats_clean_standard_strings() {
    assert_eq!(format_context_tokens(128_000), "128k");
    assert_eq!(format_context_tokens(200_000), "200k");
    assert_eq!(format_context_tokens(1_000_000), "1M");
    assert_eq!(format_context_tokens(1_048_576), "1M");
    assert_eq!(format_context_tokens(2_000_000), "2M");
    assert_eq!(format_context_tokens(0), "");
}

#[test]
fn quota_snapshot_dynamically_disables_exhausted_family_models() {
    let picker = ProviderPickerSnapshot {
        default_id: "antigravity".to_string(),
        rows: vec![ProviderPickerRow {
            id: "antigravity".to_string(),
            name: "Google Antigravity".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            models: vec![
                "claude-3-7-sonnet".to_string(),
                "gemini-2.5-flash".to_string(),
                "gemini-4-argon".to_string(),
            ],
            model_info: vec![
                ProviderModelInfo {
                    model: "claude-3-7-sonnet".to_string(),
                    context_window: 200_000,
                    vision: Some(true),
                    effort: Some("high".to_string()),
                    availability: Some(nuo_protocol::Availability::usable()),
                    ..Default::default()
                },
                ProviderModelInfo {
                    model: "gemini-2.5-flash".to_string(),
                    context_window: 1_048_576,
                    vision: Some(true),
                    availability: Some(nuo_protocol::Availability::usable()),
                    ..Default::default()
                },
                ProviderModelInfo {
                    model: "gemini-4-argon".to_string(),
                    context_window: 1_048_576,
                    vision: Some(false),
                    availability: Some(nuo_protocol::Availability::disabled(
                        "disabled: tier",
                        Some("Requires Google CodeAssist paid entitlement".to_string()),
                        None,
                    )),
                    ..Default::default()
                },
            ],
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: true,
            provider: "google-antigravity".to_string(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
        }],
    };

    // Quota snapshot where provider usage emitted a dynamic 5h limit rule for Claude
    let quotas = nuo_protocol::ProviderQuotaSnapshot {
        provider_filter: None,
        entries: vec![nuo_protocol::ConnectionQuotaEntry {
            name: "antigravity".to_string(),
            provider: "google-antigravity".to_string(),
            provider_label: "Google Antigravity".to_string(),
            account_id: None,
            is_default: true,
            state: nuo_protocol::ConnectionUsageState::Available(Box::new(
                nuo_protocol::ProviderUsage {
                    model_rules: vec![nuo_protocol::ModelAvailabilityRule {
                        pattern: "claude-*".to_string(),
                        availability: nuo_protocol::Availability::disabled(
                            "disabled: 5h limit",
                            Some("5h rolling quota limit reached for Claude Models".to_string()),
                            Some(1999999999000),
                        ),
                    }],
                    ..Default::default()
                },
            )),
            primary_balance: Some("0% Quota".to_string()),
            quota: None,
            plan: None,
            earliest_reset_ms: None,
        }],
        ..Default::default()
    };

    let ranked = models_flat_filtered_from_with_quotas(
        &picker,
        "antigravity",
        "claude-3-7-sonnet",
        "",
        Some(&quotas),
    );

    let claude = ranked
        .iter()
        .find(|r| r.model == "claude-3-7-sonnet")
        .unwrap();
    assert!(!claude.usable);
    assert_eq!(claude.display_status(), Some("disabled: 5h limit"));
    assert!(
        claude
            .reason
            .as_deref()
            .unwrap()
            .contains("5h rolling quota limit reached")
    );
    assert_eq!(claude.resets_at_ms, Some(1999999999000));
    assert_eq!(claude.format_context_window(), "200k");

    let gemini_flash = ranked
        .iter()
        .find(|r| r.model == "gemini-2.5-flash")
        .unwrap();
    assert!(gemini_flash.usable);
    assert_eq!(gemini_flash.display_status(), None);
    assert_eq!(gemini_flash.format_context_window(), "1M");

    let argon = ranked.iter().find(|r| r.model == "gemini-4-argon").unwrap();
    assert!(!argon.usable);
    assert_eq!(argon.display_status(), Some("disabled: tier"));
    assert_eq!(
        argon.reason.as_deref(),
        Some("Requires Google CodeAssist paid entitlement")
    );
}

#[test]
fn apply_quotas_re_enables_candidate_when_reset_time_passed() {
    let picker = ProviderPickerSnapshot {
        default_id: "antigravity".to_string(),
        rows: vec![nuo_protocol::ProviderPickerRow {
            id: "antigravity".to_string(),
            name: "Antigravity".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            key_ready: true,
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            provider: "google-antigravity".to_string(),
            headers: Default::default(),
            last_used_ms: None,
            auth: Default::default(),
            models: vec!["claude-3-7-sonnet".to_string()],
            model_info: vec![nuo_protocol::ProviderModelInfo {
                model: "claude-3-7-sonnet".to_string(),
                name: Some("Claude 3.7 Sonnet".to_string()),
                context_window: 200_000,
                // Initially disabled with a past reset timestamp (1000ms after epoch)
                availability: Some(nuo_protocol::Availability::disabled(
                    "disabled: 5h limit",
                    Some("5h rolling quota limit reached".to_string()),
                    Some(1000),
                )),
                ..Default::default()
            }],
        }],
    };

    let quotas = nuo_protocol::ProviderQuotaSnapshot {
        entries: vec![nuo_protocol::ConnectionQuotaEntry {
            name: "antigravity".to_string(),
            provider: "google-antigravity".to_string(),
            provider_label: "Google Antigravity".to_string(),
            account_id: None,
            is_default: true,
            state: nuo_protocol::ConnectionUsageState::Available(Box::new(
                nuo_protocol::ProviderUsage {
                    // Past reset timestamp
                    model_rules: vec![nuo_protocol::ModelAvailabilityRule {
                        pattern: "claude-*".to_string(),
                        availability: nuo_protocol::Availability::disabled(
                            "disabled: 5h limit",
                            Some("5h rolling quota limit reached".to_string()),
                            Some(1000),
                        ),
                    }],
                    ..Default::default()
                },
            )),
            primary_balance: None,
            quota: None,
            plan: None,
            earliest_reset_ms: None,
        }],
        ..Default::default()
    };

    let ranked = models_flat_filtered_from_with_quotas(
        &picker,
        "antigravity",
        "claude-3-7-sonnet",
        "",
        Some(&quotas),
    );

    let claude = ranked
        .iter()
        .find(|r| r.model == "claude-3-7-sonnet")
        .unwrap();
    // Since reset time is in the past, candidate must have been restored to usable
    assert!(claude.usable);
    assert_eq!(claude.display_status(), None);
    assert_eq!(claude.reason, None);
    assert_eq!(claude.resets_at_ms, None);
}
