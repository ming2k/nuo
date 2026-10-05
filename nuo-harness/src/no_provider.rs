//! The explicit "no provider configured" sentinel.
//!
//! Replaces the former implicit `MockProvider` fallback in [`crate::catalog`].
//! When the catalog cannot resolve a real channel for a provider id (unknown
//! id, or the entry has no usable channel), the startup install site installs
//! a [`NoProvider`] into the shared holder so the type still satisfies
//! `Arc<dyn Provider>`. The chat dispatch in `nuo-server` checks
//! [`NoProvider::ID`] up-front and refuses the send with a user-facing
//! notification, so a [`NoProvider`] should never actually be invoked — its
//! [`nuo_wire::Provider`] impl is a defensive backstop that
//! returns a clear error if it ever is.

use async_trait::async_trait;
use futures::stream::BoxStream;
use nuo_wire::{ModelRequest, Provider, ProviderCompletion};

/// The provider id reported by [`NoProvider`]. Callers that need to gate on
/// "is there a real provider installed?" compare against this constant (or
/// prefer the [`NoProvider::is`] helper, which is the recommended path).
pub const NO_PROVIDER_ID: &str = "none";

/// A non-functional provider used as the placeholder when the catalog cannot
/// resolve a real provider/channel.
///
/// Installed into the shared provider holder at startup so the holder always
/// contains *something*. The chat dispatch in `nuo-server` refuses
/// up-front when the live provider is a [`NoProvider`]; this impl is the
/// defensive backstop in case a code path reaches it without the gate.
pub struct NoProvider;

impl NoProvider {
    /// The stable sentinel id reported by [`Provider::provider_id`].
    pub const ID: &'static str = NO_PROVIDER_ID;

    /// Whether `provider` is a [`NoProvider`]. Use this instead of comparing
    /// `provider_id()` strings, so the sentinel's identity is owned by the
    /// type, not by every call site.
    pub fn is(provider: &dyn Provider) -> bool {
        provider.provider_id() == Self::ID
    }
}

#[async_trait]
impl Provider for NoProvider {
    async fn chat(
        &self,
        _request: ModelRequest,
    ) -> Result<ProviderCompletion, nuo_wire::ProviderError> {
        Err(nuo_wire::ProviderError::new(
            "none",
            nuo_wire::ProviderErrorKind::Unavailable,
            no_provider_message(),
        ))
    }

    async fn stream_chat(
        &self,
        _request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
        nuo_wire::ProviderError,
    > {
        Err(nuo_wire::ProviderError::new(
            "none",
            nuo_wire::ProviderErrorKind::Unavailable,
            no_provider_message(),
        ))
    }

    fn provider_id(&self) -> String {
        Self::ID.to_string()
    }

    fn model(&self) -> String {
        Self::ID.to_string()
    }
}

fn no_provider_message() -> String {
    "No provider configured. Run 'muta auth' or configure a provider in config.toml before sending a message.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_sentinel_identity() {
        let p = NoProvider;
        assert_eq!(p.provider_id(), NO_PROVIDER_ID);
        assert_eq!(p.model(), NO_PROVIDER_ID);
        assert!(NoProvider::is(&p));
    }

    #[test]
    fn is_returns_false_for_other_ids() {
        struct Other;
        #[async_trait]
        impl Provider for Other {
            async fn chat(
                &self,
                _request: ModelRequest,
            ) -> Result<nuo_wire::ProviderCompletion, nuo_wire::ProviderError>
            {
                unreachable!()
            }
            async fn stream_chat(
                &self,
                _request: ModelRequest,
            ) -> Result<
                BoxStream<'static, Result<String, nuo_wire::ProviderError>>,
                nuo_wire::ProviderError,
            > {
                unreachable!()
            }
            fn provider_id(&self) -> String {
                "openai".to_string()
            }
        }
        assert!(!NoProvider::is(&Other));
    }
}
