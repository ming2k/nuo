//! Provider-dialect inheritance and service-family resolution.

use super::*;

#[test]
fn provider_dialect_is_inherited_independently_of_auth_and_remote_protocol() {
    use nuo_protocol::{GoogleGenerateContentDialect, ProviderDialect};
    let _sandbox = sandboxed_paths();
    for auth in [
        ConnectionAuth::Static,
        ConnectionAuth::negotiated("google-antigravity"),
    ] {
        for remote_protocol in [None, Some(WireProtocol::GoogleGenerateContent)] {
            let mut conn = instance("dialect-inheritance", Some("google-antigravity"));
            conn.auth = auth.clone();
            // An unknown generation must inherit without a baseline entry.
            let model = "gemini-future-flash-tiered";
            let mut cache = RemoteCatalogCache::default();
            cache
                .remote_metadata
                .entry(conn.name.clone())
                .or_default()
                .insert(
                    model.into(),
                    nuo_protocol::RemoteModelMetadata {
                        protocol: remote_protocol,
                        ..Default::default()
                    },
                );
            let channel = derive_channel(
                &conn,
                model,
                &inputs_of(
                    &cache,
                    &RouteSettingsStore::default(),
                    &Credentials::default(),
                ),
            )
            .unwrap();
            assert!(matches!(
                channel.transport,
                Transport::GoogleGenerateContent {
                    dialect: GoogleGenerateContentDialect::Antigravity,
                    ..
                }
            ));
            assert_eq!(channel.model, model);
        }
    }
    // Credential type cannot turn an ordinary Google service into Antigravity.
    let mut conn = instance("native-google", Some("google"));
    conn.auth = ConnectionAuth::negotiated("google-antigravity");
    let channel = derive_channel(
        &conn,
        "gemini-3.8-flash",
        &inputs_of(
            &RemoteCatalogCache::default(),
            &RouteSettingsStore::default(),
            &Credentials::default(),
        ),
    )
    .unwrap();
    assert!(matches!(
        channel.transport,
        Transport::GoogleGenerateContent {
            dialect: GoogleGenerateContentDialect::GenerativeLanguage,
            ..
        }
    ));
    assert_eq!(
        nuo_provider::model_provider_spec("google-antigravity")
            .unwrap()
            .dialect,
        ProviderDialect::Antigravity
    );
}
