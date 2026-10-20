---
id: ADR-0061
title: "Pure Thread Topology, Headless Unified Entry, and Unburdened CLI Architecture"
status: accepted
date: 2026-11-01
scope: interface/cli, presentation/tui, domain/thread, architecture/topology
superseded_by: null
negative_knowledge: true
---

# 0061. Pure Thread Topology, Headless Unified Entry, and Unburdened CLI Architecture

- Status: Accepted
- Date: 2026-11-01
- Deciders: Nuo Architecture Working Group
- Consulted: Interface, TUI Presentation, Runtime, Developer Experience
- Informed: System Architects, Release Engineering
- Amends: [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md)

---

## Context and Problem Statement

Following the initial CLI consolidation under [ADR-0048](0048-cli-architecture-modernization-resource-symmetry-and-application-observability-topology.md), continuous usage and user feedback exposed residual architectural debt, cognitive impedance, and scope pollution across the command-line surface:

1. **The Semantic Distortion of `run`**:
   `nuo run <prompt>` suggested daemon orchestration or script execution (akin to `docker run` or `cargo run`), creating false mental models against `nuo server start`. In reality, `run` was merely a headless, one-shot turn streaming to stdout.
2. **Pseudo-CRUD Bloat (`config`, `mcp`, `skill`)**:
MCP and environment configuration transitioned to declarative-only TOML specifications. Mutations from the CLI were retired, leaving `nuo mcp`, `nuo skill`, and `nuo config` as vestigial read-only inspect commands. These cluttered top-level discovery, duplicated the TUI's dynamic `/settings` scene, and fragmented system observability away from `nuo status`.
3. **Absence of an Interactive Thread Recovery Workflow**:
   While `nuo thread ls` rendered a static tabular summary and `--resume` blindly restored the last session without discrimination, users lacked a first-class affordance to launch the interactive TUI directly into the thread history picker to select and restore specific past threads.
4. **Option Scope Pollution and Categorical Bureaucracy**:
   Top-level `--help` suffered from option scope leakage: flags applicable solely to subcommands (such as `-j, --json` belonging to `status` or headless output) were displayed globally, while artificial sectioning (`Global Options` vs `Session Options`) added visual noise without grammatical precision.

We require an uncompromising, forward-facing CLI architecture that establishes the root binary as a unified terminal/headless agent entry point, promotes `thread` as the sole interactive and administrative resource noun, purges redundant top-level commands, and provides clean, honest option scoping.

---

## Decision Drivers

- **Domain Nomenclature Purity (`[INV-CLI-06]`)**: All conversational state is strictly identified as `thread`. Deprecated `session` nomenclature and ambiguous singular verbs (`resume`) are forbidden; thread browsing and recovery must route through `nuo thread` or `-t, --thread [ID]`.
- **Root-Unified Headless Entry (`[INV-CLI-07]`)**: `run` is permanently abolished as a subcommand. Headless one-shot execution is driven directly from the root command via `--headless` or standard `-p, --prompt` flags.
- **Top-Level Cleanliness and Declarative Purity (`[INV-CLI-08]`)**: Top-level commands are strictly constrained to core lifecycle and observability (`thread`, `server`, `status`). Configuration, MCP, and skill inspection belong to declarative files, the interactive `/settings` TUI scene, and unified `nuo status`.
- **Option Scope Integrity (`[INV-CLI-09]`)**: Options must bind strictly to the command node they configure. Pseudo-global categorizations are eliminated in favor of a flat, honest `Options:` topology on the root command, with subcommand-specific options segregated to their respective help surfaces.

---

## Considered Options

### Option 1: Incremental Patching with Deprecation Warnings
- Keep `run`, `config`, `mcp`, and `skill` as top-level subcommands with deprecation warnings. Add an ad-hoc `nuo resume` verb.
- *Failure Mode*: Rejected (`[INV-AGENT-01]`). Perpetuates legacy baggage, preserves top-level visual noise, leaves `run` vs `server start` confusion intact, and adds another ambiguous singular verb (`resume`) that conflicts with the multi-thread domain model.

### Option 2: Rigid Subcommand-Only Structure (`nuo agent run ...`)
- Force all agent interactions into an explicit subcommand (`nuo agent ...` or `nuo chat ...`), stripping bare `nuo` of default interactive execution.
- *Failure Mode*: Rejected (`[INV-AGENT-01]`). Destroys terminal-first ergonomic speed. Developers expect bare `nuo` to launch the interactive workspace immediately without unnecessary ceremony.

### Option 3: Modernized Root Agent Entry, Interactive Thread Recovery, and Flat Help Topology (Chosen)
- Eliminate `run`; root `nuo` executes interactive TUI by default, or headless turn when invoked with `--headless` or `-p, --prompt`.
- Promote interactive thread recovery: `nuo -t` / `nuo --thread` (without ID) and bare `nuo thread` directly open the interactive TUI with `StartupOverlay::SessionsPicker`. Supplying an ID (`nuo -t <id>`) connects directly to that thread.
- Retire top-level `config`, `mcp`, and `skill` subcommands, routing environmental discovery to `nuo status`.
- Eliminate `Global Options` sectioning in `--help`, presenting a unified, honest `Options:` list.

---

## Decision Outcome

Chosen option: **Option 3**.

### 1. Unified Root Entrypoint and Headless Flag
- Bare `nuo`: Launches interactive TUI in fresh thread.
- `nuo "prompt"`: Launches interactive TUI with initial prompt queued.
- `nuo --headless "prompt"` / `nuo -p "prompt"`: Executes headless one-shot turn and streams to stdout.
- Subcommand `run` is completely removed from the command table.

### 2. Interactive Thread Recovery Topology
- `nuo -t` / `nuo --thread`: Launches TUI directly into the Thread Picker dialog (`StartupOverlay::SessionsPicker`).
- `nuo -t <id>` / `nuo --thread <id>`: Launches TUI attached directly to the designated thread.
- `nuo thread`: Without subcommands, defaults to interactive Thread Picker launch (identical to `nuo -t`).
- `nuo thread ls`: Emits table of threads to stdout.
- `nuo thread rm <id>`: Terminates and removes a hosted thread.

### 3. Retirement of Top-Level Pseudo-CRUD
- `nuo config`, `nuo mcp`, and `nuo skill` are removed from the active top-level command catalog.
- If invoked, the CLI responds with an actionable error directing users to declarative configuration, `nuo status`, or the `/settings` TUI scene.

### 4. Honest, Flat Help Topology
- Help surfaces expose `Usage:`, `Commands:`, and flat `Options:` without artificial `Global Options` banners.
- `-j, --json` is removed from root options and scoped strictly to `status` and `thread ls`.

---

## Invariants Ratified

- `[INV-CLI-06] Pure Thread Noun`: The conversational entity is exclusively named `thread`.
- `[INV-CLI-07] Headless Root Unified`: Headless turns are driven by `--headless` / `-p` on the root binary; `run` does not exist as a subcommand.
- `[INV-CLI-08] Minimal Subcommand Surface`: Top-level subcommands are limited to `thread`, `server`, and `status`.
- `[INV-CLI-09] Flat Option Scoping`: Help templates shall not use redundant `Global Options:` headers when options are directly valid on the command entrypoint.
