# Terminal UI (TUI) Reference

Factual reference for the `nuo-tui` terminal interface: the spatial layout of the viewport, the visual language of its text joins, the modal surfaces, and the input-to-render event pipeline.

This surface is the authoritative companion to the source comments in `crates/nuo-tui/`. Several source modules cite these pages directly (`crates/nuo-tui/src/footer_stack.rs`, `crates/nuo-tui/src/design.rs`, `crates/nuo-tui/src/overlays/provider/models.rs`), so the token values and layout contracts here must be kept in sync with `crates/nuo-tui/src/design.rs`.

---

## Pages

| Page | Covers |
| :--- | :--- |
| [Layout](layout.md) | The vertical band composition, the head/footer stacks, and the design tokens that size them. |
| [Visual Language](visual-language.md) | How labels are joined: peer enumeration, breadcrumbs, secondary measures, and cause/reason. |
| [Modals](modals.md) | The overlay/dialog framework, modal sizing presets, and the provider model list's name-first/id-fallback rule. |
| [Interaction](interaction.md) | The input → router → action → apply → render pipeline and hit-testing registry. |

For the cross-domain action matrix (which actions each scene supports), consult the [Lifecycle and Action Semantics Reference](../lifecycle-and-action-semantics.md#3-domain-specific-action-matrix).
