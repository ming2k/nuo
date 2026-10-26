//! Domain presentation and agent views for Nuo (ADR-0107).
//!
//! This crate holds the pure, zero-IO presentation layer for AI agent interactions:
//! conversational transcript models (`model`), tool presenters (`tools`), conversational
//! layout strategies (`layout`), message body rendering, trust gates, and phase displays.
//!
//! It builds strictly upon the domain-agnostic `nuo-ui-primitives` substrate ([INV-ARCH-VIEW-ZERO-IO]).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub use nuo_ui_primitives::Theme;

pub mod components;
pub mod composer_attachments;
pub mod composer_mode;
pub mod disclosure;
pub mod disclosure_config;
pub mod empty_state;
pub mod footer_stack;
pub mod layout;
pub mod markdown_table;
pub mod message_body;
pub mod model;
pub mod notice;
pub mod phase;
pub mod providers;
pub mod text_layout;
pub mod tools;
pub mod trust_gate;
