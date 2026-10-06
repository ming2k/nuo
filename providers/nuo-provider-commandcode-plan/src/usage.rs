//! Provider usage querying for Command Code.
//!
//! Queries `GET https://api.commandcode.ai/alpha/billing/credits` with the user's
//! Command Code bearer token to retrieve the credit ledger and the
//! five-hour / weekly window limits, and normalizes them into the generic
//! [`nuo_wire::ProviderUsage`] model.
//!
//! This is the `providers/` mirror of `nuo-provider-adapters`'s fetcher. It is
//! a pure projection of the control plane's answer (ADR-0014 `[INV-LANE-05]`):
//! the client renders the reported credits and windows verbatim and never
//! recomputes either. Response shape: `docs/reference/commandcode-api.yml`
//! (`/alpha/billing/credits`).

use nuo_wire::{
    BalanceQuota, PeriodicQuota, ProviderQuotaData, ProviderUsage, QuotaWindowBucket,
    QuotaWindowKind, UsageMetric,
};
use serde::Deserialize;

/// The credit ledger (`/alpha/billing/credits` → `credits`).
#[derive(Debug, Deserialize)]
struct CreditsData {
    #[serde(default, rename = "planId")]
    plan_id: Option<String>,
    #[serde(default, rename = "monthlyCredits")]
    monthly_credits: f64,
    #[serde(default, rename = "purchasedCredits")]
    purchased_credits: f64,
    #[serde(default, rename = "freeCredits")]
    free_credits: f64,
}

/// One rolling window limit (`windowLimits.fiveHour` / `windowLimits.weekly`).
#[derive(Debug, Deserialize)]
struct WindowLimit {
    #[serde(default)]
    used: f64,
    #[serde(default)]
    cap: f64,
    #[serde(default, rename = "resetAt")]
    reset_at: Option<u64>,
}

/// The windowed rate limits (`windowLimits`).
#[derive(Debug, Deserialize)]
struct WindowLimits {
    #[serde(default)]
    limited: bool,
    #[serde(default)]
    exceeded: Option<String>,
    #[serde(default, rename = "fiveHour")]
    five_hour: Option<WindowLimit>,
    #[serde(default)]
    weekly: Option<WindowLimit>,
}

#[derive(Debug, Deserialize)]
struct CommandCodeCreditsResponse {
    #[serde(default)]
    credits: Option<CreditsData>,
    #[serde(default, rename = "windowLimits")]
    window_limits: Option<WindowLimits>,
}

pub struct CommandCodeUsageFetcher;

impl CommandCodeUsageFetcher {
    pub fn matches(&self, provider: &str, base_url: &str) -> bool {
        // The canonical provider id is `commandcode-plan` (ADR-0201); the
        // legacy `commandcode` spelling is kept for old stored connections.
        matches!(provider, "commandcode" | "commandcode-plan")
            || base_url.contains("commandcode.ai")
    }

    pub async fn fetch_usage(
        &self,
        client: &nuo_provider_transport::http::Http,
        _base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        let endpoint = "https://api.commandcode.ai/alpha/billing/credits";
        let auth = format!("Bearer {api_key}");
        let resp = client
            .get(
                endpoint,
                &[
                    ("authorization", auth.as_str()),
                    ("accept", "application/json"),
                    ("user-agent", nuo_provider_transport::NUO_USER_AGENT),
                ],
            )
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.is_success() {
            return Err(format!("HTTP {}: {}", resp.status, resp.body));
        }

        let body: CommandCodeCreditsResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse Command Code credits response: {e}"))?;

        Ok(parse_commandcode_credits(body))
    }
}

/// Human-facing badge for a CommandCode `planId`. Plan ids are server data
/// (ADR-0014), so an unknown id is humanized rather than dropped.
fn plan_label(plan_id: &str) -> String {
    let trimmed = plan_id.trim();
    if trimmed.is_empty() {
        return "Command Code".to_string();
    }
    trimmed
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn reset_at_ms(raw: Option<u64>) -> Option<u64> {
    raw.filter(|tick| *tick > 0)
}

fn window_bucket(label: &str, window: QuotaWindowKind, limit: WindowLimit) -> QuotaWindowBucket {
    let used_fraction = if limit.cap > 0.0 {
        (limit.used / limit.cap).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    QuotaWindowBucket {
        window: Some(window),
        label: label.to_string(),
        group: None,
        used_fraction,
        used_amount: Some(limit.used),
        total_limit: Some(limit.cap),
        unit: Some("credits".to_string()),
        reset_at_ms: reset_at_ms(limit.reset_at),
        reset_time_str: None,
    }
}

/// Project the credits envelope into the generic usage model.
///
/// Infallible: the wire body is already deserialized, and every field is
/// optional, so an empty envelope yields an empty-but-valid projection rather
/// than an error.
fn parse_commandcode_credits(body: CommandCodeCreditsResponse) -> ProviderUsage {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    let mut metrics = Vec::new();
    let mut balance = None;
    let mut remaining: Option<f64> = None;
    let mut plan_id: Option<String> = None;

    if let Some(c) = body.credits {
        plan_id = c.plan_id.clone();
        let total = c.monthly_credits + c.purchased_credits + c.free_credits;
        remaining = Some(total);
        balance = Some(BalanceQuota {
            currency: "credits".to_string(),
            total_balance: Some(total),
            cash_balance: (c.purchased_credits > 0.0).then_some(c.purchased_credits),
            voucher_balance: (c.free_credits > 0.0).then_some(c.free_credits),
            credit_limit: None,
            consumed_amount: None,
            display_primary: Some(format!("{total:.2} credits")),
        });
        metrics.push(UsageMetric {
            label: "Monthly Credits".to_string(),
            value: format!("{:.2}", c.monthly_credits),
            unit: Some("credits".to_string()),
        });
        if c.purchased_credits > 0.0 {
            metrics.push(UsageMetric {
                label: "Purchased Credits".to_string(),
                value: format!("{:.2}", c.purchased_credits),
                unit: Some("credits".to_string()),
            });
        }
        if c.free_credits > 0.0 {
            metrics.push(UsageMetric {
                label: "Free Credits".to_string(),
                value: format!("{:.2}", c.free_credits),
                unit: Some("credits".to_string()),
            });
        }
    }

    let mut periodic = None;
    if let Some(wl) = body.window_limits {
        let mut buckets = Vec::new();
        if let Some(fh) = wl.five_hour {
            buckets.push(window_bucket("5-Hour Limit", QuotaWindowKind::Rolling5Hour, fh));
        }
        if let Some(w) = wl.weekly {
            buckets.push(window_bucket("Weekly Limit", QuotaWindowKind::Weekly, w));
        }
        if !buckets.is_empty() {
            periodic = Some(PeriodicQuota { buckets });
        }
        if wl.limited
            && let Some(reason) = wl.exceeded.as_deref().filter(|r| !r.is_empty())
        {
            metrics.push(UsageMetric {
                label: "Window Limit".to_string(),
                value: format!("exceeded ({reason})"),
                unit: None,
            });
        }
    }

    let quota = if balance.is_some() || periodic.is_some() {
        Some(ProviderQuotaData::Composite {
            balance,
            periodic,
            rate_limits: Vec::new(),
        })
    } else {
        None
    };

    ProviderUsage {
        plan: Some(
            plan_id
                .as_deref()
                .map(plan_label)
                .unwrap_or_else(|| "Command Code".to_string()),
        ),
        description: Some("Command Code API credits".to_string()),
        quota,
        primary_balance: remaining.map(|r| format!("{r:.2} credits")),
        metrics,
        updated_at_ms: now_ms,
    }
}
