//! The daemon link state (ADR-0197 D6): every outbox send is checked, and a
//! dead link is a first-class chrome state, not noise.

use nuo_contracts::AgentRequest;

use crate::app::App;

impl App {
    /// Deliver one user intent to the session driver (ADR-0197 D6).
    ///
    /// Every outbox send goes through here so a failure is *checked*:
    /// `false` means the daemon link is dead and the intent was **not**
    /// delivered (queued optimistic state will not be confirmed). The link
    /// state is latched and rendered; a send is never silently dropped.
    pub fn send_intent(&mut self, request: AgentRequest) -> bool {
        match self.tx.send(request) {
            Ok(()) => true,
            Err(_) => {
                if !self.link_down {
                    self.link_down = true;
                    tracing::error!(
                        "daemon link lost: a user intent could not be delivered; \
                         the TUI cannot reach the session driver"
                    );
                }
                false
            }
        }
    }
}
