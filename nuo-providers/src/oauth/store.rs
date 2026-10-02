//! Where OAuth credentials live — a host-supplied port, with the reference
//! file implementation.
//!
//! # Why this is a port
//!
//! An OAuth credential set is *durable identity material the host owns*
//! (ADR-0300 §1, ADR-0303 §1). The kernel's job is the mechanism — running the
//! flow, refreshing tokens, preserving rotation discipline, signing requests —
//! and it must not decide *where* the tokens are kept or in what format. A host
//! that keeps them in an OS keychain, a tenant-scoped directory, or a database
//! supplies [`CredentialStore`]; a host that wants the shipped behaviour points
//! [`FileCredentialStore`] at its own path.
//!
//! # Why the interface is a transaction
//!
//! Refresh-token rotation is destructive: once a provider has exchanged a
//! refresh token, the old one is invalid. The invariant that follows — *never
//! hand out a newly rotated token unless its replacement refresh token is
//! durable* — requires the read, the network exchange, and the write to happen
//! under one exclusive lock, which is what [`CredentialStore::lock`] grants.
//! [`CredentialSession::commit`] is the durability point: nothing is promised
//! before it returns, and a rejection-safety check that decides no write is
//! needed simply drops the session.
//!
//! # File format
//!
//! The reference implementation keeps one TOML file keyed by connection name.
//! A missing file is a normal first-run condition. Every other read or decode
//! failure is explicit: treating corruption as an empty store could let a later
//! write silently erase every login.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use futures::future::BoxFuture;
use nuo_contracts::SecretString;
use nuo_host::fsutil::FileLock;
use serde::{Deserialize, Serialize};

/// One connection's OAuth token set and associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access: SecretString,
    pub refresh: SecretString,
    /// Unix epoch milliseconds when the access token expires.
    pub expires_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<SecretString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
    /// Extensible provider attributes (e.g. account_id, project_id, qoder, etc.)
    #[serde(default, flatten)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

pub use crate::registry::qoder::{QoderRequestIdentity, QoderStoredIdentity};

impl TokenSet {
    pub fn is_valid(&self) -> bool {
        !self.access.expose_secret().trim().is_empty()
    }

    /// Retrieve a string attribute by key.
    pub fn get_attr(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).and_then(|v| v.as_str())
    }

    /// Set a string attribute by key.
    pub fn set_attr(&mut self, key: impl Into<String>, val: impl Into<String>) {
        self.attributes
            .insert(key.into(), serde_json::Value::String(val.into()));
    }

    /// Retrieve and deserialize a typed JSON attribute by key.
    pub fn get_json_attr<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.attributes
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Serialize and set a typed JSON attribute by key.
    pub fn set_json_attr<T: serde::Serialize>(&mut self, key: &str, val: &T) {
        if let Ok(v) = serde_json::to_value(val) {
            self.attributes.insert(key.into(), v);
        }
    }
}

#[derive(Debug)]
pub enum CredentialStoreError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    Serialize(toml::ser::Error),
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    Join(String),
}

impl std::fmt::Display for CredentialStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(
                f,
                "could not read the credential store {}: {source}",
                path.display()
            ),
            Self::Parse { path, source } => write!(
                f,
                "the credential store {} is malformed: {source}",
                path.display()
            ),
            Self::Serialize(source) => write!(f, "could not encode credentials: {source}"),
            Self::Write { path, source } => write!(
                f,
                "could not persist the credential store {}: {source}",
                path.display()
            ),
            Self::Lock { path, source } => write!(
                f,
                "could not lock the credential store {}: {source}",
                path.display()
            ),
            Self::Join(message) => write!(f, "the credential lock task failed: {message}"),
        }
    }
}

impl std::error::Error for CredentialStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } | Self::Lock { source, .. } => {
                Some(source)
            }
            Self::Parse { source, .. } => Some(source),
            Self::Serialize(source) => Some(source),
            Self::Join(_) => None,
        }
    }
}

