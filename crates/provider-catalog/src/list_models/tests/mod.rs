//! Catalog fetch and request-construction tests, split by surface.

pub(crate) use super::*;
use protocol::{ReasoningSupport, WireProtocol};

mod google;
mod mapping;
mod openai;
mod shapes;
