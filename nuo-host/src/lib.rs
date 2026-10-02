//! Native operating-system capabilities used by muta's business layers.
//!
//! The public API is expressed in semantic operations (local IPC, an owned
//! process tree, daemon detachment, and an advisory process lock). OS-specific
//! mechanisms stay behind those boundaries so callers never emulate a missing
//! capability with a successful no-op.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod clipboard;
pub mod environment;
pub mod fs;
pub mod fsutil;
pub mod fs_watcher;
pub mod ipc;
pub mod lock;
pub mod opener;
pub mod paths;
pub mod process;
pub mod supervised;
pub mod secure_file;
pub mod shell;
pub mod tools;
pub mod workspace_sandbox;

pub use environment::{detect_device_fingerprint, detect_runtime_environment, generate_session_id};
pub use fs_watcher::{FsEvent, FsEventKind, FsWatcher};
pub use tools::{
    EditTextTool, ExecuteCommandTool, FindFilesTool, ListDirTool, ReadTextTool, SearchTextTool,
    SystemToolContext, WriteFileTool, create_system_tools, register_system_tools,
};

#[cfg(windows)]
mod windows_security;

#[cfg(not(any(unix, windows)))]
compile_error!("muta-platform supports Unix and Windows targets only");
