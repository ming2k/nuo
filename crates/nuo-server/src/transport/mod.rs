//! Control-plane transport: sockets, discovery, wire codecs, health probes, and
//! the frontend UI bridge.

pub mod health_http;
pub mod serve;
pub mod serve_discovery;
pub mod thread_view;
pub mod ui_bridge;
pub mod wire_channel;
