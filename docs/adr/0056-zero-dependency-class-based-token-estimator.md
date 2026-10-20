---
id: ADR-0056
title: "Zero-Dependency Class-Based Token Estimator and Vendored Binary Deprecation"
status: accepted
date: 2026-10-28
scope: tokenizer, context/pressure, tool/nuo-tool, vendor/deprecation
superseded_by: null
negative_knowledge: true
---

# 0056. Zero-Dependency Class-Based Token Estimator and Vendored Binary Deprecation

- Status: Accepted
- Date: 2026-10-28
- Deciders: Nuo Architecture Working Group, Runtime Engineering
- Consulted: Runtime (`nuo-agent`), Harness (`nuo-harness`), Tooling (`nuo-tool`), Wire (`nuo-wire`)
- Informed: System Architects, Documentation Governance
- Supersedes: Legacy embedded `cl100k_base.packed` BPE vocabulary and `vendor/` tree (see "Heuristics rejection" above)
- Complements: [ADR-0019](0019-token-pressure-measurement-and-budget-accounting.md), [ADR-0055](0055-hierarchical-workspace-topology-and-component-namespace.md)

---

## Context and Problem Statement

Nuo previously embedded a 1.04 MB compiled binary vocabulary file (`vendor/cl100k_base.packed`) to run an exact byte-pair encoding (BPE) tokenizer corresponding to OpenAI's legacy `cl100k_base` model. This design suffered from critical technical and architectural liabilities:

1. **Repository Pollution & Cross-Directory Entanglement**: A dedicated `vendor/` directory at the repository root housed a raw binary blob tracked in Git. Deep member crates were forced to traverse three directory levels (`include_bytes!("../../../vendor/cl100k_base.packed")`) to compile, violating crate encapsulation and breaking standalone `cargo package` publication.
2. **Cold-Start Latency & Heap Overhead**: On first token counting invocation, the runtime was forced to decode 100,256 BPE ranks and allocate an ~8 MB `HashMap<&'static [u8], u32>` on the heap, adding 10–20 ms to initialization.
3. **Multi-Model Reality Drift**: Nuo now orchestrates 17+ model providers (Claude 3.5/3.7, DeepSeek, Qwen, Gemini, GPT-4o). None of these modern frontiers use OpenAI's old GPT-4 `cl100k_base` vocabulary (`GPT-4o` uses `o200k_base`; Claude, DeepSeek, and Qwen use their own tokenizers). Using `cl100k_base` to estimate tokens for Claude or DeepSeek was already an approximation, and one that severely overcounted Asian and CJK languages by 30%–50% due to legacy UTF-8 fragmentation.
4. **Local Role Purity**: In Nuo's architecture (`[INV-PRESS-01]`), local token calculation serves solely as a **predictive heuristic** for context pressure watermarks, sliding window compaction, and transcript truncation—never as authoritative billing facts (which are reported directly by provider API responses).

We require a zero-dependency, ultra-fast, zero-allocation token estimator that eliminates all binary blobs from the repository while providing superior statistical fidelity across all supported modern models.

---

## Decision Drivers

- **Zero Vendored Binary Artifacts**: Eradicate `vendor/cl100k_base.packed` and remove all binary blobs from Git.
- **Microsecond Throughput**: Achieve 500 MB/s – 1.5 GB/s scanning throughput via single-pass $O(N)$ linear inspection (30–50× faster than BPE iterative merges).
- **Zero Heap Allocations**: Pure stack/slice evaluation during standard `count_tokens` calls.
- **Modern Multi-Model Balance**: Accurately reflect modern BPE tokenizer compression across English prose, source code identifiers, and CJK text.
- **Mathematical Streaming Invariant**: Preserve strict equivalence where `StreamingCounter::finish()` across any chunking sequence equals `count_tokens(concatenated_stream)`.

---

## Decision Outcome

We decommission the `vendor/` directory and replace the BPE merge loop in `nuo-tool::tokenizer` with a high-performance, class-based token estimator:

1. **Category Weighting**:
   - **ASCII Words (`[a-zA-Z0-9_]`)**: Natural words of 1–5 characters count as 1 token (e.g. `fn`, `let`, `user`, `hello`). Words longer than 5 characters scale smoothly at subword rate: `1 + (len - 5 + 3) / 4`.
   - **CJK Characters**: Han, Hangul, and Kana glyphs are assigned 1 token per character, accurately reflecting DeepSeek, Qwen, Claude, and GPT-4o tokenizer token densities.
   - **Whitespace & Newlines**: Single leading spaces are absorbed; newline runs and long space indentations are amortized at 4 characters per token.
   - **Punctuation & Symbols**: Individual punctuation counts as 1 token; repeated identical sequences (e.g. `===`, `---`) are amortized at 3 characters per token.
2. **Deterministic Monotonic Truncation**:
   `truncate_to_tokens(text, max_tokens)` guarantees:
   - Slices land strictly on valid UTF-8 scalar boundaries.
   - Slices are monotonically non-decreasing with respect to token budget.
   - Returned token count never exceeds `max_tokens`.
3. **Streaming Counter Parity**:
   `StreamingCounter` buffers only the trailing unsealed lexical piece. Completed pieces are committed incrementally, ensuring `StreamingCounter::finish()` precisely matches whole-text token evaluation.

### Invariants & Behavioral Boundaries

- **`[INV-TOKEN-01] Zero External Data Blob`**: The token estimator MUST be 100% self-contained in Rust code, with zero checked-in binary vocabulary files, external data assets, or network calls.
- **`[INV-TOKEN-02] Mathematical Streaming Equivalence`**: For any text and any chunk sequence, `StreamingCounter::finish()` MUST equal `count_tokens(text)`.
- **`[INV-TOKEN-03] UTF-8 Boundary Safe Monotonic Truncation`**: `truncate_to_tokens` MUST always cut strictly on UTF-8 scalar boundaries and be non-decreasing with respect to token budgets.
- **`[INV-TOKEN-04] Multi-Model Statistical Fidelity`**: Token estimation MUST model modern multi-model representations (GPT-4o, Claude 3.5/3.7, DeepSeek, Qwen) rather than overfitting to legacy single-vendor GPT-4 BPE.

---

## Rejected Alternatives

### 1. Retain `cl100k_base.packed` Inside `crates/nuo-tool/data/`
- **Why considered**: Keeps byte-level compatibility with OpenAI GPT-4.
- **Why rejected**: Perpetuates tracking a 1 MB opaque binary blob in Git, incurs 8 MB heap allocation and 10–20 ms cold-start overhead, and retains obsolete, heavy overcounting for Chinese/Asian languages across DeepSeek and Qwen.

### 2. Introduce External Crates (e.g. `tiktoken-rs`)
- **Why considered**: Outsources vocabulary maintenance to an upstream crate.
- **Why rejected**: Bloats compilation time, pulls in regex engines and external vocabularies, and still restricts tokenization to an OpenAI-specific model rather than multi-provider realities.

### 3. Crude Character Division (`len / 4`)
- **Why considered**: Extremely trivial to implement.
- **Why rejected**: Blind character division fails catastrophically on non-ASCII scripts (e.g. 3-byte CJK UTF-8 characters would be counted as 0.75 tokens per byte or overcount by 3×) and does not support deterministic pre-commit streaming boundaries.
