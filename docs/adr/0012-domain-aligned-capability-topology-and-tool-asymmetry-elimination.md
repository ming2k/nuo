---
id: ADR-0012
title: "Domain-Aligned Capability Topology and Tool Asymmetry Elimination"
status: accepted
date: 2026-10-03
scope: workspace/topology, architecture/layering, capability/tools, domain/nuo-web, domain/nuo-code
superseded_by: null
negative_knowledge: true
---

# 0012. Domain-Aligned Capability Topology and Tool Asymmetry Elimination

- Status: Accepted
- Date: 2026-10-03
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Capability, and Interface Teams
- Informed: System Architects, Release Engineering
- Fulfills & Amends: [ADR-0007](0007-tool-ownership-and-plugin-boundaries.md) §2, [ADR-0008](0008-single-tool-contract.md)
- Amends: [ADR-0010](0010-harness-decomposition-and-agent-unification.md) §1 (retires transitional `nuo-tools-*` naming in favor of first-class domain crates)

---

## Context and Problem Statement

ADR-0008 established a single zero-runtime tool contract in `nuo-tool` (`nuo_tool::Tool`), and ADR-0010 decoupled concrete capability tools from the monolithic `nuo-harness`. However, the resulting workspace topology exhibited structural dissonance and nomenclature asymmetry:

1. **Nomenclature Asymmetry and Cognitive Friction**:
   - Capabilities such as the OS/filesystem (`nuo-host`), multi-agent coordination (`nuo-acp`), and memory storage (`nuo-persistence`) were modeled as first-class domain crates that internally implement `nuo_tool::Tool` projections in `src/tools.rs`.
   - Conversely, web retrieval and code AST analysis were segregated into crates prefixed with `nuo-tools-` (`nuo-tools-web`, `nuo-tools-code`).
   - This created an asymmetric mental model: why did `nuo-tools-acp` or `nuo-tools-host` not exist, while web and code were treated as auxiliary tool add-ons?

2. **The "Junk Drawer" Hazard and Phonetic Ambiguity**:
   - Naming crates around the noun `tools` (`nuo-tools-web`, `nuo-tools-code`) or contemplating a catch-all `nuo-tools` creates an architectural "junk drawer" anti-pattern: developers inevitably dump miscellaneous, poorly categorized logic into tool-named containers.
   - The coexistence of `nuo-tool` (the zero-runtime trait contract) and `nuo-tools-*` (concrete capability implementations) caused persistent phonetic confusion and dependency declaration errors.

3. **Inversion of Domain Capability vs Tool Projection**:
   - Web search, HTTP extraction, SSRF security gating, and snapshot isolation represent a cohesive **Web Ingestion and Retrieval Domain**.
   - Multi-language Tree-sitter parsing, AST declaration extraction, and syntactic mutation verification represent a cohesive **Code Intelligence and Syntax Domain**.
   - These are full domain capabilities in their own right, not mere "tool wrappers". The `Tool` implementation is merely their thin projection view onto the agent runtime.

---

## Decision Drivers

- **Zero-Ambiguity Contract Singularity**: `nuo-tool` must remain the single, canonical crate in the workspace bearing the word `tool`, maintaining its role as the pure contract layer.
- **Domain-First Cohesion**: All native capabilities must be first-class domain crates (`nuo-host`, `nuo-web`, `nuo-code`, `nuo-acp`, `nuo-persistence`). Tools are views/projections implemented natively inside their respective domain.
- **Prevention of Junk Drawers**: Eliminate transitional `nuo-tools-*` crates and prohibit a catch-all `nuo-tools` bag.
- **Clear Ecosystem Extension Boundary**: All external and long-tail integrations must connect via the Model Context Protocol (`nuo-mcp`), keeping core workspace crates strictly bounded.

---

## Decision Outcome

### 1. Rename Auxiliary Tool Crates to First-Class Domain Crates

The transitional `nuo-tools-*` crates introduced during ADR-0010 decomposition are promoted to first-class domain crates:
- `nuo-tools-web` $\rightarrow$ **`nuo-web`**: The canonical web intelligence, search federation (Bocha, DDG, Exa, SearXNG, Tavily), reader, and SSRF-safe scraper domain.
- `nuo-tools-code` $\rightarrow$ **`nuo-code`**: The canonical code intelligence, AST structural query, and Tree-sitter syntax verification domain.

