//! Request instruction text and block composition.
//!
//! An assembled request has two text regions: `fixed` (route-stable) and
//! `transient` (request-local). [`Block`] is the assembly-time fragment used to
//! build those regions; [`InstructionText`] is the rendered text carried by a
//! model request. Tools and messages are protocol fields and never enter these
//! blocks.
use serde::{Deserialize, Serialize};

/// Boundary choice for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRender {
    /// Wrap the block in an XML envelope with the given tag.
    Xml(&'static str),
    /// Emit the title line and body without an envelope.
    Plain,
}

/// One fragment of the `fixed` or `transient` request text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Stable identity for deduplication, override, and telemetry.
    pub id: String,
    /// Optional human-readable label, rendered as a plain title line.
    pub title: Option<String>,
    /// Optional provenance (for example `"AGENTS.md"`).
    pub source: Option<String>,
    /// Semantic content only. Body syntax (Markdown, prose, code) is preserved.
    pub body: String,
    /// Boundary choice.
    pub render: BlockRender,
}

impl Block {
    pub fn new(id: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: None,
            source: None,
            body: body.into(),
            render: BlockRender::Plain,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn with_render(mut self, render: BlockRender) -> Self {
        self.render = render;
        self
    }
}

/// Rendered instruction text for a model request.
///
/// `fixed` is the route-stable prefix. `transient` is request-local text that
/// must be emitted after the final cache breakpoint. Both are already formatted;
/// providers project them to their instruction channel without further
/// concatenation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionText {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fixed: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transient: String,
}

impl InstructionText {
    pub fn new(fixed: impl Into<String>, transient: impl Into<String>) -> Self {
        Self {
            fixed: fixed.into(),
            transient: transient.into(),
        }
    }

    /// One text region, no transient content.
    pub fn fixed(text: impl Into<String>) -> Self {
        Self {
            fixed: text.into(),
            transient: String::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fixed.trim().is_empty() && self.transient.trim().is_empty()
    }

    pub fn has_transient(&self) -> bool {
        !self.transient.trim().is_empty()
    }

    pub fn render_fixed(&self) -> &str {
        &self.fixed
    }

    pub fn render_transient(&self) -> &str {
        &self.transient
    }
}

/// Render blocks into one deterministic text region.
///
/// This is the single formatter for `fixed` and `transient` text. It owns the
/// title line, XML envelope, and block spacing so sections never carry their
/// own leading newlines or headings.
pub fn render_blocks(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        let normalized = block.body.replace("\r\n", "\n");
        let body = normalized.trim();
        if body.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        if let BlockRender::Xml(tag) = block.render {
            out.push('<');
            out.push_str(tag);
            if let Some(source) = &block.source {
                out.push_str(" source=\"");
                out.push_str(&escape_attribute(source));
                out.push('"');
            }
            out.push_str(">\n");
        }
        if let Some(title) = &block.title {
            out.push_str(title.trim());
            out.push_str("\n\n");
        }
        out.push_str(body);
        if let BlockRender::Xml(tag) = block.render {
            out.push_str("\n</");
            out.push_str(tag);
            out.push('>');
        }
    }
    out
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The route/role facts an instruction selector is evaluated against.
///
/// A missing fact never matches a set selector: callers that cannot name the
/// route must not accidentally admit route-scoped content.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstructionRoute<'a> {
    pub role: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub connection: Option<&'a str>,
    pub model: Option<&'a str>,
}

