//! Provider and model filtering, fuzzy ranking, and quota gating.

use super::ranking::{ModelSection, RankedModel, RankedProvider, provider_order};
use super::*;

/// Build the **Connections** provider rows: one per snapshot row,
/// fuzzy-filtered by `query` against the provider (instance) name and sorted
/// last-used → name. An empty `query` (browse mode) keeps every provider with
/// no match positions.
pub fn providers_filtered_from(
    picker: &ProviderPickerSnapshot,
    query: &str,
) -> Vec<RankedProvider> {
    let mut rows: Vec<RankedProvider> = Vec::new();
    for prow in picker.rows.iter() {
        let label = prow.name.clone();
        let m = if query.is_empty() {
            None
        } else {
            match fuzzy::fuzzy_match(&label, query) {
                Some(m) => Some(m),
                None => continue,
            }
        };
        rows.push(RankedProvider {
            id: prow.id.clone(),
            name: prow.name.clone(),
            model: prow.model.clone(),
            models: prow.models.clone(),
            builtin: prow.builtin,
            provider: prow.provider.clone(),
            label,
            m,
        });
    }
    rows.sort_by(|a, b| provider_order(picker, &a.id, &b.id, &a.name, &b.name));
    rows
}

/// Canonical cross-field relevance bonus awarded when a candidate model row matches
/// across both the connection label and the model identifier/name dimensions (`INV-MODEL-SEARCH-04`).
pub(crate) const BONUS_CROSS_FIELD: i64 = 1000;

/// Accumulates matched character positions across multiple query tokens
/// for a single candidate field, producing a deduplicated, sorted [`fuzzy::FuzzyMatch`] (`INV-MODEL-SEARCH-03`).
#[derive(Debug, Clone, Default)]
struct TokenAccumulator {
    positions: Vec<usize>,
}

impl TokenAccumulator {
    #[inline]
    fn add(&mut self, m: &fuzzy::FuzzyMatch) {
        self.positions.extend_from_slice(&m.positions);
    }

    #[inline]
    fn finish(mut self, score: i64) -> Option<fuzzy::FuzzyMatch> {
        if self.positions.is_empty() {
            None
        } else {
            self.positions.sort_unstable();
            self.positions.dedup();
            Some(fuzzy::FuzzyMatch {
                score,
                positions: self.positions,
            })
        }
    }
}

/// Build the **flat Models** rows: [`RankedModel`] rows sectioned into three labeled groups:
///
/// 1. **Favorites** — ★-marked models (ADR-0046);
/// 2. **Recent** — models with usage history (recency-descending);
/// 3. **All models** — ALL available models across ready providers (including favorites and recent models).
///
/// Supports order-independent multi-token cross-field hybrid search (ADR-0096):
/// - The query is decomposed by whitespace and path/tag separators (`/`, `:`, `@`, `|`) into discrete tokens.
/// - Every token must fuzzy-match at least one field (`connection_label`, `model_id`, or `model_name`)
///   for the candidate to be kept (`INV-MODEL-SEARCH-01`).
/// - Order is commutative: `azure gpt-4` and `gpt-4 azure` produce identical candidate sets (`INV-MODEL-SEARCH-02`).
/// - Highlight positions are aggregated per column without cross-field bleeding (`INV-MODEL-SEARCH-03`).
/// - Candidates spanning both connection and model receive a cross-field bonus (`INV-MODEL-SEARCH-04`)
///   and rank at the top of their respective section.
pub fn models_flat_filtered_from(
    picker: &ProviderPickerSnapshot,
    current_provider: &str,
    current_model: &str,
    query: &str,
) -> Vec<RankedModel> {
    models_flat_filtered_from_with_quotas(picker, current_provider, current_model, query, None)
}

