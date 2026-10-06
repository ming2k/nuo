//! Reasoning **depth** — "how hard should the model think before answering?"
//!
//! nuo models every provider's reasoning-depth control as a single
//! provider-independent abstraction: the [`Effort`] enum. This keeps two
//! concerns separate that are easy to conflate, and each lives on its own
//! layer:
//!
//! # Layer A — the abstraction (this module): `Effort` → public API specs
//!
//! [`Effort`] is the **only** depth concept in the codebase. The protocol layer
//! in `nuo-llm-client` translates a chosen [`Effort`] onto each **public API
//! specification** a provider speaks — not onto "a brand", but onto the wire
//! shape the spec defines:
//!
//! | API specification | wire field | form | `Effort` → wire |
//! |-------------------|-----------|------|-----------------|
//! | OpenAI Responses | `reasoning.effort` | enum string | `effort.as_str()` |
//! | OpenAI chat completions | `reasoning_effort` | enum string | `effort.as_str()` |
//! | Anthropic Messages | `output_config.effort` | enum string | `effort.as_str()` |
//! | Google generateContent | `thinkingConfig.thinkingLevel` / `.thinkingBudget` | enum string / int tokens | level / a derived bucket |
//!
//! xAI (Grok), Moonshot (Kimi), DeepSeek and Z.AI (GLM) ride these specs too —
//! they implement the OpenAI Responses / chat-completions specification, so
//! they reuse the OpenAI translation verbatim. Google is the outlier that does
//! not use the word "effort" or the standard ladder, yet it is abstracted here
//! all the same: a Gemini model declares an [`Effort`] ladder like any other,
//! and the Google protocol maps each rung onto `thinkingLevel` (3.x) or a
//! `thinkingBudget` bucket (2.5). No caller outside this module ever sees a
//! provider-specific depth shape — they see [`Effort`].
//!
//! [`Effort`] controls **depth only** and is orthogonal to the reasoning on/off
//! switch ([`crate::reasoning::ReasoningMode`]); see [`crate::reasoning`].
//!
//! # Layer B — model capability ladders and discovery
//!
//! [`Effort`] is the *vocabulary*; a model still needs to know *which rungs it
//! accepts*. That per-model ladder is a **capability**, and — like every other
//! capability (context window, reasoning, vision) — it resolves through one
//! precedence chain (ADR-0065, ADR-0270):
//!
//! ```text
//! live discovery (a preset whose RemoteCatalogSource carries effort tiers)
//!        ↓  only Kimi & Copilot advertise tiers here
//! static baseline  ←  model capability ladders in `nuo_provider::effort_ladders`
//!        ↓  the compiled-in fallback when upstream advertises nothing
//! COMMON_LADDER / &[]  (generic conservative fallback / non-reasoning model)
//! ```
//!
//! Specific model family capability ladders (`CLAUDE_*`, `OPENAI_GPT_*`, `GLM_*`, etc.)
//! are housed in the provider registry (`nuo_provider::effort_ladders`).
//! This module defines only the universal abstract vocabulary and the vendor-neutral
//! conservative fallback [`COMMON_LADDER`].
//!
//! **The vocabulary is open, not closed.** The rungs are the words
//! providers use, not a ceiling: a provider may advertise a tier the vocabulary
//! does not name. [`EffortLevel`] is the open companion type — `Known(Effort)`
//! or `Other(String)` — carried on the runtime view
//! ([`crate::model::ModelCapabilities`]) so a live-advertised tier is preserved
//! and stamped through verbatim rather than dropped. [`Effort`] itself stays
//! `Copy` and closed (the static registry depends on that); openness lives only
//! where live discovery lands. See [`EffortLevel`] below.
//!
//! This module is the implementation; the prose reference for users and
//! contributors lives in `docs/reference/effort.md`.

