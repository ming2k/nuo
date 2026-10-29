//! Workspace security attestation, trusted asset reloading, and trust command handlers.

use std::path::Path;
use std::sync::Arc;

use crate::mcp::McpRuntime;
use nuo_harness::Agent;
use nuo_harness::cognition::skills::SkillRegistry;
use nuo_persistence::config::Config;
use nuo_persistence::workspace_security::WorkspaceSecurityStore;
use nuo_wire::TrustDomain;

pub(crate) struct AssetReloadReport {
    pub snapshot: nuo_wire::WorkspaceSecuritySnapshot,
    pub connected_mcp: Vec<String>,
    pub removed_mcp: Vec<String>,
}

/// Rebuild every project-asset consumer from one freshly attested snapshot.
pub(crate) async fn reload_trusted_assets(
    agent: &Arc<Agent>,
    mcp_runtime: &Arc<McpRuntime>,
    workspace_security: &WorkspaceSecurityStore,
    project_root: &Path,
    skills_registry: &SkillRegistry,
    shared_additional_roots: &nuo_wire::SharedAdditionalRoots,
) -> Result<AssetReloadReport, String> {
    let mut snapshot = workspace_security.snapshot(project_root);
    snapshot.user_assets = compute_user_assets_trust();
    let mut effective = Config::load();
    if snapshot.mcp.is_trusted() {
        effective.merge_project_mcp(Config::load_project_mcp(project_root));
    }
    if snapshot.hooks.is_trusted() {
        effective.merge_project_hooks(Config::load_project_hooks(project_root));
    }
    if snapshot.ex_workspace.is_trusted() {
        effective
            .merge_project_additional_roots(Config::load_project_additional_roots(project_root));
    }
    super::session_ops::apply_additional_roots(shared_additional_roots, &effective, project_root);

    let mcp_report = mcp_runtime.reconfigure(effective.mcp.clone()).await;
    agent.set_hooks(crate::hooks::build_hook_registry(&effective.hooks, agent));
    skills_registry.reload().await;
    let rules = if snapshot.instructions.is_trusted() {
        crate::project::load_project_rules(project_root)?
    } else {
        String::new()
    };
    agent.set_project_rules(rules);
    agent.set_workspace_security(snapshot.clone());

    Ok(AssetReloadReport {
        snapshot,
        connected_mcp: mcp_report
            .connected
            .into_iter()
            .filter_map(|(name, ok)| ok.then_some(name))
            .collect(),
        removed_mcp: mcp_report.removed,
    })
}

