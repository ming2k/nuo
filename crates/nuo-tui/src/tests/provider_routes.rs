//! Provider/route settings tests: template editors, capability overrides, custom connection submission, model selection.

use super::*;

#[test]
fn provider_retry_state_formats_summary_and_timing() {
    let now = std::time::Instant::now();
    let state = crate::app::ProviderRetryState {
        attempt: 2,
        max_attempts: 16,
        retry_at: now + std::time::Duration::from_millis(6_600),
        failure: "HTTP 429: rate limited".to_string(),
    };
    let summary = state.summary(now);
    assert_eq!(summary, "retry 1/15 (next in 6.6s)");

    let running_state = crate::app::ProviderRetryState {
        attempt: 4,
        max_attempts: 16,
        retry_at: now - std::time::Duration::from_millis(1_200),
        failure: "HTTP 503: overloaded".to_string(),
    };
    let running_summary = running_state.summary(now);
    assert_eq!(running_summary, "retry 3/15 (running for 1.2s)");
}

#[test]
fn completions_trigger_word_pins_suggestion_on_top() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/clear".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let first = completions.first().expect("a suggestion row is present");
    assert_eq!(first.label, "/new");
    assert!(
        !first.description.is_empty(),
        "the suggestion must explain why the user is being steered"
    );
    // Accepting rewrites the whole input to the target command.
    assert_eq!(first.replace_start, 0);
    assert_eq!(first.replace_end, app.input.len());
    // No built-in starts with `/clear`, so the suggestion is the only row.
    assert_eq!(completions.len(), 1);
}

#[test]
fn completions_trigger_word_suggestion_precedes_prefix_matches() {
    // A trigger that also prefixes a real command must still pin its
    // suggestion first. `/re` is a shared prefix, not a trigger: normal
    // prefix completion with no suggestion. `/reset` is the full trigger.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(
        !app.completions().iter().any(|c| c.label == "/new"),
        "a partial trigger is prose-in-progress, not a suggestion"
    );

    app.input = "/reset".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/new"),
        "the suggestion pins on top even if a real command shares the prefix"
    );
}

#[test]
fn completions_continue_trigger_suggests_threads() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/continue".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/threads")
    );
}

#[test]
fn completions_settings_and_dashboard_purged_from_session_completions() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);

    // ADR-0041 [INV-CMD-01]: Workspace views are purged from thread completions
    app.input = "/settings".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert!(completions.is_empty(), "expected no completion for /settings");

    app.input = "/dashboard".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert!(completions.is_empty(), "expected no completion for /dashboard");
}

#[test]
fn completions_subcommand_argument_never_triggers_suggestion() {
    // `clear` is a trigger word at the top level, but as a `/permissions`
    // argument it is a real subcommand and must not be steered away.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/permissions clear".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/permissions clear"]);
}

#[test]
fn completions_intent_keywords_suggest_canonical_command() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/cron-x-intent".to_string();
    app.cursor_position = app.input.chars().count();
    // Intent keyword routing: a word that matches a command's intent_keywords
    // suggests the canonical command. `/resume` maps to `/threads`.
    app.input = "/resume".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"/threads"),
        "typing /resume should suggest /threads"
    );

    // Check intent suggestion kind and doc
    let threads_cand = completions.iter().find(|c| c.label == "/threads").unwrap();
    assert!(matches!(
        threads_cand.kind,
        crate::completion::CompletionItemKind::IntentSuggestion { .. }
    ));
    assert!(threads_cand.doc.is_some());
    let doc = threads_cand.doc.as_ref().unwrap();
    assert_eq!(doc.name, "/threads");

    // /switch suggests /models and /role
    app.input = "/switch".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"/models"));
    assert!(labels.contains(&"/role"));
}

#[test]
fn completions_candidates_carry_rich_doc_for_inspector() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/models".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let models_cand = completions
        .iter()
        .find(|c| c.label == "/models")
        .expect("find /models");
    assert!(models_cand.doc.is_some());
    let doc = models_cand.doc.as_ref().unwrap();
    assert_eq!(doc.name, "/models");
    assert!(!doc.summary.is_empty());
    assert!(!doc.usage.is_empty());
    assert_eq!(doc.category.as_deref(), Some("Model"));
}

