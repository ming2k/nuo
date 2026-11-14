//! Provider routing, picker and state-delta handling.

use std::sync::atomic::Ordering;

use nuo_protocol::AgentResponse;

use super::{M, TranslatorState};

impl TranslatorState {
    /// Provider routing, auth/connect status, picker snapshots and the
    /// provider/context slices of state deltas.
    pub(super) async fn handle_provider_state(&mut self, resp: AgentResponse) {
        match resp {
            AgentResponse::RouteSettings {
                provider_id,
                model,
                overrides,
            } => {
                self.mutations
                    .send(M::RouteSettings {
                        provider_id,
                        model,
                        overrides,
                    })
                    .await;
            }
            AgentResponse::PermissionsCleared => {
                self.mutations.send(M::ClearPermissions).await;
                self.mutations.send(M::SetPhase(None)).await;
            }
            AgentResponse::ProviderKeys(status) => {
                self.mutations
                    .send(M::KeyStatus(status.into_iter().collect()))
                    .await;
            }
            AgentResponse::ProviderPicker(snapshot) => {
                self.picker = snapshot.clone();
                self.mutations.send(M::ProviderPicker(snapshot)).await;
            }
            AgentResponse::ProviderSwitched { provider, model } => {
                // Refreshes the hint bar; NOT appended to the transcript
                // (the acknowledgment is a command ack).
                self.current_provider = provider.clone();
                self.current_model = model.clone();
                self.mutations
                    .send(M::ProviderSwitched { provider, model })
                    .await;
            }
            AgentResponse::ProviderQuotas(snapshot) => {
                self.mutations.send(M::ProviderQuotas(snapshot)).await;
            }
            AgentResponse::ConnectStatus(status) => {
                match status {
                    nuo_protocol::ConnectStatus::Pending {
                        url,
                        user_code,
                        message,
                        ..
                    } => {
                        if !url.is_empty() {
                            let url_for_open = url.clone();
                            tokio::task::spawn_blocking(move || {
                                if let Err(err) = crate::shell::browser::open_browser(&url_for_open)
                                {
                                    tracing::warn!("Failed to open browser: {err}");
                                }
                            });
                        }
                        self.mutations
                            .send(M::Oauth(crate::shell::app::OauthAddSignal::Pending {
                                url: url.clone(),
                                user_code: user_code.clone(),
                                message: message.clone(),
                            }))
                            .await;
                        // The add-flow surfaces the URL/code in the
                        // OauthPending modal, so suppress the transcript
                        // notice there. Only the reconnect flow gets it.
                        let in_add_flow = self.awaiting_oauth_add.load(Ordering::SeqCst);
                        if !in_add_flow {
                            self.mutations
                                .send(M::NoticeDisclosure(
                                    crate::shell::app::notice_catalog::ClientNotice::OauthPending {
                                        message: message.clone(),
                                        url: url.clone(),
                                        user_code: user_code.clone(),
                                    }
                                    .render(),
                                ))
                                .await;
                        }
                    }
                    nuo_protocol::ConnectStatus::Done { provider } => {
                        self.mutations
                            .send(M::Oauth(crate::shell::app::OauthAddSignal::Done))
                            .await;
                        self.mutations
                            .send(M::NoticeDisclosure(
                                crate::shell::app::notice_catalog::ClientNotice::OauthAuthorized {
                                    provider,
                                }
                                .render(),
                            ))
                            .await;
                    }
                    nuo_protocol::ConnectStatus::CatalogSyncWarning {
                        provider,
                        message,
                        kind,
                    } => {
                        // A refusal is actionable and permanent (the
                        // account may not use this provider); a
                        // transient failure is not. Say which.
                        let refused = matches!(kind, nuo_protocol::CatalogSyncFailure::Refused);
                        self.mutations
                            .send(M::NoticeDisclosure(
                                crate::shell::app::notice_catalog::ClientNotice::CatalogSyncWarning {
                                    provider,
                                    message,
                                    refused,
                                }
                                .render(),
                            ))
                            .await;
                    }
                    nuo_protocol::ConnectStatus::Failed { provider, message } => {
                        self.mutations
                            .send(M::Oauth(crate::shell::app::OauthAddSignal::Failed {
                                message: message.clone(),
                            }))
                            .await;
                        self.mutations
                            .send(M::NoticeDisclosure(
                                crate::shell::app::notice_catalog::ClientNotice::OauthConnectFailed {
                                    provider,
                                    message,
                                }
                                .render(),
                            ))
                            .await;
                    }
                }
            }
            AgentResponse::StateDelta(delta) => {
                if let (Some(provider), Some(model)) = (delta.provider, delta.model) {
                    self.mutations
                        .send(M::ProviderSwitched { provider, model })
                        .await;
                }
                if let Some(ct) = delta.context_tokens {
                    self.mutations
                        .send(M::ContextTokens {
                            session_id: delta.thread_id,
                            snapshot: ct,
                        })
                        .await;
                }
            }
            _ => {}
        }
    }
}
