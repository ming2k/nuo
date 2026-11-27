//! Declared context sources: the compile-checked prompt policy (ADR-0215).
//!
//! A source declares its topic, the inputs it reads, an optional ordering
//! relation, an optional supersede list, and a mandatory budget. Region,
//! stability band, and rank are *derived* from those declarations, so an illegal
//! combination cannot be written down and a duplicated topic cannot compile.
//!
//! The request path takes a compiled [`Policy`] and is total: layout is proven
//! once, at agent construction, never per request.
pub mod builtin;
pub mod inputs;
pub mod inspect;
pub(crate) mod registry;
pub mod source;

pub use builtin::{default_policy, default_sources};
pub use inputs::{ContextInputs, InputKey, ToolSurface};
pub use registry::{ContextPolicyError, Policy};
pub use source::{ContextSource, SourceOrder};

#[cfg(test)]
mod tests;
