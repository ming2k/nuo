//! Catalog fetch and request-construction tests, split by surface.

pub(crate) use super::*;
use nuo_provider::{Availability, ReasoningSupport, WireProtocol};

mod google;
mod mapping;
mod openai;
mod shapes;
