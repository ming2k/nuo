//! The context source declaration.
//!
//! A source states its topic, the inputs it reads, an optional ordering relation,
//! an optional supersede list, and a mandatory budget. Region, band position, and
//! rank are *derived* from those declarations (ADR-0215).
use nuo_protocol::{Block, Budget, Rank, Stability, Topic};

use super::inputs::{ContextInputs, InputKey};

/// Relative placement of a source inside its position class and stability band.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceOrder {
    /// First within its class and band.
    Head,
    /// Before the given source id. The target must be in the same class and band.
    Before(&'static str),
    /// After the given source id. The target must be in the same class and band.
    After(&'static str),
    /// Last within its class and band (default).
    #[default]
    Tail,
}

/// One declared context source.
///
/// Implementations are pure: `block` and `is_active` read `inputs` only
/// (`[INV-CTXSRC-08]`). The source memo is keyed on the inputs hash, so an
/// impure read would silently serve a stale body.
pub trait ContextSource: Send + Sync {
    /// Stable identity for overrides, disables, telemetry, and `supersedes`.
    /// Convention: `<origin>.<name>`, for example `"project.rules"`.
    fn id(&self) -> &'static str;

    /// What this source is about. One active owner per topic
    /// (`[INV-CTXSRC-01]`).
    fn topic(&self) -> Topic;

    /// Every input this source reads, in `is_active` or in `render`.
    ///
    /// Declaring an input the source does not read is harmless; reading an input
    /// it does not declare is a bug the stability derivation cannot see, so
    /// `[INV-CTXSRC-03]`'s test probe checks it.
    fn inputs(&self) -> &'static [InputKey];

    /// Placement within the source's class and band.
    fn order(&self) -> SourceOrder {
        SourceOrder::Tail
    }

    /// Sources this one replaces when both are active.
    ///
    /// A same-topic pair must declare a winner in at least one direction; a pair
    /// that names each other is `UnresolvedSupersede`.
    fn supersedes(&self) -> &'static [&'static str] {
        &[]
    }

    /// Whether the source applies to this request.
    fn is_active(&self, inputs: &ContextInputs) -> bool {
        let _ = inputs;
        true
    }

    /// Declare this source's block. `None` means "active but no text this turn".
    fn block(&self, inputs: &ContextInputs) -> Option<Block>;

    /// The size bound, with its overflow policy. Mandatory: an unbounded source
    /// is the defect this trait exists to prevent (`[INV-CTXSRC-02]`).
    fn budget(&self) -> Budget;
}

/// A source's compile-time facts, resolved once by `Policy::compile`.
///
/// Kept beside the source rather than inside it: region, band, and rank are
/// *derived* from the declaration, never stored on the source, so a source
/// cannot disagree with the policy that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceMeta {
    pub(crate) id: &'static str,
    pub(crate) topic: Topic,
    pub(crate) rank: Rank,
    pub(crate) stability: Stability,
}

impl SourceMeta {
    /// Derive the facts of one declared source.
    ///
    /// Stability is the maximum over the declared input keys: a source cannot
    /// claim more stability than its inputs have (`[INV-CTXSRC-03]`).
    pub(crate) fn derive(id: &'static str, topic: Topic, keys: &[InputKey]) -> Self {
        Self {
            id,
            topic,
            rank: topic.rank(),
            stability: keys
                .iter()
                .map(|key| key.stability())
                .max()
                .unwrap_or(Stability::Frozen),
        }
    }
}
