//! Qoder usage / quota fetcher.
//!
//! Query endpoint: `GET https://openapi.qoder.sh/api/v1/userinfo` (CN:
//! `openapi.qoder.com.cn`). Plain bearer — no COSY signature on the OpenAPI
//! surface. The response exposes subscription state and usage counters; the
//! scheme degrades gracefully when fields are absent (trial accounts report
//! fewer fields than paid plans).

use nuo_contracts::async_trait;
use nuo_contracts::{BalanceQuota, ProviderQuotaData, ProviderUsage, UsageMetric};
use serde::Deserialize;

use super::ProviderUsageFetcher;

#[derive(Debug, Deserialize)]
pub(crate) struct QoderUserInfoResponse {
    #[serde(default)]
    pub(crate) code: Option<i64>,
    #[serde(default)]
    pub(crate) data: Option<QoderUserData>,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QoderUserData {
    #[serde(default, alias = "userId")]
    pub(crate) uid: Option<String>,
    #[serde(default)]
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) plan: Option<QoderPlan>,
    #[serde(default, alias = "subscription")]
    pub(crate) plan_info: Option<QoderPlan>,
    #[serde(default)]
    pub(crate) usage: Option<QoderUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QoderPlan {
    #[serde(default, alias = "planName", alias = "name")]
    pub(crate) plan_name: Option<String>,
    #[serde(default, alias = "expireTime", alias = "expire_time")]
    pub(crate) expire_at: Option<i64>,
    #[serde(default)]
    pub(crate) status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct QoderUsage {
    /// Remaining credits this period, when the account exposes a numeric one.
    #[serde(default)]
    pub(crate) remaining: Option<f64>,
    #[serde(default)]
    pub(crate) total: Option<f64>,
    #[serde(default, alias = "usedQuota")]
    pub(crate) used: Option<f64>,
}

pub struct QoderUsageFetcher;

#[async_trait]
impl ProviderUsageFetcher for QoderUsageFetcher {
    fn matches(&self, provider: &str, base_url: &str) -> bool {
        provider == "qoder" || base_url.contains("qoder.sh") || base_url.contains("qoder.com")
    }

    async fn fetch_usage(
        &self,
        client: &crate::http::Http,
        base_url: &str,
        api_key: &str,
    ) -> Result<ProviderUsage, String> {
        // The OpenAPI host is orthogonal to the inference host: the fetcher
        // always talks to the international OpenAPI endpoint (the CN variant
        // is selected by the credential's issuer at the OAuth layer).
        let endpoint = if base_url.contains("openapi.qoder.com.cn") {
            "https://openapi.qoder.com.cn/api/v1/userinfo".to_string()
        } else {
            "https://openapi.qoder.sh/api/v1/userinfo".to_string()
        };

        let resp = client
            .get(
                &endpoint,
                &[
                    ("authorization", format!("Bearer {api_key}").as_str()),
                    ("accept", "application/json"),
                ],
            )
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !resp.is_success() {
            return Err(format!("HTTP {}: {}", resp.status, resp.body));
        }

        let body: QoderUserInfoResponse = serde_json::from_str(&resp.body)
            .map_err(|e| format!("Failed to parse Qoder userinfo response: {e}"))?;

        parse_qoder_userinfo(body)
    }
}

pub(crate) fn parse_qoder_userinfo(
    body: QoderUserInfoResponse,
) -> Result<ProviderUsage, String> {
    // Non-zero codes are errors; a missing code with data present is fine.
    if let Some(code) = body.code
        && code != 0
    {
        return Err(format!(
            "Qoder userinfo error (code {code}): {}",
            body.message.unwrap_or_else(|| "unknown".to_string())
        ));
    }
    let data = body
        .data
        .ok_or_else(|| "Qoder userinfo returned no data".to_string())?;

    let mut metrics = Vec::new();
    let mut primary_balance = None;
    let mut balance_quota = BalanceQuota {
        currency: "credits".to_string(),
        total_balance: None,
        cash_balance: None,
        voucher_balance: None,
        credit_limit: None,
        consumed_amount: None,
        display_primary: None,
    };

    if let Some(usage) = &data.usage {
        if let Some(remaining) = usage.remaining {
            primary_balance = Some(format!("{remaining:.1} credits"));
            balance_quota.total_balance = Some(remaining);
            balance_quota.display_primary = primary_balance.clone();
            metrics.push(UsageMetric {
                label: "Remaining Credits".to_string(),
                value: format!("{remaining:.1}"),
                unit: Some("credits".to_string()),
            });
        }
        if let Some(total) = usage.total {
            balance_quota.credit_limit = Some(total);
            metrics.push(UsageMetric {
                label: "Total Credits".to_string(),
                value: format!("{total:.1}"),
                unit: Some("credits".to_string()),
            });
        }
        if let Some(used) = usage.used {
            balance_quota.consumed_amount = Some(used);
            metrics.push(UsageMetric {
                label: "Used Credits".to_string(),
                value: format!("{used:.1}"),
                unit: Some("credits".to_string()),
            });
        }
    }

    let plan_label = data
        .plan
        .as_ref()
        .or(data.plan_info.as_ref())
        .and_then(|p| p.plan_name.clone());
    // Expired or inactive plans surface as a description note.
    let plan_expired = data
        .plan
        .as_ref()
        .or(data.plan_info.as_ref())
        .map(|p| {
            let expired = p.expire_at.map(|t| t < now_ms_estimate() as i64 * 1000);
            matches!(p.status.as_deref(), Some("expired")) || expired == Some(true)
        })
        .unwrap_or(false);
    // Account identity fields feed the connection detail view (who am I
    // connected as) and match the account-scoped semantics other fetchers
    // expose; they are surfaced as metrics so the UI shows the account.
    let mut metrics = metrics;
    if let Some(email) = &data.email {
        metrics.push(UsageMetric {
            label: "Account".to_string(),
            value: email.clone(),
            unit: None,
        });
    }
    if let Some(uid) = &data.uid {
        metrics.push(UsageMetric {
            label: "UID".to_string(),
            value: uid.clone(),
            unit: None,
        });
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .ok();

    Ok(ProviderUsage {
        plan: plan_label,
        description: plan_expired.then(|| "plan expired".to_string()),
        quota: Some(ProviderQuotaData::Balance(balance_quota)),
        primary_balance,
        metrics,
        updated_at_ms: now_ms,
    })
}

/// Current unix milliseconds (for plan-expiry comparisons; seconds→ms scale).
fn now_ms_estimate() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_paid_plan_response() {
        let body: QoderUserInfoResponse = serde_json::from_str(
            r#"{"code":0,"data":{"uid":"u1","email":"a@b.c",
                "plan":{"plan_name":"Qoder Pro","expire_at":1800000000000,"status":"active"},
                "usage":{"remaining":120.5,"total":300.0,"used":179.5}}}"#,
        )
        .unwrap();
        let usage = parse_qoder_userinfo(body).unwrap();
        assert_eq!(usage.plan.as_deref(), Some("Qoder Pro"));
        let primary = usage.primary_balance.unwrap();
        assert_eq!(primary, "120.5 credits");
        assert!(usage.metrics.iter().any(|m| m.label == "Remaining Credits"));
        assert!(usage.metrics.iter().any(|m| m.label == "Total Credits"));
    }