/// How much reasoning effort a model should spend before answering.
///
/// A model accepts only a subset of these levels (its
/// [`crate::model::Model::effort_levels`]); callers must clamp a requested
/// level down to what the model supports rather than sending an unsupported
/// value (which the upstream rejects with 400).
///
/// Ordered ascending by depth:
/// `None < Minimal < Low < Medium < High < Xhigh < Max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Disable reasoning when the provider supports an explicit off value.
    None,
    /// Minimal reasoning. Some providers expose this as a distinct tier below
    /// `low`.
    Minimal,
    /// Minimal reasoning; simple tasks may skip thinking entirely. Fastest and
    /// cheapest. Useful for sub-agents and trivial classification.
    Low,
    /// Moderate reasoning. A middle ground that may omit thinking on simple
    /// queries.
    Medium,
    /// The default depth — deep reasoning on all but the most trivial tasks.
    /// Equivalent to omitting `effort` entirely.
    High,
    /// Deeper-than-high reasoning with extended exploration. Only the Fable /
    /// Opus-4.7+ tier supports it; the best setting for most coding and
    /// agentic work on those models.
    Xhigh,
    /// Maximum reasoning with no depth cap. Correctness over cost; use when a
    /// wrong answer is expensive.
    Max,
    /// Ultra-deep reasoning with automatic task delegation (OpenAI GPT-6 / Astra).
    Ultra,
}

impl Effort {
    /// All levels in ascending order of depth.
    pub const ORDER: [Effort; 8] = [
        Effort::None,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
        Effort::Ultra,
    ];