#[test]
fn completions_returns_empty_when_input_does_not_trigger() {
    // Plain text without `@` or `/` produces no completions.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "hello world".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completions().is_empty());
    assert_eq!(app.completion_kind(), CompletionKind::None);
}

#[test]
fn completions_classifies_slash_input_as_slash_kind() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    assert!(completions.iter().any(|c| c.label == "/resume"));
    // Slash candidates replace the whole input.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, app.input.len());
    }
}

#[test]
fn completions_delegate_subcommand_offers_on_off() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/delegate ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"/delegate on") && labels.contains(&"/delegate off"),
        "expected both on/off subcommands, got {labels:?}"
    );
    // Candidates replace the whole input.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, app.input.len());
    }

    // Typing a prefix narrows the pair.
    app.input = "/delegate of".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/delegate off"]);

    // An unknown suffix dead-ends (no candidates, like any non-prefix).
    app.input = "/delegate x".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completions().is_empty());
}

#[test]
fn completions_expose_only_canonical_trust_subcommands() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);

    // Retired extension command is not suggested.
    app.input = "/extensions ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert!(completions.is_empty());

    // /trust <space> offers only the closed asset-domain grammar.
    app.input = "/trust ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "/trust all",
            "/trust instructions",
            "/trust ex-workspace",
            "/trust mcp",
            "/trust skills",
            "/trust hooks",
            "/trust status",
            "/trust revoke"
        ]
    );

    // Removed subcommands do not reappear through prefix matching.
    app.input = "/trust w".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.is_empty());
}

#[test]
fn add_connection_row_opens_the_template_chooser() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_preset_chooser();
    assert_eq!(
        app.surfaces.active_sheet(),
        Some(crate::surfaces::SheetKind::ProviderPreset)
    );
    assert_eq!(app.preset_choice, 0);
    // `↑/↓` wrap across the template list.
    let n = crate::PROVIDER_PRESETS.len();
    app.move_preset_choice(false);
    assert_eq!(app.preset_choice, n - 1, "wraps to the last template");
    app.move_preset_choice(true);
    assert_eq!(app.preset_choice, 0, "wraps back to the first");
}

#[test]
fn custom_connection_opens_as_a_sibling_of_the_template_chooser() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_connection_editor();
    assert_eq!(
        app.surfaces.active_sheet(),
        Some(crate::surfaces::SheetKind::CustomProvider)
    );
    assert_eq!(app.custom_provider_id.as_deref(), Some("custom"));
    assert_eq!(
        app.custom_fields,
        vec![
            crate::CustomField::Name,
            crate::CustomField::BaseUrl,
            crate::CustomField::Token,
            crate::CustomField::Model,
            crate::CustomField::Protocol,
            crate::CustomField::ClientIdentity,
        ]
    );
}

#[test]
fn custom_provider_editor_opens_empty_on_name_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.custom_name = "stale".to_string();
    app.open_custom_provider_editor(openai_template());
    assert_eq!(
        app.surfaces.active_sheet(),
        Some(crate::surfaces::SheetKind::CustomProvider)
    );
    assert_eq!(app.custom_field, 0, "opens on the Name field");
    assert!(app.custom_name.is_empty(), "buffers reset on open");
    assert!(
        app.input.is_empty(),
        "Name field borrows an empty input line"
    );
    // The template seeds the protocol and OpenAI model list.
    assert_eq!(app.custom_protocol_wire, "chat-completions");
    assert!(app.custom_models.iter().any(|m| m == "gpt-5.5"));
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
}

#[test]
fn anthropic_template_seeds_the_claude_family_without_a_model_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(anthropic_template());
    assert_eq!(app.custom_protocol_wire, "anthropic-messages");
    // The Claude family is seeded as the provider's model list…
    assert!(app.custom_models.len() > 1, "seeds multiple Claude models");
    assert!(app.custom_models.iter().any(|m| m.starts_with("claude-")));
    // …and there is no Model field (models are fixed by the provider).
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
}

