//! Provider/model ranking types used by both picker surfaces.

use super::*;

/// One selectable row in the **flat model picker** (the `Models` dialog,
/// equivalent): a single (provider, model) pair drawn from anywhere
/// in the snapshot. Built by [`models_flat_filtered_from`]; the picker
/// browses, searches, and activates these directly — there is no drill-in
/// stage.
#[derive(Clone, Debug)]

pub struct RankedModel {
    /// The section this row belongs to (Favorites, Recent, or All).
    pub section: ModelSection,
    /// Canonical id of the provider serving this model (its snapshot row id).
    pub provider_id: String,
    /// Wire model id to activate — the identity, and the string that goes on
    /// the wire, appears in `hidden_models`/favorites/route settings, and is
    /// what the user types in config. It is the *fallback* label: a row leads
    /// with [`Self::name`] when the provider publishes one, and with this id
    /// when it does not. Either way the id stays searchable.
    pub model: String,
    /// The provider's own human-readable label for this model (`DeepSeek V4.1
    /// Flash` for the wire id `deepseek-flash`), when the catalog advertises
    /// one.
    ///
    /// Optional by construction and **never** the identity: it is the row's
    /// primary label when present (so the list reads in the provider's own
    /// vocabulary), while the wire id rides along beside it so the string the
    /// user must actually type stays learnable. `None` — the common case, since
    /// stock OpenAI-compatible and Gemini catalogues publish nothing — renders
    /// the bare id exactly as before.
    pub name: Option<String>,
    /// The provider's display name, rendered as the dim `| <provider>` suffix
    /// so identical model ids served by different instances stay
    /// distinguishable in the flat list.
    pub provider_label: String,
    /// Model-specific controls surfaced by the picker snapshot. OpenAI rows
    /// can expose effort; Anthropic rows can expose effort plus thinking.
    pub effort: Option<String>,
    pub thinking: Option<bool>,
    /// Reasoning effort tiers this model supports, in ascending order
    /// (mirrors `ProviderModelInfo.effort_levels`).
    pub effort_levels: Vec<String>,
    /// Whether this model is favorited (mirrors the snapshot's per-model
    /// `favorite` flag; ADR-0046). A starred daily-driver model sorts into
    /// the leading **Favorites** section of the flat list wherever it is
    /// served and shows a `★` glyph.
    pub favorite: bool,
    /// Unix epoch ms of this model's last activation (`None` = never used).
    /// A model with usage history sorts into the **Recent** section
    /// (most-recently-used first).
    pub last_used_ms: Option<u64>,
    /// Context window limit in tokens.
    pub context_window: usize,
    /// Whether this model may be run on this account right now.
    /// `false` renders the row greyed-out and refuses activation. Undeclared
    /// availability — the common case — is always usable.
    pub usable: bool,
    /// Short status label for the row's STATUS column (e.g. "disabled: tier", "disabled: 5h limit").
    pub status_label: Option<String>,
    /// Epoch timestamp in milliseconds when this unusable verdict expires and resets.
    pub resets_at_ms: Option<u64>,
    /// Effective vision support for this model route.
    pub vision: Option<bool>,
    /// The provider's own reason for an unusable verdict, verbatim. `None`
    /// means the provider declared none; the surface then says only that the
    /// model is unavailable rather than guessing a cause. Never parsed.
    pub reason: Option<String>,
    /// The provider declared the model unusable but the user's own scope
    /// overrode it: the row is usable and must disclose the contradiction
    /// (`[INV-AVAIL-05]`).
    pub availability_overridden: bool,
    /// The availability verdict was observed before a refresh that has since
    /// failed, so it may already be out of date upstream. The row says so
    /// rather than presenting a stale verdict as freshly confirmed.
    pub availability_stale: bool,
    /// Subsequence match against the wire model ID (`model`). The renderer
    /// highlights the ID column with these positions, and uses them for the
    /// leading label column only when the row has no separate name to draw.
    pub match_id: Option<fuzzy::FuzzyMatch>,
    /// Subsequence match against the human-readable model name (`name`), if
    /// present — the positions that highlight the leading label column.
    pub match_name: Option<fuzzy::FuzzyMatch>,
    /// Subsequence match against the provider connection label
    /// (`provider_label`).
    pub match_connection: Option<fuzzy::FuzzyMatch>,
    /// Composite fuzzy relevance score across matched tokens and cross-field bonus (`INV-MODEL-SEARCH-04`).
    /// Used to rank filtered results within each section (higher is better). 0 when query is empty.
    pub relevance_score: i64,
}

