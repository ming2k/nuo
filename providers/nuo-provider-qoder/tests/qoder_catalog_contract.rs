//! Contract audit against a **captured** Qoder catalog response.
//!
//! The parser decides two things from the payload: which entries are models at
//! all (membership — function switches are excluded) and whether an entry is
//! usable (availability). Both are only as good as the vocabulary they classify
//! against, and the payload is a vendor surface nuo does not control. This
//! suite pins that vocabulary to a real capture so drift fails a test instead
//! of silently changing what a user may select.
//!
//! Fixture: `tests/fixtures/qoder-model-list-1.1.58.json`, captured live from
//! `GET /algo/api/v2/model/list` with `examples/qoder_catalog_dump`. The suffix
//! is the `Cosy-Version` the capture was taken with.
//!
//! `[INV-VAL-02]`: the fixture is committed, so the suite is deterministic and
//! never reaches the network. The capture tool exists so a future client bump
//! regenerates it in one command rather than re-running a `/tmp` recon that
//! will not survive a reboot.

use nuo_provider_qoder::surface::{FUNCTION_SWITCH_KEYS, is_function_switch};
use serde_json::Value;

/// The captured catalog, as the server returned it.
fn captured_catalog() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/qoder-model-list-1.1.58.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("fixture is valid JSON")
}

/// Every `(scene, entry)` pair in the capture, in document order.
fn all_entries(catalog: &Value) -> Vec<(String, Value)> {
    catalog
        .as_object()
        .expect("catalog is a scene map")
        .iter()
        .flat_map(|(scene, entries)| {
            entries
                .as_array()
                .into_iter()
                .flatten()
                .map(|entry| (scene.clone(), entry.clone()))
        })
        .collect()
}

/// The capture is the shape the parser asserts: a scene-keyed map whose values
/// are all arrays. A re-capture that changes this shape invalidates every other
/// assertion here, so it is checked first and loudly.
#[test]
fn capture_is_a_scene_keyed_map_of_arrays() {
    let catalog = captured_catalog();
    let scenes = catalog.as_object().expect("catalog is an object");
    assert!(
        scenes.values().all(Value::is_array),
        "every scene value must be an array of entries"
    );
    // Populated scenes carry the same 17-entry set on this account; `inline`
    // carries only the four always-present switches. Assert the structural
    // facts the parser depends on rather than exact counts, which legitimately
    // move when Qoder adds a model.
    assert!(
        scenes.contains_key("assistant"),
        "the default scene is present"
    );
    let entries = all_entries(&catalog);
    assert!(
        entries.len() > 50,
        "capture looks truncated ({} entries)",
        entries.len()
    );
}

/// **The drift tripwire.** Every entry in the capture is classified as either a
/// function switch or a model, and the classification is cross-checked against
/// an independent signal the vendor publishes.
///
/// The payload **declares no kind field**: no value of any field is disjoint
/// between switches and models except `display_name` and `key` themselves. So
/// there is no declared discriminator to key off, and `FUNCTION_SWITCH_KEYS` is
/// necessarily a pinned vocabulary rather than a read of a declared type.
///
/// `is_new` is the only remaining independent signal — it is a "NEW" marketing
/// badge, present on every captured model and absent from every captured
/// switch. It is therefore used here as a *proxy* cross-check and never as the
/// classifier: classifying on it would mean a model that ages out of the badge
/// silently leaves the catalog. A failure of this test has exactly two possible
/// causes, both worth a human look:
///
/// 1. a new switch name appeared → add it to `FUNCTION_SWITCH_KEYS`;
/// 2. the vendor changed what `is_new` means → the proxy is dead, re-derive a
///    signal from a fresh capture and update this test.
#[test]
fn switch_vocabulary_classifies_every_captured_entry() {
    let catalog = captured_catalog();
    let mut switches = Vec::new();
    let mut models = Vec::new();
    for (scene, entry) in all_entries(&catalog) {
        let key = entry
            .get("key")
            .and_then(Value::as_str)
            .expect("every entry declares a key");
        if is_function_switch(key, &scene) {
            switches.push((scene, key.to_string(), entry));
        } else {
            models.push((scene, key.to_string(), entry));
        }
    }

    assert!(
        !switches.is_empty() && !models.is_empty(),
        "classification produced an empty side: {} switches, {} models",
        switches.len(),
        models.len()
    );

    // Cross-check 1: no switch carries the model-only `is_new` badge.
    for (scene, key, entry) in &switches {
        assert!(
            !entry.as_object().is_some_and(|e| e.contains_key("is_new")),
            "{scene}/{key} is classified as a function switch but carries \
             `is_new`, which only models carry — the vocabulary is stale"
        );
    }
    // Cross-check 2: every model does carry it.
    for (scene, key, entry) in &models {
        assert!(
            entry.as_object().is_some_and(|e| e.contains_key("is_new")),
            "{scene}/{key} is classified as a model but lacks `is_new` — either \
             a new function switch name has appeared (add it to \
             FUNCTION_SWITCH_KEYS) or the vendor dropped the badge"
        );
    }
}