#[test]
fn antigravity_template_prefills_url_and_seeds_relay_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(antigravity_template());
    assert_eq!(app.custom_protocol_wire, "google-gemini");
    assert_eq!(
        app.custom_base_url,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    assert_eq!(
        app.custom_models,
        nuo_wire::model_providers::ANTIGRAVITY_OAUTH_MODELS
    );
    // No free-text Model field — the closed Gemini family is the seed.
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
    // Name and Token still start empty (the user supplies them).
    assert!(app.custom_name.is_empty());
    assert!(app.custom_token.is_empty());
}

#[test]
fn custom_provider_field_cycle_wraps_and_swaps_buffers() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let custom_template = &crate::providers::CUSTOM_TEMPLATE;
    app.open_custom_provider_editor(custom_template);
    // Fields: Name / Base URL / Token / Model / Protocol / Identity.
    let n = app.custom_fields.len() as u8;
    // Type a name, then advance: the name is stashed and the Base URL field
    // loads its (empty) buffer.
    app.input = "My Relay".to_string();
    app.cycle_custom_field(true);
    assert_eq!(app.custom_field, 1);
    assert_eq!(app.custom_name, "My Relay");
    assert!(app.input.is_empty(), "Base URL buffer is empty");
    // Wrap backward from Name (0) to the last field (Identity).
    app.cycle_custom_field(false); // 1 -> 0
    assert_eq!(app.custom_field, 0);
    assert_eq!(app.input, "My Relay", "Name buffer reloads into the line");
    app.cycle_custom_field(false); // 0 -> n-1 (wrap)
    assert_eq!(app.custom_field, n - 1);
}

#[test]
fn custom_provider_model_is_an_unmodified_text_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The real generic template: it exposes the Model field and seeds no
    // models, so the flow under test is exactly what ships.
    let free_model_template = &crate::providers::CUSTOM_TEMPLATE;
    app.open_custom_provider_editor(free_model_template);
    assert!(app.custom_model.is_empty());
    // Focus Model and type an arbitrary, case-sensitive id. No registry-backed
    // suggestions or normalization participate.
    app.custom_field = 3;
    assert_eq!(app.current_custom_field(), Some(crate::CustomField::Model));
    app.load_custom_field();
    app.input = "My Private Model/V2".to_string();
    app.stash_custom_field();
    assert_eq!(app.custom_model, "My Private Model/V2");
}

#[test]
fn custom_provider_cycles_protocol_and_request_identity() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(&crate::providers::CUSTOM_TEMPLATE);

    app.custom_field = 4;
    assert_eq!(
        app.current_custom_field(),
        Some(crate::CustomField::Protocol)
    );
    app.cycle_custom_choice(true);
    assert_eq!(app.custom_protocol_wire, "responses");

    app.custom_field = 5;
    assert_eq!(
        app.current_custom_field(),
        Some(crate::CustomField::ClientIdentity)
    );
    app.cycle_custom_choice(true);
    assert_eq!(
        app.custom_client_identity,
        nuo_wire::ClientIdentity::OpenCode
    );
}

#[test]
fn custom_provider_edit_restores_protocol_and_request_identity() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_edit_provider_editor(
        "relay".to_string(),
        "Relay".to_string(),
        "anthropic-messages".to_string(),
        "https://relay.example/v1/messages".to_string(),
        nuo_wire::ConnectionAuth::ApiKey,
        false,
        nuo_wire::ClientIdentity::ClaudeCode,
    );
    assert_eq!(app.custom_protocol_wire, "anthropic-messages");
    assert_eq!(
        app.custom_client_identity,
        nuo_wire::ClientIdentity::ClaudeCode
    );
    assert!(app.custom_fields.contains(&crate::CustomField::Protocol));
    assert!(
        app.custom_fields
            .contains(&crate::CustomField::ClientIdentity)
    );
}

