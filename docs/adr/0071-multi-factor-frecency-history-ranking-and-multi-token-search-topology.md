---
id: ADR-0071
title: "Multi-Factor Frecency History Ranking, Order-Independent Multi-Token Search, and Prompt Recall Topology"
status: accepted
date: 2026-11-10
scope: tui/nuo-tui, protocol/nuo-protocol, persistence/nuo-persistence, interaction/composer, ux/history
superseded_by: null
negative_knowledge: true
---

# 0071. Multi-Factor Frecency History Ranking, Order-Independent Multi-Token Search, and Prompt Recall Topology

- Status: Accepted
- Date: 2026-11-10
- Deciders: Nuo Architecture Working Group, TUI Presentation Team, Persistence SSOT Team
- Consulted: Interface, Usability, Interaction, Runtime, Protocol Teams
- Complements: [ADR-0044](0044-composer-native-history-search-ghost-preview-and-draft-stash-topology.md), [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md)

---

## Context and Problem Statement

In [ADR-0044](0044-composer-native-history-search-ghost-preview-and-draft-stash-topology.md) and [ADR-0051](0051-composer-canonical-quadrant-modality-atomic-draft-capsule-and-in-place-recall-topology.md), Nuo established Composer-native history search (`Ctrl-R` overlay drawer) and separated in-place sequential stepping (`Recall`, session-scoped) from overlay search (`Search`, cross-session global).

However, operators consistently encountered severe cognitive friction and discovery failure in real-world workflows: *"I can never find what I'm looking for, or it takes extreme effort to find it."* Forensic analysis identified four fundamental architectural and algorithmic flaws in the legacy history subsystem:

1. **Rigid Single-Subsequence Permutation Failure ("Cannot Find")**:
   The legacy search matcher evaluated the entire composer buffer as a single, contiguous subsequence needle. If an operator typed two or more keywords—such as `nuo test` when the prompt was `cargo test --package nuo-tui`, or `历史 权重` when the prompt was `权重机制中的历史模块`—the matcher enforced strict monotonic character order across the whole string. Because `test` occurred before `nuo-tui`, the subsequence check failed completely, returning **zero candidates**. Operators were punished for typing multiple keywords or introducing trailing whitespace.

2. **Zero Frequency Accounting (Absence of Frecency)**:
   The system tracked only `created_at_ms`. Re-submitting a command 100 times refreshed its timestamp under global deduplication, but discarded all usage frequency telemetry. A workhorse prompt used 50 times daily had identical weight to an accidental typo submitted 5 minutes ago.

3. **CJK Ideograph Phrase Degradation**:
   The fuzzy matcher's word boundary heuristics (`is_word_boundary`, `is_right_boundary`) relied on ASCII whitespace and punctuation transitions. In Chinese, Japanese, and Korean (CJK) ideographic text where words are not delimited by spaces, continuous multi-character conceptual terms (such as `权重` or `检索`) were misclassified as internal scattered substrings, receiving meager scoring bonuses and being drowned out by accidental character collisions.

4. **Coarse-Grained Recency Cliff**:
   Recency was modeled as four rigid discrete buckets (`< 1h`, `< 1d`, `< 1w`, `< 1m`) with an arbitrary +40 cap, failing to capture working-set hot loops (< 10 minutes) or provide smooth decay curves.

---

## Decision Drivers

- **Uncompromising Ergonomics & High Recall (`[INV-HIST-04]`)**: Operators must find prompts naturally using multiple keywords in any order, with zero false rejections from intervening text or keyword permutations.
- **Multi-Factor Frecency SSOT (`[INV-HIST-05]`)**: Combine match quality, bounded logarithmic usage frequency, smooth exponential-gradient recency decay, and session/workspace affinity into a deterministic composite ranking score.
- **Frequency-Aware Durable Persistence (`[INV-HIST-06]`)**: Track execution frequency (`use_count`) across protocol wire representations and durable SQLite storage with atomic increment on deduplication.
- **CJK Semantic Equality**: Treat contiguous CJK character matches with first-class word/phrase scoring parity to western space-delimited words.
- **Zero Latency Budget Degradation**: Maintain sub-30ms search response across 100,000+ history entries without heap allocation in inner match loops.

