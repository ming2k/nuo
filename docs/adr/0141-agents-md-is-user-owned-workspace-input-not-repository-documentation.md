---
id: ADR-0141
title: "`AGENTS.md` Is User-Owned Workspace Input, Not Repository Documentation"
status: accepted
date: 2026-12-19
scope: architecture/governance, docs/dev, agent/directives, host/nuo-host, host/nuo
superseded_by: null
negative_knowledge: true
---

# 0141. `AGENTS.md` Is User-Owned Workspace Input, Not Repository Documentation

- Status: Accepted
- Date: 2026-12-19
- Deciders: Nuo Architecture Working Group
- Consulted: Developer Experience, Host/Security, Documentation Governance
- Amends: [ADR-0067](0067-system-invariant-governance-consolidation-and-pseudo-tag-retirement.md) — §2 ("Streamlining `AGENTS.md`") made repository documentation the *author* of the file's structure; that authorship is withdrawn here while the ADR's docgov decoupling stands. [ADR-0055](0055-hierarchical-workspace-topology-and-component-namespace.md) — `[INV-TOPO-04]` root cleanliness keeps its prohibition on library crates at the root, but "governance metadata" is no longer a category that can absorb an agent-guidance file.

---

## Context and Problem Statement

`AGENTS.md` accumulated two mutually exclusive identities, and nothing in the repository detected the conflict.

**Identity A — user-owned workspace input.** The shipped product treats the file as untrusted content supplied by whoever owns the workspace:

- `nuo_host::security::PROJECT_RULE_FILES` lists `AGENTS.md` beside `.cursorrules` and `.windsurfrules` as project-authored prompt input.
- `WorkspaceTrustDomain::Instructions` gates it: its content is digested, a change on disk quarantines it, and `/trust deny instructions` refuses it without revoking other grants.
- `nuo_server::project::init_nuo_config` *generates a starter template into the user's project* and then leaves it alone.

In that model the file belongs to the user running Nuo, and the repository has no standing to dictate its shape.

**Identity B — repository doctrine.** Simultaneously, repository documentation bound the file normatively:

- ADR-0067 §2 instructed how `AGENTS.md` must be "re-architected", and its Links section carried the only real markdown hyperlink to it.
- ADR-0094 and ADR-0126 justified decision drivers by citing a section of the agent directive file as an authority (`AGENTS.md §3`).
- ADR-0105 and ADR-0106 stated that named architectural red-lines "are stated as red-lines in `AGENTS.md`", making the file the citation target for invariants that live in ADRs.
- ADR-0022 pointed readers at "`AGENTS.md` machine invariants" for `[INV-AGENT-01]`/`[INV-AGENT-02]`.
- `docs/dev/standards/taxonomy.md` §3 admitted the file to the repository root whitelist as documentation.

### Three concrete failures this produced

1. **The citation chain was already dead.** `[INV-AGENT-01]` and `[INV-AGENT-02]` had been defined only inside the agent directive file; commit `65cd815` (ADR-0067) deleted the definitions and rewrote the file, but 22 documents still cite them: 27 occurrences of `[INV-AGENT-01]` (incl. `index.md`) and 3 of `[INV-AGENT-02]`, none of them resolving to a definition. No gate noticed: the ADR lineage checker matches `ADR-\d{4}` only, and `INV-` tokens have no gate at all.
2. **The user's file was subject to a documentation judgment.** `nuo::dev::doc_links` scanned every root-level `*.md` (`rel_str.starts_with("docs/") || !rel_str.contains('/')`), so `AGENTS.md` inherited target-existence, absolute-locator, and anchor rules. A user rewriting their own agent guidance could fail `cargo test -p nuo --test doc_governance` for a link the repository never authored.
3. **The normative references were structurally unverifiable.** `AGENTS.md §3` is prose, not a link: `doc_links` strips inline code before matching, so a section-number citation can never be checked and silently rots when the file is reordered.

The root cause is not sloppy editing. It is that nothing distinguished *artifact under documentation governance* from *runtime input*, and no mechanism prevented documentation from binding the second kind.

---

## Decision Drivers