impl RankedModel {
    /// The row's list section.
    pub fn section(&self) -> ModelSection {
        self.section
    }

    /// Whether the provider published a name that differs from the wire id, so
    /// the row has a *distinct* identity to lead with.
    ///
    /// `Some(name)` equal to `model` is not distinct: the provider repeated the
    /// id, and drawing it twice would waste a column to say the same thing.
    pub fn has_distinct_name(&self) -> bool {
        self.name
            .as_deref()
            .is_some_and(|name| name != self.model.as_str())
    }

    /// The row's primary identity as drawn: the provider's own label when it
    /// published a distinct one, the wire id otherwise. Never empty
    /// ("name-first, id-fallback").
    ///
    /// Presentation only — the wire value every config surface keys on is still
    /// [`Self::model`].
    pub fn display_label(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| *name != self.model.as_str())
            .unwrap_or(self.model.as_str())
    }

    /// Formats the context window size as a compact string (e.g. "128k", "200k", "1M").
    pub fn format_context_window(&self) -> String {
        format_context_tokens(self.context_window)
    }

    /// Returns the short status label to display in the STATUS column if unusable.
    pub fn display_status(&self) -> Option<&str> {
        if self.usable {
            None
        } else {
            self.status_label.as_deref().or(Some("disabled"))
        }
    }
}

/// Formats raw token count into a clean, compact standard string (e.g. "128k", "200k", "1M").
pub fn format_context_tokens(tokens: usize) -> String {
    if tokens == 0 {
        String::new()
    } else if tokens >= 1_000_000 {
        let m = tokens as f64 / 1_000_000.0;
        if (m.round() - m).abs() < 0.05 {
            format!("{:.0}M", m.round())
        } else {
            format!("{:.1}M", m)
        }
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// The three sections of the flat Models picker list, in display order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ModelSection {
    /// ★-favorited models (ADR-0046) — pinned user intent leads the list.
    Favorites,
    /// Models with usage history, most recently used first.
    Recent,
    /// Every remaining (provider, model) pair, ASCII by model id.
    All,
}

impl ModelSection {
    /// The section's label row: `FAVORITES`, `RECENT`, `ALL MODELS`. Rendered
    /// as a dim uppercase tag (the same section-tag voice as the chrome
    /// labels, e.g. the todo bar's `TODOS`).
    pub fn label(self) -> &'static str {
        match self {
            ModelSection::Favorites => "FAVORITES",
            ModelSection::Recent => "RECENT",
            ModelSection::All => "ALL MODELS",
        }
    }
}

/// One selectable row in the **Connections** provider list. Carries everything
/// the renderer and input handler need (copied out of the snapshot row), so
/// neither re-indexes the snapshot.
pub struct RankedProvider {
    /// Canonical provider id.
    pub id: String,
    /// Display name (the fuzzy target; mirrors `label`).
    pub name: String,
    /// Active model wire id.
    pub model: String,
    /// Every model id this provider serves.
    pub models: Vec<String>,
    /// `true` for curated providers, `false` for user-defined connections. Drives
    /// the built-in/custom grouping and whether `e` opens the full meta editor.
    pub builtin: bool,
    /// The model provider this connection points at (`"openai"`, …),
    /// surfaced so the Connections list can show the provider *type* beside
    /// the connection name.
    pub provider: String,
    /// The rendered label — the provider's display name (the instance name).
    pub label: String,
    /// The fuzzy match against `label`, or `None` in browse mode (empty query).
    pub m: Option<fuzzy::FuzzyMatch>,
}

impl RankedProvider {
    /// Whether the provider hosts more than one model. Informational for the
    /// Connections list (the flat Models picker lists each pair individually,
    /// so no drill-in remains).
    #[cfg(test)]
    pub fn is_multi_model(&self) -> bool {
        self.models.len() > 1
    }
}

/// The last-used-desc → name ordering of the Connections provider list. Pulls
/// each provider's recency signal from its snapshot row. (Favorite is
/// model-level now — ADR-0046 — so the Connections list no longer sorts by it.)
pub(super) fn provider_order(
    picker: &ProviderPickerSnapshot,
    a_id: &str,
    b_id: &str,
    a_name: &str,
    b_name: &str,
) -> std::cmp::Ordering {
    let used = |id: &str| {
        picker
            .rows
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.last_used_ms)
    };
    let a_used = used(a_id);
    let b_used = used(b_id);
    b_used.cmp(&a_used).then_with(|| a_name.cmp(b_name))
}
