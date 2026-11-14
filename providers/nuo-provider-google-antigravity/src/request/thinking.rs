//! Reasoning-depth resolution: Gemini 3.x levels and Gemini 2.5 budgets.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleThinking {
    /// `thinkingLevel` for a Gemini 3.x model: the closed wire enum
    /// `LOW`/`MEDIUM`/`HIGH` (via [`thinking_level_wire`]). Never
    /// `none`/`xhigh`/`max` (Gemini 3.x rejects them); the resolver clamps
    /// those down before constructing this variant.
    Level(nuo_protocol::Effort),
    /// `thinkingBudget` for a Gemini 2.5 model: a token count in the model's
    /// supported range. `0` disables thinking (Flash/Lite only — Pro's floor is
    /// `128`, so the resolver only emits `0` when the model's ladder allows
    /// [`nuo_protocol::Effort::None`]).
    Budget(i64),
}

/// Resolve a channel's raw reasoning-effort override into the wire directive
/// this Gemini model expects, clamping it to the model's supported levels.
///
/// The model's **effort ladder** decides which surface applies: a
/// `thinkingLevel` ladder (e.g. `minimal..=high`) maps each rung
/// to a level enum; a `thinkingBudget` ladder (including `max`) maps each rung to a token bucket
/// (the bucket cap is `max_budget`, e.g. `24576` for Gemini 2.5 Flash or
/// `32768` for Pro). Returns `None` when there is nothing to stamp — an empty
/// ladder (non-reasoning / unknown model) or an unset override — leaving the
/// server default in place.
///
/// `max_budget` is only consulted for the budget surface; pass any value for a
/// level-only model.
pub fn resolve_thinking(
    effort: Option<nuo_protocol::Effort>,
    effort_levels: &[nuo_protocol::EffortLevel],
    max_budget: u32,
) -> Option<GoogleThinking> {
    // A model that does not advertise a depth ladder has no thinking surface to
    // stamp (non-reasoning models like Gemini 2.5 Flash-Lite / 2.0 Flash, or an
    // unknown model with a default empty ladder). Fall back to the server
    // default rather than guessing a form the upstream may reject.
    if effort_levels.is_empty() {
        return None;
    }
    let requested = effort?;
    // Gemini's ladders are always compiled-in known rungs, so extract the known
    // subset for ranking; `Other` (a provider tier outside the vocabulary) has
    // no Gemini surface to map onto and is ignored here.
    let known: Vec<nuo_protocol::Effort> = effort_levels
        .iter()
        .filter_map(nuo_protocol::EffortLevel::as_known)
        .collect();
    let clamped = requested.clamp_to(&known);
    // Decide the surface from the ladder's deepest rung: the budget ladder tops
    // out at `max`, the level ladder at `high`. This keeps the model-specific
    // choice in one place and never inspects the free-form model id string.
    let is_budget = known.contains(&nuo_protocol::Effort::Max);
    Some(if is_budget {
        GoogleThinking::Budget(thinking_budget_wire(clamped, max_budget))
    } else {
        GoogleThinking::Level(clamped)
    })
}