1. **Trust boundaries follow the product, not the paperwork.** The trust gate already declares `AGENTS.md` untrusted workspace content. Repository governance that treats it as doctrine contradicts the shipped security model — the same file cannot be both an attested input and a normative source.
2. **Mechanisms over exhortations** (invariants.md §1). A convention that documentation "should not" cite the file is worth nothing; the boundary must be a gate that fails the build.
3. **A user-owned asset must not be gated by its host repository.** Migration friction on a file the user owns is a defect regardless of how tidy the resulting docs are.
4. **Dead citation chains are worse than no citation.** Once the definitions left `AGENTS.md`, the remaining `[INV-AGENT-01]` sites were unresolvable references masquerading as binding red-lines.

---

## Architectural Decisions

### 1. Ownership is declared by exclusion, not by description

`nuo::dev::doc_links` owns a `USER_OWNED_SURFACES` constant listing root files that are **not** repository documentation. `AGENTS.md` is the sole entry. Excluded files are dropped from the documentation file set entirely, so they are never subject to link, anchor, or absolute-locator rules.

This is deliberately a *scope exclusion* rather than a rule exemption: exemptions invite "except for this case" growth, while scope exclusion states the file was never in the governed set.

### 2. The coupling is closed in the other direction by two decidable rules

The same checker applies two rules to every governed documentation line:

1. **Nomination.** A user-owned surface must be named as an inline code span. A bare filename in prose is rejected, because a filename that is not marked as a filename reads as an authority claim.
2. **Citation.** A code-styled mention qualified by a section number (for example the string `AGENTS.md §3`) is rejected. Code styling must not launder a citation of the file's contents.

The obvious implementation — strip inline code, then match — is wrong in both directions, and this ADR was first written with it:

- It **let the real defect through**. The rotted coupling appeared as `` `(AGENTS.md §3)` `` — a section citation whose filename merely *looked* like code; stripping the code span removed the only token the checker looked for, so the most important instance of the anti-pattern passed.
- It **punished legitimate mentions**. Product documentation that says "create `AGENTS.md` at the root" names a filename, not an authority, and must not fail a documentation gate.

The rules are therefore evaluated over the raw line with knowledge of where code spans are, and are orthogonal: nomination is about *how the file is named*, citation is about *whether its contents are being bound*. Discussing the anti-pattern itself remains possible by quoting the whole citation as a single code span, which is how this ADR and the `adr.md`/`taxonomy.md` rule text state it.

Enforcement is pinned by unit tests in `crates/nuo/src/dev/doc_links.rs`:
`user_owned_agents_md_is_outside_doc_governance`, `bare_mention_is_rejected_with_an_actionable_message`,
`code_styled_section_citation_is_rejected`, `code_styled_filename_is_permitted_in_prose_and_tables`,
`quoting_agents_md_in_code_is_permitted`.

### 3. Repository doctrine moves back into the standards suite

Every principle that was cited as a section of the agent directive file already has a canonical, gate-protected statement in `docs/dev/standards/`:

| Former citation | Canonical home |
| :--- | :--- |
| `AGENTS.md §3` "mechanism over exhortation" | [`invariants.md`](../dev/standards/invariants.md) §1 (Core Principle: Mechanisms Over Exhortations) |
| "negative knowledge / rejected alternatives" | [`adr.md`](../dev/standards/adr.md) §3 (template) and §4 (reference resolution) |
| "blameless postmortems" | [`postmortems.md`](../dev/standards/postmortems.md) §1 |
| "context routing / superseded is not active design" | [`adr.md`](../dev/standards/adr.md) §2 (in-place lifecycle) |

Repository documents now cite those, not the user's file.

### 4. `AGENTS.md` in this repository is ordinary agent guidance

The root `AGENTS.md` is maintained by the people who work in this repository, for the agents they run here. It is a peer of any workspace's agent guidance, not a specification the Standard Suite derives from. `taxonomy.md` §3 continues to permit the file at the root — that is a cleanliness rule about *location*, not a claim of documentation authority.

### 5. Undefined invariant tags are retired, not resurrected

The two dangling tags are removed from their last surviving citation sites rather than re-minted:

- `[INV-AGENT-01]` ("negative knowledge") — the requirement is enforced by [`adr.md`](../dev/standards/adr.md) §3, and by this ADR's own Negative Knowledge section. Re-minting the tag would re-create the coupling this ADR removes.
- `[INV-AGENT-02]` ("context routing / Chesterton's Fence") — the requirement is [`adr.md`](../dev/standards/adr.md) §2's lifecycle: a `superseded` record is read as a constraint, not as an active design.

