//! One-shot live capture of the raw Qoder scene catalog, for re-deriving the
//! wire contract when Qoder ships a new client. Run manually:
//!
//! ```text
//! cargo run -p nuo-server --example qoder_catalog_dump -- <connection> [out.json]
//! ```
//!
//! It writes the response **verbatim** (pretty-printed, no field filtering) so
//! the artifact stays a faithful record of what the server said rather than of
//! what nuo currently parses out of it — a parser bug must not be able to hide
//! inside its own evidence base.
//!
//! This exists because the recon fixtures that originally characterized the
//! surface lived under `/tmp` and did not survive a reboot (integration doc
//! §5.4). `[INV-VAL-02]` requires that tests never depend on unversioned local
//! artifacts, so the captured JSON is committed under `tests/fixtures/` and
//! this example is the documented way to regenerate it.
//!
//! Hits the real API, so it is deliberately an example, not a test.

// A one-shot operator tool: failing loudly with `expect`/`panic!` on a broken
// pre-condition is the point, not a defect to handle gracefully.
#![allow(clippy::expect_used, clippy::panic)]

use nuo_model_codec::CatalogShape;
use nuo_model_codec::{ConnectionAuth, CredentialSource as _};
use nuo_provider_transport::http::{Http, Request};
use nuo_oauth::OAuthCredentialSource;
use nuo_provider_qoder::oauth::stored_qoder_request_identity;
use nuo_provider_qoder::QoderCatalogSigning;
use nuo_provider::CatalogSigning;
use nuo_provider::catalog_root_for_connection;
use nuo_provider_catalog::models_endpoint_for;

/// The catalog URL, built from the connection's elected inference host (falling
/// back to the pinned spec root) exactly as the catalog sync layer does, so the
/// dump exercises the same transport the server uses.
fn catalog_url(base: &str) -> Result<String, String> {
    let endpoint = models_endpoint_for(CatalogShape::SceneMap, base).map_err(|e| e.to_string())?;
    let mut url = endpoint;
    for (index, (name, value)) in CatalogShape::SceneMap.query().iter().enumerate() {
        url.push(if index == 0 && !url.contains('?') {
            '?'
        } else {
            '&'
        });
        url.push_str(name);
        url.push('=');
        url.push_str(value);
    }
    Ok(url)
}

#[tokio::main]
async fn main() {
    let connection = std::env::args().nth(1).unwrap_or_else(|| "qod".to_string());
    let out_path = std::env::args().nth(2);

    // Resolve the bearer through the real credential source so an expiring
    // token refreshes rather than failing the capture. The host points at this
    // machine's state directory; the example is an operator tool, so the paths
    // are the shipped ones.
    let home = std::env::var("HOME").expect("HOME");
    let state_dir = std::path::Path::new(&home).join(".local/state/nuo");
    let host = nuo_provider::CredentialHost::file_backed(
        state_dir.join("auth.toml"),
        state_dir.join("machine_id"),
    );
    let source = OAuthCredentialSource::new(
        &host,
        connection.clone(),
        ConnectionAuth::Subscription {
            provider: std::borrow::Cow::Borrowed("qoder"),
        },
    );
    let resolved = source
        .resolve_auth()
        .await
        .unwrap_or_else(|e| panic!("could not resolve credentials for '{connection}': {e}"));
    let bearer = resolved.token.expose_secret().to_string();

    // The stored identity carries the elected inference endpoint and the `uid`
    // that binds the COSY signature; without it the service returns
    // `403 code 101` (integration doc §5.2).
    let identity = stored_qoder_request_identity(host.store().as_ref(), &connection)
        .unwrap_or_else(|| {
            panic!("no stored Qoder identity for '{connection}'; authorize it first")
        });
    let base = catalog_root_for_connection(host.store().as_ref(), &connection)
        .unwrap_or_else(|| "https://api3.qoder.sh".to_string());
    println!("uid={} base={base}", identity.uid);

    let signing: Box<dyn CatalogSigning> = Box::new(QoderCatalogSigning::new(identity, bearer));
    let signed_path = CatalogShape::SceneMap
        .signed_path()
        .expect("the scene-map shape declares a signed path");
    let url = catalog_url(&base).expect("catalog url");
    let signed = signing.sign(signed_path).expect("catalog signature");

    let mut request = Request::new(http::Method::GET, &url)
        .header("authorization", signed.authorization)
        .header("cosy-date", signed.date)
        .header("cosy-key", signed.key);
    for (name, value) in signing.identity_subject_headers() {
        request = request.header(&name, value);
    }
    for (name, value) in signing.identity_headers() {
        request = request.header(&name, value);
    }

    let client = Http::control_plane().expect("http client");
    let reply = client.send(request).await.expect("catalog request");
    if !reply.status.is_success() {
        panic!(
            "catalog returned HTTP {}: {}",
            reply.status.as_u16(),
            &reply.body[..reply.body.len().min(500)]
        );
    }

    // Re-parse only to pretty-print and summarize; no field is dropped. A
    // non-JSON body is a protocol change and must abort loudly rather than be
    // committed as if it were the documented shape.
    let parsed: serde_json::Value = serde_json::from_str(&reply.body)
        .unwrap_or_else(|e| panic!("catalog body is not JSON ({e}); raw:\n{}", reply.body));
    let pretty = serde_json::to_string_pretty(&parsed).expect("re-serialize");

    println!("HTTP {} — scenes:", reply.status.as_u16());
    if let Some(scenes) = parsed.as_object() {
        for (scene, entries) in scenes {
            println!(
                "  {scene:18} {:3} entries",
                entries.as_array().map(Vec::len).unwrap_or(0)
            );
        }
    }

    match out_path {
        Some(path) => {
            std::fs::write(&path, format!("{pretty}\n")).expect("write fixture");
            println!("wrote {path}");
        }
        None => println!("{pretty}"),
    }
}
