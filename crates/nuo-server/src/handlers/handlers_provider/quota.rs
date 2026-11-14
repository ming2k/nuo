//! Provider quotas, rate limits, credit balance, and usage telemetry.
use nuo_persistence::config::Config;
use nuo_persistence::connection_usage::ConnectionUsage;
use nuo_protocol::{AgentResponse, SecretString, WireProtocol};
use tokio::sync::mpsc;

use super::{build_live_picker_state, is_auth_error};

/// Mask an API key for safe display (e.g. `sk-12...abcd`).
pub(crate) fn mask_api_key(key: &str) -> Option<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= 8 {
        Some("********".to_string())
    } else {
        Some(format!(
            "{}...{}",
            &trimmed[..4],
            &trimmed[trimmed.len() - 4..]
        ))
    }
}

/// `AgentRequest::QueryConnectionDetail` — return connection details immediately and query live provider usage in background.
pub(crate) async fn query_connection_detail(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
    force_refresh: bool,
) {
    let stores = nuo_model_routing::Stores::load();
    let Some(connection) = stores.connections.get(&id) else {
        return;
    };

    let entry = nuo_model_routing::derive_entry(connection, &stores.inputs());
    let (protocol, base_url) = entry
        .default_channel()
        .map(|c| match &c.transport {
            nuo_engine::Transport::OpenAiChatCompletions { base_url, .. } => {
                ("openai".to_string(), base_url.clone())
            }
            nuo_engine::Transport::OpenAiResponses { base_url, .. } => {
                ("openai_responses".to_string(), base_url.clone())
            }
            nuo_engine::Transport::AnthropicMessages { base_url, .. } => {
                ("anthropic".to_string(), base_url.clone())
            }
            nuo_engine::Transport::GoogleGenerateContent { base_url, .. } => {
                ("google".to_string(), base_url.clone())
            }
            nuo_engine::Transport::GoogleInteractions { base_url, .. } => {
                ("google_interactions".to_string(), base_url.clone())
            }
        })
        .unwrap_or_else(|| {
            let spec =
                crate::composition::provider_registry::model_provider_spec(&connection.provider);
            let p = spec
                .as_ref()
                .map(|s| s.protocol)
                .unwrap_or(WireProtocol::OpenAiChatCompletions);
            let url = spec.map(|s| s.root_url.to_string()).unwrap_or_default();
            (p.to_string(), url)
        });

    let provider_label =
        nuo_protocol::model_providers::model_provider_label(&connection.provider).to_string();

    let raw_key = nuo_model_routing::resolve_credential(connection, &stores.creds);
    let api_key_masked = mask_api_key(raw_key.expose_secret());

    let api_key_source = if connection.auth.is_negotiated() {
        "Negotiated (Session)".to_string()
    } else if let Some(env) = connection.api_key_env.as_deref() {
        if std::env::var(env).is_ok() {
            format!("Environment (${env})")
        } else {
            format!("Missing (${env} not set)")
        }
    } else if stores.creds.api_key(&connection.name).is_some() {
        "credentials.toml".to_string()
    } else {
        "Not configured".to_string()
    };

    let user_agent = entry
        .default_channel()
        .map(|c| c.transport.user_agent().to_string())
        .filter(|ua| !ua.is_empty())
        .unwrap_or_else(|| nuo_provider::NUO_USER_AGENT.to_string());

    let models = entry
        .channels
        .iter()
        .map(|c| c.model.clone())
        .collect::<Vec<_>>();
    let model_info = entry
        .channels
        .iter()
        .map(nuo_model_routing::channel_model_info)
        .collect::<Vec<_>>();
    let usage_store = nuo_persistence::connection_usage::ConnectionUsage::load();
    let active_model = usage_store
        .last_model_for(&connection.name)
        .filter(|m| entry.offers_model(m))
        .map(|m| m.to_string())
        .or_else(|| entry.default_channel().map(|c| c.model.clone()));
    let active_channel = active_model
        .as_deref()
        .and_then(|m| entry.channel_for_model(m))
        .or_else(|| entry.default_channel());
    let active_channel_info = active_channel.map(nuo_model_routing::channel_model_info);
    // The server resolved these already; no protocol sniffing here. Depth and
    // the switch are what the route would actually stamp (ADR-0126).
    let active_model_effort = active_channel_info
        .as_ref()
        .and_then(|info| info.effort.clone());
    let active_model_thinking = active_channel_info.as_ref().and_then(|info| info.thinking);

    let auth_type = if connection.auth.is_negotiated() {
        format!("Negotiated ({:?})", connection.auth)
    } else if connection.api_key_env.is_some() {
        "Static (Environment)".to_string()
    } else {
        "Static (API Key)".to_string()
    };

    let mut initial_detail = nuo_protocol::ConnectionDetail {
        name: connection.name.clone(),
        provider: connection.provider.clone(),
        provider_label,
        protocol,
        base_url: base_url.clone(),
        auth_type,
        api_key_masked,
        api_key_source,
        headers: connection.headers.clone(),
        user_agent,
        models,
        model_info,
        active_model,
        active_model_effort,
        active_model_thinking,
        usage: nuo_protocol::ConnectionUsageState::Fetching,
        budget: connection.budget.clone(),
    };

    if force_refresh {
        crate::runtime::usage_cache::shared_usage_cache().invalidate(&connection.name);
    } else if let Some(cached) =
        crate::runtime::usage_cache::shared_usage_cache().get(&connection.name)
    {
        initial_detail.usage = cached;
        let _ = resp_tx.send(AgentResponse::ConnectionDetail(initial_detail));
        return;
    }

    // Phase 1: Send local detail snapshot immediately so UI renders instantly.
    let _ = resp_tx.send(AgentResponse::ConnectionDetail(initial_detail.clone()));

    // Phase 2: Async remote query in background task.
    let resp_tx_bg = resp_tx.clone();
    let conn = connection.clone();
    let base_url_bg = base_url.clone();
    tokio::spawn(async move {
        let (usage, _) = fetch_usage_for_connection(&conn, &raw_key, &base_url_bg).await;
        crate::runtime::usage_cache::shared_usage_cache().put(&conn.name, usage.clone());
        initial_detail.usage = usage;
        let _ = resp_tx_bg.send(AgentResponse::ConnectionDetail(initial_detail));
    });
}