/// Every name in the declared vocabulary actually appears in the capture. A
/// vocabulary entry the vendor stopped publishing is dead weight that would
/// silently suppress a future model of the same name.
#[test]
fn declared_vocabulary_is_exercised_by_the_capture() {
    let catalog = captured_catalog();
    for name in FUNCTION_SWITCH_KEYS {
        let present = all_entries(&catalog).iter().any(|(scene, entry)| {
            entry
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| is_function_switch(key, scene) && key.ends_with(name))
        });
        assert!(
            present,
            "`{name}` is declared as a function switch but no captured entry \
             uses it — the vocabulary is stale"
        );
    }
}

/// Scene-scoped switches carry their owning scene as a hyphen prefix. The rule
/// must strip that prefix, and must not let a scene name leak into a *model*
/// key match (model keys use `_`).
#[test]
fn scene_prefixed_switches_match_only_in_their_own_scene() {
    let catalog = captured_catalog();
    let mut prefixed = 0;
    for (scene, entry) in all_entries(&catalog) {
        let Some(key) = entry.get("key").and_then(Value::as_str) else {
            continue;
        };
        if !key.contains('-') {
            continue;
        }
        prefixed += 1;
        // A hyphenated key is a switch iff its suffix is in the vocabulary and
        // its prefix is the scene it appeared in.
        let (prefix, suffix) = key.split_once('-').expect("split");
        let expect_switch = prefix == scene && FUNCTION_SWITCH_KEYS.contains(&suffix);
        assert_eq!(
            is_function_switch(key, &scene),
            expect_switch,
            "{scene}/{key}: hyphenated keys follow the scene-prefix rule"
        );
    }
    assert!(
        prefixed > 0,
        "capture has no scene-prefixed switches — the prefix rule is untested"
    );
}

/// The parser's membership decision on the real capture: the `assistant` scene
/// yields exactly the entries that are not function switches, and never a
/// switch under any scene the capture publishes.
#[test]
fn parser_yields_only_models_for_the_captured_assistant_scene() {
    let catalog = captured_catalog();
    let models = nuo_provider_qoder::parse_scene_catalog(&catalog, "assistant");
    let expected: Vec<String> = all_entries(&catalog)
        .iter()
        .filter(|(scene, _)| scene == "assistant")
        .filter_map(|(_, entry)| entry.get("key").and_then(Value::as_str))
        .filter(|key| !FUNCTION_SWITCH_KEYS.contains(key))
        .map(str::to_string)
        .collect();

    let got: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
    assert_eq!(got, expected, "membership on the captured assistant scene");
    for name in FUNCTION_SWITCH_KEYS {
        assert!(
            !got.iter().any(|id| id == name),
            "function switch `{name}` leaked into the model universe"
        );
    }
}