---

## Architectural Decisions

### 1. Multi-Token Query Architecture (`crates/nuo-tui/src/fuzzy.rs`)

Replace the single-needle assumption with a zero-allocation `QueryMatcher`:

- **Token Decomposition**: The search query is trimmed and split into whitespace-delimited tokens (`split_whitespace()`).
- **Fast Rejection Cascade**: For each item in the history slice, every token must satisfy case-insensitive subsequence testing (`is_subsequence`). If any token fails, the candidate is discarded in O(|haystack|) with zero allocations.
- **Independent Token DP Scoring**: Each token is evaluated with the thread-local DP scratchpad to compute optimal alignment and positions.
- **Composite Score & Highlight Union**:
  - `Base = sum(token_scores)`
  - `Bonus All Tokens (+200)`: Reward for matching every user-specified constraint.
  - `Bonus Token Order (+150)`: Additional bonus if the tokens appear in the left-to-right order typed by the operator.
  - `Bonus Exact Phrase (+300)`: High bonus if the candidate literally contains the full trimmed query as a contiguous phrase.
  - `Highlight Positions`: Unified, sorted, and deduplicated character indices across all tokens, ensuring the drawer highlights every matched keyword simultaneously.

### 2. Multi-Factor Frecency Ranking Formula (`crates/nuo-tui/src/app/history.rs`)

When a search query is present, candidates are scored and sorted by a multi-dimensional composite weight:

$$\text{Total Score} = \text{Score}_{\text{match}} + \text{Score}_{\text{recency}} + \text{Score}_{\text{frequency}} + \text{Score}_{\text{affinity}}$$

1. **Match Quality Score ($\text{Score}_{\text{match}}$)**:
   - Full exact match: `+600`
   - Prompt prefix match (`first_pos == 0 && is_contiguous`): `+350`
   - Whole word boundary match (`left_boundary && right_boundary`): `+250`
   - CJK contiguous ideograph match (`is_contiguous && is_cjk`): `+250`
   - Word prefix match: `+180`
   - Scatter penalty: proportional to character spread span.
2. **Smooth Recency Gradient ($\text{Score}_{\text{recency}}$)**:
   Relative to the newest recorded entry $\Delta t = t_{\text{max}} - t_{\text{entry}}$:
   - $\Delta t < 10\text{ min}$: `+60` (active hot loop)
   - $\Delta t < 1\text{ hour}$: `+45`
   - $\Delta t < 6\text{ hours}$: `+35`
   - $\Delta t < 24\text{ hours}$: `+25`
   - $\Delta t < 3\text{ days}$: `+18`
   - $\Delta t < 7\text{ days}$: `+12`
   - $\Delta t < 30\text{ days}$: `+5`
   - Older: `+0`
3. **Logarithmic Frequency Scaling ($\text{Score}_{\text{frequency}}$)**:
   - $\text{Score}_{\text{frequency}} = \text{clamp}(25.0 \times \ln(\text{use\_count}), 0, 120)$
   - $\text{use\_count} = 1$: `+0` (baseline, no inflation for one-off commands)
   - $\text{use\_count} = 2$: `+17`
   - $\text{use\_count} = 5$: `+40`
   - $\text{use\_count} = 10$: `+58`
   - $\text{use\_count} = 50$: `+98`
   - $\text{use\_count} \ge 100$: `+115..+120` (bounded ceiling prevents historical monopoly)
4. **Context Affinity ($\text{Score}_{\text{affinity}}$)**:
   - Current thread match (`session_id == current_session_id`): `+25`
   - Current workspace match (`workspace == current_workspace`): `+10`
5. **Stable Tiebreaker**:
   - Equal total scores preserve chronological newest-first ordering via stable slice sorting.

### 3. Protocol and Persistence Frequency Tracking (`Migration 25`)

