//! Provider usage querying for Command Code.
//!
//! Queries `GET https://api.commandcode.ai/alpha/billing/credits` with the user's
//! Command Code bearer token to retrieve credits balance and five-hour/weekly limits.

use nuo_wire::{ProviderUsage, UsageMetric};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CreditsData {
    #[serde(default, rename = "monthlyCredits")]
    monthly_credits: f64,
    #[serde(default, rename = "purchasedCredits")]
    purchased_credits: f64,
    #[serde(default, rename = "freeCredits")]
    free_credits: f64,
}

#[derive(Debug, Deserialize)]
struct WindowLimit {
    #[serde(default)]
    used: f64,
    #[serde(default)]
    cap: f64,
}

#[derive(Debug, Deserialize)]
struct WindowLimits {
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
        provider == "commandcode" || provider == "commandcode-plan" || base_url.contains("commandcode.ai")
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

        let mut metrics = Vec::new();
        let mut primary_balance = None;

        if let Some(c) = body.credits {
            let total = c.monthly_credits + c.purchased_credits + c.free_credits;
            primary_balance = Some(format!("{:.2} credits", total));
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
        }

        if let Some(wl) = body.window_limits {
            if let Some(fh) = wl.five_hour {
                metrics.push(UsageMetric {
                    label: "5h Limit".to_string(),
                    value: format!("{:.2} / {:.0}", fh.used, fh.cap),
                    unit: Some("credits".to_string()),
                });
            }
            if let Some(w) = wl.weekly {
                metrics.push(UsageMetric {
                    label: "Weekly Limit".to_string(),
                    value: format!("{:.2} / {:.0}", w.used, w.cap),
                    unit: Some("credits".to_string()),
                });
            }
        }

        Ok(ProviderUsage {
            plan: None,
            description: Some("Command Code API credits".to_string()),
            quota: None,
            primary_balance,
            metrics,
            updated_at_ms: Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            ),
        })
    }
}
