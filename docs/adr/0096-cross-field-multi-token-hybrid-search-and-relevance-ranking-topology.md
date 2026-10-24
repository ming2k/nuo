---
id: ADR-0096
title: "Cross-Field Multi-Token Hybrid Search and Relevance Ranking Topology for Model Catalog Surface"
status: accepted
date: 2026-11-28
scope: tui/nuo-tui, interaction/models, search/fuzzy, ux/catalog
superseded_by: null
negative_knowledge: true
---

# 0096. Cross-Field Multi-Token Hybrid Search and Relevance Ranking Topology for Model Catalog Surface

- Status: Accepted
- Date: 2026-11-28
- Deciders: Nuo Architecture Working Group, TUI Presentation Team, Interaction SSOT Team
- Consulted: Model Wire Team, UI Subsystem Team, Protocol Governance
- Complements: [ADR-0071](0071-multi-factor-frecency-history-ranking-and-multi-token-search-topology.md), [ADR-0076](0076-canonical-unselectable-list-item-topology-and-inert-skip-navigation.md)

---

## Context and Problem Statement

The daily-driver model switcher dialog (`/models`, `Ctrl+M`) flattens every ready provider connection and its served models into a unified, sectioned list (`Favorites` → `Recent` → `All models`). Each rendered model row exposes three distinct visual and semantic columns:
1. **Model Name**: The human-readable display label (or wire ID fallback when no distinct marketing name exists);
2. **Model Wire ID**: The literal technical identity sent over the wire;
3. **Connection Name**: The configured provider instance label (`· <provider_label>`).

In multi-provider environments, identical or overlapping model IDs frequently exist across different connections (e.g., `gpt-4o` served simultaneously by `OpenAI Official` and `Azure (Corporate)`, or `deepseek-chat` served by `DeepSeek Official`, `OpenRouter`, and `SiliconFlow`).

However, the legacy model search implementation applied a naive, single-needle disjunctive check:
```rust
// Legacy flaw: evaluates single needle against each field independently in isolation
let id_m = fuzzy::fuzzy_match(model, query);
let name_m = info.name.as_deref().and_then(|name| fuzzy::fuzzy_match(name, query));
let conn_m = conn_match.clone();
if id_m.is_none() && name_m.is_none() && conn_m.is_none() { continue; }
```

This single-needle architecture broke down completely under real-world usage:
1. **Cross-Field Hybrid Failure ("Zero Results")**: When an operator searched using keywords spanning both the connection and the model (e.g., `azure gpt-4`, `deepseek r1`, `anthropic claude`, or path-style `azure/gpt-4o`), the query failed across all three fields because no single field contained both keywords. Operators were forced to clear the search box, scroll manually, and visually scan the dim connection column.
2. **Monotonic Order Rigidity**: Even if a virtual composite haystack were constructed (e.g., `provider/model`), operators who typed the model first followed by the connection (e.g., `sonnet anthropic` or `gpt4 azure`) would fail due to DP monotonic character ordering.
3. **Multi-Word Intra-Model Degradation**: Multi-term model searches (such as `3.5 sonnet` for `claude-3-5-sonnet`) suffered heavy gap penalties or false rejections when evaluated as a single needle.
4. **Absence of Cross-Field Intent Prioritization**: Filtering preserved only static alphabetical ordering within sections. A candidate matching both the connection and model dimensions was given no ranking elevation over incidental partial matches.

A clean break from the single-needle legacy implementation is required to establish multi-token cross-field hybrid search with order independence, per-column highlight attribution, and relevance-weighted section ranking.

---

## Decision Drivers

- **Intuitive Cross-Field Disambiguation (`[INV-MODEL-SEARCH-01]`)**: Operators must be able to target models by combining connection names and model identifiers (e.g., `azure gpt`, `deepseek r1`, `anthropic/sonnet`) without experiencing false zero-result rejections.
- **Order-Independent Symmetrical Matching (`[INV-MODEL-SEARCH-02]`)**: `connection model` and `model connection` must evaluate to the identical candidate set with equivalent relevance scoring.
- **Column-Isolated Highlight Attribution (`[INV-MODEL-SEARCH-03]`)**: Character highlight positions must never cross column boundaries or suffer indexing offsets. Tokens matching the connection must highlight only the connection column, and tokens matching the model identifier or display name must highlight only their respective columns.
- **Relevance Elevation & Cross-Field Prioritization (`[INV-MODEL-SEARCH-04]`)**: Candidates satisfying both the connection and model dimensions must receive a decisive relevance bonus over candidates that match only one dimension.
- **Sub-Millisecond Interactive Response**: Searching across hundreds of candidate routes must execute well within terminal refresh budgets (< 1ms) without inner-loop heap thrashing.