/// Fetch live provider usage for a single connection, handling OAuth resolution and 401 retry.
pub(crate) async fn fetch_usage_for_connection(
    connection: &nuo_persistence::connections::Connection,
    raw_key: &SecretString,
    base_url: &str,
) -> (nuo_protocol::ConnectionUsageState, Option<String>) {
    let (api_key, project, account_id, is_negotiated) = if connection.auth.is_negotiated() {
        let source = nuo_provider::auth::OAuthCredentialSource::new(
            &crate::composition::credentials_host::host(),
            &connection.name,
            connection.auth.clone(),
        );
        match nuo_protocol::CredentialSource::resolve_auth(&source).await {
            Ok(auth) => {
                let project = auth
                    .extension::<nuo_protocol::auth::GoogleAuthMetadata>()
                    .map(|m| m.project_id.clone());
                let account_id = auth.user_email.clone().or_else(|| project.clone());
                (
                    auth.token.expose_secret().to_string(),
                    project,
                    account_id,
                    true,
                )
            }
            Err(err) => {
                return (nuo_protocol::ConnectionUsageState::Error(err), None);
            }
        }
    } else {
        (raw_key.expose_secret().to_string(), None, None, false)
    };

    let mut usage = crate::composition::provider_registry::fetch_provider_usage_ext(
        &connection.provider,
        base_url,
        &api_key,
        project.as_deref(),
    )
    .await;

    if is_negotiated
        && let nuo_protocol::ConnectionUsageState::Error(ref err) = usage
        && is_auth_error(err)
    {
        let source = nuo_provider::auth::OAuthCredentialSource::new(
            &crate::composition::credentials_host::host(),
            &connection.name,
            connection.auth.clone(),
        );
        let rejected = SecretString::from(api_key.as_str());
        if let Ok(refreshed) =
            nuo_protocol::CredentialSource::force_refresh_after_rejection(&source, &rejected).await
        {
            let refreshed_project = refreshed
                .extension::<nuo_protocol::auth::GoogleAuthMetadata>()
                .map(|m| m.project_id.as_str())
                .or(project.as_deref());
            usage = crate::composition::provider_registry::fetch_provider_usage_ext(
                &connection.provider,
                base_url,
                refreshed.token.expose_secret(),
                refreshed_project,
            )
            .await;
        }
    }

    (usage, account_id)
}