    /// The wire string sent in the provider's effort field
    /// (`output_config.effort` for Anthropic).
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
            Effort::Ultra => "ultra",
        }
    }

    /// The level's position in [`Effort::ORDER`], for comparison/clamping.
    fn rank(self) -> usize {
        Self::ORDER.iter().position(|e| *e == self).unwrap_or(2)
    }

    /// A short, human-facing description of the tier, shown next to the
    /// segmented effort selector in the model-settings editor so each rung of
    /// the ladder reads as a meaningful choice rather than a bare label. Keep
    /// each to one line — the picker renders it as a caption under the row.
    pub const fn description(self) -> &'static str {
        match self {
            Effort::None => "reasoning off — direct answers only",
            Effort::Minimal => "barely thinks — fastest, simplest tasks",
            Effort::Low => "light reasoning — quick, cheap, simple work",
            Effort::Medium => "balanced — moderate reasoning depth",
            Effort::High => "deep reasoning — the default for real work",
            Effort::Xhigh => "very deep — extended exploration for hard problems",
            Effort::Max => "maximum depth — no cap; correctness over cost",
            Effort::Ultra => "ultra depth — maximum reasoning with delegation",
        }
    }

    /// Parse a lowercase effort string (`"none"`/`"minimal"`/`"low"`/
    /// `"medium"`/`"high"`/`"xhigh"`/`"max"`/`"ultra"`) into the typed [`Effort`].
    /// Returns `None` for
    /// anything else so an unrecognized config value is silently ignored
    /// rather than treated as an error — the caller keeps its default.
    pub fn parse(s: &str) -> Option<Effort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Effort::None),
            "minimal" => Some(Effort::Minimal),
            "low" => Some(Effort::Low),
            "medium" => Some(Effort::Medium),
            "high" => Some(Effort::High),
            "xhigh" => Some(Effort::Xhigh),
            "max" => Some(Effort::Max),
            "ultra" => Some(Effort::Ultra),
            _ => None,
        }
    }

    /// Clamp `self` down to the highest allowed level ≤ `self` (so a requested
    /// `xhigh` on a model that tops out at `high` becomes `high`, never an
    /// unsupported value). When nothing allowed ranks ≤ the request, snap **up**
    /// to the ladder's shallowest tier — the ladder is authoritative, so
    /// emitting an unsupported `high` would earn a 400 (Kimi K3's
    /// `low`/`high`/`max` ladder clamps a legacy `medium` override up to
    /// `low`).
    pub fn clamp_to(self, allowed: &[Effort]) -> Effort {
        let req = self.rank();
        allowed
            .iter()
            .copied()
            .filter(|e| e.rank() <= req)
            .max_by_key(|e| e.rank())
            .unwrap_or_else(|| {
                allowed
                    .iter()
                    .copied()
                    .min_by_key(|e| e.rank())
                    .unwrap_or(Effort::High)
            })
    }

    /// The channel-level default effort tier for a model that advertises an
    /// effort ladder: GPT families default to [`Effort::Medium`] (their wire
    /// middle tier); every other family defaults to [`Effort::High`] clamped
    /// to the ladder — a ladder topping out below `high` resolves to its
    /// deepest tier, and a ladder omitting both `high` and `medium` snaps up
    /// to the nearest supported rung. Never emits a tier the ladder does not
    /// contain; a model with an empty ladder has no default (`None`), and the
    /// request must not stamp an effort field at all.
    ///
    /// Single source of truth for **both** the picker's display fallback and
    /// the request-boundary default in the provider factory. Before it was
    /// shared, the picker showed `high` while the wire sent nothing (the raw
    /// channel override was `None`), so an always-thinking endpoint gated
    /// only by `reasoning_effort` (Zhipu GLM-5.x) fell back to its
    /// server-side default — far deeper than the tier the UI promised.
    pub fn channel_default(family: &str, effort_levels: &[Effort]) -> Option<Effort> {
        if effort_levels.is_empty() {
            return None;
        }
        let preferred =
            if family == "gpt" || family == "openai-subscription" || family.starts_with("gpt") {
                Effort::Medium
            } else {
                Effort::High
            };
        Some(preferred.clamp_to(effort_levels))
    }

    /// Resolve this known requested effort against a channel's **open** effort
    /// ladder ([`EffortLevel`], which may carry provider-advertised tiers the
    /// vocabulary does not name). Returns the wire string to stamp.
    ///
    /// Known rungs clamp by rank exactly as [`clamp_to`](Self::clamp_to) does.
    /// [`EffortLevel::Other`] rungs cannot be ranked, so they participate only
    /// by **exact name match**: if the request's wire string equals an `Other`
    /// rung, it passes through verbatim (the provider named it, so the provider
    /// honors it); otherwise `Other` rungs are invisible to the ranking. When
    /// no ranked rung fits, the ladder's shallowest **known** rung is used
    /// (`Other` is never a fallback default — its depth is unknowable).
    ///
    /// This keeps the flexibility honest: a tier outside the vocabulary reaches
    /// the wire when the request names it, but never pretends to a rank it
    /// cannot have. A request for an `Other` tier is expressed via
    /// [`EffortLevel`] directly, not through a known [`Effort`].
    pub fn clamp_to_levels(self, allowed: &[EffortLevel]) -> EffortLevel {
        // Exact-name passthrough: a request whose wire string an `Other` rung
        // matches wins verbatim. (The request is a known Effort, so this only
        // fires when an `Other` rung happens to reuse a known name — rare, but
        // keeps the contract total.)
        let req_str = self.as_str();
        for level in allowed {
            if let EffortLevel::Other(s) = level
                && s == req_str
            {
                return EffortLevel::Other(s.clone());
            }
        }
        // Ranked clamp over the known rungs only.
        let req = self.rank();
        let known: Vec<Effort> = allowed.iter().filter_map(EffortLevel::as_known).collect();
        let clamped = known
            .iter()
            .copied()
            .filter(|e| e.rank() <= req)
            .max_by_key(|e| e.rank())
            .unwrap_or_else(|| {
                known
                    .iter()
                    .copied()
                    .min_by_key(|e| e.rank())
                    .unwrap_or(Effort::High)
            });
        EffortLevel::Known(clamped)
    }

    /// Translate this depth into a Google Gemini 2.5 `thinkingBudget` integer,
    /// the form Gemini 2.5 (Flash/Pro) accepts instead of an enum. Gemini 2.5
    /// takes a token budget in a model-specific range (`max_budget`):
    /// `gemini-2.5-flash` tops out at `24576`; `gemini-2.5-pro` at `32768`.
    ///
    /// A chosen [`Effort`] **pins** the budget — it is a deliberate request,
    /// never "let the server decide". (Gemini's own "dynamic" is `-1`, the
    /// server default when the field is *omitted*; nuo reaches that by not
    /// stamping the field at all — an unset channel effort — not by mapping any
    /// rung to `-1`.) `minimal` ~10%,
    /// `low` ~25%, `medium` ~50% of `max_budget`; `high`/`xhigh`/`max` all pin
    /// to the model's full cap (`xhigh` is not a native Gemini rung — the
    /// protocol layer clamps it down to `high` first — and `max` differs from
    /// `high` only in intent: it explicitly names the cap).
    ///
    /// `None` maps to `0`, the only way to turn thinking off — but Gemini 2.5
    /// Pro rejects `0` (its floor is `128`), so callers must only honor
    /// [`Effort::None`] when the model actually supports an off budget.
    pub const fn gemini_thinking_budget(self, max_budget: u32) -> i64 {
        match self {
            // 0 = off. Only honored by models whose floor is 0 (Flash/Lite);
            // Pro rejects it (floor 128), so the protocol layer must skip
            // stamping None there.
            Effort::None => 0,
            // The floor of 1 guarantees a nonzero bucket even on a tiny max
            // budget; `core::cmp::max` is not const-stable yet, so the helper
            // spells the clamp out. `max(1)` matters only for unrealistically
            // small `max_budget` values, but keeps the contract honest.
            Effort::Minimal => nonzero(max_budget as u64 / 10),
            Effort::Low => nonzero(max_budget as u64 / 4),
            Effort::Medium => nonzero(max_budget as u64 / 2),
            // A deliberate request pins the budget to the model's full cap —
            // never dynamic (`-1`). `xhigh` is not native to Gemini; the
            // protocol layer clamps it to `high` before reaching here, and it
            // resolves to the same cap regardless.
            Effort::High | Effort::Xhigh | Effort::Max | Effort::Ultra => max_budget as i64,
        }
    }
}

