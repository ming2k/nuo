//! Live catalog fetch for standard industry shapes (`OpenAi`, `Anthropic`, `Google`).

use protocol::RemoteCatalogSource;
use provider::catalog::{fetch_bearer_models_response, finalize_catalog_response};

use super::{
    CatalogShape, DiscoveredModel, ModelListError, RemoteCatalogOptions, RemoteCatalogRequest,
    RemoteCatalogUpdate, models_endpoint_for, parse_models_with_dimensions, validate_catalog_shape,
};

/// Pinned Anthropic API version header sent on `/models` requests.
const ANTHROPIC_VERSION: &str = "2023-06-01";

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

/// Fetch or conditionally revalidate a live model catalog for a standard
/// [`CatalogShape`].
pub async fn fetch_remote_catalog(
    req: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let protocol = match req.source {
        RemoteCatalogSource::Endpoint(shape) => shape,
        RemoteCatalogSource::Custom(id) => {
            return Err(ModelListError::BadEndpoint(format!(
                "custom catalog source '{id}' must be dispatched to its owning provider"
            )));
        }
        RemoteCatalogSource::None => {
            return Err(ModelListError::BadEndpoint(
                "provider declares no remote catalog".to_string(),
            ));
        }
    };

    let endpoint = models_endpoint_for(protocol, req.base_url)?;
    let user_agent = req.user_agent.unwrap_or(provider::NUO_USER_AGENT);
    let client = provider::transport::Http::control_plane().map_err(ModelListError::Http)?;

    let response = match protocol {
        CatalogShape::OpenAi => fetch_bearer_models_response(&req, options).await?,
        CatalogShape::Anthropic => {
            let mut request = provider::transport::Request::new(netune::Method::GET, &endpoint)
                .header("user-agent", user_agent)
                .header("x-api-key", req.api_key.expose_secret())
                .header("anthropic-version", ANTHROPIC_VERSION);
            if req.api_key.expose_secret().trim().is_empty() {
                request = request.header("x-api-key", "");
            }
            for (name, value) in req.extra_headers {
                request = request.header(name, *value);
            }
            client.send(request).await.map_err(ModelListError::Http)?
        }
        CatalogShape::Google => {
            let mut request = provider::transport::Request::new(netune::Method::GET, &endpoint)
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

    finalize_catalog_response(response, options, true, |json| {
        validate_catalog_shape(protocol, json)?;
        Ok(parse_models_with_dimensions(protocol, json, req.dimensions))
    })
}