    #[test]
    fn parses_trial_shape_without_usage() {
        let body: QoderUserInfoResponse = serde_json::from_str(
            r#"{"code":0,"data":{"uid":"u2","email":"x@y.z"}}"#,
        )
        .unwrap();
        let usage = parse_qoder_userinfo(body).unwrap();
        assert!(usage.primary_balance.is_none());
        // Trial accounts report only account identity (Account + UID), no
        // credit metrics.
        assert!(
            usage.metrics.iter().all(|m| m.label == "Account" || m.label == "UID"),
            "trial metrics must carry identity only: {:?}",
            usage.metrics
        );
        assert!(!usage.metrics.iter().any(|m| m.label.contains("Credits")));
    }

    #[test]
    fn error_code_surfaces_message() {
        let body: QoderUserInfoResponse =
            serde_json::from_str(r#"{"code":401,"message":"token expired"}"#).unwrap();
        let err = parse_qoder_userinfo(body).unwrap_err();
        assert!(err.contains("401") && err.contains("token expired"));
    }

    #[test]
    fn fetcher_matches_qoder_surfaces() {
        assert!(QoderUsageFetcher.matches("qoder", "https://api2.qoder.sh"));
        assert!(QoderUsageFetcher.matches("other", "https://openapi.qoder.com.cn/x"));
        assert!(!QoderUsageFetcher.matches("openai", "https://api.openai.com"));
    }
}