/// Where a host keeps OAuth credentials.
///
/// Object-safe and `'static` so a host can hold one handle and share it with
/// every flow it starts. Implementations must be safe to call concurrently:
/// [`CredentialStore::lock`] is what serializes writers, and a store with no
/// cross-process contention (an in-memory map, a keychain) may return a session
/// that holds no lock at all.
pub trait CredentialStore: Send + Sync + 'static {
    /// The tokens currently stored for `connection`, if any.
    ///
    /// Reads must not be transactional: this is the "is there a credential
    /// here" query, used by readiness checks and catalog roots. Anything that
    /// decides *and* writes goes through [`Self::lock`].
    fn read(&self, connection: &str) -> Result<Option<TokenSet>, CredentialStoreError>;

    /// Take exclusive access to the store for one read-modify-write window.
    ///
    /// The exclusive region spans the caller's network exchange, so it is
    /// expressed as a session rather than a closure: the caller decides when the
    /// window ends, and [`CredentialSession::commit`] is what makes changes
    /// durable.
    fn lock(&self) -> BoxFuture<'static, Result<Box<dyn CredentialSession>, CredentialStoreError>>;
}

/// Exclusive read-modify-write session over a [`CredentialStore`].
///
/// Dropping the session without [`CredentialSession::commit`] abandons the
/// changes: that is the correct outcome when a caller discovers — after reading —
/// that the credential it was about to replace had already been rotated by
/// somebody else.
pub trait CredentialSession: Send {
    /// The tokens currently stored for `connection`.
    fn get(&self, connection: &str) -> Option<TokenSet>;

    /// Stage `tokens` for `connection`. An access token that is empty after
    /// trimming is refused and the stored value is left alone: a blank bearer is
    /// never a credential.
    ///
    /// Returns whether the value was staged.
    fn set(&mut self, connection: &str, tokens: TokenSet) -> bool;

    /// Stage removal of `connection`'s tokens, returning what was there.
    fn remove(&mut self, connection: &str) -> Option<TokenSet>;

    /// Make every staged change durable, then release the exclusive region.
    fn commit(&self) -> BoxFuture<'static, Result<(), CredentialStoreError>>;
}

/// The reference implementation: one TOML file, one cross-process lock.
///
/// A host with no better place to keep tokens points this at its own path; the
/// format is the shipped one, and hosts that want a different shape implement
/// [`CredentialStore`] directly.
pub struct FileCredentialStore {
    auth_file: PathBuf,
}

impl FileCredentialStore {
    pub fn new(auth_file: impl Into<PathBuf>) -> Self {
        Self {
            auth_file: auth_file.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.auth_file
    }

    fn load_from(path: &Path) -> Result<StoredTokens, CredentialStoreError> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StoredTokens::default());
            }
            Err(source) => {
                return Err(CredentialStoreError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&content).map_err(|source| CredentialStoreError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    fn save_to(path: &Path, tokens: &StoredTokens) -> Result<(), CredentialStoreError> {
        let bytes = toml::to_string_pretty(tokens)
            .map_err(CredentialStoreError::Serialize)?
            .into_bytes();
        nuo_host::fsutil::atomic_write_bytes(path, &bytes).map_err(|source| {
            CredentialStoreError::Write {
                path: path.to_path_buf(),
                source,
            }
        })
    }
}

impl std::fmt::Debug for FileCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileCredentialStore")
            .field("auth_file", &self.auth_file)
            .finish()
    }
}

/// The on-disk shape: token sets keyed by connection name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredTokens {
    #[serde(default)]
    tokens: BTreeMap<String, TokenSet>,
}

impl CredentialStore for FileCredentialStore {
    fn read(&self, connection: &str) -> Result<Option<TokenSet>, CredentialStoreError> {
        Ok(Self::load_from(&self.auth_file)?.tokens.get(connection).cloned())
    }

