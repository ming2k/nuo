//! Overlay/modal/navigation tests: pickers, transient sheets, sub-layer pop, queue management, view switching, chrome restoration.

// Integration test target: assertions fail by panicking, so the production
// no-panic lints are meaningless here (the exemption ends at the test boundary).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

mod config;
mod connections;
mod dialogs;
mod editor;
mod history;
mod inspectors;
mod models;
mod paste;
mod queue;
mod scenes;
mod sessions;