pub(crate) fn extract_quota_metrics(
    usage_state: &nuo_protocol::ConnectionUsageState,
) -> (
    Option<String>,
    Option<nuo_protocol::ProviderQuotaData>,
    Option<String>,
    Option<u64>,
) {
    match usage_state {
        nuo_protocol::ConnectionUsageState::Available(u) => {
            let reset = u.quota.as_ref().and_then(|q| match q {
                nuo_protocol::ProviderQuotaData::Periodic(p) => {
                    p.buckets.iter().filter_map(|b| b.reset_at_ms).min()
                }
                nuo_protocol::ProviderQuotaData::Composite {
                    periodic: Some(p), ..
                } => p.buckets.iter().filter_map(|b| b.reset_at_ms).min(),
                _ => None,
            });
            (
                u.primary_balance.clone(),
                u.quota.clone(),
                u.plan.clone(),
                reset,
            )
        }
        _ => (None, None, None, None),
    }
}

pub(crate) fn build_provider_quota_snapshot(
    provider_filter: &Option<String>,
    entries: &[nuo_protocol::ConnectionQuotaEntry],
) -> nuo_protocol::ProviderQuotaSnapshot {
    let total_accounts = entries.len();
    let available_accounts = entries
        .iter()
        .filter(|e| matches!(e.state, nuo_protocol::ConnectionUsageState::Available(_)))
        .count();
    let depleted_accounts = entries
        .iter()
        .filter(|e| match &e.state {
            nuo_protocol::ConnectionUsageState::Error(_) => true,
            nuo_protocol::ConnectionUsageState::Available(u) => u
                .primary_balance
                .as_deref()
                .is_some_and(|b| b.starts_with("0%") || b.contains("DEPLETED")),
            _ => false,
        })
        .count();

    nuo_protocol::ProviderQuotaSnapshot {
        provider_filter: provider_filter.clone(),
        entries: entries.to_vec(),
        total_accounts,
        available_accounts,
        depleted_accounts,
        updated_at_ms: chrono::Utc::now().timestamp_millis() as u64,
    }
}

