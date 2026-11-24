//! The server link state (D6, ADR-0251): every outbox send is checked, and a
//! dead link is a first-class chrome state, not noise.
use protocol::{AgentRequest, AttachAction};

use crate::shell::app::App;
use crate::shell::app::notice_catalog::ClientNotice;
use crate::shell::client::ClientOutbound;

impl App {
    /// Send one outbound frame over the single FIFO client link (ADR-0251
    /// `[INV-MUX-04]`).
    pub fn send_outbound(&mut self, outbound: ClientOutbound) -> bool {
        match self.link.tx.send(outbound) {
            Ok(()) => true,
            Err(_) => {
                if !self.link.link_down {
                    self.link.link_down = true;
                    self.link.relink_requested = true;
                    self.record_client_notice(ClientNotice::LinkSendFailed);
                    host::diagnostics::record_event(
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

    /// Deliver one user intent to the currently viewed thread's session driver
    /// (D6, ADR-0251).
    pub fn send_intent(&mut self, request: AgentRequest) -> bool {
        let thread_id = (!self.session.current_session_id.is_empty())
            .then(|| self.session.current_session_id.clone());
        self.send_outbound(ClientOutbound::Request { thread_id, request })
    }

    /// Deliver one user intent explicitly addressed to `thread_id`.
    pub fn send_intent_to(&mut self, thread_id: impl Into<String>, request: AgentRequest) -> bool {
        self.send_outbound(ClientOutbound::Request {
            thread_id: Some(thread_id.into()),
            request,
        })
    }

    /// Subscribe the live multiplexed connection to another thread (attach or
    /// create new) without tearing down `App` or existing tabs (ADR-0251).
    pub fn subscribe_thread(
        &mut self,
        action: AttachAction,
        project: Option<std::path::PathBuf>,
    ) -> bool {
        self.send_outbound(ClientOutbound::Subscribe { action, project })
    }

    /// Detach a closed tab's thread from the live multiplexed connection
    /// (ADR-0251).
    pub fn unsubscribe_thread(&mut self, thread_id: impl Into<String>) -> bool {
        self.send_outbound(ClientOutbound::Unsubscribe {
            thread_id: thread_id.into(),
        })
    }
}