/// Scope selectors for one project-authored instruction.
///
/// All set selectors must match. Unset selectors are wildcards. `role`,
/// `provider`, and `connection` are exact (case-insensitive) because they name
/// finite, configured entities; `model` is always a glob pattern, and an exact
/// model id is simply a pattern without wildcards. There is deliberately no
/// numeric priority: matching entries compose in scope order and then in
/// declaration (path) order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InstructionSelector {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// Glob matched against the wire model id (`*` and `?`; `/` is ordinary
    /// text, so `*` spans a vendor prefix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl InstructionSelector {
    /// Whether the selector applies to every route.
    pub fn is_global(&self) -> bool {
        self.role.is_none()
            && self.provider.is_none()
            && self.connection.is_none()
            && self.model.is_none()
    }

    /// Scope rank: broader scopes sort earlier, the model pattern sorts last.
    pub fn scope_rank(&self) -> u8 {
        if self.model.is_some() {
            4
        } else if self.connection.is_some() {
            3
        } else if self.provider.is_some() {
            2
        } else if self.role.is_some() {
            1
        } else {
            0
        }
    }

    /// Number of set selectors; the tie-breaker within one scope rank.
    pub fn set_count(&self) -> usize {
        usize::from(self.role.is_some())
            + usize::from(self.provider.is_some())
            + usize::from(self.connection.is_some())
            + usize::from(self.model.is_some())
    }

    /// Deterministic composition key: broad scopes first, then fewer selectors,
    /// then declaration order (the caller's stable sort keeps that).
    pub fn order_key(&self) -> (u8, usize) {
        (self.scope_rank(), self.set_count())
    }

    /// Human-readable selector summary, used as a user-instruction heading.
    pub fn display_label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(role) = &self.role {
            parts.push(format!("role {role}"));
        }
        if let Some(provider) = &self.provider {
            parts.push(format!("provider {provider}"));
        }
        if let Some(connection) = &self.connection {
            parts.push(format!("connection {connection}"));
        }
        if let Some(model) = &self.model {
            parts.push(format!("model {model}"));
        }
        if parts.is_empty() {
            "global".to_string()
        } else {
            parts.join(" | ")
        }
    }

    /// Reject empty selector values and malformed model patterns at load time.
    pub fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("role", &self.role),
            ("provider", &self.provider),
            ("connection", &self.connection),
        ] {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                return Err(format!("instruction selector `{field}` is empty"));
            }
        }
        if let Some(model) = &self.model {
            if model.trim().is_empty() {
                return Err("instruction selector `model` is empty".to_string());
            }
            glob::Pattern::new(model)
                .map_err(|error| format!("invalid `model` pattern '{model}': {error}"))?;
        }
        Ok(())
    }

    /// Whether this selector applies to `route`.
    pub fn matches(&self, route: &InstructionRoute<'_>) -> bool {
        let exact = |wanted: &Option<String>, actual: Option<&str>| match wanted {
            None => true,
            Some(wanted) => actual.is_some_and(|actual| actual.eq_ignore_ascii_case(wanted)),
        };
        if !exact(&self.role, route.role)
            || !exact(&self.provider, route.provider)
            || !exact(&self.connection, route.connection)
        {
            return false;
        }
        match &self.model {
            None => true,
            Some(pattern) => {
                let Ok(glob) = glob::Pattern::new(pattern) else {
                    return false;
                };
                route.model.is_some_and(|model| {
                    glob.matches_with(
                        &model.to_ascii_lowercase(),
                        glob::MatchOptions {
                            case_sensitive: false,
                            require_literal_separator: false,
                            require_literal_leading_dot: false,
                        },
                    )
                })
            }
        }
    }
}

/// Where a scoped instruction was authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstructionSource {
    /// User configuration (`[[instructions.user]]`), cross-project route tuning.
    User,
    /// Repository files (`.nuo/instructions/**/*.md`), trusted and attested.
    Project,
}

/// One scoped instruction with its scope and provenance.
///
/// `id` is the stable provenance (`user:<n>` or the repository-relative path);
/// `ordinal` is the declaration order within its source (config array index or
/// sorted path order), used as the deterministic tie-breaker after scope rank.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopedInstruction {
    pub id: String,
    pub source: InstructionSource,
    pub ordinal: usize,
    #[serde(default)]
    pub selector: InstructionSelector,
    pub text: String,
}