pub fn models_flat_filtered_from_with_quotas(
    picker: &ProviderPickerSnapshot,
    current_provider: &str,
    current_model: &str,
    query: &str,
    quotas: Option<&nuo_protocol::ProviderQuotaSnapshot>,
) -> Vec<RankedModel> {
    let _ = (current_provider, current_model);
    let tokens: Vec<&str> = query
        .split(|c: char| c.is_whitespace() || c == '/' || c == ':' || c == '@' || c == '|')
        .filter(|s| !s.is_empty())
        .collect();
    let has_query = !tokens.is_empty();

    let mut candidates: Vec<RankedModel> = Vec::new();
    for prow in &picker.rows {
        // Daily-driver model picker only shows models from ready/authenticated connections.
        if !prow.key_ready {
            continue;
        }

        // Hoist provider-name token evaluation across all served models (`INV-MODEL-SEARCH-01`)
        let prow_token_matches: Vec<Option<fuzzy::FuzzyMatch>> = if has_query {
            tokens
                .iter()
                .map(|t| fuzzy::fuzzy_match(&prow.name, t))
                .collect()
        } else {
            Vec::new()
        };

        for model in &prow.models {
            let info = prow.model_info.iter().find(|info| info.model == *model);

            // Cross-field multi-token matching: connection label, model id, and model display name.
            let (match_id, match_name, match_connection, relevance_score) = if !has_query {
                (None, None, None, 0)
            } else {
                let mut conn_acc = TokenAccumulator::default();
                let mut id_acc = TokenAccumulator::default();
                let mut name_acc = TokenAccumulator::default();

                let mut matched_conn = false;
                let mut matched_model = false;
                let mut all_tokens_matched = true;
                let mut total_score: i64 = 0;
                let mut conn_score: i64 = 0;
                let mut id_score: i64 = 0;
                let mut name_score: i64 = 0;

                for (idx, token) in tokens.iter().enumerate() {
                    let conn_m = &prow_token_matches[idx];
                    let id_m = fuzzy::fuzzy_match(model, token);
                    let name_m = info
                        .and_then(|i| i.name.as_deref())
                        .and_then(|name| fuzzy::fuzzy_match(name, token));

                    if conn_m.is_none() && id_m.is_none() && name_m.is_none() {
                        all_tokens_matched = false;
                        break;
                    }

                    let token_conn_score = conn_m.as_ref().map(|m| m.score).unwrap_or(0);
                    let token_id_score = id_m.as_ref().map(|m| m.score).unwrap_or(0);
                    let token_name_score = name_m.as_ref().map(|m| m.score).unwrap_or(0);
                    // Match quality against the model dimension takes the best representation
                    // (wire id vs display name) rather than artificially doubling redundant names.
                    let token_model_score = token_id_score.max(token_name_score);

                    total_score += token_conn_score + token_model_score;
                    conn_score += token_conn_score;
                    id_score += token_id_score;
                    name_score += token_name_score;

                    if let Some(m) = conn_m {
                        matched_conn = true;
                        conn_acc.add(m);
                    }
                    if let Some(m) = &id_m {
                        matched_model = true;
                        id_acc.add(m);
                    }
                    if let Some(m) = &name_m {
                        matched_model = true;
                        name_acc.add(m);
                    }
                }

                if !all_tokens_matched {
                    continue;
                }

                let final_conn = conn_acc.finish(conn_score);
                let final_id = id_acc.finish(id_score);
                let final_name = name_acc.finish(name_score);

                let cross_field_bonus = if matched_conn && matched_model {
                    BONUS_CROSS_FIELD
                } else {
                    0
                };

                let score = total_score + cross_field_bonus;
                (final_id, final_name, final_conn, score)
            };

            candidates.push(RankedModel {
                section: ModelSection::All,
                provider_id: prow.id.clone(),
                model: model.clone(),
                name: info.and_then(|i| i.name.clone()),
                provider_label: prow.name.clone(),
                effort: info.and_then(|i| i.effort.clone()),
                thinking: info.and_then(|i| i.thinking),
                effort_levels: info.map(|i| i.effort_levels.clone()).unwrap_or_default(),
                favorite: info.map(|i| i.favorite).unwrap_or(false),
                last_used_ms: info.and_then(|i| i.last_used_ms),
                context_window: info.map(|i| i.context_window).unwrap_or(128_000),
                vision: info.and_then(|i| i.vision),
                usable: info
                    .map(|i| i.availability.as_ref().is_none_or(|a| a.usable))
                    .unwrap_or(true),
                status_label: info.and_then(|i| {
                    i.availability
                        .as_ref()
                        .filter(|a| !a.usable)
                        .and_then(|a| a.status_label.clone())
                }),
                resets_at_ms: info.and_then(|i| {
                    i.availability
                        .as_ref()
                        .filter(|a| !a.usable)
                        .and_then(|a| a.resets_at_ms)
                }),
                reason: info.and_then(|i| {
                    i.availability
                        .as_ref()
                        .filter(|a| !a.usable)
                        .and_then(|a| a.reason.clone())
                }),
                availability_overridden: info.map(|i| i.availability_overridden).unwrap_or(false),
                availability_stale: info.map(|i| i.availability_stale).unwrap_or(false),
                match_id,
                match_name,
                match_connection,
                relevance_score,
            });
        }
    }

    if let Some(quotas) = quotas {
        apply_quotas_to_candidates(&mut candidates, quotas);
    }

    // 1. Favorites section: starred models, sorted by relevance score when searching, else ASCII model id.
    let mut favorites: Vec<RankedModel> = candidates
        .iter()
        .filter(|r| r.favorite)
        .cloned()
        .map(|mut r| {
            r.section = ModelSection::Favorites;
            r
        })
        .collect();
    favorites.sort_by(|a, b| {
        if has_query {
            b.relevance_score
                .cmp(&a.relevance_score)
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider_label.cmp(&b.provider_label))
        } else {
            a.model
                .cmp(&b.model)
                .then_with(|| a.provider_label.cmp(&b.provider_label))
        }
    });

    // 2. Recent section: models with usage history, sorted by relevance then recency when searching, else recency desc.
    let mut recent: Vec<RankedModel> = candidates
        .iter()
        .filter(|r| r.last_used_ms.is_some())
        .cloned()
        .map(|mut r| {
            r.section = ModelSection::Recent;
            r
        })
        .collect();
    recent.sort_by(|a, b| {
        let a_used = a.last_used_ms.unwrap_or(0);
        let b_used = b.last_used_ms.unwrap_or(0);
        if has_query {
            b.relevance_score
                .cmp(&a.relevance_score)
                .then_with(|| b_used.cmp(&a_used))
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider_label.cmp(&b.provider_label))
        } else {
            b_used
                .cmp(&a_used)
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider_label.cmp(&b.provider_label))
        }
    });

    // 3. All models section: contains EVERY candidate model across ready providers.
    let mut all: Vec<RankedModel> = candidates
        .into_iter()
        .map(|mut r| {
            r.section = ModelSection::All;
            r
        })
        .collect();
    all.sort_by(|a, b| {
        if has_query {
            b.relevance_score
                .cmp(&a.relevance_score)
                .then_with(|| a.model.cmp(&b.model))
                .then_with(|| a.provider_label.cmp(&b.provider_label))
        } else {
            a.model
                .cmp(&b.model)
                .then_with(|| a.provider_label.cmp(&b.provider_label))
        }
    });

    let mut rows = Vec::with_capacity(favorites.len() + recent.len() + all.len());
    rows.extend(favorites);
    rows.extend(recent);
    rows.extend(all);
    rows
}

