//! Block-level semantic document structures (paragraphs, code, tables).
use super::inlines::Inline;
use super::types::TableAlignment;

/// A single semantic block within a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// Plain text paragraph.
    Text(Inline),
    /// Display math block (`$$…$$` or `\[…\]`).
    Math { content: String },
    /// Inline or fenced code.
    Code {
        language: Option<String>,
        content: String,
    },
    /// A heading.
    Heading { level: u8, inline: Inline },
    /// A list item, preserving its marker and nesting level.
    ListItem {
        inline: Inline,
        ordered: Option<u64>,
        depth: usize,
        checked: Option<bool>,
    },
    /// A blockquote.
    Quote(Inline),
    /// A GFM-style table, kept as a semantic unit so columns stay aligned and
    /// copy yields the rendered grid rather than re-wrapped prose.
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        aligns: Vec<TableAlignment>,
        /// Pre-rendered aligned grid (what is drawn and what copy returns).
        rendered: String,
    },
    /// A horizontal rule.
    Rule,
    /// Soft / hard line break marker.
    Break,
}

impl Block {
    /// Returns the raw text content of this block (without formatting).
    pub fn raw_text(&self) -> &str {
        match self {
            Block::Text(inline) | Block::Quote(inline) => &inline.content,
            Block::Math { content } => content,
            Block::Code { content, .. } => content,
            Block::Heading { inline, .. } => &inline.content,
            Block::ListItem { inline, .. } => &inline.content,
            Block::Table { rendered, .. } => rendered,
            Block::Rule => "",
            Block::Break => "\n",
        }
    }

    /// Returns the inline markup metadata of this block, if it has one.
    pub fn inline(&self) -> Option<&Inline> {
        match self {
            Block::Text(inline)
            | Block::Quote(inline)
            | Block::Heading { inline, .. }
            | Block::ListItem { inline, .. } => Some(inline),
            _ => None,
        }
    }

    /// Returns true if this block is empty.
    pub fn is_empty(&self) -> bool {
        self.raw_text().is_empty()
    }
}