/// A reasoning-depth level **as a channel knows it** — either a known rung of
/// the [`Effort`] vocabulary or an opaque wire string a provider advertises that
/// nuo has no name for yet.
///
/// This is the **open** companion to the closed [`Effort`] enum. [`Effort`] is
/// the ordered vocabulary clamp/UI/config key off of; it must stay small and
/// `Copy` (the static `Model` registry is `Copy`). But a provider's live
/// `/models` may advertise a tier the vocabulary does not name (e.g. a future
/// `"turbo"`), and [ADR-0065] makes that advertisement authoritative. Dropping
/// it would silently downgrade a live capability — so the runtime, per-channel
/// view ([`crate::model::ModelCapabilities`] / [`crate::model::RemoteModelMetadata`])
/// carries [`EffortLevel`] to preserve unknown tiers verbatim and stamp them
/// through to the wire.
///
/// ### Where each type lives
///
/// | Type | Lifetime | Carries unknowns? |
/// |------|----------|-------------------|
/// | `Effort` (`&'static [Effort]`) | static registry (`Model`, `Copy`) | **no** — vetted compile-time vocabulary |
/// | `EffortLevel` (`Vec<EffortLevel>`) | runtime view (`ModelCapabilities`, `Clone`) | **yes** — live-advertised tiers preserved |
///
/// ### Ordering
///
/// [`Effort::clamp_to`] orders by the known ladder; an [`EffortLevel::Other`]
/// has **no rank**. When a request resolves to `Other`, the clamp cannot
/// compare it and passes it through verbatim (the provider named it, so the
/// provider honors it); the request path logs the unranked passthrough rather
/// than silently snapping. A `Known` level clamps against `Known` rungs as
/// before. This keeps the flexibility honest: a tier outside the vocabulary
/// reaches the wire, but never pretends to a depth it cannot be ranked at.
///
/// [ADR-0065]: ../adr/0065-runtime-fitted-model-capability-overlay.md
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum EffortLevel {
    /// A rung of the known [`Effort`] vocabulary. Serializes as its wire
    /// string (`"high"`), so existing persisted TOML round-trips unchanged.
    Known(Effort),
    /// A provider-advertised wire string the vocabulary does not name.
    /// Serialized as the raw string and stamped through to the wire verbatim.
    Other(String),
}