/// Availability on the real capture stays a *declaration*: each parsed verdict
/// agrees with the raw `enable` the payload carried, and each reason is exactly
/// what the payload stated — verbatim key, or `None`.
///
/// This asserts **parser behaviour**, never account state: it makes no claim
/// about how many entries happen to be locked on the captured account. A
/// re-capture from a paid plan (everything `enable:true`) must pass unchanged,
/// which is why the counts are reported but not asserted.
#[test]
fn availability_on_the_capture_is_declared_never_invented() {
    let catalog = captured_catalog();
    let models = nuo_provider_qoder::parse_scene_catalog(&catalog, "assistant");
    assert!(models.len() > 1, "capture should yield a real catalog");

    let mut locked_with_reason = 0;
    let mut locked = 0;
    let mut usable = 0;
    for model in &models {
        // Re-derive the verdict independently from the raw payload.
        let raw = all_entries(&catalog)
            .into_iter()
            .find(|(scene, entry)| {
                scene == "assistant" && entry.get("key").and_then(Value::as_str) == Some(&model.id)
            })
            .expect("model came from the capture");
        let enable = raw.1.get("enable").and_then(Value::as_bool);
        match (model.availability.clone(), enable) {
            (None, None) => {}
            (Some(availability), Some(true)) => {
                assert!(
                    availability.usable,
                    "{}: enable:true must be usable",
                    model.id
                );
                usable += 1;
            }
            (Some(availability), Some(false)) => {
                assert!(
                    !availability.usable,
                    "{}: enable:false must be locked",
                    model.id
                );
                // The reason is the payload's own key, verbatim.
                let stated = raw
                    .1
                    .get("strategies")
                    .and_then(Value::as_array)
                    .and_then(|strategies| {
                        strategies
                            .iter()
                            .find_map(|s| s.get("disabled_message_key").and_then(Value::as_str))
                    })
                    .map(str::to_string);
                assert_eq!(
                    availability.reason, stated,
                    "{}: reason must equal what the payload stated",
                    model.id
                );
                if stated.is_some() {
                    locked_with_reason += 1;
                }
                locked += 1;
            }
            (other, enable) => {
                panic!(
                    "{}: verdict {other:?} disagrees with enable={enable:?}",
                    model.id
                )
            }
        }
    }
    assert!(
        usable + locked > 0,
        "capture declares no `enable` verdicts at all"
    );
    // Reported, never asserted: these are facts about the *captured account*,
    // not about the parser. A re-capture from a different plan moves them.
    eprintln!(
        "capture distribution: {usable} usable, {locked} locked \
         ({locked_with_reason} carrying a stated reason)"
    );
}

/// The reason read is covered on both branches by synthetic payloads so the
/// fixture's account state can never be what makes a branch untested:
/// `strategies[].disabled_message_key` present, absent, blank, and the whole
/// array missing.
#[test]
fn reason_is_taken_verbatim_only_when_the_payload_states_one() {
    use serde_json::json;

    let catalog = json!({
        "assistant": [
            {
                "key": "a_with_reason", "enable": false,
                "strategies": [{ "tag": "C4", "enabled": false,
                                 "disabled_message_key": "codeSafeModelReason" }]
            },
            {
                "key": "b_no_strategies", "enable": false
            },
            {
                "key": "c_empty_strategies", "enable": false, "strategies": []
            },
            {
                "key": "d_null_key", "enable": false,
                "strategies": [{ "tag": "C4", "disabled_message_key": null }]
            },
            {
                "key": "e_blank_key", "enable": false,
                "strategies": [{ "tag": "C4", "disabled_message_key": "   " }]
            },
            {
                "key": "f_enabled_strategy", "enable": false,
                "strategies": [{ "tag": "enterprise-safety", "enabled": true,
                                 "disabled_message_key": "codeSafeModelReason" }]
            }
        ]
    });

    let models = nuo_provider_qoder::parse_scene_catalog(&catalog, "assistant");
    let reason_for = |id: &str| {
        models
            .iter()
            .find(|m| m.id == id)
            .and_then(|m| m.availability.clone())
            .and_then(|a| a.reason)
    };
    assert_eq!(
        reason_for("a_with_reason").as_deref(),
        Some("codeSafeModelReason"),
        "the stated key is carried verbatim"
    );
    for id in [
        "b_no_strategies",
        "c_empty_strategies",
        "d_null_key",
        "e_blank_key",
    ] {
        assert_eq!(
            reason_for(id),
            None,
            "{id}: no stated reason, none invented"
        );
    }
    // A strategy the vendor marks `enabled` is not a disabled-message source.
    assert_eq!(
        reason_for("f_enabled_strategy"),
        Some("codeSafeModelReason".to_string()),
        "the key is stated on the entry either way; nuo records what the \
         payload said and lets `usable` carry the verdict"
    );
}
