//! The server link state (D6): every outbox send is checked, and a
//! dead link is a first-class chrome state, not noise.
use nuo_protocol::AgentRequest;

use crate::shell::app::App;
use crate::shell::app::notice_catalog::ClientNotice;

impl App {
    /// Deliver one user intent to the session driver (D6).
    ///
    /// Every outbox send goes through here so a failure is *checked*:
    /// `false` means the server link is dead and the intent was **not**
    /// delivered (queued optimistic state will not be confirmed). The link
    /// state is latched and rendered; a send is never silently dropped.
    pub fn send_intent(&mut self, request: AgentRequest) -> bool {
        match self.link.tx.send(request) {
            Ok(()) => true,
            Err(_) => {
                if !self.link.link_down {
                    self.link.link_down = true;
                    self.link.relink_requested = true;
                    self.record_client_notice(ClientNotice::LinkSendFailed);
                    nuo_host::diagnostics::record_event(
                        "client",
                        "link_lost",
                        "outbox channel closed while delivering intent; auto-relink requested",
                        None,
                    );
                    tracing::error!(
                        "server link lost: a user intent could not be delivered; \
                         the TUI cannot reach the session driver"
                    );
                }
                false
            }
        }
    }
}