    fn lock(&self) -> BoxFuture<'static, Result<Box<dyn CredentialSession>, CredentialStoreError>> {
        let path = self.auth_file.clone();
        Box::pin(async move {
            // A real cross-process lock, acquired off the async worker: the
            // exclusive region spans a network exchange, so it must not hold a
            // runtime thread while it waits.
            let lock_path = path.clone();
            let lock = tokio::task::spawn_blocking(move || FileLock::acquire(&lock_path))
                .await
                .map_err(|error| CredentialStoreError::Join(error.to_string()))?
                .map_err(|source| CredentialStoreError::Lock {
                    path: path.clone(),
                    source,
                })?;
            // Re-read under the lock: whatever was true before we acquired it is
            // not what we are about to modify.
            let tokens = Self::load_from(&path)?;
            Ok(Box::new(FileCredentialSession {
                path,
                tokens,
                staged: BTreeMap::new(),
                _lock: lock,
            }) as Box<dyn CredentialSession>)
        })
    }
}

/// A [`CredentialSession`] holding the file lock and the staged writes.
struct FileCredentialSession {
    path: PathBuf,
    /// What was on disk when the exclusive region opened.
    tokens: StoredTokens,
    /// Changes layered over `tokens`, each applied by whole entry at commit.
    staged: BTreeMap<String, Staged>,
    _lock: FileLock,
}

/// One staged change. An explicit enum, so "set then remove" and "remove then
/// set" both mean what they say and neither is encoded as a sentinel value that
/// could reach the disk.
#[derive(Debug, Clone)]
enum Staged {
    Put(TokenSet),
    Remove,
}

impl FileCredentialSession {
    fn effective(&self, connection: &str) -> Option<&TokenSet> {
        match self.staged.get(connection) {
            Some(Staged::Put(tokens)) => Some(tokens),
            Some(Staged::Remove) => None,
            None => self.tokens.tokens.get(connection),
        }
    }
}

impl CredentialSession for FileCredentialSession {
    fn get(&self, connection: &str) -> Option<TokenSet> {
        self.effective(connection).cloned()
    }

    fn set(&mut self, connection: &str, tokens: TokenSet) -> bool {
        if !tokens.is_valid() {
            tracing::warn!(
                connection = %connection,
                "refusing to store an empty access token; the stored credential is unchanged"
            );
            return false;
        }
        self.staged.insert(connection.to_string(), Staged::Put(tokens));
        true
    }

    fn remove(&mut self, connection: &str) -> Option<TokenSet> {
        let previous = self.effective(connection).cloned();
        self.staged.insert(connection.to_string(), Staged::Remove);
        previous
    }

    fn commit(&self) -> BoxFuture<'static, Result<(), CredentialStoreError>> {
        let path = self.path.clone();
        // Nothing staged: the exclusive region closes with no write at all. This
        // is the common path — a caller reads a token, discovers it is still
        // live, and returns — and it must not rewrite the file, because another
        // process may have rotated the credential in between.
        if self.staged.is_empty() {
            return Box::pin(async { Ok(()) });
        }
        let mut merged = self.tokens.clone();
        for (connection, change) in &self.staged {
            match change {
                Staged::Put(tokens) => {
                    merged.tokens.insert(connection.clone(), tokens.clone());
                }
                Staged::Remove => {
                    merged.tokens.remove(connection);
                }
            }
        }
        Box::pin(async move { FileCredentialStore::save_to(&path, &merged) })
    }
}

