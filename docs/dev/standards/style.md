# Documentation Style & Hyperlink Standards

This document specifies the style guide, markdown conventions, and hyperlink standards for all documentation in the repository.

---

## 1. Hyperlink Standards

1. **Relative Paths**: All cross-references between documentation files must use explicit relative paths (e.g., `[Decision Record](../../adr/0001-flat-workspace-and-microkernel-capability-topology.md)`).
2. **Never Use Absolute Locators**: Never write hardcoded local paths (e.g., `/home/...` or `file://...`) or absolute repo URLs that break across mirrors or offline workflows.
3. **Target Existence**: Every relative link must resolve to a valid file on disk. Dead links or broken targets degrade epistemic trust and fail validation.
4. **Anchor Hygiene**: When linking to section headers, use clean kebab-case anchors matching the heading text.

---

## 2. Formatting & Markdown Conventions

1. **Header Hierarchy**:
   - Exactly one `# Level 1 Heading` per file at line 1.
   - Subsections descend logically: `## Level 2`, followed by `### Level 3`. Avoid skipping heading levels arbitrarily.
2. **Typed Code Blocks**:
   - Always declare an explicit language identifier on fenced code blocks (e.g., ````rust````, ````bash````, ````yaml````, ````text````). Never leave fences untyped.
3. **Lists and Tables**:
   - Use clean Markdown tables with aligned delimiters.
   - For lists, prefer `-` for unordered bullet points and `1.` for sequential procedures.

---

## 3. Tone and Vocabulary

1. **Pragmatic and Objective**: Write in active, direct, and unambiguous language. Avoid marketing hyperbole or unnecessary jargon.
2. **Mechanisms Over Aspirations**: When describing behavioral constraints, describe the concrete enforcing mechanism (compiler proof, type, test assertion, or CI gate) rather than vague prose.
3. **Zero Dead Links**: Never link to non-existent historical tickets or dead internal paths.

---

## 4. Name the Mechanism, Not Its Output

A name is a contract with the next reader. Three rules follow:

1. **Name the role, not the verb the consumer performs.** A field or type that
   selects a presentation is named for what it selects. `boundary: Boundary`
   selects a boundary; `render: BlockRender` describes what a renderer will do
   with it, which makes the instruction indistinguishable from the result.
2. **Never claim a format the producer does not emit.** `Tagged` names the tag
   pair the renderer actually writes. `Xml` claims a declaration, escaping,
   nesting, and CDATA that the same renderer does not produce; a reader who
   believes the name will assume hostile bytes are safe.
3. **Never claim an absence of syntax.** `Bare` says there is no boundary. It
   says nothing about whether the body is Markdown, prose, or code; body syntax
   and boundary choice are orthogonal (`RFC-0002` §6).

These are style rules, not invariants. No compiler or test proves a name; the
testable properties belong in the ADR that owns the type.