`[INV-AGENT-05]`, `[INV-AGENT-06]`, and `[INV-AGENT-07]` are **not** affected: they are defined in [ADR-0009](0009-single-agent-runtime.md) and [ADR-0010](0010-harness-decomposition-and-agent-unification.md) and remain resolvable.

---

## Consequences

### Positive

- **The trust model and the documentation model agree.** The file the product treats as untrusted input is no longer a normative source for the repository that ships the product.
- **Users can restructure their own agent guidance.** No documentation gate judges a workspace-owned file.
- **The boundary is mechanical.** A future edit that re-introduces "stated in `AGENTS.md`" fails `cargo test -p nuo --test doc_governance`, not a review conversation.
- **30 dead tag citations collapse into resolvable standards references.** Every remaining claim points at a document that exists, owns the rule, and is itself link-and-anchor checked.
- **Ambiguous citations become checkable ones.** `AGENTS.md §3` — unverifiable prose — is replaced by `invariants.md` §1, whose heading anchor `doc_links` validates.

### Negative & Trade-offs

- **One more thing to know.** Contributors must cite `docs/dev/standards/` rather than the file that agents read first. This is the cost of removing an authority claim, and it is paid once per citation.
- **Historical ADR prose is rewritten.** ADR-0067 §2 and the driver/context sentences in ADR-0022/0094/0105/0106 are edited in place. Because these records are `accepted`, the edits are scoped to withdrawing the authorship claim; no decision outcome is altered, and each site keeps its technical reasoning verbatim.

---

## Rejected Alternatives & Negative Knowledge

### 1. Keep the prose, add a warning comment in `AGENTS.md`

- **Why considered**: Zero churn; a comment such as "documentation may cite this file" would signal intent.
- **Why rejected**: This is precisely the exhortation that `invariants.md` §1 forbids. The dead `[INV-AGENT-01]` chain existed *because* the coupling was maintained by prose; a comment cannot fail a build.

### 2. Sanitize the root file instead of excluding it

- **Why considered**: Keep `AGENTS.md` inside the documentation set and relax only the rules it trips.
- **Why rejected**: Per-rule exemptions grow. The file would still be *in* the governed set, which is the actual defect: it would remain eligible for any future rule added to `check_doc_links`. Exclusion is one decision, not a set of exceptions.

### 3. Re-mint `[INV-AGENT-01]`/`[INV-AGENT-02]` inside `invariants.md`

- **Why considered**: Preserve the existing 30 citation sites unchanged; simply restore a definition so the tags resolve again.
- **Why rejected**: It would preserve the semantic debt. The "invariants" in question are authoring-process rules (`write a rejected-alternatives section`), which `invariants.md` §2 classifies as **Implementation Note / process policy, not System Invariants** — minting `INV-` identifiers for them is exactly the tag inflation ADR-0067 retired. The right fix is a citation to the standards document that already owns the rule.

### 4. Leave `AGENTS.md` in the root whitelist as "governance metadata"

- **Why considered**: `taxonomy.md` §3 already lists it, and root location is a real cleanliness concern.
- **Why rejected**: *Location*. The whitelist governs where a file may live; ADR-0067's `[INV-TOPO-04]` reading of "governance metadata" was doing double duty as an *authority* claim. §3 keeps the location rule and loses the authority claim — the file stays at the root because users expect it there, not because the repository wrote it.

---

## Links

- Related Standards: [Documentation Taxonomy](../dev/standards/taxonomy.md) §3, [Invariant Standards](../dev/standards/invariants.md) §1, [ADR Standards](../dev/standards/adr.md) §2–§4, [Blameless Postmortems](../dev/standards/postmortems.md) §1
- Related ADRs: [ADR-0067](0067-system-invariant-governance-consolidation-and-pseudo-tag-retirement.md) (docgov decoupling, tag anti-inflation), [ADR-0127](0127-workspace-trust-review-obligation-request-lifecycle-and-consistent-refusal.md) (the trust gate that makes `AGENTS.md` untrusted input), [ADR-0055](0055-hierarchical-workspace-topology-and-component-namespace.md) (root cleanliness), [ADR-0090](0090-autonomous-persistence-resilience-transparent-lease-handoff-and-zero-plaintext-credentials.md) (the `[INV-PERSIST-LOCK-01]` citizenship pattern this ADR follows)
- Enforcement: `nuo::dev::doc_links` (`USER_OWNED_SURFACES`, user-owned-surface citation check), `cargo test -p nuo --test doc_governance`