---

## Architectural Decisions

### 1. Delimited Multi-Token Decomposition

The active picker query is decomposed into discrete search tokens using whitespace and universal path/tag separators (`/`, `:`, `@`, `|`):
```rust
let tokens: Vec<&str> = query
    .split(|c: char| c.is_whitespace() || c == '/' || c == ':' || c == '@' || c == '|')
    .filter(|s| !s.is_empty())
    .collect();
```
- **Whitespace Delimitation**: Supports standard natural language multi-keyword entry (`azure gpt-4`, `sonnet anthropic`).
- **Path & Tag Separator Delimitation**: Supports canonical registry/path syntax (`ollama/llama3`, `openrouter/deepseek`, `openai/gpt-4o`, `ollama:llama3`, `gpt@azure`).
- **Trailing & Repeated Delimiter Resilience**: Filtering out empty tokens guarantees that in-progress typing (e.g., `azure ` with a trailing space, or `azure::gpt`) does not collapse candidate lists.

### 2. Conjunctive Token Coverage Across Fields (`[INV-MODEL-SEARCH-01]`)

For every candidate row `(connection_label, model_id, model_name)`:
- Every token $t_k \in \text{tokens}$ must fuzzy-match at least one field in `{connection_label, model_id, model_name}`.
- If any token $t_k$ matches none of the candidate's fields, the candidate is discarded immediately.
- If all tokens are satisfied, the candidate is accepted.

### 3. Order-Independent Evaluation (`[INV-MODEL-SEARCH-02]`)

Because each token $t_k$ is checked independently against all available fields, token ordering is completely commutative:
$$\text{Tokens}(A, B) \iff \text{Tokens}(B, A)$$
Typing `gpt-4 azure` yields the identical candidate set and highlight allocations as `azure gpt-4`.

### 4. Per-Column Highlight Aggregation & Deduplication (`[INV-MODEL-SEARCH-03]`)

Each field maintains an independent accumulator:
- Tokens matching `connection_label` contribute to `conn_accumulator`;
- Tokens matching `model_id` contribute to `id_accumulator`;
- Tokens matching `model_name` contribute to `name_accumulator`.

When multiple tokens match the same field (e.g., `3.5` and `sonnet` matching `claude-3-5-sonnet`), the matched character indices are merged, sorted, and deduplicated:
```rust
positions.sort_unstable();
positions.dedup();
```
The resulting `FuzzyMatch` instances populate `RankedModel.match_connection`, `match_id`, and `match_name` respectively. The presentation renderer in `overlays/provider/models.rs` paints highlight styles directly onto the corresponding columns without index drift.

### 5. Relevance Scoring, Normalized Model Quality, and Cross-Field Bonus (`[INV-MODEL-SEARCH-04]`)

To ensure candidates matching user intent float to the top of their respective sections:
1. **Normalized Model Dimension Scoring**: When a token matches both `model_id` and `model_name` (e.g., a redundant marketing name identical to the wire id), the model dimension match takes $\max(S_{\text{id}}, S_{\text{name}})$ rather than artificially inflating the score through duplication.
2. **Base Score**: Sum of token match contributions across dimensions ($\sum [S_{\text{conn}}(t_k) + \max(S_{\text{id}}(t_k), S_{\text{name}}(t_k))]$).
3. **Cross-Field Bonus (`BONUS_CROSS_FIELD = 1000`)**: Awarded when a candidate simultaneously matches both the connection dimension (`match_connection.is_some()`) and the model dimension (`match_id.is_some() || match_name.is_some()`).
4. **Section-Internal Ranking**:
   - When a search query is active (`!tokens.is_empty()`), candidates within each section (`Favorites`, `Recent`, `All`) are sorted primarily by `relevance_score` descending, with stable tiebreakers (ASCII model ID, provider label).
   - When the search query is empty, stable default ordering (Favorites by model ID, Recent by recency timestamp, All by model ID) is preserved exactly.