pub(crate) fn compute_user_assets_trust() -> nuo_wire::WorkspaceTrustState {
    let config = Config::load();
    let ledger = nuo_persistence::AssetAttestationLedger::load();
    let mut user_asset_count = 0;
    let mut user_untrusted_count = 0;
    let mut user_changed_count = 0;
    let mut user_expired_count = 0;
    for (name, server_cfg) in &config.mcp {
        if server_cfg.sandbox_root.is_none() && server_cfg.enabled {
            user_asset_count += 1;
            let locator = nuo_wire::security::AssetLocator::UserMcp { name: name.clone() };
            let spec = if let Some(url) = &server_cfg.url {
                nuo_wire::security::AssetSpec::RemoteEndpoint {
                    url: url.clone(),
                    headers: server_cfg
                        .environment
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            } else {
                nuo_wire::security::AssetSpec::Process {
                    command: server_cfg.command.clone(),
                    env: server_cfg
                        .environment
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            };
            match ledger.status(&locator, &spec) {
                nuo_wire::security::AttestationStatus::Trusted
                | nuo_wire::security::AttestationStatus::SessionEphemeral
                | nuo_wire::security::AttestationStatus::Denied => {}
                nuo_wire::security::AttestationStatus::Changed => {
                    user_changed_count += 1;
                    user_untrusted_count += 1;
                }
                nuo_wire::security::AttestationStatus::Expired => {
                    user_expired_count += 1;
                    user_untrusted_count += 1;
                }
                nuo_wire::security::AttestationStatus::Quarantined => {
                    user_untrusted_count += 1;
                }
            }
        }
    }
    if user_asset_count == 0 {
        nuo_wire::WorkspaceTrustState::Absent
    } else if user_untrusted_count == 0 {
        nuo_wire::WorkspaceTrustState::Trusted
    } else if user_changed_count > 0 || user_expired_count > 0 {
        nuo_wire::WorkspaceTrustState::Changed
    } else {
        nuo_wire::WorkspaceTrustState::Quarantined
    }
}

pub(crate) fn trust_user_assets() -> Vec<String> {
    let config = Config::load();
    let ledger = nuo_persistence::AssetAttestationLedger::load();
    let mut trusted = Vec::new();
    for (name, server_cfg) in &config.mcp {
        if server_cfg.sandbox_root.is_none() {
            let locator = nuo_wire::security::AssetLocator::UserMcp { name: name.clone() };
            let spec = if let Some(url) = &server_cfg.url {
                nuo_wire::security::AssetSpec::RemoteEndpoint {
                    url: url.clone(),
                    headers: server_cfg
                        .environment
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            } else {
                nuo_wire::security::AssetSpec::Process {
                    command: server_cfg.command.clone(),
                    env: server_cfg
                        .environment
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            };
            if let Ok(()) = ledger.trust_asset(&locator, &spec) {
                trusted.push(format!("user:mcp:{name}"));
            }
        }
    }
    trusted
}

pub(crate) fn deny_user_assets() -> Vec<String> {
    let config = Config::load();
    let ledger = nuo_persistence::AssetAttestationLedger::load();
    let mut denied = Vec::new();
    for (name, server_cfg) in &config.mcp {
        if server_cfg.sandbox_root.is_none() {
            let locator = nuo_wire::security::AssetLocator::UserMcp { name: name.clone() };
            let spec = if let Some(url) = &server_cfg.url {
                nuo_wire::security::AssetSpec::RemoteEndpoint {
                    url: url.clone(),
                    headers: server_cfg
                        .environment
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            } else {
                nuo_wire::security::AssetSpec::Process {
                    command: server_cfg.command.clone(),
                    env: server_cfg
                        .environment
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                }
            };
            if let Ok(()) = ledger.deny_asset(&locator, &spec) {
                denied.push(format!("user:mcp:{name}"));
            }
        }
    }
    denied
}

/// Parse a trust-domain name from user input.
///
/// Shared by the grant grammar and `/trust deny`, so a user learns one
/// vocabulary for both answers — the alternative is remembering one spelling for
/// yes and a different one for no.
pub(crate) fn parse_trust_domain(sub: &str) -> Result<TrustDomain, String> {
    let normalized = sub.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "mcp" | ".nuo/mcp.json" | "mcp.json" => Ok(TrustDomain::Mcp),
        "skills" | ".nuo/skills" | ".nuo/skills/" => Ok(TrustDomain::Skills),
        "hooks" | ".nuo/hooks" | ".nuo/hooks/" => Ok(TrustDomain::Hooks),
        "instructions" | "agents" | "rules" | "agents.md" => Ok(TrustDomain::Instructions),
        "ex-workspace" | "ex-workspaces" | "externals" | "workspace" | ".nuo/config.toml" => {
            Ok(TrustDomain::ExWorkspace)
        }
        "user-assets" | "user" | "user_assets" => Ok(TrustDomain::UserAssets),
        other => Err(format!(
            "Unknown trust domain `{other}`. Valid domains: `instructions` (AGENTS.md), `ex-workspace`, `mcp`, `skills`, `hooks`, `user-assets`, or `all`."
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustRoute {
    GrantAll,
    Grant(TrustDomain),
    /// Refuse the present content of every domain, recording the refusal.
    DenyAll,
    /// Refuse one domain's present content, recording the refusal.
    Deny(TrustDomain),
    Revoke,
    Status,
}

pub(crate) fn trust_route(name: &str, parts: &[&str]) -> Result<TrustRoute, String> {
    if name == "untrust" {
        return if parts.len() == 1 {
            Ok(TrustRoute::Revoke)
        } else {
            Err(
                "/untrust accepts no arguments. Use `/trust deny [domain]` to refuse \
                  project assets without revoking every grant."
                    .to_string(),
            )
        };
    }
    // `deny` is a distinct verb, not a modifier of `trust`: it records a human
    // refusal so the workspace stops asking until the content changes. It used
    // to have no command at all — `deny_domains()` existed and was reachable
    // only from the pre-attach gate's unselected rows — so the attach banner
    // could tell the user to "ignore" the escalation and then point at
    // `/untrust`, which revokes *every* grant and re-arms the gate as
    // `Quarantined`. The exit the message named did not exist.
    match parts.get(1).copied() {
        Some("deny") => match parts.get(2).copied() {
            None | Some("all") => Ok(TrustRoute::DenyAll),
            Some(domain) => parse_trust_domain(domain).map(TrustRoute::Deny),
        },
        _ => trust_route_grant(parts),
    }
}

fn trust_route_grant(parts: &[&str]) -> Result<TrustRoute, String> {
    match parts.get(1).copied() {
        None | Some("all") => Ok(TrustRoute::GrantAll),
        Some("mcp") => Ok(TrustRoute::Grant(TrustDomain::Mcp)),
        Some("skills") => Ok(TrustRoute::Grant(TrustDomain::Skills)),
        Some("hooks") => Ok(TrustRoute::Grant(TrustDomain::Hooks)),
        Some("instructions") | Some("agents") | Some("rules") => {
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        }
        Some("ex-workspace") | Some("ex-workspaces") | Some("externals") | Some("workspace") => {
            Ok(TrustRoute::Grant(TrustDomain::ExWorkspace))
        }
        Some("user-assets") | Some("user") | Some("user_assets") => {
            Ok(TrustRoute::Grant(TrustDomain::UserAssets))
        }
        Some("status") => Ok(TrustRoute::Status),
        Some("revoke") => Ok(TrustRoute::Revoke),
        Some(other) => Err(format!(
            "Unknown /trust subcommand '{other}'. Use `/trust`, `/trust all`, `/trust instructions`, \
             `/trust ex-workspace`, `/trust mcp`, `/trust skills`, `/trust hooks`, `/trust user-assets`, \
             `/trust deny [domain]`, `/trust status`, or `/trust revoke`."
        )),
    }
}
