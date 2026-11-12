//! Remote-surface admission and relay hygiene policies.

use super::*;

#[test]
fn relay_surfaces_default_to_curated_noise_hygiene() {
    // ADR-0081 [INV-CATALOG-06]: a surface that publishes bare ids publishes a
    // noisy catalog too, so its default admission gate is `Curated`. The
    // predicate is the spec's declared display policy, never a hardcoded
    // provider-id list.
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "my-go".to_string(),
        vec![
            "deepseek-v4.1-flash".to_string(),
            "glm-5.3".to_string(),
            "beam-free".to_string(),
            "hy4-preview".to_string(),
            "gemini-4-argon-contributor".to_string(),
            "omen-alpha".to_string(),
        ],
    );
    let connection = instance("my-go", Some("opencode-go"));

    let models = route_models(&connection, &cache, declarations());
    assert!(models.contains(&"deepseek-v4.1-flash".to_string()));
    assert!(models.contains(&"glm-5.3".to_string()));
    assert!(
        !models.contains(&"beam-free".to_string()),
        "promotional tier pruned"
    );
    assert!(
        !models.contains(&"hy4-preview".to_string()),
        "preview snapshot pruned"
    );
    assert!(
        !models.contains(&"gemini-4-argon-contributor".to_string()),
        "contributor node pruned"
    );
    assert!(!models.contains(&"omen-alpha".to_string()), "alpha pruned");

    // Sovereign injection bypasses the hygiene filter unconditionally.
    let mut sovereign = connection.clone();
    sovereign.models.include = vec![nuo_persistence::connections::DeclaredModel {
        id: "beam-free".into(),
        ..Default::default()
    }];
    assert!(
        route_models(&sovereign, &cache, declarations()).contains(&"beam-free".to_string()),
        "an explicit include is never filtered"
    );

    // A curated native surface keeps its open admission: DeepSeek's own
    // `/models` is trustworthy and its ids are not relay noise.
    let mut native_cache = RemoteCatalogCache::default();
    native_cache.connection_models.insert(
        "my-deepseek".to_string(),
        vec!["deepseek-v4.1-flash".to_string()],
    );
    let native = instance("my-deepseek", Some("deepseek"));
    assert_eq!(
        route_models(&native, &native_cache, declarations()),
        vec!["deepseek-v4.1-flash"]
    );
}

#[test]
fn relay_labels_and_seeds_survive_the_hygiene_valve() {
    ensure_test_init();
    // An ids-only relay advertises no metadata at all, so the structural label
    // must be produced from the id itself (ADR-0081 [INV-CATALOG-05]) — gating
    // the mapping on an existing remote record is how it silently never runs.
    let mut cache = RemoteCatalogCache::default();
    cache.connection_models.insert(
        "my-go".to_string(),
        vec!["deepseek-v4.1-flash".to_string(), "beam-free".to_string()],
    );
    let connection = instance("my-go", Some("opencode-go"));
    let inputs = inputs_with_cache(&cache);

    let ch = derive_channel(&connection, "deepseek-v4.1-flash", &inputs).expect("channel");
    assert_eq!(
        ch.remote.as_ref().and_then(|r| r.name.as_deref()),
        Some("DeepSeek V4.1 Flash"),
        "a bare relay id is lifted into typography"
    );
    assert_eq!(
        channel_model_info(&ch).name.as_deref(),
        Some("DeepSeek V4.1 Flash"),
        "the picker snapshot carries the mapped label"
    );
    assert_eq!(
        ch.model, "deepseek-v4.1-flash",
        "identity stays the wire id"
    );

    // A promotional id discovered from the relay is pruned from the listing.
    let models = route_models(&connection, &cache, declarations());
    assert!(models.contains(&"deepseek-v4.1-flash".to_string()));
    assert!(!models.contains(&"beam-free".to_string()));

    // ...but an id the surface's own baseline curates is never filtered by that
    // same valve: `opencode-go` seeds `*-free` tiers deliberately.
    let mut seeded_cache = RemoteCatalogCache::default();
    seeded_cache.connection_models.insert(
        "my-go".to_string(),
        vec![
            "deepseek-v4.1-flash".to_string(),
            "deepseek-v4.1-flash-free".to_string(),
            "glm-5.3-flash-free".to_string(),
        ],
    );
    let seeded = route_models(&connection, &seeded_cache, declarations());
    assert!(
        seeded.contains(&"deepseek-v4.1-flash-free".to_string()),
        "a seeded free tier survives its own hygiene valve: {seeded:?}"
    );
    assert!(
        seeded.contains(&"glm-5.3-flash-free".to_string()),
        "compiled baseline and surface must agree: {seeded:?}"
    );
}
