//! Live catalog fetch: request construction, auth, conditional revalidation.

use std::collections::HashSet;

use serde_json::Value;

use super::{
    CatalogShape, DiscoveredModel, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate, append_query, models_endpoint_for, parse_models_with_dimensions,
    validate_catalog_shape,
};

pub async fn list_models(
    req: RemoteCatalogRequest<'_>,
) -> Result<Vec<DiscoveredModel>, ModelListError> {
    match fetch_remote_catalog(req, RemoteCatalogOptions::default()).await? {
        RemoteCatalogUpdate::Modified { models, .. } => Ok(models),
        RemoteCatalogUpdate::NotModified { .. } => Err(ModelListError::Parse(
            "endpoint returned 304 without a conditional request".to_string(),
        )),
    }
}

/// Fetch or conditionally revalidate a live model catalog. Unlike
/// [`list_models`], this retains the response ETag and represents HTTP 304
/// without forcing callers to discard their cached catalog.
pub async fn fetch_remote_catalog(
    req: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let endpoint = models_endpoint_for(req.protocol, req.base_url)?;
    let user_agent = req.user_agent.unwrap_or(nuo_provider::NUO_USER_AGENT);

    let client = nuo_provider::transport::Http::control_plane().map_err(ModelListError::Http)?;

    let response = match req.protocol {
        CatalogShape::SceneMap => {
            // The catalog rides the dialect's own request signing. The signer
            // and the identity headers are supplied by the catalog sync layer as
            // data, so this fetcher names no provider.
            let signing = req.catalog_signing.ok_or_else(|| {
                ModelListError::Parse(
                    "the signed catalog shape requires a catalog signer".to_string(),
                )
            })?;
            let signed_path = req.protocol.signed_path().ok_or_else(|| {
                ModelListError::Parse("catalog shape declares no signed path".to_string())
            })?;
            let url = append_query(&endpoint, req.protocol.query());
            let signed = signing.sign(signed_path).map_err(ModelListError::Parse)?;
            let mut request = nuo_provider::transport::Request::new(netune::Method::GET, &url)
                .header("user-agent", user_agent)
                .header("authorization", signed.authorization)
                .header("cosy-date", signed.date)
                .header("cosy-key", signed.key);
            for (name, value) in signing.identity_subject_headers() {
                request = request.header(name.as_str(), value);
            }
            for (name, value) in signing.identity_headers() {
                request = request.header(name.as_str(), value);
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            nuo_provider::transport::Http::control_plane()
                .map_err(ModelListError::Http)?
                .send(request)
                .await
                .map_err(ModelListError::Http)?
        }
        CatalogShape::OpenAi => {
            // OpenAI auth: a bearer when a key is set, NO header when keyless
            // (some relays reject a malformed bearer). Mirrors the chat path.
            let mut request = nuo_provider::transport::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent);
            if !req.api_key.expose_secret().trim().is_empty() {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", req.api_key.expose_secret()),
                );
            }
            if let Some(etag) = options.etag {
                request = request.header("if-none-match", etag);
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Codex => {
            // ChatGPT Codex models catalog: requires client_version query param,
            // originator header, and optional ChatGPT-Account-Id header.
            const CODEX_VERSION: &str = "0.153.4";
            let endpoint = append_query(&endpoint, &[("client_version", CODEX_VERSION)]);
            let mut request = nuo_provider::transport::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent)
                .header("originator", "codex_cli_rs");
            if !req.api_key.expose_secret().trim().is_empty() {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", req.api_key.expose_secret()),
                );
            }
            if let Some(account_id) = req.account_id {
                request = request.header("chatgpt-account-id", account_id);
            }
            if let Some(etag) = options.etag {
                request = request.header("if-none-match", etag);
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Anthropic => {
            // Anthropic auth: x-api-key + the pinned API version. The version
            // header is mandatory on every Anthropic request including the
            // models list endpoint.
            let mut request = nuo_provider::transport::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent)
                .header("x-api-key", req.api_key.expose_secret())
                .header("anthropic-version", anthropic_version());
            if req.api_key.expose_secret().trim().is_empty() {
                // A keyless request still sends the headers (harmless) but
                // most Anthropic relays require a key; the snapshot fallback
                // covers the keyless-misconfigured case.
                request = request.header("x-api-key", "");
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::GoogleCloudCode => {
            // Wire truth (live 2026-10-14): `fetchAvailableModels` takes
            // exactly `{"project": "…"}` with only the auth headers — no
            // `x-goog-api-client` (the shipped client never sends it).
            // Consumer accounts route through companion project `aicode-consumers`
            // unless an explicit non-empty account/project ID was provided.
            let project = req
                .account_id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or("aicode-consumers");
            let mut request =
                nuo_provider::transport::Request::new(netune::Method::POST, &endpoint)
                    .header("user-agent", user_agent)
                    .json(&serde_json::json!({ "project": project }));
            if !req.api_key.expose_secret().trim().is_empty() {
                request = request.header(
                    "authorization",
                    format!("Bearer {}", req.api_key.expose_secret()),
                );
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Google => {
            // Google AI Studio auth: x-goog-api-key header is the official standard.
            // OAuth/IAM bearer tokens use Authorization: Bearer.
            // Avoid leaking credentials in URL query parameters.
            let mut request = nuo_provider::transport::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent);
            let secret = req.api_key.expose_secret().trim();
            if !secret.is_empty() {
                if secret.starts_with("ya29.") || secret.starts_with("Bearer ") {
                    let bearer = if secret.starts_with("Bearer ") {
                        secret.to_string()
                    } else {
                        format!("Bearer {secret}")
                    };
                    request = request.header("authorization", bearer);
                } else {
                    request = request.header("x-goog-api-key", secret);
                }
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
    };

    let status = response.status;
    let response_etag = response
        .headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if status == http::StatusCode::NOT_MODIFIED {
        return Ok(RemoteCatalogUpdate::NotModified {
            etag: response_etag.or_else(|| options.etag.map(str::to_string)),
        });
    }
    if !status.is_success() {
        return Err(ModelListError::Status(status.as_u16(), response.body));
    }

    let body = response.body;
    let json: Value = serde_json::from_str(&body)
        .map_err(|e| ModelListError::Parse(format!("response is not valid JSON: {e}")))?;

    validate_catalog_shape(req.protocol, &json)?;
    let mut models = parse_models_with_dimensions(req.protocol, &json, req.dimensions);
    if req.protocol == CatalogShape::Codex || req.protocol == CatalogShape::GoogleCloudCode {
        // Codex and Google CloudCode have semantic curation order (agentModelSorts),
        // so preserve it while discarding duplicate slugs.
        let mut seen = HashSet::new();
        models.retain(|model| seen.insert(model.id.clone()));
    } else {
        // Stable order regardless of API ordering: sort by id then de-dup.
        models.sort_by(|a, b| a.id.cmp(&b.id));
        models.dedup_by(|a, b| a.id == b.id);
    }
    Ok(RemoteCatalogUpdate::Modified {
        models,
        etag: response_etag,
    })
}

/// The pinned Anthropic API version sent on every request. Mirrors the chat
/// request header so a relay that pins a version accepts the models call too.
fn anthropic_version() -> &'static str {
    nuo_provider_anthropic::request::ANTHROPIC_VERSION
}