- **Protocol Layer (`crates/nuo-protocol/src/history.rs`)**:
  - Add `use_count: u32` to `HistoryEntry` with backward-compatible Serde semantics (`#[serde(default = "default_use_count", skip_serializing_if = "is_default_use_count")]`).
  - `merge_history`: colliding identities retain `slot.use_count = slot.use_count.max(entry.use_count)`.
- **Persistence Layer (`crates/nuo-persistence`)**:
  - Advance `CURRENT_DB_VERSION` to `25`.
  - Migration 25: `ALTER TABLE input_history ADD COLUMN use_count INTEGER NOT NULL DEFAULT 1;`.
  - `record_input_history`: on global deduplication (`dedup = true`), atomically query the existing row's `use_count` and increment `final_count = prev.max(entry.use_count).saturating_add(1)`.
  - `load_input_history` & `save_input_history`: read and write `use_count` round-trip.

---

## Normative Invariants Established

- **`[INV-HIST-04]` Multi-Token Order-Independent Keyword Recall**:
  Whitespace-delimited search queries in `HistorySearch` must match candidate prompts regardless of keyword permutation, intervening text, or line breaks. All matched token ranges must be highlighted concurrently in the drawer display.
  *Enforcement*: `tests::history_recall::test_history_multi_token_and_cjk_recall` and `fuzzy::tests::multi_token_matching_supports_any_order`.

- **`[INV-HIST-05]` Multi-Factor Frecency Ranking Topology**:
  Candidate ranking under a non-empty search query must compute a deterministic composite score incorporating match quality, log-scaled submission frequency, smooth recency decay, and session/workspace context affinity. A high-frequency prompt must outrank a recent one-off prompt of similar match quality.
  *Enforcement*: `tests::history_recall::test_history_comprehensive_weighting_mechanism`.

- **`[INV-HIST-06]` Frequency-Aware Schema SSOT**:
  `input_history` records must durably persist prompt submission count. Repeated submissions under global deduplication must increment `use_count` monotonically without resetting to 1.
  *Enforcement*: `db::tests::input_history_tracks_use_count_across_records` and migration 25 catalog test.

---

## Consequences

### Positive
- **High Recall Accuracy**: Operators can type partial keywords in any order (`nuo test`, `build cargo`, `历史 权重`) and immediately find the desired prompt.
- **Predictable Frecency**: Frequently executed workflows naturally surface at the top of query results, while recent one-offs do not pollute top slots.
- **Zero Legacy Burden**: Clean architectural integration across wire protocol, SQLite migrations, and zero-allocation TUI matching.
- **CJK Parity**: First-class handling of non-space-delimited Asian languages without artificial match penalties.

### Negative & Trade-offs
- Database version increments to `v25`, requiring schema migration upon upgrade (automated and instantaneous via SQLite `ALTER TABLE`).

---

## Rejected Alternatives & Negative Knowledge

1. **Linear Frequency Weighting**:
   - *Why rejected*: Linear weighting ($k \cdot N$) allows heavily repeated trivial commands (e.g. `ls` or `cargo check` run 1,000 times) to permanently overshadow perfect textual matches on newer, highly specific prompts. Logarithmic scaling ($\ln(1 + N)$) bounded at `+120` provides meaningful frequency promotion while ensuring strong semantic matches always win.

2. **FTS5 SQLite Engine for Prompt Input History**:
   - *Why rejected*: `input_history` is an in-memory active working set (< 100,000 short strings) accessed on every keystroke during live composer typing. Routing live interactive keystroke filtering through SQLite IPC round-trips would introduce latency jitter, IPC serialization overhead, and thread scheduling contention compared to native in-memory O(N) streaming DP matching.

3. **Permutation Regex Generation**:
   - *Why rejected*: Generating regex permutations for multi-token queries creates combinatorial explosions ($N!$) and high heap allocation churn. Compiling discrete token matchers with fast boolean subsequence rejection achieves order independence in microseconds with zero allocations.