/// An in-memory credential store: no file, no lock file, no persistence.
///
/// Two consumers make this worth shipping rather than leaving to hosts: an
/// embedding that runs provider flows but keeps nothing durable
/// ([`super::host::CredentialHost::none`]), and tests that must exercise flow
/// behaviour without touching a real user's `auth.toml`.
///
/// The exclusive region is a `tokio` mutex held by the session, so it survives
/// the caller's network exchange without occupying a worker thread; reads and
/// the commit write take the map briefly under a `std` mutex, which is what lets
/// [`CredentialStore::read`] stay synchronous.
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    tokens: std::sync::Arc<std::sync::Mutex<BTreeMap<String, TokenSet>>>,
    exclusive: std::sync::Arc<tokio::sync::Mutex<()>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, TokenSet>> {
        self.tokens.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl CredentialStore for InMemoryCredentialStore {
    fn read(&self, connection: &str) -> Result<Option<TokenSet>, CredentialStoreError> {
        Ok(self.entries().get(connection).cloned())
    }

    fn lock(&self) -> BoxFuture<'static, Result<Box<dyn CredentialSession>, CredentialStoreError>> {
        let tokens = std::sync::Arc::clone(&self.tokens);
        let exclusive = std::sync::Arc::clone(&self.exclusive);
        Box::pin(async move {
            let guard = exclusive.lock_owned().await;
            let snapshot = tokens
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            Ok(Box::new(InMemoryCredentialSession {
                tokens,
                snapshot,
                staged: BTreeMap::new(),
                _exclusive: guard,
            }) as Box<dyn CredentialSession>)
        })
    }
}

struct InMemoryCredentialSession {
    tokens: std::sync::Arc<std::sync::Mutex<BTreeMap<String, TokenSet>>>,
    snapshot: BTreeMap<String, TokenSet>,
    staged: BTreeMap<String, Staged>,
    _exclusive: tokio::sync::OwnedMutexGuard<()>,
}

impl InMemoryCredentialSession {
    fn effective(&self, connection: &str) -> Option<&TokenSet> {
        match self.staged.get(connection) {
            Some(Staged::Put(tokens)) => Some(tokens),
            Some(Staged::Remove) => None,
            None => self.snapshot.get(connection),
        }
    }
}

impl CredentialSession for InMemoryCredentialSession {
    fn get(&self, connection: &str) -> Option<TokenSet> {
        self.effective(connection).cloned()
    }

    fn set(&mut self, connection: &str, tokens: TokenSet) -> bool {
        if !tokens.is_valid() {
            tracing::warn!(
                connection = %connection,
                "refusing to store an empty access token; the stored credential is unchanged"
            );
            return false;
        }
        self.staged.insert(connection.to_string(), Staged::Put(tokens));
        true
    }

    fn remove(&mut self, connection: &str) -> Option<TokenSet> {
        let previous = self.effective(connection).cloned();
        self.staged.insert(connection.to_string(), Staged::Remove);
        previous
    }

