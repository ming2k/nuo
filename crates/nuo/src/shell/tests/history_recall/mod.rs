//! Input history, recall, session resume backfill, and on-disk history isolation tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub(super) use super::*;

mod draft;
mod entries;
mod esc;
mod modality;
mod recall;
mod restore;
mod search;
