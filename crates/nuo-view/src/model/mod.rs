//! Semantic data model: the parsed document, the rendered-cell layout map, and
//! selection state. These are the data the drawing tree reads and paints into
//! the engine grid; they hold no drawing logic of their own.

pub mod document;
pub mod interactive;
pub mod layout;
mod markdown;
pub mod selection;
