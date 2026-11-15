//! Request-family handlers. Each module translates one `AgentRequest` family
//! into runtime calls; it owns no session state of its own.

pub mod handlers_chat;
pub mod handlers_history;
pub mod handlers_permission;
pub mod handlers_provider;
pub mod handlers_slash;
pub mod handlers_thread;
pub mod handlers_websearch;
pub mod slash_handler;