#[test]
fn custom_connection_submits_with_the_typed_model_and_url() {
    // Create-flow projection for a custom connection: fields Name/Base
    // URL/Token/Model, and the submitted `AddConnection` carries the typed
    // model id (not a seeded list) plus the relay endpoint.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let template = &crate::providers::CUSTOM_TEMPLATE;
    // The editor exposes the four basic strings plus API and identity selectors.
    assert_eq!(
        template.fields(),
        vec![
            crate::CustomField::Name,
            crate::CustomField::BaseUrl,
            crate::CustomField::Token,
            crate::CustomField::Model,
            crate::CustomField::Protocol,
            crate::CustomField::ClientIdentity,
        ]
    );
    app.open_custom_provider_editor(template);
    app.custom_name = "WeChat".to_string();
    app.custom_base_url = "https://chatapi.weixin.qq.com/openai/v1/chat/completions".to_string();
    app.custom_token = "tok".to_string();
    // Focus the Model field and type the cased id directly.
    app.custom_field = 3;
    app.load_custom_field();
    app.input = "GLM-5.2".to_string();
    app.stash_custom_field();
    assert_eq!(app.custom_model, "GLM-5.2");

    // Submit: the request carries the `custom` provider, the single typed
    // model, and the endpoint — a case-sensitive id travels
    // verbatim (the WeChat endpoint 400s on the lowercase spelling).
    let payload = serde_json::json!({
        "name": app.custom_name,
        "provider": app.custom_provider_id,
        "protocol": app.custom_protocol_wire,
        "base_url": app.custom_base_url,
        "models": [app.custom_model],
    });
    assert_eq!(payload["models"][0], "GLM-5.2");
    assert_eq!(payload["provider"], "custom");
    assert_eq!(payload["protocol"], "chat-completions");
    assert_eq!(
        payload["base_url"],
        "https://chatapi.weixin.qq.com/openai/v1/chat/completions"
    );
}

#[test]
fn completions_path_returns_top_level_for_bare_at() {
    // Stage 1: bare `@` lists entity namespaces (@file:, @skill:).
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml", "src/main.rs", "README.md"], &["src"]);
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Path);

    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"@file:"));
    assert!(labels.contains(&"@skill:"));
    // Bare `@` must NOT dump raw files (two-stage completion).
    assert!(!labels.contains(&"@file:Cargo.toml"));

    // Stage 2: selecting or typing `@file:` lists top-level entries.
    app.input = "@file:".to_string();
    app.cursor_position = 6;
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"@file:src/"));
    assert!(labels.contains(&"@file:Cargo.toml"));
    assert!(labels.contains(&"@file:README.md"));
    // No nested paths leak into top-level @file: menu.
    assert!(!labels.iter().any(|l| l.contains("main.rs")));
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, 6);
    }
}

#[test]
fn completions_path_descends_into_subdirectory() {
    // Stage 2: `@file:src/` triggers directory descend: only paths under `src/` match.
    let (mut app, _tmp) = app_in_tempdir(
        &["src/main.rs", "src/util/mod.rs", "tests/smoke.rs"],
        &["src", "src/util", "tests"],
    );
    app.input = "@file:src/".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"@file:src/"));
    assert!(labels.contains(&"@file:src/main.rs"));
    assert!(labels.contains(&"@file:src/util/"));
    assert!(labels.contains(&"@file:src/util/mod.rs"));
    // Nothing from `tests/` leaks in — descend is a prefix match.
    assert!(!labels.iter().any(|l| l.contains("tests")));
}

#[test]
fn completions_path_substring_match_picks_files_across_dirs() {
    // Stage 2: `@file:main` finds `src/main.rs` via substring match.
    let (mut app, _tmp) = app_in_tempdir(&["src/main.rs", "lib/other.rs"], &["src", "lib"]);
    app.input = "@file:main".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"@file:src/main.rs"));
    assert!(!labels.iter().any(|l| l.contains("other.rs")));
}

#[test]
fn completions_path_skips_dotgit_directory() {
    // `.git/` is always excluded even though hidden files are kept in Stage 2.
    let (mut app, _tmp) = app_in_tempdir(
        &[".git/HEAD", ".git/config", "src/main.rs", ".env"],
        &[".git", "src"],
    );
    app.input = "@file:".to_string();
    app.cursor_position = 6;
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    // Hidden files like `.env` are listed; `.git/` and its contents are not.
    assert!(labels.contains(&"@file:.env"));
    assert!(labels.contains(&"@file:src/"));
    assert!(!labels.iter().any(|l| l.contains(".git")));
}

