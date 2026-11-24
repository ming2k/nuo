//! Auxiliary request-family handlers for [`DriverCtx`] (MCP, input history,
//! web-search/UI settings, and server endpoint inspection).

use std::sync::Arc;

use protocol::{AgentRequest, AgentResponse};

use super::DriverCtx;

impl DriverCtx {
    /// MCP server toggle/reconnect requests.
    pub(super) async fn handle_mcp(
        &mut self,
        req: AgentRequest,
        config: &persistence::config::Config,
    ) {
        let agent = Arc::clone(&self.agent);
        let skills_registry = Arc::clone(&self.skills_registry);
        let mcp_runtime = Arc::clone(&self.mcp_runtime);
        let resp_tx = self.resp_tx.clone();
        match req {
            AgentRequest::ToggleMcpServer { name, enabled } => {
                crate::handlers::handlers_thread::toggle_mcp_server(
                    &agent,
                    &skills_registry,
                    &mcp_runtime,
                    config,
                    &resp_tx,
                    name,
                    enabled,
                )
                .await;
            }
            AgentRequest::ReconnectMcpServer { name } => {
                crate::handlers::handlers_thread::reconnect_mcp_server(
                    &agent,
                    &skills_registry,
                    &mcp_runtime,
                    config,
                    &resp_tx,
                    name,
                )
                .await;
            }
            other => unreachable!("MCP request routed to wrong handler: {other:?}"),
        }
    }

    /// Input history and history search.
    pub(super) async fn handle_history(&mut self, req: AgentRequest) {
        let resp_tx = self.resp_tx.clone();
        match req {
            AgentRequest::QueryInputHistory => {
                crate::handlers::handlers_history::query_input_history(&resp_tx).await;
            }
            AgentRequest::RecordInputHistory { entries, dedup } => {
                crate::handlers::handlers_history::record_input_history(entries, dedup).await;
            }
            AgentRequest::DeleteInputHistoryEntry {
                text,
                created_at_ms,
            } => {
                crate::handlers::handlers_history::delete_input_history_entry(text, created_at_ms)
                    .await;
            }
            AgentRequest::QueryRouteSettings { provider_id, model } => {
                crate::handlers::handlers_history::query_route_settings(
                    &provider_id,
                    &model,
                    &resp_tx,
                );
            }
            AgentRequest::SearchHistory {
                query,
                workspace,
                limit,
            } => {
                crate::handlers::handlers_history::search_history(
                    &query,
                    workspace.as_deref(),
                    limit,
                    &resp_tx,
                )
                .await;
            }
            other => unreachable!("history request routed to wrong handler: {other:?}"),
        }
    }

    /// TUI layout/color and web-search configuration requests.
    pub(super) async fn handle_websearch_ui(
        &mut self,
        req: AgentRequest,
        config: &mut persistence::config::Config,
    ) {
        let resp_tx = self.resp_tx.clone();
        let websearch_shared = self.websearch_shared.clone();
        match req {
            AgentRequest::UpdateTuiLayout(layout) => {
                let _ = resp_tx.send(AgentResponse::TuiLayoutUpdated(layout));
            }
            AgentRequest::UpdateTuiColorScheme { name, custom } => {
                let _ = resp_tx.send(AgentResponse::TuiColorSchemeUpdated { name, custom });
            }
            AgentRequest::QueryWebSearchConfig => {
                crate::handlers::handlers_websearch::query(&*config, &websearch_shared, &resp_tx);
            }
            AgentRequest::UpdateWebSearchConfig(update) => {
                crate::handlers::handlers_websearch::update(
                    config,
                    &websearch_shared,
                    *update,
                    &resp_tx,
                )
                .await;
            }
            other => unreachable!("websearch/UI request routed to wrong handler: {other:?}"),
        }
    }

    /// Local server endpoint inspection and dynamic-port toggling.
    pub(super) async fn handle_server(&mut self, req: AgentRequest) {
        let resp_tx = self.resp_tx.clone();
        match req {
            AgentRequest::QueryServerEndpoints => {
                let report = crate::transport::serve::get_current_endpoints().await;
                let _ = resp_tx.send(AgentResponse::ServerEndpointsStatus(report));
            }
            AgentRequest::ToggleServerPort { port, enable } => {
                let report = if enable {
                    crate::transport::serve::open_dynamic_tcp(port).await
                } else {
                    crate::transport::serve::close_dynamic_tcp().await
                };
                if let Ok(rep) = report {
                    let _ = resp_tx.send(AgentResponse::ServerEndpointsStatus(rep));
                }
            }
            other => unreachable!("server request routed to wrong handler: {other:?}"),
        }
    }
}