### 6. Provider-Row Hoisted Token Evaluation and Zero-Cloning Iteration

To eliminate redundant fuzzy match computations and memory churn:
- Token evaluation against provider names is hoisted to the provider row level:
  ```rust
  let prow_token_matches: Vec<Option<fuzzy::FuzzyMatch>> = tokens
      .iter()
      .map(|t| fuzzy::fuzzy_match(&prow.name, t))
      .collect();
  ```
  For a provider serving 50 models, `fuzzy_match(&prow.name, t)` runs once per token rather than 50 times.
- Model catalog iteration borrows directly from `ProviderPickerRow.model_info` via reference without allocating fallback structs or cloning metadata on non-matching candidates.
- Search query edits automatically re-anchor list cursor and scroll offset to index 0 (`follow = true`), ensuring the operator immediately addresses the top-ranked match.

---

## Invariants Established

- **`[INV-MODEL-SEARCH-01] Conjunctive Multi-Token Coverage`**: Every non-empty search token in a model catalog query must fuzzy-match at least one field (`connection_label`, `model_id`, or `model_name`). A candidate missing any token must be rejected.
- **`[INV-MODEL-SEARCH-02] Order-Independent Symmetrical Resolution`**: Candidate acceptance and score calculation must be strictly invariant under permutation of query tokens.
- **`[INV-MODEL-SEARCH-03] Column-Isolated Highlight Attribution`**: Highlight positions must only reflect character indices within the specific field matched. No cross-field offset bleeding or synthetic index shifts are permitted.
- **`[INV-MODEL-SEARCH-04] Cross-Field Prioritization Elevation`**: Candidates satisfying both connection and model dimensions must receive the canonical cross-field bonus (`BONUS_CROSS_FIELD`), ensuring targeted routes rank strictly above single-field incidental matches.

---

## Consequences

### Positive
- **Instant Disambiguation**: Operators can disambiguate overlapping model IDs across connections in seconds (e.g., `corp gpt4` vs `openai gpt4`).
- **Flexible Mental Models**: Supports natural whitespace multi-keyword entry, reverse-order entry, and path-style entry (`connection/model`).
- **Preserved Section Geometry**: Respects Nuo's three-tier model hierarchy (`Favorites` → `Recent` → `All models`) while dynamically elevating the most relevant results within each tier.
- **Crystal-Clear Visual Feedback**: Simultaneously highlights matched tokens across the model name, wire ID, and connection columns.

### Negative & Trade-offs
- Multiple tokens require iterating token matches per candidate row. (Mitigated by hoisting provider-name evaluation and early exit on unmatched tokens).

---

## Rejected Alternatives & Negative Knowledge

### Alternative A: Virtual Composite Haystack (Rejected)
- **Why considered**: Concatenating `format!("{}/{}", connection, model)` into a single string and running a single `fuzzy_match` requires minimal code changes.
- **Why rejected**: Monotonically binds search order (`connection/model` rejects `model connection`). Large intervening provider names impose severe gap penalties. Slicing character highlight offsets across UTF-8 boundaries back into separate columns is brittle and error-prone.

### Alternative B: Syntax-Driven Namespace Prefixes (Rejected)
- **Why considered**: Requiring `@connection` or `connection:` explicitly eliminates parsing ambiguity.
- **Why rejected**: Excessive cognitive friction. Operators in fast switcher modals expect fluid keyword matching without having to recall specific prefix syntax.

### Alternative C: Strict Cross-Field Partitioning (Rejected)
- **Why considered**: Forcing at least one token to match connection and at least one to match model guarantees pure hybrid search.
- **Why rejected**: Regresses intra-model multi-word searches. If an operator types `claude sonnet` or `3.5 haiku`, both tokens match only the model, which strict cross-partitioning would incorrectly reject.

---

## Links

- Complements: [ADR-0071](0071-multi-factor-frecency-history-ranking-and-multi-token-search-topology.md) (Multi-Factor Frecency History Ranking and Multi-Token Search)
- Complements: [ADR-0076](0076-canonical-unselectable-list-item-topology-and-inert-skip-navigation.md) (Canonical Unselectable List Item Topology)
