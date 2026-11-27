//! Picker state projection and model-info effort ladders.

use super::*;

// picker

#[test]
fn build_picker_state_reflects_instances() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("deepseek", Some("deepseek"))],
    };
    instances.save().unwrap();
    let config = Config {
        default_connection: "deepseek".to_string(),
        default_model: Some("deepseek-flash".to_string()),
        ..Default::default()
    };
    let snapshot = build_picker_state(
        &config,
        &nuo_persistence::connection_usage::ConnectionUsage::default(),
    );
    assert_eq!(snapshot.default_id, "deepseek");
    let row = snapshot
        .rows
        .iter()
        .find(|r| r.id == "deepseek")
        .expect("deepseek row");
    assert_eq!(row.name, "deepseek");
    assert_eq!(row.provider, "deepseek");
    assert!(row.models.contains(&"deepseek-flash".to_string()));
}

#[test]
fn channel_model_info_effort_ladders_survive() {
    // A Gemini model advertises an effort ladder, so its picker row exposes an
    // effort defaulting to `high` (the ladder's top rung) when unset.
    let gemini37 = nuo_provider::catalog::Channel {
        id: "default".to_string(),
        transport: Transport::GoogleGenerateContent {
            base_url: "https://cloudcode-pa.googleapis.com".to_string(),
            dialect: Default::default(),
            user_agent: "antigravity/cli".to_string(),
            headers: Vec::new(),
        },
        credentials: nuo_protocol::static_credential(""),
        model: "gemini-3.7-flash".to_string(),
        remote: None,
        user_overrides: None,
        tunings: Default::default(),
        prompt_cache_preference: nuo_protocol::PromptCachePreference::default(),
        prompt_cache: nuo_protocol::PromptCacheCapabilities::unsupported(),
    };
    let info = channel_model_info(&gemini37);
    assert_eq!(info.protocol, WireProtocol::GoogleGenerateContent.as_str());
    assert_eq!(info.effort.as_deref(), Some("high"));
    assert_eq!(info.thinking, None);
}