impl EffortLevel {
    /// The wire string to stamp onto the request (`"high"`, or the opaque
    /// provider string for [`Other`](Self::Other)).
    pub fn as_str(&self) -> &str {
        match self {
            EffortLevel::Known(e) => e.as_str(),
            EffortLevel::Other(s) => s,
        }
    }

    /// The known rung, when this is one. `None` for [`Other`](Self::Other).
    pub fn as_known(&self) -> Option<Effort> {
        match self {
            EffortLevel::Known(e) => Some(*e),
            EffortLevel::Other(_) => None,
        }
    }

    /// `true` when this is a known rung of the vocabulary.
    pub const fn is_known(&self) -> bool {
        matches!(self, EffortLevel::Known(_))
    }

    /// Parse a wire string into a level: a known rung when the vocabulary
    /// names it, else [`Other`](Self::Other) carrying the raw string. This is
    /// the **non-dropping** parse — unlike [`Effort::parse`], it never returns
    /// `None`, so a provider-advertised tier is always preserved.
    pub fn parse(s: &str) -> EffortLevel {
        match Effort::parse(s) {
            Some(e) => EffortLevel::Known(e),
            None => EffortLevel::Other(s.trim().to_string()),
        }
    }
}

impl From<Effort> for EffortLevel {
    fn from(e: Effort) -> EffortLevel {
        EffortLevel::Known(e)
    }
}

/// `≥ 1` floor for a Gemini budget bucket. `core::cmp::max` is not yet
/// const-stable, so [`Effort::gemini_thinking_budget`] spells the clamp through
/// this helper. Kept private: only the budget translation needs it.
const fn nonzero(tokens: u64) -> i64 {
    if tokens < 1 { 1 } else { tokens as i64 }
}

// Baseline value-sets (Layer B).
//
// Each const is the **seed ladder** for the model family named in its doc.
// The precedence is live discovery → these baselines → `&[]` (see the module
// doc). The first doc line of each const states whether upstream advertises
// tiers (so the baseline is just a pre-fetch seed) or advertises nothing (so
// the baseline *is* the effective ladder, sourced from prose docs).

