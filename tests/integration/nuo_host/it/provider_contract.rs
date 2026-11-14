//! Cross-provider contract suite.
//!
//! Per-provider unit tests prove a parser or a preset; this suite proves the
//! *registry-wide* contract every `providers/*` crate must satisfy before the
//! composition root may link it. It is intentionally table-driven: a new
//! provider is covered the moment it is added to
//! `nuo_server::provider_registry::MODEL_PROVIDER_SPECS`.

use std::collections::BTreeSet;

use nuo_server::provider_registry::MODEL_PROVIDER_SPECS;

#[test]
fn provider_specs_are_unique_and_well_formed() {
    let mut ids = BTreeSet::new();
    for spec in MODEL_PROVIDER_SPECS {
        let id = spec.id.as_ref();
        assert!(
            ids.insert(id.to_string()),
            "duplicate model provider id `{id}`; [INV-PROV-06] requires exactly one definition"
        );
        assert!(
            !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && !id.starts_with('-')
                && !id.ends_with('-'),
            "provider id `{id}` must be lower kebab-case"
        );
        spec.validate()
            .unwrap_or_else(|error| panic!("provider `{id}` is structurally invalid: {error}"));

        assert!(
            spec.root_url.starts_with("https://"),
            "provider `{id}` root `{}` must be an https endpoint; user-declared providers are the \
             only surface allowed a custom root, and they never enter this table",
            spec.root_url
        );
        assert!(
            !spec.root_url.ends_with('/'),
            "provider `{id}` root `{}` must not end in `/`; endpoint algebra owns the separator",
            spec.root_url
        );

        let mut header_names = BTreeSet::new();
        for (name, value) in spec.default_headers.iter() {
            assert!(
                !name.trim().is_empty() && name.eq_ignore_ascii_case(name.trim()),
                "provider `{id}` has a malformed default header name `{name}`"
            );
            assert!(
                !value.trim().is_empty(),
                "provider `{id}` default header `{name}` has an empty value"
            );
            assert!(
                header_names.insert(name.to_ascii_lowercase()),
                "provider `{id}` declares duplicate default header `{name}`"
            );
        }

        let mut seed_models = BTreeSet::new();
        for model in spec.models {
            assert!(
                !model.trim().is_empty() && !model.chars().any(char::is_whitespace),
                "provider `{id}` seeds a malformed model id `{model:?}`"
            );
            assert!(
                seed_models.insert((*model).to_string()),
                "provider `{id}` seeds duplicate model `{model}`"
            );
        }

        let mut baseline_ids = BTreeSet::new();
        for baseline in spec.baselines {
            assert!(
                baseline_ids.insert(baseline.id),
                "provider `{id}` repeats baseline model `{}`",
                baseline.id
            );
            assert!(
                spec.dialect.supports(baseline.protocol),
                "provider `{id}` baseline `{}` speaks {:?}, which its {:?} dialect does not support",
                baseline.id,
                baseline.protocol,
                spec.dialect
            );
        }
    }
}

#[test]
fn every_seeded_model_resolves_an_endpoint_and_prompt_cache_policy() {
    for spec in MODEL_PROVIDER_SPECS {
        let id = spec.id.as_ref();
        let endpoint = spec
            .endpoint(spec.protocol)
            .unwrap_or_else(|error| panic!("provider `{id}` endpoint is invalid: {error}"));
        assert!(
            endpoint.starts_with(spec.root_url.as_ref()),
            "provider `{id}` endpoint `{endpoint}` escapes its root `{}`",
            spec.root_url
        );

        for model in spec
            .models
            .iter()
            .copied()
            .chain(spec.baselines.iter().map(|baseline| baseline.id))
        {
            let protocol = spec.model_protocol(model);
            assert!(
                spec.dialect.supports(protocol),
                "provider `{id}` model `{model}` resolves protocol {protocol:?} outside its dialect"
            );
            // Resolution is pure data; it must never panic for any known model.
            let _ = spec.prompt_cache.resolve(model);
            let _ = spec.resolve_display_label(Some(model), model);
        }
    }
}