#[test]
fn model_editor_owns_caret_only_for_provider_key_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    app.editor_model_settings_only = false;
    app.editor_field = 0;
    assert_eq!(app.caret_owner(), CaretOwner::Overlay);

    app.editor_model_settings_only = true;
    app.editor_field = 1;
    assert_eq!(app.caret_owner(), CaretOwner::None);
}

/// Deleting the highlighted row from the sessions picker must leave the cursor
/// on the **same line** (the next session slides up into the removed slot), not
/// jump back to the top. The `DeleteSelectedSession` event-loop arm does this
/// optimistically — it removes the row and clamps `modal_index` — so this pins
/// that core behaviour: a mid-list delete keeps the index, and a delete of the
/// last row clamps to the new last row rather than wrapping to 0.
#[test]
fn sessions_picker_delete_keeps_cursor_on_the_same_line() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.sessions_overview = (0..5)
        .map(|i| overview_row(&format!("s{i}")))
        .collect::<Vec<_>>();
    app.open_dialog(crate::surfaces::DialogKind::Sessions);

    // Delete the row at index 2 (mid-list). The cursor must stay at 2 — now
    // pointing at "s3", which slid into the freed slot.
    app.modal_index = 2;
    let idx = app.modal_index.min(app.sessions_overview.len() - 1);
    let deleted = app.sessions_overview.remove(idx);
    assert_eq!(deleted.id, "s2");
    app.modal_index = app.modal_index.min(app.sessions_overview.len() - 1);
    assert_eq!(app.modal_index, 2, "mid-list delete keeps the cursor put");
    assert_eq!(
        app.sessions_overview[app.modal_index].id, "s3",
        "the next session slid into the removed slot"
    );

    // Delete the now-last row (index 3 in the shrunken 4-row list, which holds
    // s4). The list then has 3 rows, so the cursor must clamp to index 2 (the
    // new last row), not jump to 0.
    app.modal_index = 3;
    let idx = app.modal_index.min(app.sessions_overview.len() - 1);
    app.sessions_overview.remove(idx);
    app.modal_index = app.modal_index.min(app.sessions_overview.len() - 1);
    assert_eq!(
        app.modal_index, 2,
        "deleting the last row clamps to the new last row, not the top"
    );
}

/// Regression: after a delete the backend pushes a fresh `SessionsOverview`,
/// and the event loop used to treat *every* such push as an "open the picker"
/// request — resetting `modal_index` to 0 and `session_scroll` to 0. That
/// snapped the selection back to the top on each delete, undoing the optimistic
/// local removal. The refresh path must preserve the cursor/scroll when the
/// modal is already open, resetting only on a genuine open (closed → open).
#[test]
fn sessions_picker_data_refresh_does_not_reset_cursor_when_already_open() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.sessions_overview = (0..5)
        .map(|i| overview_row(&format!("s{i}")))
        .collect::<Vec<_>>();
    app.open_dialog(crate::surfaces::DialogKind::Sessions);
    app.set_active_index(3);
    app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().scroll = 2;

    // Simulate the refresh path (open_sessions signal + fresh overview) with
    // the dialog ALREADY open: cursor and scroll must be preserved.
    let opening = app.active_dialog() != Some(crate::surfaces::DialogKind::Sessions); // false
    if opening {
        app.open_dialog(crate::surfaces::DialogKind::Sessions);
        app.set_active_index(0);
        app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().scroll = 0;
        app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().follow = true;
    }
    assert_eq!(app.active_index(), 3, "refresh while open keeps the cursor");
    assert_eq!(
        app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().scroll, 2,
        "refresh while open keeps the scroll"
    );

    // Now simulate opening from a different scene/dialog (the genuine-open case):
    // cursor and scroll reset to the top.
    app.reset_to_thread();
    let opening = app.active_dialog() != Some(crate::surfaces::DialogKind::Sessions); // true
    if opening {
        app.open_dialog(crate::surfaces::DialogKind::Sessions);
        app.set_active_index(0);
        app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().scroll = 0;
        app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().follow = true;
    }
    assert_eq!(app.active_index(), 0, "a genuine open resets the cursor");
    assert_eq!(
        app.surfaces.dlg_mut::<crate::surfaces::SessionsDialog>().scroll, 0,
        "a genuine open resets the scroll"
    );
}

