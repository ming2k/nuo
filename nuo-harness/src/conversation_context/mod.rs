//! Durable, event-driven context added to an agent's live conversation window.
//!
//! These messages are distinct from request projection: lifecycle owners decide
//! when to append them, and the resulting provenance survives persistence.
//!
//! `@`-references are *asset references* (ADR-0288): they are resolved here, on
//! the durable side, into canonical envelopes; `mentions` owns the shared
//! lexical helpers (code-span masking, boundary guards, address
//! canonicalization) used by the resolvers and by the request projection.

mod files;
mod mentions;
mod messages;
mod skills;

pub(crate) use files::inject_mentioned_files;
pub(crate) use mentions::{canonicalize_addresses, is_reference_site};
pub(crate) use messages::{hidden_user, hidden_user_with_reason, tool_image, visible_user};
pub(crate) use skills::inject_mentioned_skills;
