//! The measured context plan: what one request actually costs, block by block.
//!
//! [`ContextPlan`] is the single value an inspector renders and the renderer
//! consumes, so the table an operator reads and the text the model receives
//! cannot diverge ([`INV-CTXPLAN-01]`, ADR-0217).
use serde::{Deserialize, Serialize};

use crate::instructions::Block;
use crate::{Position, Rank, Stability, Topic};

/// How the model reaches bytes a source had to drop.
///
/// Every lossy projection names one of these; an elision without a recovery
/// route is a silent loss and is rejected at policy compile time.
///
/// `Tool.tool` is an owned name, not a `&'static str`: this type is part of the
/// inspection wire (ADR-0220), and a wire value cannot carry a lifetime. The one
/// place that names a tool is a budget constructor, so the allocation is paid
/// once per compiled source, not per request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resume {
    /// A workspace file the model can read from a line offset.
    File { path: String, from_line: usize },
    /// A tool call that returns the remainder.
    Tool { tool: String },
    /// An `@`-addressable asset.
    Mention { address: String },
    /// No in-workspace route exists: the notice names the source and asks the
    /// operator to shorten it. Legal only for configuration-only inputs.
    Operator,
}

impl Resume {
    /// The recovery instruction rendered into the notice.
    pub fn instruction(&self) -> String {
        match self {
            Self::File { path, from_line } => {
                format!("read `{path}` from line {from_line} for the rest")
            }
            Self::Tool { tool } => format!("call `{tool}` for the remainder"),
            Self::Mention { address } => format!("reference `{address}` for the remainder"),
            Self::Operator => "ask the operator to shorten the source".to_string(),
        }
    }
}

/// What a source does when its body exceeds its [`Budget`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Overflow {
    /// Truncate at a structural boundary and append a notice naming the loss and
    /// its recovery route.
    Elide { resume: Resume },
    /// Replace the body with a one-line index entry the model can dereference.
    Index { resume: Resume },
    /// Omit the element entirely. Legal only where the prompt models the absence
    /// (for example an empty catalog).
    Drop,
}

/// A declared size bound. Token-denominated: bytes answer a transport question,
/// never a context question (`nuo_protocol::pressure::estimate_bytes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub tokens: usize,
    pub overflow: Overflow,
}

impl Budget {
    /// A budget that elides at a file offset.
    pub fn elide_to_file(tokens: usize, path: impl Into<String>) -> Self {
        Self {
            tokens,
            overflow: Overflow::Elide {
                resume: Resume::File {
                    path: path.into(),
                    from_line: 0,
                },
            },
        }
    }

    /// A budget that elides with an operator-visible notice only.
    pub fn elide_to_operator(tokens: usize) -> Self {
        Self {
            tokens,
            overflow: Overflow::Elide {
                resume: Resume::Operator,
            },
        }
    }

    /// A budget that replaces the body with a tool-derived index.
    pub fn index_to_tool(tokens: usize, tool: impl Into<String>) -> Self {
        Self {
            tokens,
            overflow: Overflow::Index {
                resume: Resume::Tool { tool: tool.into() },
            },
        }
    }
}

/// What a source lost and how to get it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Elision {
    /// Tokens the full body would have cost.
    pub full_tokens: usize,
    /// Tokens the emitted body costs.
    pub kept_tokens: usize,
    pub resume: Resume,
}

impl Elision {
    /// Tokens removed by this elision.
    pub fn dropped_tokens(&self) -> usize {
        self.full_tokens.saturating_sub(self.kept_tokens)
    }
}

/// A machine-readable account of one lossy or partial decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextNotice {
    /// The source that produced the notice.
    pub source_id: String,
    pub kind: NoticeKind,
    /// Rendered model-visible text, already appended to the block body.
    pub message: String,
}

/// Why a notice exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// The body was truncated to its budget.
    Elided,
    /// The body was replaced by an index entry.
    Indexed,
    /// The source was dropped by its own overflow policy.
    Dropped,
    /// The source was dropped to satisfy the instruction-region ceiling.
    CeilingEvicted,
    /// A bounded enumeration showed fewer entries than exist.
    Truncated,
}

/// One block as the assembler measured it.
///
/// Inactive sources are retained with a reason: "why is this block not in my
/// prompt" is the most common debugging question, and an absent row cannot
/// answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBlock {
    /// Stable source id.
    pub id: String,
    pub topic: Topic,
    pub rank: Rank,
    pub position: Position,
    pub stability: Stability,
    /// Measured cost of the emitted body. Zero when inactive.
    pub tokens: usize,
    /// Set when the body was reduced.
    pub elision: Option<Elision>,
    /// Whether the source contributed text to this request.
    pub active: bool,
    /// Why an inactive source contributed nothing.
    pub inactive_reason: Option<&'static str>,
}

impl PlannedBlock {
    /// An inactive row, so the inspection table can explain an absence.
    pub fn inactive(
        id: impl Into<String>,
        topic: Topic,
        stability: Stability,
        reason: &'static str,
    ) -> Self {
        Self {
            id: id.into(),
            topic,
            rank: topic.rank(),
            position: topic.position(),
            stability,
            tokens: 0,
            elision: None,
            active: false,
            inactive_reason: Some(reason),
        }
    }
}

/// The persistable projection of one planned block.
///
/// Deliberately narrower than [`PlannedBlock`]: durable records carry what
/// answers an operational question, not the rendered text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedBlockRecord {
    pub id: String,
    pub topic: Topic,
    pub rank: Rank,
    pub stability: Stability,
    pub tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inactive: bool,
}

impl From<&PlannedBlock> for PlannedBlockRecord {
    fn from(planned: &PlannedBlock) -> Self {
        Self {
            id: planned.id.clone(),
            topic: planned.topic,
            rank: planned.rank,
            stability: planned.stability,
            tokens: planned.tokens,
            dropped_tokens: planned.elision.as_ref().map(Elision::dropped_tokens),
            inactive: !planned.active,
        }
    }
}

/// How the cacheable prefix moved since the previous request on this thread.
///
/// [`Self::Head`] is a defect signal: a frame or `Frozen` block changed while the
/// route did not, which means a source is impure, the ordering is unstable, or a
/// workspace value was classified too stably.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrefixDrift {
    /// Byte-identical prefix.
    #[default]
    None,
    /// Only `Session`/`Workspace`/`Turn` blocks moved.
    Suffix { blocks: Vec<String> },
    /// A frame or `Frozen` block changed with no route change. A defect.
    Head { blocks: Vec<String> },
    /// The identity/route changed, so a new prefix is expected.
    Route,
}

impl PrefixDrift {
    /// Whether this drift is an unexplained defect.
    pub fn is_defect(&self) -> bool {
        matches!(self, Self::Head { .. })
    }
}

/// One request's measured context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextPlan {
    /// Every declared source, active or not, in emission order.
    pub blocks: Vec<PlannedBlock>,
    /// The rendered instruction region.
    pub fixed: Vec<Block>,
    pub fixed_tokens: usize,
    pub notices: Vec<ContextNotice>,
    pub drift: PrefixDrift,
}

impl ContextPlan {
    /// Whether any declared source contributed nothing this request.
    pub fn has_inactive(&self) -> bool {
        self.blocks.iter().any(|block| !block.active)
    }

    /// The planned row for one source id.
    pub fn block(&self, id: &str) -> Option<&PlannedBlock> {
        self.blocks.iter().find(|block| block.id == id)
    }
}