/// Translate a clamped [`Effort`](nuo_protocol::Effort) into a Gemini 2.5
/// `thinkingBudget` integer — the form that surface accepts instead of an
/// enum. The model's ceiling is `max_budget` (`gemini-2.5-flash` tops out at
/// `24576`, `gemini-2.5-pro` at `32768`; see [`super::url::max_thinking_budget`]).
///
/// A chosen depth **pins** the budget: it is a deliberate request, never "let
/// the server decide". (Gemini's own dynamic value is `-1`, which is the
/// server default *when the field is omitted* — nuo reaches that by not
/// stamping the field at all, i.e. an unset channel effort, never by mapping a
/// rung onto `-1`.) `minimal` ≈ 10%, `low` ≈ 25%, `medium` ≈ 50% of
/// `max_budget`; `high` and everything deeper pin the model's full cap
/// (`xhigh` is not a native Gemini rung — the caller clamps it to `high` first
/// — and `max` differs from `high` only in intent: it names the cap
/// explicitly).
///
/// [`Effort::None`](nuo_protocol::Effort::None) maps to `0`, the only way to
/// turn thinking off — but Gemini 2.5 Pro rejects `0` (floor `128`), so the
/// caller must only honor an off rung where the model's floor is actually `0`.
///
/// This is provider-owned wire knowledge and lives here, beside
/// [`thinking_level_wire`], rather than on the framework's shared
/// [`Effort`](nuo_protocol::Effort)
/// vocabulary: the bucket fractions are one surface's encoding, not a property
/// of "reasoning depth" (ADR-0126).
pub const fn thinking_budget_wire(effort: nuo_protocol::Effort, max_budget: u32) -> i64 {
    use nuo_protocol::Effort;
    // `core::cmp::max` is not const-stable, so the `≥ 1` floor is spelled out.
    // It only matters for unrealistically small `max_budget` values, but keeps
    // the contract honest: a nonzero bucket never rounds down to "off".
    const fn floor_one(tokens: u64) -> i64 {
        if tokens < 1 { 1 } else { tokens as i64 }
    }
    match effort {
        Effort::None => 0,
        Effort::Minimal => floor_one(max_budget as u64 / 10),
        Effort::Low => floor_one(max_budget as u64 / 4),
        Effort::Medium => floor_one(max_budget as u64 / 2),
        Effort::High | Effort::Xhigh | Effort::Max | Effort::Ultra => max_budget as i64,
    }
}

/// The `thinkingLevel` values a Google Gemini 3.x surface accepts, shallowest
/// first. Live-verified against `docs/reference/openapi/antigravity.yml`
/// (`ThinkingConfig.thinkingLevel.enum`), whose capture also shows the shipped
/// `agy` sending `thinkingLevel: "LOW"` on its title lane.
pub(super) const THINKING_LEVELS: [&str; 3] = ["LOW", "MEDIUM", "HIGH"];

/// Translate a clamped [`Effort`](nuo_protocol::Effort) onto the
/// `thinkingLevel` string this surface accepts.
///
/// This is provider-owned wire knowledge (ADR-0074/ADR-0113), and
/// [`Effort::as_str`](nuo_protocol::Effort::as_str) is the wrong value here for
/// two independent reasons:
///
/// - **The enum is upper-case and case-sensitive.** `"high"` is not `HIGH`.
///   Google answers a mismatch with `400 INVALID_ARGUMENT` carrying only its
///   generic *"Request contains an invalid argument."* — no field name, no
///   `fieldViolations`, nothing a caller could attribute. That is the failure
///   this function exists to make unreachable.
/// - **`minimal` is not a member.** The Antigravity *catalog* advertises
///   `minimal`/`low`/`medium`/`high` as the display ladder for every Gemini
///   3.x row, but the field itself accepts exactly three values. `minimal`
///   therefore clamps up to the shallowest real rung instead of being sent
///   verbatim.
///
/// The deeper vocabulary rungs (`xhigh`/`max`/`ultra`) have no surface in this
/// enum and clamp down to `HIGH`; [`resolve_thinking`] has already clamped
/// against the model's declared ladder, so this is defense in depth.
pub fn thinking_level_wire(effort: nuo_protocol::Effort) -> &'static str {
    use nuo_protocol::Effort;
    let wire = match effort {
        // Below this enum's floor — the shallowest accepted rung stands in.
        Effort::None | Effort::Minimal | Effort::Low => THINKING_LEVELS[0],
        Effort::Medium => THINKING_LEVELS[1],
        // `High` and everything deeper than this enum's ceiling.
        Effort::High | Effort::Xhigh | Effort::Max | Effort::Ultra => THINKING_LEVELS[2],
    };
    debug_assert!(
        THINKING_LEVELS.contains(&wire),
        "thinkingLevel must be a member of the accepted enum, got {wire}"
    );
    wire
}
