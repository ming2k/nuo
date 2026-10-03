//! Bridge between agent-owned runtime state and the concrete `tools` module.
//!
//! Keep concrete-tool construction here so the ReAct loop continues to work
//! only with `dyn Tool` / `ToolSet`. Tools that merely need injected state
//! belong in [`crate::tools`]; tools that construct or control agents belong in
//! this crate.

use std::sync::{Arc, Mutex};

/// Add the concrete tools whose lifetime is tied to one agent instance.
pub(crate) fn install_agent_owned_tools(
    toolset: &mut nuo_wire::ToolSet,
    todos: Arc<Mutex<nuo_wire::TodoList>>,
    round_counter: Arc<Mutex<u64>>,
) {
    let context = crate::tools::TodoToolContext::new(todos, round_counter);
    toolset.upsert(Arc::new(crate::tools::TodoTool::new(context)));
}
