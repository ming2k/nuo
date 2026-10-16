# Documentation Style & Hyperlink Standards

This document specifies the style guide, markdown conventions, and hyperlink standards for all documentation in Nuo.

---

## 1. Hyperlink Standards

1. **Relative Paths**: All cross-references between documentation files must use explicit relative paths (e.g., `[ADR-0005](../adr/0005-unified-binary-and-concentric-runtime-architecture.md)`).
2. **Never Use Absolute File URLs**: Never write hardcoded local paths (e.g., `/home/...` or `file://...`) or absolute repo URLs that break across mirrors or forks.
3. **Target Existence**: Every relative link must resolve to a valid file. Dead links or broken anchors block publication.
4. **Anchor Hygiene**: When linking to section headers, use clean GitHub-flavored kebab-case anchors matching the heading text.

---

## 2. Formatting & Markdown Conventions

1. **Header Hierarchy**:
   - Exactly one `# Level 1 Heading` per file at line 1.
   - Subsections must descend logically: `## Level 2`, followed by `### Level 3`. Never skip heading levels (e.g., `#` directly to `###`).
2. **Code Blocks**:
   - Always declare an explicit language identifier on fenced code blocks (e.g., ````rust````, ````bash````, ````yaml````, ````text````). Never leave fences untyped.
3. **Lists and Tables**:
   - Use clean Markdown tables with aligned delimiters.
   - For lists, prefer `-` for unordered bullet points and `1.` for sequential procedures.

---

## 3. Tone and Vocabulary

1. **Pragmatic and Objective**: Write in active, direct, and unambiguous language. Avoid pseudo-academic jargon or hyperbolic marketing claims.
2. **Mechanisms Over Aspirations**: When describing behavioral constraints, specify the enforcing type, function, or test rather than vague prose.
3. **Zero Dead Links**: Never link to deprecated external repositories or non-existent historical tickets.
