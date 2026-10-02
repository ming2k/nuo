//! Agent extension point for projecting the next model-visible context.

use async_trait::async_trait;
use nuo_contracts::Message;

/// Mid-turn model-context projection hook.
///
/// After each ReAct turn, when context pressure crosses the configured budget,
/// the agent hands the live message list to the gate and asks it to produce the
/// next model-visible window. A replacement swaps the live list; `None` leaves
/// it untouched. The implementation owns durability policy and archives
/// original content before returning a replacement.
#[async_trait]
pub trait ContextProjectionGate: Send + Sync {
    async fn project_context(&self, messages: Vec<Message>) -> Option<Vec<Message>>;
}