impl ScopedInstruction {
    /// The attribution heading rendered above the entry.
    pub fn heading(&self) -> String {
        match self.source {
            InstructionSource::Project => self.id.clone(),
            InstructionSource::User => {
                format!("user | {}", self.selector.display_label())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_text_distinguishes_fixed_and_transient() {
        let text = InstructionText::new("fixed text", "transient text");
        assert!(!text.is_empty());
        assert!(text.has_transient());
        assert_eq!(text.render_fixed(), "fixed text");
        assert_eq!(text.render_transient(), "transient text");

        let fixed_only = InstructionText::fixed("only fixed");
        assert!(!fixed_only.is_empty());
        assert!(!fixed_only.has_transient());
    }

    #[test]
    fn render_blocks_is_deterministic_and_owns_presentation() {
        let blocks = vec![
            Block::new("agent.identity", "You are an AI assistant.")
                .with_render(BlockRender::Plain),
            Block::new("project.rules", "Follow project rules.")
                .with_title("Project Rules (AGENTS.md)")
                .with_source("AGENTS.md")
                .with_render(BlockRender::Xml("policy")),
            Block::new("request.transient", "Temporary note.")
                .with_title("Current Turn")
                .with_render(BlockRender::Plain),
        ];
        let rendered = render_blocks(&blocks);
        assert!(rendered.starts_with("You are an AI assistant."));
        assert!(rendered.contains("<policy source=\"AGENTS.md\">"));
        assert!(rendered.contains("Project Rules (AGENTS.md)"));
        assert!(rendered.contains("Follow project rules."));
        assert!(rendered.contains("</policy>"));
        assert!(rendered.ends_with("Current Turn\n\nTemporary note."));
        assert!(!rendered.starts_with('\n'));
        assert!(!rendered.ends_with('\n'));
        assert!(!rendered.contains("\n\n\n"));
    }

    #[test]
    fn empty_blocks_are_skipped() {
        let blocks = vec![Block::new("empty", "   "), Block::new("kept", "body")];
        assert_eq!(render_blocks(&blocks), "body");
    }

    fn route<'a>(
        role: Option<&'a str>,
        provider: Option<&'a str>,
        connection: Option<&'a str>,
        model: &'a str,
    ) -> InstructionRoute<'a> {
        InstructionRoute {
            role,
            provider,
            connection,
            model: Some(model),
        }
    }

    #[test]
    fn instruction_selector_matches_route_facts_and_model_patterns() {
        let global = InstructionSelector::default();
        assert!(global.is_global());
        assert!(global.matches(&route(None, None, None, "any/model")));

        let scoped = InstructionSelector {
            role: Some("Reviewer".into()),
            provider: Some("commandcode-goat".into()),
            connection: Some("cc-fox".into()),
            model: Some("*deepseek*".into()),
        };
        assert!(scoped.matches(&route(
            Some("reviewer"),
            Some("COMMANDCODE-GOAT"),
            Some("cc-FOX"),
            "deepseek/deepseek-v4-flash-fast"
        )));
        // A set selector requires the fact to exist and agree.
        assert!(!scoped.matches(&route(
            None,
            Some("commandcode-goat"),
            Some("cc-fox"),
            "deepseek/x"
        )));
        assert!(!scoped.matches(&route(
            Some("reviewer"),
            Some("commandcode-goat"),
            Some("cc-fox"),
            "gpt-6-sol"
        )));
    }

    #[test]
    fn instruction_selector_ordering_is_broad_to_narrow() {
        let rank = |selector: InstructionSelector| selector.order_key();
        assert_eq!(rank(InstructionSelector::default()), (0, 0));
        assert_eq!(
            rank(InstructionSelector {
                role: Some("ops".into()),
                ..Default::default()
            }),
            (1, 1)
        );
        assert_eq!(
            rank(InstructionSelector {
                provider: Some("openai".into()),
                ..Default::default()
            }),
            (2, 1)
        );
        assert_eq!(
            rank(InstructionSelector {
                connection: Some("cc-fox".into()),
                ..Default::default()
            }),
            (3, 1)
        );
        // The model pattern is the most specific dimension even when combined.
        assert_eq!(
            rank(InstructionSelector {
                connection: Some("cc-fox".into()),
                model: Some("*deepseek*".into()),
                ..Default::default()
            }),
            (4, 2)
        );
    }

    #[test]
    fn instruction_selector_validation_rejects_empty_values_and_bad_patterns() {
        assert!(InstructionSelector::default().validate().is_ok());
        assert!(
            InstructionSelector {
                role: Some("  ".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            InstructionSelector {
                model: Some("[".into()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
