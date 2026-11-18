//! Qoder (`scene-map`) live remote catalog discovery.

use protocol::ApiRoot;
use provider::catalog::{
    ModelListError, RemoteCatalogOptions, RemoteCatalogRequest, RemoteCatalogUpdate, append_query,
    finalize_catalog_response,
};
use serde_json::Value;

use crate::parse_scene_catalog;

pub const CATALOG_PATH: &str = "algo/api/v2/model/list";
pub const CATALOG_SIGNED_PATH: &str = "/api/v2/model/list";
pub const DEFAULT_SCENE: &str = "assistant";

/// Fetch and parse Qoder's COSY-signed scene-map catalog.
pub async fn fetch_remote_catalog(
    req: RemoteCatalogRequest<'_>,
    options: RemoteCatalogOptions<'_>,
) -> Result<RemoteCatalogUpdate, ModelListError> {
    let root = ApiRoot::parse(req.base_url).map_err(ModelListError::BadEndpoint)?;
    let endpoint = append_query(&root.append(CATALOG_PATH), &[("Encode", "1")]);
    let user_agent = req.user_agent.unwrap_or(provider::NUO_USER_AGENT);

    let signing = req.catalog_signing.ok_or_else(|| {
        ModelListError::Parse("the signed catalog shape requires a catalog signer".to_string())
    })?;
    let signed = signing
        .sign(CATALOG_SIGNED_PATH)
        .map_err(ModelListError::Parse)?;

    let mut request = provider::transport::Request::new(netune::Method::GET, &endpoint)
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

    let response = provider::transport::Http::control_plane()
        .map_err(ModelListError::Http)?
        .send(request)
        .await
        .map_err(ModelListError::Http)?;

    let scene = req
        .dimensions
        .iter()
        .find(|(name, _)| *name == "scene")
        .map(|(_, value)| *value)
        .unwrap_or(DEFAULT_SCENE);

    finalize_catalog_response(response, options, true, |json| {
        let valid = json
            .as_object()
            .is_some_and(|map| map.values().any(Value::is_array));
        if !valid {
            return Err(ModelListError::Parse(
                "response is not a scene-keyed map of model arrays".to_string(),
            ));
        }
        Ok(parse_scene_catalog(json, scene))
    })
}