### 2. Contract and Capability Symmetry

The workspace achieves complete structural symmetry. Every capability domain owns its core logic and exports its agent tool projections conforming to `nuo_tool::Tool`:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                   Layer 0: Zero-Runtime Contract                       │
│               nuo-tool (Trait, Descriptor, ToolSchema)                 │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ implemented by (via "tools" projection)
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   Layer 1: First-Class Domain Crates                   │
│                                                                        │
│  • nuo-host        (Filesystem, process execution, workspace roots)    │
│  • nuo-web         (Search federation, Jina reader, SSRF isolation)    │
│  • nuo-code        (Tree-sitter AST queries, syntax guard)             │
│  • nuo-acp         (Multi-agent fabric, mailbox, delegation)           │
│  • nuo-persistence (Role memory, transactional state, snapshot store)  │
└────────────────────────────────────────────────────────────────────────┘
```

### 3. Open Ecosystem via MCP (`nuo-mcp`)

To prevent future crate sprawl or "junk drawer" pressure for long-tail integrations (e.g., GitHub, Slack, Postgres, Docker), all external capabilities must enter through `nuo-mcp` as standard Model Context Protocol servers. No ad-hoc tool crates shall be added to the core workspace.

---

## Invariants & Behavioral Boundaries

- **`[INV-TOOL-13] Unambiguous Contract Singularity`**: `nuo-tool` is the sole crate in the workspace bearing the name `tool`. No auxiliary crates may use `nuo-tools-*` or `nuo-tools` naming.
- **`[INV-DOMAIN-01] Domain-First Capability Ownership`**: All native capabilities are domain crates (`nuo-host`, `nuo-web`, `nuo-code`, `nuo-acp`, `nuo-persistence`). Tools are views/projections implemented natively inside each domain crate implementing `nuo_tool::Tool`.
- **`[INV-EXT-01] Long-Tail Tool Extension Boundary`**: Long-tail, vendor-specific, and external integrations MUST NOT be introduced as workspace crates or dumped into generic tool containers. They must interface through standard MCP (`nuo-mcp`).

---

## Positive Consequences

- **Cognitive Symmetry**: Uniform crate hierarchy across all domains (`nuo-host`, `nuo-web`, `nuo-code`, `nuo-acp`, `nuo-persistence`).
- **Phonetic Clarity**: Eliminates confusion between `nuo-tool` and `nuo-tools-*`.
- **Compile-Time Efficiency**: Retains physical crate decoupling so Tree-sitter C bindings and HTTP scrapers remain isolated from the core harness.
- **Architectural Immunity**: Prevents regression into utility/junk-drawer anti-patterns.

---

## Negative Consequences & Trade-offs

- **Workspace Path Realignment**: Requires moving directories `nuo-tools-web` $\rightarrow$ `nuo-web` and `nuo-tools-code` $\rightarrow$ `nuo-code`, updating root `Cargo.toml` and downstream import paths.

---

## Rejected Alternatives & Negative Knowledge

### Catch-All `nuo-tools` Mega-Crate (Rejected)
- **Why considered**: Consolidating web and code tools into a single `nuo-tools` crate with feature flags (`features = ["web", "code"]`) reduces workspace member count.
- **Why rejected**: Creates a dangerous "junk drawer" anti-pattern. Developers would inevitably dump unclassified or prototype tools into `nuo-tools` rather than identifying clean domain boundaries. Furthermore, `nuo-tools` phonetically collides with the canonical `nuo-tool` contract crate.

### Splitting Separate `nuo-tools-acp`, `nuo-tools-host` Crates (Rejected)
- **Why considered**: Enforcing strict physical separation between domain engines and tool adapters across all modules.
- **Why rejected**: Extreme over-engineering. Tool adapters are thin facades (~100–300 lines) that map JSON arguments to domain methods. Splitting them into standalone crates forces domain crates to expose private internals, doubles crate maintenance overhead, and complicates SemVer management without providing any isolation benefit.

### Retaining `nuo-tools-web` and `nuo-tools-code` Asymmetry (Rejected)
- **Why considered**: Avoids renaming directories and updating package references.
- **Why rejected**: Preserves an unprincipled historical artifact of ADR-0010 decomposition. It leaves contributors confused about why some tools are embedded in domain crates while others live in prefixed satellite packages.
