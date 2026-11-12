//! Reasoning-level and thinking-budget resolution.

use super::*;

#[test]
fn resolve_thinking_level_for_gemini_3x() {
    // Gemini 3.x uses a level ladder; each rung maps to thinkingLevel,
    // clamping down from unsupported depths.
    use nuo_provider::effort::Effort;
    let gemini_level: &[Effort] = &[Effort::Minimal, Effort::Low, Effort::Medium, Effort::High];
    let level: Vec<nuo_provider::EffortLevel> =
        gemini_level.iter().copied().map(Into::into).collect();
    assert_eq!(
        resolve_thinking(Some(Effort::High), &level, 0),
        Some(GoogleThinking::Level(Effort::High))
    );
    // max/xhigh clamp down to high (Gemini 3.x has no deeper rung).
    assert_eq!(
        resolve_thinking(Some(Effort::Max), &level, 0),
        Some(GoogleThinking::Level(Effort::High))
    );
    assert_eq!(
        resolve_thinking(Some(Effort::Minimal), &level, 0),
        Some(GoogleThinking::Level(Effort::Minimal))
    );
    // No override → server default.
    assert_eq!(resolve_thinking(None, &level, 0), None);
}

#[test]
fn resolve_thinking_budget_for_gemini_2_5() {
    // Gemini 2.5 uses a budget ladder; rungs map to token buckets against
    // the model's max (Flash: 24576).
    use nuo_provider::effort::Effort;
    let gemini_budget: &[Effort] = &[
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Max,
    ];
    let budget: Vec<nuo_provider::EffortLevel> =
        gemini_budget.iter().copied().map(Into::into).collect();
    assert_eq!(
        resolve_thinking(Some(Effort::Medium), &budget, 24576),
        Some(GoogleThinking::Budget(12288))
    );
    assert_eq!(
        resolve_thinking(Some(Effort::Max), &budget, 24576),
        Some(GoogleThinking::Budget(24576))
    );
    // Pro's larger cap scales the bucket.
    assert_eq!(
        resolve_thinking(Some(Effort::Medium), &budget, 32768),
        Some(GoogleThinking::Budget(16384))
    );
}

#[test]
fn resolve_thinking_empty_ladder_is_no_override() {
    // A non-reasoning / unknown model (empty ladder) never stamps thinking.
    assert_eq!(
        resolve_thinking(Some(nuo_provider::Effort::High), &[], 24576),
        None
    );
}

#[test]
fn body_stamps_thinking_level_and_budget() {
    // Level → thinkingLevel string. The wire enum is UPPER-CASE and closed
    // (`LOW`/`MEDIUM`/`HIGH`); a lower-cased rung is a 400 that names no
    // field, which is why the translation lives in one typed place.
    let level_body = body(
        vec![Message::new(Role::User, "think")],
        test_body_input(
            None,
            false,
            Some(GoogleThinking::Level(nuo_provider::Effort::Medium)),
        ),
    );
    assert_eq!(
        level_body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "MEDIUM"
    );
    // Budget → thinkingBudget integer.
    let budget_body = body(
        vec![Message::new(Role::User, "think")],
        test_body_input(None, false, Some(GoogleThinking::Budget(8192))),
    );
    assert_eq!(
        budget_body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        8192
    );
}

#[test]
fn thinking_budget_wire_buckets_within_the_models_cap() {
    use super::thinking_budget_wire;
    use nuo_provider::Effort;

    // Gemini 2.5 Flash: 0–24576. Each rung is a fraction of the cap, and
    // the bucket never escapes the model's ceiling.
    assert_eq!(thinking_budget_wire(Effort::None, 24576), 0);
    assert_eq!(thinking_budget_wire(Effort::Minimal, 24576), 2457);
    assert_eq!(thinking_budget_wire(Effort::Low, 24576), 6144);
    assert_eq!(thinking_budget_wire(Effort::Medium, 24576), 12288);
    assert_eq!(thinking_budget_wire(Effort::High, 24576), 24576);
    assert_eq!(thinking_budget_wire(Effort::Max, 24576), 24576);
    // Gemini 2.5 Pro: 128–32768. Buckets scale by the larger cap.
    assert_eq!(thinking_budget_wire(Effort::Medium, 32768), 16384);
    assert_eq!(thinking_budget_wire(Effort::High, 32768), 32768);
    // A floor of 1 is guaranteed even on a tiny max budget, so a nonzero
    // rung can never round down into "off".
    assert_eq!(thinking_budget_wire(Effort::Minimal, 5), 1);
    assert_eq!(thinking_budget_wire(Effort::Low, 2), 1);
}

#[test]
fn thinking_level_wire_is_always_a_member_of_the_accepted_enum() {
    use crate::request::thinking::{THINKING_LEVELS, thinking_level_wire};
    use nuo_provider::Effort;

    // The whole closed `Effort` vocabulary must map into the three accepted
    // values — never through `as_str()`, whose case and rung set differ.
    for effort in Effort::ORDER {
        let wire = thinking_level_wire(effort);
        assert!(
            THINKING_LEVELS.contains(&wire),
            "{effort:?} produced {wire}, which is not in {THINKING_LEVELS:?}"
        );
        assert_ne!(
            wire,
            effort.as_str(),
            "{effort:?} must not be stamped as its generic effort spelling"
        );
    }
    // The exact rung mapping, including the clamp at each end.
    assert_eq!(thinking_level_wire(Effort::Low), "LOW");
    assert_eq!(thinking_level_wire(Effort::Medium), "MEDIUM");
    assert_eq!(thinking_level_wire(Effort::High), "HIGH");
    // `minimal` is advertised by the catalog but is NOT a wire value.
    assert_eq!(thinking_level_wire(Effort::Minimal), "LOW");
    // Deeper-than-ceiling rungs clamp down, never up past the enum.
    assert_eq!(thinking_level_wire(Effort::Xhigh), "HIGH");
    assert_eq!(thinking_level_wire(Effort::Max), "HIGH");
    assert_eq!(thinking_level_wire(Effort::Ultra), "HIGH");
    assert_eq!(thinking_level_wire(Effort::None), "LOW");
}

#[test]
fn max_thinking_budget_per_gemini_2_5_model() {
    assert_eq!(max_thinking_budget("gemini-2.5-pro"), 32768);
    assert_eq!(max_thinking_budget("gemini-2.5-flash"), 24576);
    assert_eq!(max_thinking_budget("gemini-2.5-flash-lite"), 24576);
    // Gemini 3.x and non-reasoning models have no budget surface.
    assert_eq!(max_thinking_budget("gemini-3.7-flash"), 0);
    assert_eq!(max_thinking_budget("gemini-3.5-flash"), 0);
    assert_eq!(max_thinking_budget("gemini-2.0-flash"), 0);
}
