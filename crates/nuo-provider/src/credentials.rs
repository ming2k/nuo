//! Durable credential material, token models, and store contracts (ADR-0015).

use futures::future::BoxFuture;
use nuo_host::fsutil::FileLock;
use nuo_model_codec::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

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

/// Errors occurring during credential storage read/write/lock operations.
#[derive(Debug)]
pub enum CredentialStoreError {
    Read { path: PathBuf, source: std::io::Error },
    Parse { path: PathBuf, source: toml::de::Error },
    Serialize(toml::ser::Error),
    Write { path: PathBuf, source: std::io::Error },
    Lock { path: PathBuf, source: std::io::Error },
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

/// Port defining where and how a host stores OAuth credentials.
pub trait CredentialStore: Send + Sync + 'static {
    /// Read tokens stored for `connection`, if any.
    fn read(&self, connection: &str) -> Result<Option<TokenSet>, CredentialStoreError>;

    /// Acquire exclusive access to the store for a read-modify-write window.
    fn lock(&self) -> BoxFuture<'static, Result<Box<dyn CredentialSession>, CredentialStoreError>>;
}

/// Exclusive read-modify-write session over a [`CredentialStore`].
pub trait CredentialSession: Send {
    /// Retrieve tokens currently stored for `connection`.
    fn get(&self, connection: &str) -> Option<TokenSet>;

    /// Stage updated tokens for `connection`.
    fn set(&mut self, connection: &str, tokens: TokenSet) -> bool;

    /// Stage removal of `connection`'s tokens, returning what was there.
    fn remove(&mut self, connection: &str) -> Option<TokenSet>;

    /// Commit all staged changes and make them durable.
    fn commit(&self) -> BoxFuture<'static, Result<(), CredentialStoreError>>;
}

/// Port defining the host's stable installation identity.
pub trait DeviceIdentity: Send + Sync + 'static {
    /// Stable across runs; two different installations must not collide.
    fn stable_id(&self) -> String;
}

/// Reference implementation: a file the host owns, read once per process.
pub struct FileDeviceIdentity {
    path: PathBuf,
    cached: OnceLock<String>,
}

impl FileDeviceIdentity {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            cached: OnceLock::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for FileDeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileDeviceIdentity")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity for FileDeviceIdentity {
    fn stable_id(&self) -> String {
        self.cached
            .get_or_init(|| {
                if let Ok(existing) = std::fs::read_to_string(&self.path) {
                    let id = existing.trim();
                    if !id.is_empty() {
                        return id.to_string();
                    }
                }
                let fresh = uuid::Uuid::new_v4().to_string();
                if let Err(error) = nuo_host::fsutil::atomic_write_bytes(&self.path, fresh.as_bytes()) {
                    tracing::warn!(
                        path = %self.path.display(),
                        %error,
                        "could not persist the device identity; provider flows will see a new one on next run"
                    );
                }
                fresh
            })
            .clone()
    }
}

/// Ephemeral per-process identity for testing or non-persistent hosts.
#[derive(Debug, Default)]
pub struct PerProcessIdentity {
    id: String,
}

impl PerProcessIdentity {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl DeviceIdentity for PerProcessIdentity {
    fn stable_id(&self) -> String {
        self.id.clone()
    }
}

/// On-disk representation of stored tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredTokens {
    #[serde(default)]
    tokens: BTreeMap<String, TokenSet>,
}

/// Reference implementation: one TOML file, one cross-process lock.
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
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
        let text = toml::to_string_pretty(tokens).map_err(CredentialStoreError::Serialize)?;
        nuo_host::fsutil::atomic_write_bytes(path, text.as_bytes()).map_err(|source| {
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

impl CredentialStore for FileCredentialStore {
    fn read(&self, connection: &str) -> Result<Option<TokenSet>, CredentialStoreError> {
        Ok(Self::load_from(&self.auth_file)?.tokens.get(connection).cloned())
    }

    fn lock(&self) -> BoxFuture<'static, Result<Box<dyn CredentialSession>, CredentialStoreError>> {
        let path = self.auth_file.clone();
        Box::pin(async move {
            let lock_path = path.clone();
            let lock = tokio::task::spawn_blocking(move || FileLock::acquire(&lock_path))
                .await
                .map_err(|error| CredentialStoreError::Join(error.to_string()))?
                .map_err(|source| CredentialStoreError::Lock {
                    path: path.clone(),
                    source,
                })?;
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

#[derive(Debug, Clone)]
enum Staged {
    Put(TokenSet),
    Remove,
}

struct FileCredentialSession {
    path: PathBuf,
    tokens: StoredTokens,
    staged: BTreeMap<String, Staged>,
    _lock: FileLock,
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

/// An in-memory credential store for tests and ephemeral sessions.
#[derive(Debug, Default)]
pub struct InMemoryCredentialStore {
    tokens: Arc<StdMutex<BTreeMap<String, TokenSet>>>,
    exclusive: Arc<tokio::sync::Mutex<()>>,
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
        let tokens = Arc::clone(&self.tokens);
        let exclusive = Arc::clone(&self.exclusive);
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
                _guard: guard,
            }) as Box<dyn CredentialSession>)
        })
    }
}

struct InMemoryCredentialSession {
    tokens: Arc<StdMutex<BTreeMap<String, TokenSet>>>,
    snapshot: BTreeMap<String, TokenSet>,
    staged: BTreeMap<String, Staged>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
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
        let tokens = Arc::clone(&self.tokens);
        let staged = self.staged.clone();
        Box::pin(async move {
            let mut entries = tokens.lock().unwrap_or_else(|error| error.into_inner());
            for (connection, change) in staged {
                match change {
                    Staged::Put(set) => {
                        entries.insert(connection, set);
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

/// Combined host handle holding credential storage and device identity ports.
#[derive(Clone)]
pub struct CredentialHost {
    store: Arc<dyn CredentialStore>,
    device: Arc<dyn DeviceIdentity>,
}

impl CredentialHost {
    pub fn new(store: Arc<dyn CredentialStore>, device: Arc<dyn DeviceIdentity>) -> Self {
        Self { store, device }
    }

    pub fn file_backed(auth_file: impl Into<PathBuf>, device_file: impl Into<PathBuf>) -> Self {
        Self {
            store: Arc::new(FileCredentialStore::new(auth_file)),
            device: Arc::new(FileDeviceIdentity::new(device_file)),
        }
    }

    pub fn none() -> Self {
        Self {
            store: Arc::new(InMemoryCredentialStore::new()),
            device: Arc::new(PerProcessIdentity::new()),
        }
    }

    pub fn store(&self) -> &Arc<dyn CredentialStore> {
        &self.store
    }

    pub fn device(&self) -> &Arc<dyn DeviceIdentity> {
        &self.device
    }
}

impl std::fmt::Debug for CredentialHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHost").finish_non_exhaustive()
    }
}