/// Universal conservative fallback ladder: `low`/`medium`/`high` (ADR-0270).
///
/// Safe default subset for any model whose deeper tiers (`xhigh`/`max`) are
/// unknown. Concrete vendor/model family capability ladders are maintained in
/// `nuo_provider::effort_ladders`.
pub const COMMON_LADDER: &[Effort] = &[Effort::Low, Effort::Medium, Effort::High];

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FULL: &[Effort] = &[
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::Xhigh,
        Effort::Max,
    ];
    const TEST_GAPPED: &[Effort] = &[Effort::Low, Effort::High, Effort::Max];
    const TEST_LEVEL: &[Effort] = &[Effort::Minimal, Effort::Low, Effort::Medium, Effort::High];
    const TEST_GLM: &[Effort] = &[Effort::Low, Effort::High, Effort::Xhigh, Effort::Max];

    #[test]
    fn parse_round_trips() {
        for e in Effort::ORDER {
            assert_eq!(Effort::parse(e.as_str()), Some(e));
        }
        assert_eq!(Effort::parse("nonsense"), None);
        assert_eq!(Effort::parse("  HIGH "), Some(Effort::High));
    }

    #[test]
    fn every_tier_has_a_nonempty_description() {
        for e in Effort::ORDER {
            assert!(
                !e.description().is_empty(),
                "{:?} is missing a picker caption",
                e
            );
        }
        // None must read as the explicit off state, distinct from the tiers.
        assert!(Effort::None.description().contains("off"));
    }

    #[test]
    fn clamp_downgrades_unsupported_tier() {
        // xhigh on a model that tops out at high → high.
        assert_eq!(Effort::Xhigh.clamp_to(COMMON_LADDER), Effort::High);
        // max on a full-tier model stays max.
        assert_eq!(Effort::Max.clamp_to(TEST_FULL), Effort::Max);
        // low is honored everywhere.
        assert_eq!(Effort::Low.clamp_to(COMMON_LADDER), Effort::Low);
    }

    #[test]
    fn clamp_snaps_up_to_shallowest_supported_tier() {
        // A ladder that skips `medium`: a `medium` override snaps up to `low`.
        assert_eq!(Effort::Medium.clamp_to(TEST_GAPPED), Effort::Low);
        // high is on the ladder and stays; max is honored too.
        assert_eq!(Effort::High.clamp_to(TEST_GAPPED), Effort::High);
        assert_eq!(Effort::Max.clamp_to(TEST_GAPPED), Effort::Max);
        // An empty ladder keeps the historical wire-default fallback.
        assert_eq!(Effort::Low.clamp_to(&[]), Effort::High);
    }

    #[test]
    fn level_ladder_clamps_deep_rungs_down() {
        // Level ladder tops out at `high`; xhigh/max clamp down, never escape.
        assert_eq!(Effort::Max.clamp_to(TEST_LEVEL), Effort::High);
        assert_eq!(Effort::Xhigh.clamp_to(TEST_LEVEL), Effort::High);
        // minimal is the floor and is honored.
        assert_eq!(Effort::Minimal.clamp_to(TEST_LEVEL), Effort::Minimal);
    }

    #[test]
    fn gapped_ladders_clamp() {
        assert_eq!(Effort::Medium.clamp_to(TEST_GAPPED), Effort::Low);
        assert_eq!(Effort::Xhigh.clamp_to(TEST_GAPPED), Effort::High);
        assert_eq!(Effort::Xhigh.clamp_to(TEST_GLM), Effort::Xhigh);
        assert_eq!(Effort::Medium.clamp_to(TEST_GLM), Effort::Low);
    }

    #[test]
    fn gemini_thinking_budget_buckets_within_range() {
        // Gemini 2.5 Flash: 0–24576. Each rung is a fraction of the cap.
        assert_eq!(Effort::None.gemini_thinking_budget(24576), 0);
        assert_eq!(Effort::Minimal.gemini_thinking_budget(24576), 2457);
        assert_eq!(Effort::Low.gemini_thinking_budget(24576), 6144);
        assert_eq!(Effort::Medium.gemini_thinking_budget(24576), 12288);
        assert_eq!(Effort::High.gemini_thinking_budget(24576), 24576);
        assert_eq!(Effort::Max.gemini_thinking_budget(24576), 24576);
        // Gemini 2.5 Pro: 128–32768. Buckets scale by the larger cap.
        assert_eq!(Effort::Medium.gemini_thinking_budget(32768), 16384);
        assert_eq!(Effort::High.gemini_thinking_budget(32768), 32768);
        // A floor of 1 is guaranteed even on a tiny max budget.
        assert_eq!(Effort::Minimal.gemini_thinking_budget(5), 1);
    }

    #[test]
    fn effort_level_parse_never_drops() {
        // Known rungs parse to Known.
        assert_eq!(EffortLevel::parse("high"), EffortLevel::Known(Effort::High));
        assert_eq!(
            EffortLevel::parse("  MAX "),
            EffortLevel::Known(Effort::Max)
        );
        // An unknown provider tier is preserved verbatim as Other — the whole
        // point: a live-advertised tier outside the vocabulary is not lost.
        assert_eq!(
            EffortLevel::parse("turbo"),
            EffortLevel::Other("turbo".to_string())
        );
        assert_eq!(
            EffortLevel::parse("draft"),
            EffortLevel::Other("draft".to_string())
        );
    }

    #[test]
    fn effort_level_round_trips_through_serde() {
        // Known serializes as its wire string (back-compat with persisted TOML).
        let high = EffortLevel::Known(Effort::High);
        assert_eq!(serde_json::to_string(&high).unwrap(), "\"high\"");
        assert_eq!(
            serde_json::from_str::<EffortLevel>("\"high\"").unwrap(),
            EffortLevel::Known(Effort::High)
        );
        // Other serializes as the raw string and round-trips.
        let turbo = EffortLevel::Other("turbo".to_string());
        assert_eq!(serde_json::to_string(&turbo).unwrap(), "\"turbo\"");
        assert_eq!(
            serde_json::from_str::<EffortLevel>("\"turbo\"").unwrap(),
            EffortLevel::Other("turbo".to_string())
        );
    }

    #[test]
    fn channel_default_matches_the_picker_rule() {
        // GLM-5.x: ladder low/high/xhigh/max → high.
        assert_eq!(
            Effort::channel_default(
                "glm",
                &[Effort::Low, Effort::High, Effort::Xhigh, Effort::Max]
            ),
            Some(Effort::High)
        );
        // GPT families default to their wire middle tier.
        assert_eq!(
            Effort::channel_default("gpt", &[Effort::Low, Effort::Medium, Effort::High]),
            Some(Effort::Medium)
        );
        assert_eq!(
            Effort::channel_default(
                "openai-subscription",
                &[
                    Effort::Low,
                    Effort::Medium,
                    Effort::High,
                    Effort::Xhigh,
                    Effort::Max,
                    Effort::Ultra
                ]
            ),
            Some(Effort::Medium)
        );
        // A ladder without high/medium snaps up to its shallowest tier.
        assert_eq!(
            Effort::channel_default("kimi", &[Effort::Low, Effort::Max]),
            Some(Effort::Low)
        );
        // A ladder capped below high resolves to its deepest tier.
        assert_eq!(
            Effort::channel_default("llama", &[Effort::Minimal, Effort::Low]),
            Some(Effort::Low)
        );
        // No ladder → no default: the request omits the effort field.
        assert_eq!(Effort::channel_default("glm", &[]), None);
    }

    #[test]
    fn clamp_to_levels_ranks_known_and_passes_through_other() {
        // Known request against a ladder with an Other rung: Other is invisible
        // to ranking; the request clamps among known rungs.
        let ladder = vec![
            EffortLevel::Known(Effort::Low),
            EffortLevel::Other("turbo".to_string()),
            EffortLevel::Known(Effort::High),
        ];
        // xhigh (above the known max of high) clamps down to high; turbo is not
        // a ranking target.
        assert_eq!(
            Effort::Xhigh.clamp_to_levels(&ladder),
            EffortLevel::Known(Effort::High)
        );
        // A request shallower than the floor snaps up to the shallowest known.
        assert_eq!(
            Effort::Minimal.clamp_to_levels(&ladder),
            EffortLevel::Known(Effort::Low)
        );
    }

    #[test]
    fn clamp_to_levels_exact_name_match_passes_through() {
        // If an Other rung reuses a known request's wire string, it passes
        // through verbatim (provider named it, provider honors it).
        let ladder = vec![EffortLevel::Other("high".to_string())];
        assert_eq!(
            Effort::High.clamp_to_levels(&ladder),
            EffortLevel::Other("high".to_string())
        );
    }

    #[test]
    fn clamp_to_levels_other_is_never_a_default() {
        // When no ranked rung fits, the fallback is the shallowest KNOWN rung —
        // never an Other tier, whose depth is unknowable.
        let ladder = vec![
            EffortLevel::Other("turbo".to_string()),
            EffortLevel::Known(Effort::High),
        ];
        assert_eq!(
            Effort::Minimal.clamp_to_levels(&ladder),
            EffortLevel::Known(Effort::High)
        );
    }
}