// Unified surface router: transient stack, per-view drafts, queue hook,
// sub-layer pop, switcher filter.

#[test]
fn model_editor_esc_pops_back_to_its_picker() {
    // The surface stack replaces `editor_return_to`: an editor opened from
    // Models returns to Models; one opened from Connections returns to
    // Connections — the same editor, two parents, no hard-coding.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Models);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    app.pop_transient_surface();
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Models),
        "pops to Models"
    );

    // From Connections: the same editor, a different pushed parent.
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.surfaces
        .present_sheet(crate::surfaces::SheetKind::ModelEditor);
    app.pop_transient_surface();
    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Connections),
        "pops to Connections"
    );
}

#[test]
fn custom_connection_submits_with_multiple_comma_separated_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let template = &crate::providers::CUSTOM_TEMPLATE;
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.open_custom_provider_editor(template);
    assert_eq!(
        app.surfaces.active_sheet(),
        Some(crate::surfaces::SheetKind::CustomProvider)
    );
    app.custom_name = "WeChat Multi".to_string();
    app.custom_base_url = "https://chatapi.weixin.qq.com/openai/v1/chat/completions".to_string();
    app.custom_token = "tok".to_string();
    app.custom_field = 3;
    app.input = "gpt-4o, gpt-4o-mini, claudy".to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;

    crate::event_loop::actions::handle_submit_custom_provider(&mut app);

    let req1 = rx.try_recv().expect("should send RegisterProvider request");
    match req1 {
        nuo_wire::AgentRequest::RegisterProvider { id, root_url, .. } => {
            assert_eq!(id, "custom-wechat-multi");
            assert_eq!(root_url, "https://chatapi.weixin.qq.com/openai/v1/chat/completions");
        }
        _ => panic!("Expected RegisterProvider request"),
    }

    let req2 = rx.try_recv().expect("should send AddConnection request");
    match req2 {
        nuo_wire::AgentRequest::AddConnection {
            name,
            provider,
            models,
            ..
        } => {
            assert_eq!(name, "WeChat Multi", "the raw trimmed name is sent");
            assert_eq!(provider, "custom-wechat-multi");
            assert_eq!(models, vec!["gpt-4o", "gpt-4o-mini", "claudy"]);
        }
        _ => panic!("Expected AddConnection request"),
    }
}

#[test]
fn curated_template_submits_the_provider_id_without_a_protocol_override() {
    // A curated provider owns its wire: the create request carries the provider
    // id (the template's id) and no protocol/base-url override.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.open_custom_provider_editor(openai_template());
    // The focused Name field owns the composer line.
    app.input = "OpenAI Work".to_string();
    app.custom_token = "sk-test".to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;

    crate::event_loop::actions::handle_submit_custom_provider(&mut app);

    let req = rx.try_recv().expect("should send a request");
    match req {
        nuo_wire::AgentRequest::AddConnection {
            name,
            provider,
            models,
            ..
        } => {
            assert_eq!(name, "OpenAI Work");
            assert_eq!(provider, "openai");
            assert!(!models.is_empty(), "seeded model list travels");
        }
        _ => panic!("Expected AddConnection request"),
    }
}