    fn commit(&self) -> BoxFuture<'static, Result<(), CredentialStoreError>> {
        if self.staged.is_empty() {
            return Box::pin(async { Ok(()) });
        }
        let tokens = std::sync::Arc::clone(&self.tokens);
        let staged = self.staged.clone();
        Box::pin(async move {
            let mut entries = tokens.lock().unwrap_or_else(|error| error.into_inner());
            for (connection, change) in staged {
                match change {
                    Staged::Put(value) => {
                        entries.insert(connection, value);
                    }
                    Staged::Remove => {
                        entries.remove(&connection);
                    }
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tokens(access: &str) -> TokenSet {
        TokenSet {
            access: access.into(),
            refresh: format!("{access}-refresh").into(),
            expires_ms: 1_700_000_000_000,
            id_token: None,
            token_type: Some("Bearer".into()),
            scope: None,
            user_email: None,
            attributes: serde_json::Map::new(),
        }
    }

    fn temp_store() -> (FileCredentialStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("muta-cred-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (FileCredentialStore::new(dir.join("auth.toml")), dir)
    }

    #[test]
    fn round_trips_custom_attributes() {
        let mut t = tokens("attr-test");
        t.set_attr("account_id", "acct-123");
        t.set_attr("project_id", "proj-456");
        let serialized = toml::to_string(&t).unwrap();
        let reparsed: TokenSet = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.get_attr("account_id"), Some("acct-123"));
        assert_eq!(reparsed.get_attr("project_id"), Some("proj-456"));
    }

    #[tokio::test]
    async fn connection_namespaces_stay_exact() {
        let (store, dir) = temp_store();
        let mut session = store.lock().await.unwrap();
        session.set("personal-chatgpt", tokens("personal"));
        session.set("work-chatgpt", tokens("work"));
        session.commit().await.unwrap();

        assert_eq!(
            store.read("personal-chatgpt").unwrap().unwrap().access.expose_secret(),
            "personal"
        );
        assert_eq!(
            store.read("work-chatgpt").unwrap().unwrap().access.expose_secret(),
            "work"
        );
        assert!(
            store.read("chatgpt").unwrap().is_none(),
            "a preset/provider fallback key is never consulted"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_store_is_an_error_not_an_empty_store() {
        let (store, dir) = temp_store();
        std::fs::write(store.path(), "[tokens\ninvalid").unwrap();
        assert!(matches!(
            store.read("anything"),
            Err(CredentialStoreError::Parse { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_store_is_empty() {
        let (store, dir) = temp_store();
        assert!(store.read("nothing-here").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn rejecting_an_empty_access_token_leaves_the_stored_credential_alone() {
        let (store, dir) = temp_store();
        {
            let mut session = store.lock().await.unwrap();
            assert!(session.set("live", tokens("real")));
            session.commit().await.unwrap();
        }

        let mut session = store.lock().await.unwrap();
        assert!(!session.set("live", tokens("")));
        assert!(!session.set("live", tokens("   ")));
        // A refusal must not disturb what is stored, nor make the session dirty
        // on its own.
        assert_eq!(session.get("live").unwrap().access.expose_secret(), "real");
        session.commit().await.unwrap();
        assert_eq!(
            store.read("live").unwrap().unwrap().access.expose_secret(),
            "real"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn dropping_a_session_without_commit_abandons_it() {
        let (store, dir) = temp_store();
        {
            let mut session = store.lock().await.unwrap();
            session.set("abandoned", tokens("value"));
            // Dropped, not committed: this is the "another process already
            // rotated it" path, which must write nothing.
        }
        assert!(store.read("abandoned").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn removal_is_honoured_even_after_a_set_in_the_same_session() {
        let (store, dir) = temp_store();
        {
            let mut session = store.lock().await.unwrap();
            session.set("churn", tokens("first"));
            session.commit().await.unwrap();
        }

        let mut session = store.lock().await.unwrap();
        assert!(session.set("churn", tokens("second")));
        assert!(session.remove("churn").is_some());
        session.commit().await.unwrap();
        assert!(
            store.read("churn").unwrap().is_none(),
            "the last word in the session wins"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_commit_with_nothing_staged_does_not_touch_the_file() {
        let (store, dir) = temp_store();
        {
            let mut session = store.lock().await.unwrap();
            session.set("kept", tokens("value"));
            session.commit().await.unwrap();
        }
        let before = std::fs::read_to_string(store.path()).unwrap();

        let session = store.lock().await.unwrap();
        session.commit().await.unwrap();
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn concurrent_locked_read_modify_write_does_not_lose_tokens() {
        let (store, dir) = temp_store();
        let store = Arc::new(store);
        let mut handles = Vec::new();
        for i in 0..5 {
            let store = Arc::clone(&store);
            handles.push(tokio::spawn(async move {
                let mut session = store.lock().await.unwrap();
                let id = format!("connection-{i}");
                session.set(&id, tokens(&format!("access-{i}")));
                session.commit().await.unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        for i in 0..5 {
            let id = format!("connection-{i}");
            assert_eq!(
                store.read(&id).unwrap().unwrap().access.expose_secret(),
                format!("access-{i}")
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