/// Query aggregated provider quota inspection overview across configured connections (ADR-0036).
///
/// Executes in two phases to guarantee immediate UI rendering and eliminate all-or-nothing blocking:
/// Phase 1: Immediately emit a skeleton snapshot with cached or `Fetching` states.
/// Phase 2: Stream concurrent background requests (bounded by 4); on each individual connection resolution,
/// emit an updated snapshot so the UI incrementally reflects quota progress without blocking.
pub async fn query_provider_quotas(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_filter: Option<String>,
    force_refresh: bool,
) {
    use futures::StreamExt;
    let stores = nuo_model_routing::Stores::load();
    let config = Config::load();
    let default_conn_id = config.default_connection.clone();

    let targets: Vec<nuo_persistence::connections::Connection> = stores
        .connections
        .connections
        .iter()
        .filter(|c| {
            if let Some(ref filter) = provider_filter
                && c.provider != *filter
                && c.auth.negotiated_provider() != Some(filter.as_str())
            {
                return false;
            }
            crate::composition::provider_registry::model_provider_spec(&c.provider)
                .and_then(|s| s.quota)
                .is_some()
        })
        .cloned()
        .collect();

    if targets.is_empty() {
        let empty_snap = build_provider_quota_snapshot(&provider_filter, &[]);
        let _ = resp_tx.send(AgentResponse::ProviderQuotas(empty_snap));
        return;
    }

    let mut entries = Vec::with_capacity(targets.len());
    let mut needs_fetch = Vec::new();

    for conn in &targets {
        if force_refresh {
            crate::runtime::usage_cache::shared_usage_cache().invalidate(&conn.name);
        }

        let (usage_state, cached_account_id) = if !force_refresh
            && let Some(cached) = crate::runtime::usage_cache::shared_usage_cache().get(&conn.name)
        {
            (cached, None)
        } else {
            needs_fetch.push(conn.clone());
            (nuo_protocol::ConnectionUsageState::Fetching, None)
        };

        let (primary_balance, quota, plan, earliest_reset_ms) = extract_quota_metrics(&usage_state);

        entries.push(nuo_protocol::ConnectionQuotaEntry {
            name: conn.name.clone(),
            provider: conn.provider.clone(),
            provider_label: nuo_protocol::model_providers::model_provider_label(&conn.provider)
                .to_string(),
            account_id: cached_account_id,
            is_default: conn.name == default_conn_id,
            primary_balance,
            quota,
            plan,
            state: usage_state,
            earliest_reset_ms,
        });
    }

    // Sort entries stably: active connection first, then by name
    entries.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });

    // Phase 1: Emit initial skeleton snapshot immediately so UI renders instantaneously.
    let _ = resp_tx.send(AgentResponse::ProviderQuotas(
        build_provider_quota_snapshot(&provider_filter, &entries),
    ));

    if needs_fetch.is_empty() {
        return;
    }

    // Phase 2: Concurrent bounded fan-out (buffer_unordered(4)) streaming incremental updates.
    let prepared_fetch: Vec<_> = needs_fetch
        .into_iter()
        .map(|conn| {
            let inputs = stores.inputs();
            let entry = nuo_model_routing::derive_entry(&conn, &inputs);
            let base_url = entry
                .default_channel()
                .map(|c| c.transport.base_url().to_string())
                .unwrap_or_else(|| {
                    crate::composition::provider_registry::model_provider_spec(&conn.provider)
                        .map(|s| s.root_url.to_string())
                        .unwrap_or_default()
                });
            let raw_key = nuo_model_routing::resolve_credential(&conn, &stores.creds);
            (conn, raw_key, base_url)
        })
        .collect();

    let mut stream = futures::stream::iter(prepared_fetch)
        .map(|(conn, raw_key, base_url)| async move {
            let (fetched, acct) = fetch_usage_for_connection(&conn, &raw_key, &base_url).await;
            crate::runtime::usage_cache::shared_usage_cache().put(&conn.name, fetched.clone());
            (conn.name, fetched, acct)
        })
        .buffer_unordered(4);

    while let Some((conn_name, usage_state, acct)) = stream.next().await {
        if let Some(entry) = entries.iter_mut().find(|e| e.name == conn_name) {
            let (primary_balance, quota, plan, earliest_reset_ms) =
                extract_quota_metrics(&usage_state);
            entry.state = usage_state;
            entry.primary_balance = primary_balance;
            entry.quota = quota;
            entry.plan = plan;
            entry.earliest_reset_ms = earliest_reset_ms;
            if acct.is_some() {
                entry.account_id = acct;
            }

            // Immediately emit updated snapshot as each item resolves
            let _ = resp_tx.send(AgentResponse::ProviderQuotas(
                build_provider_quota_snapshot(&provider_filter, &entries),
            ));

            let usage_store = ConnectionUsage::load();
            let picker = build_live_picker_state(&config, &usage_store);
            let _ = resp_tx.send(AgentResponse::ProviderPicker(picker));
        }
    }
}

/// Query all connections' live provider usage concurrently and stream updates.
pub(crate) async fn query_all_connections_usage(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    force_refresh: bool,
) {
    use futures::StreamExt;
    let stores = nuo_model_routing::Stores::load();
    let connections: Vec<String> = stores
        .connections
        .connections
        .iter()
        .filter(|c| {
            crate::composition::provider_registry::model_provider_spec(&c.provider)
                .and_then(|s| s.quota)
                .is_some()
        })
        .map(|c| c.name.clone())
        .collect();

    let mut stream = futures::stream::iter(connections)
        .map(|conn_id| {
            let tx = resp_tx.clone();
            async move {
                query_connection_detail(&tx, conn_id, force_refresh).await;
            }
        })
        .buffer_unordered(4);

    while stream.next().await.is_some() {}
}
