//! `nuo quota [provider] [--refresh]` CLI command (ADR-0036).
//!
//! Provides fast, headless terminal inspection of provider quotas, sliding-window
//! allowances, and account pool status.

use std::error::Error;
use tokio::sync::mpsc;

use nuo_wire::{AgentResponse, ConnectionUsageState, ProviderQuotaData, ProviderQuotaSnapshot};

pub async fn run(
    provider_filter: Option<String>,
    force_refresh: bool,
) -> Result<(), Box<dyn Error>> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    nuo_server::handlers_provider::query_provider_quotas(&tx, provider_filter, force_refresh)
        .await;
    drop(tx);

    let mut last_snapshot = None;
    while let Some(msg) = rx.recv().await {
        if let AgentResponse::ProviderQuotas(snap) = msg {
            last_snapshot = Some(snap);
        }
    }

    let Some(snapshot) = last_snapshot else {
        eprintln!("nuo: failed to retrieve provider quota overview");
        return Ok(());
    };

    render_quota_table(&snapshot);
    Ok(())
}

fn render_quota_table(snapshot: &ProviderQuotaSnapshot) {
    if snapshot.entries.is_empty() {
        if let Some(ref filter) = snapshot.provider_filter {
            println!("No quota-capable connections found for provider `{filter}`.");
        } else {
            println!("No connections with remote quota tracking configured.");
        }
        return;
    }

    println!();
    let title = if let Some(ref filter) = snapshot.provider_filter {
        format!("Provider Quota Pool [{filter}]")
    } else {
        "Provider Quota Pool".to_string()
    };
    println!("{title}");
    println!("{}", "─".repeat(88));
    println!(
        "{:<20} {:<22} {:<12} {:<14} {:<12} {}",
        "CONNECTION", "PROVIDER", "STATUS", "BALANCE / QUOTA", "RESET IN", "ACCOUNT"
    );
    println!("{}", "─".repeat(88));

    for entry in &snapshot.entries {
        let name_col = if entry.is_default {
            format!("{} (active)", entry.name)
        } else {
            entry.name.clone()
        };

        let status_col = match &entry.state {
            ConnectionUsageState::Available(_) => "AVAILABLE".to_string(),
            ConnectionUsageState::Fetching => "FETCHING".to_string(),
            ConnectionUsageState::Unsupported => "UNSUPPORTED".to_string(),
            ConnectionUsageState::Error(err) => {
                if err.contains("429") || err.contains("exhausted") {
                    "DEPLETED".to_string()
                } else {
                    "ERROR".to_string()
                }
            }
        };

        let balance_col = entry
            .primary_balance
            .clone()
            .or_else(|| {
                entry.quota.as_ref().and_then(|q| match q {
                    ProviderQuotaData::Periodic(p) => p.buckets.first().map(|b| {
                        let pct = (b.used_fraction * 100.0).round() as u32;
                        format!("{pct}% used")
                    }),
                    ProviderQuotaData::Balance(b) => b.display_primary.clone().or_else(|| {
                        b.total_balance.map(|t| format!("{t:.2} {}", b.currency))
                    }),
                    ProviderQuotaData::Composite {
                        balance, periodic, ..
                    } => balance
                        .as_ref()
                        .and_then(|b| {
                            b.display_primary.clone().or_else(|| {
                                b.total_balance.map(|t| format!("{t:.2} {}", b.currency))
                            })
                        })
                        .or_else(|| {
                            periodic.as_ref().and_then(|p| {
                                p.buckets.first().map(|b| {
                                    let pct = (b.used_fraction * 100.0).round() as u32;
                                    format!("{pct}% used")
                                })
                            })
                        }),
                })
            })
            .unwrap_or_else(|| "-".to_string());

        let reset_col = entry
            .earliest_reset_ms
            .map(|rt| {
                let now = chrono::Utc::now().timestamp_millis() as u64;
                if rt > now {
                    let diff_secs = (rt - now) / 1000;
                    let hours = diff_secs / 3600;
                    let mins = (diff_secs % 3600) / 60;
                    if hours > 0 {
                        format!("in {hours}h {mins}m")
                    } else {
                        format!("in {mins}m")
                    }
                } else {
                    "due".to_string()
                }
            })
            .unwrap_or_else(|| "-".to_string());

        let account_col = entry.account_id.as_deref().unwrap_or("-");

        println!(
            "{:<20} {:<22} {:<12} {:<14} {:<12} {}",
            truncate(&name_col, 19),
            truncate(&entry.provider, 21),
            status_col,
            balance_col,
            reset_col,
            truncate(account_col, 20)
        );
    }

    println!("{}", "─".repeat(88));
    println!(
        "Pool Health: {} accounts total  •  {} available  •  {} depleted / error",
        snapshot.total_accounts, snapshot.available_accounts, snapshot.depleted_accounts
    );
    println!();
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    } else {
        s.to_string()
    }
}
