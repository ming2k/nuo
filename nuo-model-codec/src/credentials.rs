//! Dynamic authentication credentials provider interface.

use crate::error::{Result, WireError};
use async_trait::async_trait;
use std::fmt::Debug;

/// Dynamic credentials provider for authenticating with model wire endpoints.
///
/// Decouples wire protocol requests from static API key configurations, allowing
/// host environments to dynamically refresh OAuth2 tokens, fetch IAM/STS credentials,
/// or load ephemeral secrets without restarting client connections.
#[async_trait]
pub trait CredentialsProvider: Send + Sync + Debug {
    /// Returns a valid authorization credential (e.g. API key or Bearer access token).
    async fn get_token(&self) -> Result<String>;
}

/// A static, immutable API key credentials provider.
#[derive(Clone)]
pub struct StaticApiKey {
    token: String,
}

impl StaticApiKey {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl Debug for StaticApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticApiKey")
            .field("token", &"***REDACTED***")
            .finish()
    }
}

#[async_trait]
impl CredentialsProvider for StaticApiKey {
    async fn get_token(&self) -> Result<String> {
        Ok(self.token.clone())
    }
}

/// A closure or callback-based dynamic credentials provider.
pub struct DynamicCredentials<F> {
    fetcher: F,
}

impl<F> DynamicCredentials<F> {
    pub fn new(fetcher: F) -> Self {
        Self { fetcher }
    }
}

impl<F> Debug for DynamicCredentials<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicCredentials").finish()
    }
}

#[async_trait]
impl<F, Fut> CredentialsProvider for DynamicCredentials<F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
{
    async fn get_token(&self) -> Result<String> {
        (self.fetcher)()
            .await
            .map_err(WireError::Credentials)
    }
}