/// Merges live periodic quota snapshots into model candidates.
///
/// Dispatches to the provider's own live [`nuo_protocol::ModelAvailabilityRule`] declarations carried
/// on the connection's usage state. If a rule's reset time has already passed, it is ignored.
pub fn apply_quotas_to_candidates(
    candidates: &mut [RankedModel],
    quotas: &nuo_protocol::ProviderQuotaSnapshot,
) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    for candidate in candidates.iter_mut() {
        if candidate.availability_overridden {
            continue;
        }

        // Static entitlement restrictions (e.g. tier required) take precedence over temporary quota limits
        if !candidate.usable
            && candidate
                .status_label
                .as_deref()
                .is_some_and(|s| s.contains("tier"))
        {
            continue;
        }

        // If an earlier temporary quota limit has expired past its reset time, clear it back to usable
        if !candidate.usable
            && let Some(reset_ms) = candidate.resets_at_ms
            && now_ms >= reset_ms
        {
            candidate.usable = true;
            candidate.status_label = None;
            candidate.reason = None;
            candidate.resets_at_ms = None;
        }

        let entry = quotas
            .entries
            .iter()
            .find(|e| e.name == candidate.provider_id)
            .or_else(|| {
                quotas
                    .entries
                    .iter()
                    .find(|e| e.provider == candidate.provider_id)
            });
        let Some(entry) = entry else {
            continue;
        };

        // 1. Check provider-evaluated dynamic model rules
        if let nuo_protocol::ConnectionUsageState::Available(ref usage) = entry.state {
            let mut rule_applied = false;
            for rule in &usage.model_rules {
                if nuo_protocol::simple_glob_matches(&rule.pattern, &candidate.model) {
                    if let Some(reset_ms) = rule.availability.resets_at_ms
                        && now_ms >= reset_ms
                    {
                        continue;
                    }

                    candidate.usable = rule.availability.usable;
                    candidate.status_label = rule.availability.status_label.clone();
                    candidate.reason = rule.availability.reason.clone();
                    candidate.resets_at_ms = rule.availability.resets_at_ms;
                    rule_applied = true;
                    break;
                }
            }
            if rule_applied {
                continue;
            }
        }

        // 2. Generic exact-label fallback for simple periodic buckets (Periodic or Composite)
        let periodic_ref = match entry.quota {
            Some(nuo_protocol::ProviderQuotaData::Periodic(ref periodic)) => Some(periodic),
            Some(nuo_protocol::ProviderQuotaData::Composite {
                periodic: Some(ref periodic),
                ..
            }) => Some(periodic),
            _ => None,
        };
        if let Some(periodic) = periodic_ref {
            for bucket in &periodic.buckets {
                if bucket.used_fraction < 0.999 {
                    continue;
                }
                if let Some(reset_ms) = bucket.reset_at_ms
                    && now_ms >= reset_ms
                {
                    continue;
                }

                let lower_label = bucket.label.to_ascii_lowercase();
                let matches_bucket = candidate.model == bucket.label
                    || candidate.model == bucket.unit.as_deref().unwrap_or("")
                    || (lower_label.contains("claude") && candidate.model.starts_with("claude-"))
                    || (lower_label.contains("gemini") && candidate.model.starts_with("gemini-"));

                if matches_bucket {
                    let status_label = match bucket.window {
                        Some(nuo_protocol::QuotaWindowKind::Rolling5Hour) => "disabled: 5h limit",
                        Some(nuo_protocol::QuotaWindowKind::Daily) => "disabled: daily quota",
                        Some(nuo_protocol::QuotaWindowKind::Weekly) => "disabled: weekly quota",
                        _ => "disabled: quota",
                    };
                    let reason = format!(
                        "Quota limit reached for {}",
                        bucket.group.as_deref().unwrap_or(&bucket.label)
                    );

                    candidate.usable = false;
                    candidate.status_label = Some(status_label.to_string());
                    candidate.reason = Some(reason);
                    candidate.resets_at_ms = bucket.reset_at_ms;
                    break;
                }
            }
        }
    }
}

