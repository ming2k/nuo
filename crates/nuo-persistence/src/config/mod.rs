//! User configuration schema and persistence.
use nuo_host::fsutil;
use nuo_host::paths;
use nuo_protocol::{CompactionPolicy, McpServerConfig, WebConfig};
use nuo_tool::SkillsConfig;

pub use nuo_protocol::ConnectionAuth as ConfigChannelAuth;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub mod catalog;
pub mod credentials;
pub mod routes;
pub mod types;

pub use catalog::*;
pub use credentials::*;
pub use routes::*;
pub use types::*;

#[derive(Debug, Serialize, Clone)]
pub struct Config {
    pub default_connection: String,
    pub mcp: HashMap<String, McpServerConfig>,
    /// Versioned context lifecycle and admission policy.
    pub context: nuo_protocol::context_policy::ContextPolicy,
    /// Maximum number of attempts for a single model request when the connection returns a
    /// transient error (HTTP 408/429/5xx, connection, timeout). The initial try
    /// counts as the first attempt, so this is the *total* attempts, not extra
    /// retries. Clamped to `[1, 60]` at the call site.
    pub connection_retry_max_attempts: usize,
    /// Maximum number of **stream restarts** after a model turn has already
    /// produced visible output (assistant text, reasoning, or a tool-call
    /// announcement). A mid-stream failure is not the same fault as a request
    /// that never produced output: the attempt's visible state must be
    /// discarded and the provider is usually going to fail the same way again,
    /// so this budget is deliberately small. `0` allows no restart at all.
    /// Clamped to `[0, 10]` at the call site.
    pub connection_stream_restart_max_attempts: usize,
    /// Base delay (ms) for the bounded stepped backoff between retries:
    /// `1s -> 2s -> 5s -> 10s -> 10s ...` (scaled by `base_ms`), capped by `connection_retry_max_ms`.
    pub connection_retry_base_ms: u64,
    /// Hard cap (ms) on a single backoff delay, including the exponential growth.
    /// A server-supplied `Retry-After`/`retry-after-ms` header still wins but is
    /// itself capped at this value.
    pub connection_retry_max_ms: u64,
    /// The model id to use within the active connection. For single-model
    /// connections this mirrors the connection's pinned model; for multi-model
    /// connections (opencode-go) it selects which of the connection's models is
    /// active. `None` falls back to the connection's default model.
    #[serde(default)]
    pub default_model: Option<String>,
    /// Favorite model ids for quick access in the Models picker (ADR-0046
    /// moved favorite from provider-level to per-model). Stored as a flat list
    /// of model wire ids; a starred daily-driver model sorts to the top of the
    /// flat list wherever it is served.
    #[serde(default)]
    pub favorites: Vec<String>,
    /// Model ids or glob patterns (e.g. `"gemini-3.6-flash*"`) to hide from
    /// model pickers across all connections.
    #[serde(default)]
    pub hidden_models: Vec<String>,
    /// Skill configuration (`[skills]` table).
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Declarative permission rules (`[permissions]` table). Each entry is a
    /// `[[permissions.allow]]` rule (`tool` + `scope`) pre-seeded into the
    /// allowlist at startup, so default policies are data-driven rather than
    /// only interactive. Runtime "Always" decisions still add to the persisted
    /// `permissions.json`; these config rules are re-applied on every start.
    #[serde(default)]
    pub permissions: PermissionConfig,
    /// Filesystem roots admitted in addition to the active project root.
    /// This is user-owned global policy and is independent of project asset
    /// trust.
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    /// Bash command safety policy (`[bash_policy]` table). Built-in dangerous
    /// command rules are compiled into the agent; this config supplies only
    /// user overrides/additional rules and guard toggles.
    #[serde(default)]
    pub bash_policy: BashPolicyConfig,
    /// Web-tool behavior (`[web]`): one provider per axis and shared network policy.
    #[serde(default)]
    pub web: WebConfig,
    /// Top-level agent behaviour (`[agent]` table):
    /// opt-in hard-stop budget and the doom-loop guard toggle. See [`AgentConfig`]
    /// for the per-field semantics and TOML examples.
    #[serde(default)]
    pub agent: AgentConfig,
    /// Lifecycle event hooks (`[[hooks]]` array, ADR-0025). Each entry fires a
    /// shell command at one lifecycle point; see [`HookSpec`].
    #[serde(default)]
    pub hooks: Vec<HookSpec>,
    /// Per-model tool-variant selection (`[tool_variants."<model-id>"]`
    /// table). When talking to the named model, each listed capability is
    /// realized by the named variant instead of its default. See
    /// [`ToolVariantsConfig`].
    #[serde(default)]
    pub tool_variants: ToolVariantsConfig,
    /// User-authored scoped instructions (`[[instructions.user]]`, ADR-0165).
    /// The user source of the unified instruction pipeline.
    #[serde(default)]
    pub instructions: UserInstructionsConfig,
    /// Server lifecycle knobs: the `[server]` (or legacy `[server]`) table of
    /// `config.toml`. Controls how the session server exits — its shutdown
    /// grace budget and its idle-empty auto-exit.
    pub server: ServerConfig,
}