#[test]
fn editor_rename_sends_the_rename_transaction_then_the_metadata_edit() {
    // ADR-0201 INV-4: the connection name IS the identity, so a rename is its
    // own atomic request. The editor emits it first (the metadata edit is keyed
    // by the new name) and then the `EditConnection` metadata update.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.provider_picker
        .rows
        .push(nuo_wire::ProviderPickerRow {
            id: "my-relay".to_string(),
            name: "My Relay".to_string(),
            model: "glm-5.2".to_string(),
            models: vec!["glm-5.2".to_string()],
            model_info: Vec::new(),
            builtin: false,
            protocol: "chat-completions".to_string(),
            base_url: "https://relay.example.com/v1".to_string(),
            key_ready: true,
            provider: "custom".to_string(),
            client_identity: Default::default(),
            last_used_ms: None,
            auth: nuo_wire::ConnectionAuth::ApiKey,
        });
    app.open_edit_provider_editor(
        "my-relay".to_string(),
        "My Relay".to_string(),
        "chat-completions".to_string(),
        "https://relay.example.com/v1".to_string(),
        nuo_wire::ConnectionAuth::ApiKey,
        false,
        nuo_wire::ClientIdentity::Native,
    );
    // The Name field owns the composer line; typing a new name renames.
    app.custom_field = 0;
    app.input = "My Relay Renamed".to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;

    crate::event_loop::actions::handle_submit_custom_provider(&mut app);

    let rename = rx.try_recv().expect("rename transaction first");
    match rename {
        nuo_wire::AgentRequest::RenameConnection { from, to } => {
            assert_eq!(from, "my-relay");
            assert_eq!(to, "My Relay Renamed");
        }
        other => panic!("Expected RenameConnection first, got {other:?}"),
    }
    let reg = rx.try_recv().expect("register provider second");
    match reg {
        nuo_wire::AgentRequest::RegisterProvider { id, root_url, .. } => {
            assert_eq!(id, "custom");
            assert_eq!(root_url, "https://relay.example.com/v1");
        }
        other => panic!("Expected RegisterProvider second, got {other:?}"),
    }
    let edit = rx.try_recv().expect("metadata edit third");
    match edit {
        nuo_wire::AgentRequest::EditConnection {
            name,
            provider,
            ..
        } => {
            assert_eq!(name, "My Relay Renamed", "the edit follows the rename");
            assert_eq!(provider, "custom");
        }
        other => panic!("Expected EditConnection third, got {other:?}"),
    }
}

#[tokio::test]
async fn open_active_connection_detail_opens_standalone_and_closes_to_none() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_provider = "anthropic-prod".to_string();

    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;

    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    crate::event_loop::open_active_connection_detail(&mut app, &runtime, "s1");

    assert_eq!(
        app.active_dialog(),
        Some(crate::surfaces::DialogKind::Connections)
    );
    assert!(app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail);
    assert!(app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone);

    let req = rx.try_recv().expect("should query connection detail");
    match req {
        nuo_wire::AgentRequest::QueryConnectionDetail { id, .. } => {
            assert_eq!(id, "anthropic-prod");
        }
        _ => panic!("Expected QueryConnectionDetail request"),
    }

    // Esc in standalone connection detail closes directly to None (no drill-down backout)
    crate::event_loop::handle_close_modal(&mut app, "s1");
    assert!(app.surfaces.active_overlay().is_none());
    assert!(!app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail);
    assert!(!app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_standalone);
}

#[tokio::test]
async fn connection_detail_quota_update_preserves_scroll_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    // Initial phase 1 detail arrives with Fetching usage
    let detail_phase1 = nuo_wire::ConnectionDetail {
        name: "google-antigravity".to_string(),
        provider: "google-antigravity".to_string(),
        provider_label: "Google Antigravity".to_string(),
        protocol: "google".to_string(),
        base_url: "https://daily-cloudcode-pa.googleapis.com".to_string(),
        auth_type: "OAuth (AntigravityOAuth)".to_string(),
        api_key_masked: Some("ya29...".to_string()),
        api_key_source: "OAuth".to_string(),
        client_identity: nuo_wire::ClientIdentity::Antigravity,
        user_agent: "antigravity".to_string(),
        models: vec!["gemini-2.5-pro".to_string()],
        model_info: vec![],
        active_model: Some("gemini-2.5-pro".to_string()),
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Fetching,
    };

    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::ConnectionDetail(detail_phase1.clone()),
    );

    assert_eq!(app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll, 0);
    assert_eq!(
        app.connection_detail.as_ref().map(|d| &d.name),
        Some(&"google-antigravity".to_string())
    );

    // User scrolls down while reading the connection details
    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll = 5;

    // Phase 2 quotas arrive for the same connection
    let mut detail_phase2 = detail_phase1.clone();
    detail_phase2.usage = nuo_wire::ConnectionUsageState::Available(Box::default());

    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::ConnectionDetail(detail_phase2),
    );

    // Scroll must be preserved and not jump to 0!
    assert_eq!(
        app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_scroll, 5,
        "quota loading must preserve scroll position"
    );
}

