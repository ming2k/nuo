//! OAuth flow authorization, token acquisition, and connect pipeline.
use nuo_engine::Agent;
use nuo_persistence::config::Config;
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_persistence::connections::Connections;
use nuo_protocol::{AgentResponse, Provider};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use super::{ActivateEnv, activate, build_live_picker_state};

pub(crate) struct PendingOAuthAuthorization {
    pub auth: nuo_protocol::ConnectionAuth,
    pub tokens: nuo_provider::TokenSet,
}

/// `AgentRequest::AuthorizeOAuth` — run an OAuth login before a connection
/// exists ("+ Add connection → xAI OAuth / ChatGPT OAuth"). `auth`
/// selects which provider's flow to run. The returned token set remains
/// session-local until `AddConnection` consumes it into the final connection id.
pub async fn authorize(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    method: nuo_protocol::LoginMethod,
    auth: nuo_protocol::ConnectionAuth,
) -> Option<nuo_provider::TokenSet> {
    let Some(cfg) = auth
        .negotiated_provider()
        .and_then(nuo_provider::auth::oauth_config)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_protocol::ConnectStatus::Failed {
                provider: "oauth".to_string(),
                message: "not an OAuth provider".to_string(),
            },
        ));
        return None;
    };
    let label = cfg.provider_id.to_string();
    run_oauth(resp_tx, &label, method, cfg).await
}

/// `AgentRequest::ConnectProvider` — re-auth an existing OAuth connection, then
/// activate it.
///
/// After a successful login, runs live catalog sync so the connection's
/// model list reflects the account's real entitlements immediately (rather
/// than waiting for the next launch). Catalog sync failures are non-fatal: the
/// connection keeps its previous model subset.
pub async fn connect(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
    method: nuo_protocol::LoginMethod,
) {
    if run_oauth_for_connect(resp_tx, provider_id.clone(), method).await {
        connect_post_oauth(
            config,
            agent,
            provider_for_task,
            resp_tx,
            provider_usage,
            provider_id,
        )
        .await;
    }
}

/// Run the OAuth portion of connect in a non-blocking way.
pub async fn run_oauth_for_connect(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_id: String,
    method: nuo_protocol::LoginMethod,
) -> bool {
    let connections = Connections::load();
    let auth_mode = connections
        .get(&provider_id)
        .map(|p| p.auth.clone())
        .unwrap_or_default();
    let Some(cfg) = auth_mode
        .negotiated_provider()
        .and_then(nuo_provider::auth::oauth_config)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_protocol::ConnectStatus::Failed {
                provider: provider_id,
                message: "not an OAuth provider".to_string(),
            },
        ));
        return false;
    };
    let Some(mut tokens) = run_oauth(resp_tx, &provider_id, method, cfg).await else {
        return false;
    };
    let mut store = match crate::composition::credentials_host::store().lock().await {
        Ok(store) => store,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_protocol::ConnectStatus::Failed {
                    provider: provider_id,
                    message: format!("could not lock OAuth credential store: {error}"),
                },
            ));
            return false;
        }
    };
    if tokens.refresh.is_empty()
        && let Some(previous) = store.get(&provider_id)
        && !previous.refresh.is_empty()
    {
        tokens.refresh = previous.refresh.clone();
    }
    store.set(&provider_id, tokens);
    if let Err(error) = store.commit().await {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_protocol::ConnectStatus::Failed {
                provider: provider_id,
                message: format!("could not persist OAuth credentials: {error}"),
            },
        ));
        return false;
    }
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        nuo_protocol::ConnectStatus::Done {
            provider: provider_id.clone(),
        },
    ));
    true
}

/// Run the post-OAuth catalog sync and activation logic for connect.
pub async fn connect_post_oauth(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
) {
    // Live catalog sync: fetch the provider's actual model list with the
    // fresh token so the picker shows the account's real entitlements right
    // away. A failure keeps the previous subset; each failure is reported back
    // as a warning so the user knows *why* the list did not refresh.
    let outcome = nuo_model_routing::sync_connection_routes(&provider_id).await;
    if outcome.changed {
        nuo_model_routing::sync_fitted_model_registry();
    }
    nuo_model_routing::prune_stale_models(config, provider_usage);
    for failure in &outcome.failures {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            nuo_protocol::ConnectStatus::CatalogSyncWarning {
                provider: failure.connection.clone(),
                message: failure.message.clone(),
                kind: if failure.refused {
                    nuo_protocol::CatalogSyncFailure::Refused
                } else {
                    nuo_protocol::CatalogSyncFailure::Transient
                },
            },
        ));
    }
    let model = build_live_picker_state(config, provider_usage)
        .rows
        .into_iter()
        .find(|r| r.id == provider_id)
        .map(|r| r.model)
        .unwrap_or_default();
    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        provider_id,
        model,
    )
    .await;
}

/// Shared OAuth exchange for any provider: browser loopback (PKCE) or device
/// code. Persistence belongs to the caller because only it knows the final,
/// exact connection namespace.
pub(crate) async fn run_oauth(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    label: &str,
    method: nuo_protocol::LoginMethod,
    cfg: nuo_provider::auth::OAuthConfig,
) -> Option<nuo_provider::TokenSet> {
    use nuo_provider::auth::OAuth;

    let oauth = OAuth::new(cfg.clone(), crate::composition::credentials_host::host());

    let login = match oauth.begin_login(method).await {
        Ok(login) => login,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_protocol::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message: error.to_string(),
                },
            ));
            return None;
        }
    };
    let prompt = login.prompt();
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        nuo_protocol::ConnectStatus::Pending {
            provider: label.to_string(),
            url: prompt.url.clone(),
            user_code: prompt.user_code.clone().unwrap_or_default(),
            message: prompt.message.clone(),
        },
    ));

    let tokens = match login.complete().await {
        Ok(t) => t,
        Err(error) => {
            let message = oauth.format_login_error(&error);
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                nuo_protocol::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message,
                },
            ));
            return None;
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let token_set =
        nuo_provider::auth::build_token_set_from_login(&oauth, label, tokens, now_ms).await;
    Some(token_set)
}

pub(crate) fn is_auth_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("unauthenticated")
        || lower.contains("invalid_token")
        || lower.contains("token expired")
}