#[derive(Deserialize)]
pub(super) struct RawConfig {
    #[serde(default)]
    default_connection: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    mcp: Option<HashMap<String, McpServerConfig>>,
    #[serde(default)]
    context: Option<nuo_protocol::context_policy::ContextPolicy>,
    #[serde(default)]
    compaction: Option<CompactionPolicy>,
    #[serde(default)]
    compaction_preserve_rounds: Option<usize>,
    #[serde(default)]
    compaction_summarize: Option<bool>,
    #[serde(default)]
    compaction_prune: Option<bool>,
    #[serde(default)]
    compaction_prune_protect_tokens: Option<usize>,
    #[serde(default)]
    connection_retry_max_attempts: Option<usize>,
    #[serde(default)]
    connection_stream_restart_max_attempts: Option<usize>,
    #[serde(default)]
    connection_retry_base_ms: Option<u64>,
    #[serde(default)]
    connection_retry_max_ms: Option<u64>,
    #[serde(default)]
    favorites: Option<Vec<String>>,
    #[serde(default)]
    hidden_models: Option<Vec<String>>,
    #[serde(default)]
    skills: Option<SkillsConfig>,
    #[serde(default)]
    permissions: Option<PermissionConfig>,
    #[serde(default)]
    workspace: Option<WorkspaceConfig>,
    #[serde(default)]
    bash_policy: Option<BashPolicyConfig>,
    #[serde(default)]
    web: Option<WebConfig>,
    #[serde(default)]
    agent: Option<AgentConfig>,
    #[serde(default)]
    hooks: Option<Vec<HookSpec>>,
    #[serde(default)]
    tool_variants: Option<ToolVariantsConfig>,
    #[serde(default)]
    instructions: Option<UserInstructionsConfig>,
    /// Retired key: recognized only to fail with a precise migration message.
    #[serde(default)]
    model_guidance: Option<toml::Value>,
    #[serde(default)]
    server: Option<ServerConfig>,
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawConfig::deserialize(deserializer)?;
        let mut cfg = Config::default();
        if let Some(c) = raw.default_connection {
            cfg.default_connection = c;
        }
        if let Some(m) = raw.default_model {
            cfg.default_model = Some(m);
        }
        if let Some(mcp) = raw.mcp {
            cfg.mcp = mcp;
        }
        if raw.compaction.is_some()
            || raw.compaction_preserve_rounds.is_some()
            || raw.compaction_summarize.is_some()
            || raw.compaction_prune.is_some()
            || raw.compaction_prune_protect_tokens.is_some()
        {
            return Err(serde::de::Error::custom(
                "legacy 'compaction.*' configuration is retired [INV-POLICY-01]; run `nuo context migrate` to convert to versioned `context.*` policy",
            ));
        }
        if let Some(ctx) = raw.context {
            ctx.validate().map_err(serde::de::Error::custom)?;
            cfg.context = ctx;
        }
        if let Some(a) = raw.connection_retry_max_attempts {
            cfg.connection_retry_max_attempts = a;
        }
        if let Some(a) = raw.connection_stream_restart_max_attempts {
            cfg.connection_stream_restart_max_attempts = a;
        }
        if let Some(b) = raw.connection_retry_base_ms {
            cfg.connection_retry_base_ms = b;
        }
        if let Some(m) = raw.connection_retry_max_ms {
            cfg.connection_retry_max_ms = m;
        }
        if let Some(f) = raw.favorites {
            cfg.favorites = f;
        }
        if let Some(h) = raw.hidden_models {
            cfg.hidden_models = h;
        }
        if let Some(s) = raw.skills {
            cfg.skills = s;
        }
        if let Some(p) = raw.permissions {
            cfg.permissions = p;
        }
        if let Some(w) = raw.workspace {
            cfg.workspace = w;
        }
        if let Some(b) = raw.bash_policy {
            cfg.bash_policy = b;
        }
        if let Some(web) = raw.web {
            cfg.web = web;
        }
        if let Some(a) = raw.agent {
            cfg.agent = a;
        }
        if let Some(h) = raw.hooks {
            cfg.hooks = h;
        }
        if let Some(tv) = raw.tool_variants {
            cfg.tool_variants = tv;
        }
        if raw.model_guidance.is_some() {
            return Err(serde::de::Error::custom(
                "`[model_guidance]` is retired; move its entries to `[[instructions.user]]` \
                 (provider/connection/model selectors, model is a glob)",
            ));
        }
        if let Some(instructions) = raw.instructions {
            instructions.validate().map_err(serde::de::Error::custom)?;
            cfg.instructions = instructions;
        }
        if let Some(d) = raw.server {
            cfg.server = d;
        }
        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_connection: String::new(),
            mcp: HashMap::new(),
            context: nuo_protocol::context_policy::ContextPolicy::default(),
            connection_retry_max_attempts: 30,
            connection_stream_restart_max_attempts: 3,
            connection_retry_base_ms: 1_000,
            connection_retry_max_ms: 10_000,
            default_model: None,
            favorites: Vec::new(),
            hidden_models: Vec::new(),
            skills: SkillsConfig::default(),
            permissions: PermissionConfig::default(),
            workspace: WorkspaceConfig::default(),
            bash_policy: BashPolicyConfig::default(),
            web: WebConfig::default(),
            agent: AgentConfig::default(),
            hooks: Vec::new(),
            tool_variants: ToolVariantsConfig::default(),
            instructions: UserInstructionsConfig::default(),
            server: ServerConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainServerFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    lifecycle: Option<DomainServerLifecycle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<DomainServerNetwork>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<ServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainServerLifecycle {
    #[serde(skip_serializing_if = "Option::is_none")]
    shutdown_grace_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_exit_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainServerNetwork {
    #[serde(skip_serializing_if = "Option::is_none")]
    local_auth: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainClientFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    connection: Option<DomainClientConnection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resilience: Option<DomainClientResilience>,
    #[serde(skip_serializing_if = "Option::is_none")]
    models: Option<DomainClientModels>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainClientConnection {
    #[serde(skip_serializing_if = "Option::is_none")]
    default_connection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainClientResilience {
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_max_attempts: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_restart_max_attempts: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_base_delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_max_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainClientModels {
    #[serde(skip_serializing_if = "Option::is_none")]
    favorites: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hidden: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct DomainAgentFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<nuo_protocol::context_policy::ContextPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permissions: Option<PermissionConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspaceConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bash_policy: Option<BashPolicyConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    web: Option<WebConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skills: Option<SkillsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp: Option<HashMap<String, McpServerConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hooks: Option<Vec<HookSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_variants: Option<ToolVariantsConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<UserInstructionsConfig>,
    /// Retired key: recognized only to fail with a precise migration message.
    #[serde(skip_serializing_if = "Option::is_none")]
    model_guidance: Option<toml::Value>,
}

/// Fail-fast boundary for the configuration matrix (`[INV-CONF-04]`).
///
/// A malformed or unmigratable config file is unrecoverable: carrying on would run
/// the daemon on a half-applied domain matrix, which is precisely the silent
/// degradation that invariant forbids. Kept as *one* auditable abort point rather
/// than scattered `panic!`s, and exempted from the workspace `panic` deny because
/// no caller can act on the error — `Config::load` is deliberately infallible.
#[allow(clippy::panic)]
fn fatal_config(message: std::fmt::Arguments<'_>) -> ! {
    panic!("nuo: {message}");
}

impl Config {
    pub fn load() -> Self {
        Self::promote_legacy_config_if_needed();
        let mut cfg = Config::default();
        Self::apply_domain_overlays(&mut cfg);
        cfg
    }

    /// One-way promotion (ADR-0031 `[INV-CONF-04]`). If the retired monolithic
    /// `config.toml` is present and no domain file has been written yet, it is
    /// translated into the domain matrix and then removed. There is no runtime
    /// fallback: after promotion `config.toml` is gone for good.
    fn promote_legacy_config_if_needed() {
        let legacy = Self::config_file_path();
        if !legacy.exists() {
            return;
        }
        let domains_exist = Self::server_config_file_path().exists()
            || Self::client_config_file_path().exists()
            || Self::agent_config_file_path().exists();
        if domains_exist {
            // The domain matrix is authoritative; the legacy file is a stale
            // artifact from a previous install.
            let _ = fs::remove_file(&legacy);
            return;
        }
        if let Err(error) = Self::migrate_legacy_config() {
            fatal_config(format_args!(
                "could not migrate legacy config.toml: {error}"
            ));
        }
        let _ = fs::remove_file(&legacy);
    }

    /// Translate the legacy `config.toml` into the ADR-0031 domain matrix.
    /// Deterministic and offline: writes `server.toml`, `client.toml`, and
    /// `agent.toml`. `terminal.toml` is owned by `nuo-tui` and is not touched.
    pub fn migrate_legacy_config() -> Result<(), String> {
        let legacy = Self::config_file_path();
        let source = fs::read_to_string(&legacy).map_err(|e| e.to_string())?;
        let source = crate::web_migration::migrate_config_source(&source);
        let cfg = Self::parse_legacy_config(&source)?;
        cfg.save_domain_files().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Parse a legacy monolithic `config.toml` source into the aggregate. Used
    /// only by the one-way promotion; the active runtime parses the domain
    /// files with `deny_unknown_fields` via [`Self::apply_domain_overlays`].
    fn parse_legacy_config(source: &str) -> Result<Config, String> {
        toml::from_str::<Config>(source).map_err(|e| e.to_string())
    }

    /// Strictly validate one domain file's source against its schema. Used by
    /// `nuo config check`; unknown keys and type errors are reported.
    pub(crate) fn validate_domain_source(label: &str, content: &str) -> Result<(), String> {
        match label {
            "server.toml" => toml::from_str::<DomainServerFile>(content)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            "client.toml" => toml::from_str::<DomainClientFile>(content)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            "agent.toml" => toml::from_str::<DomainAgentFile>(content)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            other => Err(format!("unknown domain file `{other}`")),
        }
    }

    /// Read one domain file. Absent → `None`; present but malformed → hard
    /// failure (fail-fast, `[INV-CONF-04]`).
    fn load_domain<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
        let content = fs::read_to_string(path).ok()?;
        match toml::from_str::<T>(&content) {
            Ok(value) => Some(value),
            Err(error) => fatal_config(format_args!(
                "invalid config file {}: {error}",
                path.display()
            )),
        }
    }

    fn apply_domain_overlays(cfg: &mut Config) {
        // 1. server.toml
        if let Some(server) =
            Self::load_domain::<DomainServerFile>(&Self::server_config_file_path())
        {
            if let Some(lc) = server.lifecycle {
                if let Some(s) = lc.shutdown_grace_secs {
                    cfg.server.shutdown_grace_secs = s;
                }
                if let Some(i) = lc.idle_exit_minutes {
                    cfg.server.idle_exit_minutes = i;
                }
            }
            if let Some(nw) = server.network {
                if let Some(la) = nw.local_auth {
                    cfg.server.local_auth = la;
                }
            }
            if let Some(d) = server.server {
                cfg.server = d;
            }
        }

        // 2. client.toml
        if let Some(client) =
            Self::load_domain::<DomainClientFile>(&Self::client_config_file_path())
        {
            if let Some(conn) = client.connection {
                if let Some(c) = conn.default_connection {
                    cfg.default_connection = c;
                }
                if let Some(m) = conn.default_model {
                    cfg.default_model = Some(m);
                }
            }
            if let Some(res) = client.resilience {
                if let Some(a) = res.retry_max_attempts {
                    cfg.connection_retry_max_attempts = a;
                }
                if let Some(a) = res.stream_restart_max_attempts {
                    cfg.connection_stream_restart_max_attempts = a;
                }
                if let Some(b) = res.retry_base_delay_ms {
                    cfg.connection_retry_base_ms = b;
                }
                if let Some(m) = res.retry_max_delay_ms {
                    cfg.connection_retry_max_ms = m;
                }
            }
            if let Some(models) = client.models {
                if let Some(f) = models.favorites {
                    cfg.favorites = f;
                }
                if let Some(h) = models.hidden {
                    cfg.hidden_models = h;
                }
            }
        }

        // 3. agent.toml
        if let Some(agent_doc) =
            Self::load_domain::<DomainAgentFile>(&Self::agent_config_file_path())
        {
            if let Some(a) = agent_doc.agent {
                cfg.agent = a;
            }
            if let Some(c) = agent_doc.context {
                if let Err(error) = c.validate() {
                    fatal_config(format_args!("invalid agent.toml [context]: {error}"));
                }
                cfg.context = c;
            }
            if let Some(p) = agent_doc.permissions {
                cfg.permissions = p;
            }
            if let Some(b) = agent_doc.bash_policy {
                cfg.bash_policy = b;
            }
            if let Some(w) = agent_doc.web {
                cfg.web = w;
            }
            if let Some(s) = agent_doc.skills {
                cfg.skills = s;
            }
            if let Some(m) = agent_doc.mcp {
                cfg.mcp = m;
            }
            if let Some(h) = agent_doc.hooks {
                cfg.hooks = h;
            }
            if let Some(tv) = agent_doc.tool_variants {
                cfg.tool_variants = tv;
            }
            if agent_doc.model_guidance.is_some() {
                fatal_config(format_args!(
                    "agent.toml `[model_guidance]` is retired; move its entries to \
                     `[[instructions.user]]` (provider/connection/model selectors, model is a glob)"
                ));
            }
            if let Some(instructions) = agent_doc.instructions {
                if let Err(error) = instructions.validate() {
                    fatal_config(format_args!("invalid agent.toml instructions: {error}"));
                }
                cfg.instructions = instructions;
            }
            if let Some(ws) = agent_doc.workspace {
                cfg.workspace = ws;
            }
        }
    }

    /// Persist the aggregate into the ADR-0031 domain matrix
    /// (`server.toml`, `client.toml`, `agent.toml`). Each file is written from
    /// a serialisable domain document, so the on-disk shape is exactly the
    /// strict schema the loader parses.
    pub fn save_domain_files(&self) -> Result<(), Box<dyn std::error::Error>> {
        let server = DomainServerFile {
            lifecycle: Some(DomainServerLifecycle {
                shutdown_grace_secs: Some(self.server.shutdown_grace_secs),
                idle_exit_minutes: Some(self.server.idle_exit_minutes),
            }),
            network: Some(DomainServerNetwork {
                local_auth: Some(self.server.local_auth),
            }),
            server: None,
        };
        fsutil::atomic_write_bytes(
            &Self::server_config_file_path(),
            toml::to_string_pretty(&server)?.as_bytes(),
        )?;

        let client = DomainClientFile {
            connection: Some(DomainClientConnection {
                default_connection: Some(self.default_connection.clone()),
                default_model: self.default_model.clone(),
            }),
            resilience: Some(DomainClientResilience {
                retry_max_attempts: Some(self.connection_retry_max_attempts),
                stream_restart_max_attempts: Some(self.connection_stream_restart_max_attempts),
                retry_base_delay_ms: Some(self.connection_retry_base_ms),
                retry_max_delay_ms: Some(self.connection_retry_max_ms),
            }),
            models: Some(DomainClientModels {
                favorites: Some(self.favorites.clone()),
                hidden: Some(self.hidden_models.clone()),
            }),
        };
        fsutil::atomic_write_bytes(
            &Self::client_config_file_path(),
            toml::to_string_pretty(&client)?.as_bytes(),
        )?;

        let agent = DomainAgentFile {
            agent: Some(self.agent.clone()),
            context: Some(self.context.clone()),
            permissions: Some(self.permissions.clone()),
            workspace: Some(self.workspace.clone()),
            bash_policy: Some(self.bash_policy.clone()),
            web: Some(self.web.clone()),
            skills: Some(self.skills.clone()),
            mcp: Some(self.mcp.clone()),
            hooks: Some(self.hooks.clone()),
            tool_variants: Some(self.tool_variants.clone()),
            instructions: if self.instructions.is_empty() {
                None
            } else {
                Some(self.instructions.clone())
            },
            model_guidance: None,
        };
        fsutil::atomic_write_bytes(
            &Self::agent_config_file_path(),
            toml::to_string_pretty(&agent)?.as_bytes(),
        )?;

        Ok(())
    }

    /// Load only the `[mcp.*]` table from a project-local `.nuo/config.toml`
    /// Returns an empty map when the file or table is absent.
    ///
    /// This reads a *narrow* projection — just the mcp table — so unrelated
    /// well-formed keys do not affect the result. Project-scope MCP stays
    /// quarantined until the current MCP-domain digest is trusted; this
    /// function is pure parsing, and the caller applies the decision.
    pub fn load_project_mcp(project_root: &std::path::Path) -> HashMap<String, McpServerConfig> {
        let path = project_root.join(".nuo/config.toml");
        // Deserialize into a struct that only declares `mcp`, ignoring every
        // other key the project file may carry (deny_unknown_fields off).
        #[derive(Deserialize)]
        struct ProjectMcpProjection {
            #[serde(default)]
            mcp: HashMap<String, McpServerConfig>,
        }
        let mut servers = match fs::read_to_string(&path) {
            Ok(content) => match toml::from_str::<ProjectMcpProjection>(&content) {
                Ok(parsed) => parsed.mcp,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "project .nuo/config.toml has invalid [mcp.*]; ignoring that MCP source"
                    );
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };

        // `.nuo/mcp.json` follows the common MCP client shape while retaining
        // Nuo's `read_only` and `enabled` policy fields. A JSON definition with
        // the same name replaces the TOML entry, giving the dedicated file a
        // deterministic precedence.
        #[derive(Deserialize, Default)]
        struct ProjectMcpJson {
            #[serde(default, rename = "mcpServers")]
            mcp_servers: HashMap<String, ProjectMcpJsonServer>,
        }
        #[derive(Deserialize)]
        struct ProjectMcpJsonServer {
            command: String,
            #[serde(default)]
            args: Vec<String>,
            #[serde(default, rename = "env")]
            environment: HashMap<String, String>,
            #[serde(default = "default_true")]
            enabled: bool,
            #[serde(default)]
            read_only: bool,
            /// Config-time tool scoping (follow-up): original-name
            /// allow/deny lists, same semantics as `[mcp.<name>]` TOML.
            #[serde(default)]
            allow_tools: Vec<String>,
            #[serde(default)]
            deny_tools: Vec<String>,
        }
        fn default_true() -> bool {
            true
        }

        let json_path = project_root.join(".nuo/mcp.json");
        if let Ok(content) = fs::read_to_string(&json_path) {
            match serde_json::from_str::<ProjectMcpJson>(&content) {
                Ok(parsed) => {
                    for (name, entry) in parsed.mcp_servers {
                        let mut command = Vec::with_capacity(entry.args.len() + 1);
                        command.push(entry.command);
                        command.extend(entry.args);
                        servers.insert(
                            name,
                            McpServerConfig {
                                url: None,
                                command,
                                environment: entry.environment,
                                enabled: entry.enabled,
                                read_only: entry.read_only,
                                allow_tools: entry.allow_tools,
                                deny_tools: entry.deny_tools,
                                sandbox_root: None,
                            },
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        path = %json_path.display(),
                        error = %err,
                        "project .nuo/mcp.json is invalid; ignoring that MCP source"
                    );
                }
            }
        }

        let root =
            std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
        for config in servers.values_mut() {
            config.sandbox_root = Some(root.clone());
        }
        servers
    }

    /// Load role-scoped MCP servers from `$XDG_CONFIG_HOME/nuo/roles/<role>/mcp.json`.
    pub fn load_role_mcp(role: &str) -> HashMap<String, McpServerConfig> {
        let json_path = paths::get().role_mcp_file(role);
        let mut servers = HashMap::new();
        if let Ok(content) = fs::read_to_string(&json_path) {
            #[derive(Deserialize, Default)]
            struct RoleMcpJson {
                #[serde(default, rename = "mcpServers")]
                mcp_servers: HashMap<String, RoleMcpJsonServer>,
            }
            #[derive(Deserialize)]
            struct RoleMcpJsonServer {
                command: String,
                #[serde(default)]
                args: Vec<String>,
                #[serde(default, rename = "env")]
                environment: HashMap<String, String>,
                #[serde(default = "default_true")]
                enabled: bool,
                #[serde(default)]
                read_only: bool,
                #[serde(default)]
                allow_tools: Vec<String>,
                #[serde(default)]
                deny_tools: Vec<String>,
            }
            fn default_true() -> bool {
                true
            }
            if let Ok(parsed) = serde_json::from_str::<RoleMcpJson>(&content) {
                for (name, entry) in parsed.mcp_servers {
                    let mut command = Vec::with_capacity(entry.args.len() + 1);
                    command.push(entry.command);
                    command.extend(entry.args);
                    servers.insert(
                        name,
                        McpServerConfig {
                            url: None,
                            command,
                            environment: entry.environment,
                            enabled: entry.enabled,
                            read_only: entry.read_only,
                            allow_tools: entry.allow_tools,
                            deny_tools: entry.deny_tools,
                            sandbox_root: None,
                        },
                    );
                }
            }
        }
        servers
    }

    /// Merge a role-scoped MCP server set into this config.
    pub fn merge_role_mcp(&mut self, role_mcp: HashMap<String, McpServerConfig>) {
        for (name, server) in role_mcp {
            self.mcp.insert(name, server);
        }
    }

    /// Merge a project-local MCP server set into this (global-origin) config.
    /// A project entry with the same name as a global entry **replaces** it
    /// wholesale; project entries with new names are added. The
    /// result is the effective `[mcp.*]` set the runtime connects to.
    pub fn merge_project_mcp(&mut self, project_mcp: HashMap<String, McpServerConfig>) {
        for (name, cfg) in project_mcp {
            self.mcp.insert(name, cfg);
        }
    }

    /// Load only the `[[hooks]]` array from a project-local
    /// `.nuo/config.toml`. Returns an empty vec when the file or table is
    /// absent. Like [`Self::load_project_mcp`], this is a *narrow* projection
    /// (just the hooks array) so an unrelated key in the project file does not
    /// fail the whole load. Project-scope hooks are quarantined until their
    /// exact extension content is trusted; the caller applies the gate.
    ///
    /// A project `[[hooks]]` entry whose `command` points at a project-supplied
    /// script (e.g. `.nuo/hooks/lint.sh`) is the same class of hazard as a
    /// project `[mcp.*]` server: a cloned/vendored repo must not gain shell
    /// execution merely because the user opened it.
    pub fn load_project_hooks(project_root: &std::path::Path) -> Vec<HookSpec> {
        let path = project_root.join(".nuo/config.toml");
        let Some(content) = fs::read_to_string(&path).ok() else {
            return Vec::new();
        };
        // Deserialize into a struct that only declares `hooks`, ignoring every
        // other key the project file may carry (deny_unknown_fields off).
        #[derive(Deserialize)]
        struct ProjectHooksProjection {
            #[serde(default)]
            hooks: Vec<HookSpec>,
        }
        match toml::from_str::<ProjectHooksProjection>(&content) {
            Ok(mut parsed) => {
                let root = std::fs::canonicalize(project_root)
                    .unwrap_or_else(|_| project_root.to_path_buf());
                for hook in &mut parsed.hooks {
                    hook.sandbox_root = Some(root.clone());
                }
                parsed.hooks
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "project .nuo/config.toml has invalid [[hooks]]; ignoring project hooks"
                );
                Vec::new()
            }
        }
    }

    /// Load only the `[workspace].additional_roots` array from a project-local
    /// `.nuo/config.toml`. Returns an empty vec when the file or table is
    /// absent. Like [`Self::load_project_mcp`] and [`Self::load_project_hooks`],
    /// this is a narrow projection (just the workspace table). Project-scope
    /// additional roots remain quarantined until the `ex-workspace` domain is trusted.
    pub fn load_project_additional_roots(project_root: &std::path::Path) -> Vec<String> {
        let path = project_root.join(".nuo/config.toml");
        let Some(content) = fs::read_to_string(&path).ok() else {
            return Vec::new();
        };
        #[derive(Deserialize)]
        struct ProjectWorkspaceProjection {
            #[serde(default)]
            workspace: ProjectWorkspaceConfig,
        }
        #[derive(Deserialize, Default)]
        struct ProjectWorkspaceConfig {
            #[serde(default)]
            additional_roots: Vec<String>,
        }
        match toml::from_str::<ProjectWorkspaceProjection>(&content) {
            Ok(parsed) => parsed.workspace.additional_roots,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "project .nuo/config.toml has invalid [workspace]; ignoring project additional_roots"
                );
                Vec::new()
            }
        }
    }

    /// Append project-local `[workspace].additional_roots` to this config's additional roots.
    pub fn merge_project_additional_roots(&mut self, project_roots: Vec<String>) {
        for root in project_roots {
            if !self.workspace.additional_roots.contains(&root) {
                self.workspace.additional_roots.push(root);
            }
        }
    }

    /// Resolve the `[workspace].additional_roots` policy for an active project.
    /// Global and trusted project-local additional roots are resolved and canonicalized
    /// against `project_root`. Missing or invalid paths are skipped gracefully rather
    /// than dropping all admitted roots.
    pub fn resolve_workspace_additional_roots(
        &self,
        project_root: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, String> {
        self.resolve_workspace_additional_roots_detailed(project_root)
            .map(|detailed| detailed.admitted)
    }

    /// Resolve the `[workspace].additional_roots` policy, returning admitted paths
    /// and explicit skipped entries for error tracking and diagnostics.
    pub fn resolve_workspace_additional_roots_detailed(
        &self,
        project_root: &std::path::Path,
    ) -> Result<ResolvedAdditionalRoots, String> {
        if self.workspace.additional_roots.is_empty() {
            return Ok(ResolvedAdditionalRoots::default());
        }
        let canonical_root =
            std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut admitted = Vec::new();
        let mut skipped = Vec::new();
        for raw in &self.workspace.additional_roots {
            let expanded: std::path::PathBuf = if raw == "~" {
                match &home {
                    Some(h) => h.clone(),
                    None => {
                        skipped.push((raw.clone(), "'~' used but HOME is unset".to_string()));
                        continue;
                    }
                }
            } else if let Some(rest) = raw.strip_prefix("~/") {
                match &home {
                    Some(h) => h.join(rest),
                    None => {
                        skipped.push((raw.clone(), "uses '~' but HOME is unset".to_string()));
                        continue;
                    }
                }
            } else {
                let p = std::path::Path::new(raw);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    canonical_root.join(raw)
                }
            };
            let expanded = if expanded.is_absolute() {
                expanded
            } else {
                canonical_root.join(&expanded)
            };
            let canonical = match std::fs::canonicalize(&expanded) {
                Ok(c) => c,
                Err(err) => {
                    skipped.push((raw.clone(), format!("path does not exist ({err})")));
                    continue;
                }
            };
            if !canonical.is_dir() {
                skipped.push((raw.clone(), "path is not a directory".to_string()));
                continue;
            }
            if canonical == canonical_root {
                skipped.push((
                    raw.clone(),
                    "workspace root itself; already admitted".to_string(),
                ));
                continue;
            }
            if canonical.starts_with(&canonical_root) {
                skipped.push((
                    raw.clone(),
                    "inside workspace; already admitted".to_string(),
                ));
                continue;
            }
            // Distinct spellings of the same directory (relative plus
            // absolute, a symlinked twin) collapse silently: admission is a
            // set, and the second mention grants nothing new to reject.
            if !admitted.contains(&canonical) {
                admitted.push(canonical);
            }
        }
        Ok(ResolvedAdditionalRoots { admitted, skipped })
    }

    /// Append project-local `[[hooks]]` to this config's (global-origin) hooks.
    /// Project hooks are appended *after* global ones so the global ordering is
    /// preserved; hook semantics within one event are order-independent (each
    /// hook decides independently), so concatenation is sufficient.
    pub fn merge_project_hooks(&mut self, project_hooks: Vec<HookSpec>) {
        self.hooks.extend(project_hooks);
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        Self::save_inner(self, false)
    }

    /// Persist config while leaving the on-disk `default_provider` /
    /// `default_model` selection untouched. Used by mutations that are not
    /// selection changes (favorites, provider metadata edits, TUI
    /// preferences) so they never leak the in-memory selection — which may
    /// carry a resumed session's provider pin — into `config.toml`. The
    /// `/models` switch itself calls [`Config::save`]: updating the global
    /// Persist config while leaving the on-disk `default_connection` /
    /// `default_model` selection untouched.
    pub fn save_preserving_connection_selection(&self) -> Result<(), Box<dyn std::error::Error>> {
        Self::save_inner(self, true)
    }

    /// Merge-save: flush this snapshot to `config.toml` without clobbering a
    /// concurrent user hand-edit.
    ///
    /// The in-memory `Config` is loaded once at process start and
    /// lives on for the server's whole lifetime, while the file can change
    /// under it at any moment. A naive whole-file rewrite therefore resurrects
    /// anything the user deleted — the reported bug: removing
    /// `[workspace].additional_roots` from `~/.config/nuo/config.toml` while a
    /// session was open, then doing anything that saves (a `/models` switch, a
    /// favorite toggle), put the deleted entries straight back.
    ///
    /// The fix is **ownership-based reconciliation**: only the fields a save
    /// call actually *means* to write come from `self`; everything else comes
    /// from the on-disk document read under the lock.
    ///
    /// * **Runtime-owned** — the selection pair. `Config::save` (deliberate
    ///   selection change) writes `self`'s values; `save_preserving…` keeps
    ///   the disk's, falling back to `self` only when the disk default is
    ///   empty/never set. The runtime is the authority for the value it just
    ///   changed, and each such change is an explicit user action.
    /// * **User-owned** — every other field (workspace, favorites, web,
    ///   agent, mcp, hooks, …) is taken verbatim from disk. The disk was
    ///   potentially edited *after* the snapshot was taken, so it is the
    ///   newer truth; a stale snapshot must never overwrite it. Runtime
    ///   mutations of user-owned tables (web settings, favorites, `nuo mcp
    ///   add`) follow load → mutate → save, so their in-memory copy already
    ///   matches disk and this rule is a no-op for them — with one bounded
    ///   exception: a runtime mutation racing a same-table hand edit in the
    ///   millisecond window loses the runtime mutation. That is the honest
    ///   resolution of an inherently ambiguous race; a silent clobber of the
    ///   user's edit is strictly worse.
    ///
    /// The reconciled document is written out but **not** folded back into
    /// `self`: a save is a pure flush, so a handler holding `&mut Config`
    /// never silently absorbs a user edit mid-request.
    ///
    /// Note on `[workspace].additional_roots` specifically: the runtime's
    /// project-trust merge (`merge_project_additional_roots`) is applied to
    /// local clones and resolution views only — the persisted file has always
    /// meant *user-declared* global roots. Reconciling that array from disk
    /// (instead of unioning the snapshot) therefore preserves the intended
    /// semantics exactly, and makes the user's deletion permanent.
    fn save_inner(
        &self,
        preserve_connection_selection: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Serialise against other `nuo` instances (ADR-0018 pattern). The lock
        // is anchored on the client file (which owns the selection pair).
        let lock_anchor = Self::client_config_file_path();
        let _lock = fsutil::FileLock::acquire(&lock_anchor)
            .map_err(|e| format!("could not lock config file: {e}"))?;

        // Ownership-based reconciliation (ADR-0031): the user-owned domain
        // tables come from the on-disk domain files *as they are right now*, so
        // a stale snapshot can never resurrect a hand-deleted entry. Only the
        // runtime-owned selection pair is taken from `self`, and only when this
        // is a deliberate selection change.
        let mut out = Config::default();
        Self::apply_domain_overlays(&mut out);
        if preserve_connection_selection {
            if out.default_connection.is_empty() {
                out.default_connection = self.default_connection.clone();
            }
        } else {
            out.default_connection = self.default_connection.clone();
            out.default_model = self.default_model.clone();
        }

        out.save_domain_files()?;
        Ok(())
    }

    pub fn config_file_path() -> PathBuf {
        paths::get().config_file()
    }

    /// Primary server server configuration path (`$XDG_CONFIG_HOME/nuo/server.toml`, ADR-0031).
    pub fn server_config_file_path() -> PathBuf {
        paths::get().server_config_file()
    }

    /// Primary client configuration path (`$XDG_CONFIG_HOME/nuo/client.toml`, ADR-0031).
    pub fn client_config_file_path() -> PathBuf {
        paths::get().client_config_file()
    }

    /// Primary terminal configuration path (`$XDG_CONFIG_HOME/nuo/terminal.toml`, ADR-0031).
    pub fn terminal_config_file_path() -> PathBuf {
        paths::get().terminal_config_file()
    }

    /// Primary agent configuration path (`$XDG_CONFIG_HOME/nuo/agent.toml`, ADR-0031).
    pub fn agent_config_file_path() -> PathBuf {
        paths::get().agent_config_file()
    }
}

#[cfg(test)]
pub use nuo_protocol::{RemoteModelMetadata, SecretString};
pub use nuo_tool::HookEventKind;

#[cfg(test)]
pub(crate) mod tests;