#[tokio::test]
async fn connection_detail_refresh_action_queries_active_detail_id() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    app.open_dialog(crate::surfaces::DialogKind::Connections);
    app.surfaces.dlg_mut::<crate::surfaces::ConnectionsDialog>().info_detail = true;
    app.connection_detail = Some(nuo_wire::ConnectionDetail {
        name: "custom-relay".to_string(),
        provider: "custom".to_string(),
        provider_label: "Custom connection".to_string(),
        protocol: "openai".to_string(),
        base_url: "https://example.com".to_string(),
        auth_type: "API Key".to_string(),
        api_key_masked: Some("sk-...".to_string()),
        api_key_source: "credentials.toml".to_string(),
        client_identity: nuo_wire::ClientIdentity::Native,
        user_agent: "nuo".to_string(),
        models: vec![],
        model_info: vec![],
        active_model: None,
        active_model_effort: None,
        active_model_thinking: None,
        usage: nuo_wire::ConnectionUsageState::Unsupported,
    });

    let flow = crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::RefreshProviderModels,
        "s1",
    )
    .await;

    assert_eq!(flow, crate::event_loop::actions::ActionFlow::Handled);
    assert_eq!(
        app.connection_detail.as_ref().map(|d| &d.usage),
        Some(&nuo_wire::ConnectionUsageState::Fetching)
    );

    let req = rx
        .try_recv()
        .expect("should query connection detail for refresh");
    match req {
        nuo_wire::AgentRequest::QueryConnectionDetail { id, .. } => {
            assert_eq!(id, "custom-relay");
        }
        _ => panic!("Expected QueryConnectionDetail request"),
    }
}

#[tokio::test]
async fn models_modal_refresh_action_provides_feedback_and_deduplicates() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();

    app.open_dialog(crate::surfaces::DialogKind::Models);
    assert!(!app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing);

    // First press: initiates refresh, sets models_refreshing, shows toast, sends request
    let flow = crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::RefreshProviderModels,
        "s1",
    )
    .await;

    assert_eq!(flow, crate::event_loop::actions::ActionFlow::Handled);
    assert!(app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing);
    assert_eq!(app.copy_toast_message, "Refreshing models…");
    assert!(app.copy_toast_until.is_some());

    let req = rx.try_recv().expect("should send refresh request");
    assert!(matches!(
        req,
        nuo_wire::AgentRequest::RefreshProviderModels
    ));

    // Second press while refreshing: warns already in progress, does not send duplicate request
    let flow2 = crate::event_loop::actions::dispatch_action_for_test(
        &mut app,
        &runtime,
        crate::input::InputAction::RefreshProviderModels,
        "s1",
    )
    .await;

    assert_eq!(flow2, crate::event_loop::actions::ActionFlow::Handled);
    assert!(app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing);
    assert_eq!(app.copy_toast_message, "Model refresh already in progress…");
    assert!(rx.try_recv().is_err(), "should not send duplicate request");

    // When ProviderPicker snapshot arrives, models_refreshing resets to false
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::ProviderPicker(
            nuo_wire::ProviderPickerSnapshot::default(),
        ),
    );
    assert!(!app.surfaces.dlg_mut::<crate::surfaces::ModelsDialog>().refreshing);

    // When NoticeToast arrives, local copy toast is cleared so server notice is visible
    app.copy_toast_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
    crate::event_loop::apply::apply(
        &mut app,
        &runtime,
        crate::event_loop::AppMutation::NoticeToast {
            severity: crate::model::document::NoticeSeverity::Info,
            text: "Model list refreshed (up to date)".to_string(),
        },
    );
    assert!(app.copy_toast_until.is_none());
    assert_eq!(
        app.notice_toast_message,
        "Model list refreshed (up to date)"
    );
}
