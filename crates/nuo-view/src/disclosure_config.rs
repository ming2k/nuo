//! Pure disclosure policy: the default-expand resolution shared by the
//! transcript and step renderers, decoupled from the shell configuration type.
//!
//! Presentation code must not name the shell's `TuiConfig`; it depends on this
//! narrow, pure [`DisclosureSource`] port instead, and the shell implements it
//! for its configuration ([INV-ARCH-UI-ZERO-IO], ADR-0072). Keeping the
//! resolution here lets the transcript/step modules become pure projection.

/// The `[tui.default_expanded]` key controlling the reasoning-trace default.
pub const THINKING_KEY: &str = "thinking";

/// A read-only source of per-name disclosure defaults. The shell implements it
/// for its configuration so presentation code never depends on the shell type.
pub trait DisclosureSource {
    /// The configured default-expand state for `name`, if the user set one.
    fn disclosure_default(&self, name: &str) -> Option<bool>;
}

impl DisclosureSource for std::collections::HashMap<String, bool> {
    fn disclosure_default(&self, name: &str) -> Option<bool> {
        self.get(name).copied()
    }
}

impl<T: DisclosureSource + ?Sized> DisclosureSource for std::sync::Arc<T> {
    fn disclosure_default(&self, name: &str) -> Option<bool> {
        (**self).disclosure_default(name)
    }
}

/// Effective default-expand state for a tool step: an explicit setting wins;
/// otherwise the declared component default applies; a tool with no declared
/// component (MCP / unknown) stays collapsed.
pub fn tool_expanded(source: &impl DisclosureSource, name: &str) -> bool {
    source.disclosure_default(name).unwrap_or_else(|| {
        crate::tools::component_for(name)
            .map(|component| component.default_expanded())
            .unwrap_or(false)
    })
}

/// Effective default-expand state for a reasoning trace (collapsed by default).
pub fn reasoning_expanded(source: &impl DisclosureSource) -> bool {
    source.disclosure_default(THINKING_KEY).unwrap_or(false)
}