/// One **body line** of the flat Models list: either a selectable row or a
/// dim section label. The body the renderer paints is
/// [`models_body_lines`] — `row_index` addresses into it skip the label rows
/// (the selection cursor is a *row* cursor, not a *line* cursor, so ↑/↓ can
/// never land on a label).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelBodyLine {
    /// A selectable row — the payload is the row's index into the flat
    /// [`RankedModel`] slice (the same slice `modal_index` addresses).
    Row(usize),
    /// A dim section label — the payload is the section being announced.
    /// Never selectable.
    Section(ModelSection),
}

/// Map every selectable row index to its body-line index, inserting a
/// [`ModelBodyLine::Section`] label before each non-empty section in display
/// order. Rows whose section is empty produce no lines at all, so an empty
/// Favorites section, for instance, renders no `FAVORITES` header. The
/// returned vector's length is the body's total line count (what the modal's
/// scroll math must use); `row_line[i]` is where flat row `i` paints.
///
/// Blank row between sections is *not* included here — the renderer adds one
/// spacer line before every section label after the first (see
/// `model_list_body`), keeping this mapping pure row/label geometry.
pub fn models_body_lines(models: &[RankedModel]) -> (Vec<ModelBodyLine>, Vec<usize>) {
    let mut lines: Vec<ModelBodyLine> = Vec::with_capacity(models.len() + 3);
    let mut row_line: Vec<usize> = Vec::with_capacity(models.len());
    // Sections arrive in display order because `models_flat_filtered_from`
    // sorts by `ModelSection` first — walk the boundary transitions.
    let mut current: Option<ModelSection> = None;
    for (i, rm) in models.iter().enumerate() {
        let section = rm.section();
        if current != Some(section) {
            lines.push(ModelBodyLine::Section(section));
            current = Some(section);
        }
        row_line.push(lines.len());
        lines.push(ModelBodyLine::Row(i));
    }
    (lines, row_line)
}
