//! User configuration schema and persistence.
//!
//! Deserializes/serializes the TOML config file (`agent`, `tui`, providers,
//! channels, MCP servers, hooks, skills, web-search) via [`crate::fsutil`]'s
//! atomic-write helpers, and loads/saves the input history. Config is state
//! (recency-merged under a companion file lock, ADR-0018); the live
//! provider/model selection telemetry lives in [`crate::provider_usage`].

use crate::fsutil;
use crate::paths;
use nuo_contracts::{
    CompactionPolicy, HookEventKind, McpServerConfig, RemoteModelMetadata, SecretString,
    SkillsConfig, TrajectoryGuardConfig, VariantSelection, WebConfig, WebProviderAxis,
};

/// Re-export so server/TUI can use the config-layer path without depending on
/// core's auth module name directly for `AddProvider`.
pub use nuo_contracts::ConnectionAuth as ConfigChannelAuth;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

/// Reserved `[tui.default_expanded]` key that controls reasoning traces.
/// Reasoning isn't a tool, so each frontend addresses it by name.
pub const THINKING_KEY: &str = "thinking";

/// User-tunable top-level agent behaviour, deserialized from the optional `[agent]`
/// table of `config.toml`.
/// All fields default sensibly, so a `config.toml` with no `[agent]` table
/// (or a partially specified one) is valid.
///
/// ```toml
/// [agent]
/// # Hard-stop a round after this many total ReAct turns. 0 (the default)
/// # means no hard stop — an opt-in execution budget only.
/// # hard_stop_turns = 0
///
/// # Never supervise a command needing stdin (sudo/gpg/passwd/…).
/// # Instead run it sealed (immediate-EOF stdin) so it fails fast
/// # with a non-interactive remedy hint.
/// # skip_interactive_input = false
///
/// # Doom-loop guard (variant-loop defense). On by default; one
/// same-signature re-run is tolerated before a block (ADR-0148). Opt out
/// here, or restore the strict first-repeat block with `threshold = 2`.
/// See [`DoomGuardConfig`]. The historical `nudge` key spelling still
/// # [agent.trajectory_guard]
/// # enabled = false
/// # threshold = 2
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    /// `0` (the default) means uncapped. Mutated at runtime via
    /// `Agent::set_hard_stop_turns`.
    pub hard_stop_turns: usize,
    /// Whether the model may supply stdin bytes for a `bash` command it emits
    /// (the opt-in "automatic flow" path, α). Default `false`: the command
    /// tool schema exposes no `stdin` parameter and a command that needs input
    /// either gets it from the operator (interactive-classifier →
    /// runtime supervision) or fails fast with a non-interactive remedy hint.
    /// When `true`, the schema **dynamically** adds a `stdin` field the model
    /// can fill, and the dispatch layer threads it through as
    /// `InputContract::Prefilled`. This is the explicit authorization that
    /// "input may come from the model" — without it, stdin is structurally
    /// unreachable from the model's arguments. Wired through
    /// `Agent::set_allow_model_stdin`.
    pub allow_model_stdin: bool,
    /// Whether an interactive command (one the interactive classifier
    /// matches: `sudo`/`gpg`/`passwd`/TUI editors/`read`/…) should **never** be
    /// supervised for runtime input.
    ///
    /// Default `false`: a command needing input is run supervised (held-open
    /// stdin pipe + controlling terminal), the runtime examiner detects the
    /// wait, and the operator is prompted via the input panel. When `true`, the
    /// command is run sealed — immediate-EOF stdin — and fails fast with a
    /// non-interactive remedy hint, exactly as in unattended mode. This is the
    /// right setting for users who find the prompt disruptive and prefer to
    /// retry the command themselves (or let the model retry with a
    /// non-interactive form). Wired through `Agent::set_skip_interactive_input`.
    ///
    /// Note: this only governs the *input-supervision* path; it does not turn
    /// the agent unattended, so ordinary tool confirmations still apply.
    pub skip_interactive_input: bool,
    /// ADR-0141: how an autonomous session (no human channel attached —
    /// piped headless, CI, cron) settles an `ask_user` question. Wire
    /// format: `"fail_closed"` (default) or `"recommended_labeled"`.
    /// Fail-closed refuses the question and tells the model to resolve the
    /// ambiguity itself; recommended-labeled answers each question with
    /// its first (recommended) option, with an explicit
    /// `[answered by policy, not by user]` label so the model cannot
    /// mistake the recommendation for a human decision.
    #[serde(default)]
    pub ask_user_fallback: nuo_contracts::human_request::AutonomousFallbackPolicy,
    /// Trajectory-guard configuration (`nuo_harness::trajectory_guard`, ADR-0247).
    /// Default **enabled** (`window: 16`, `threshold: 4`, `cognitive_review: true`).
    #[serde(default)]
    pub trajectory_guard: TrajectoryGuardConfig,
}

// `TrajectoryGuardConfig` is defined in `nuo_contracts::trajectory_guard_config`
// and re-exported above. It is the `[agent.trajectory_guard]` TOML table.

/// Declarative permission configuration — the `[permissions]` table. Lets users
/// pre-declare "always allow" rules in `config.toml` so default policies are
/// data-driven, not purely interactive:
///
/// ```toml
/// [[permissions.allow]]
/// tool = "execute_command"
/// scope = "*"
///
/// [[permissions.allow]]
/// tool = "read_text"
/// scope = "*"
/// ```
///
/// These seed the allowlist at startup; runtime "Always" decisions still write
/// to the persisted `permissions.json`. A config rule with scope `"*"` allows
/// every call to that tool without prompting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionConfig {
    /// Rules to pre-seed the "always allow" allowlist at startup.
    pub allow: Vec<PermissionRuleConfig>,
}

/// User-owned filesystem admission policy for the active workspace.
///
/// This table lives in the global `config.toml`, not in project-authored
/// `.nuo/config.toml`: repository content must never be able to widen its own
/// filesystem boundary. Relative entries are resolved from the active
/// workspace root.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// Additional directory roots that native file tools may access.
    pub additional_roots: Vec<String>,
}

/// Diagnostic report of resolved additional roots and skipped entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAdditionalRoots {
    /// Canonical, admitted directory paths.
    pub admitted: Vec<std::path::PathBuf>,
    /// Configured entries that could not be admitted, with the failure reason.
    pub skipped: Vec<(String, String)>,
}

/// Safety policy for model-issued `bash` commands. Built-in dangerous-command
/// rules are compiled into the agent so the config only contains user choices:
/// toggles and project-local overrides/additions.
///
/// ```toml
/// [bash_policy]
/// enabled = true
///
/// [[bash_policy.rules]]
/// name = "deny git reset hard"
/// match = "regex"
/// pattern = '(?i)\bgit\s+reset\s+--hard\b'
/// action = "deny"
/// reason = "This project never allows git reset --hard from the agent."
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyConfig {
    /// Master switch for the bash policy guard. Defaults to `true` so dangerous
    /// built-in commands are protected even when the user has broadly allowed
    /// the `bash` tool.
    pub enabled: bool,
    /// Whether an explicit user `allow` rule may override a compiled-in `deny`
    /// rule. Defaults to `false`; user `allow` rules can still override
    /// compiled-in `confirm` rules.
    pub allow_user_override_builtin_deny: bool,
    /// Project/user-defined rules. Evaluated before built-in `confirm` rules,
    /// but built-in `deny` rules remain a hard floor unless
    /// `allow_user_override_builtin_deny` is set.
    pub rules: Vec<BashPolicyRuleConfig>,
}

impl Default for BashPolicyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_user_override_builtin_deny: false,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BashPolicyRuleConfig {
    /// Human-readable rule name shown in policy decisions.
    pub name: String,
    /// Matcher type. TOML uses `match = "regex"`; `matcher` is accepted as an
    /// alias for callers that avoid the keyword-like field name.
    #[serde(rename = "match", alias = "matcher")]
    pub matcher: BashPolicyMatcherConfig,
    /// Pattern consumed by the selected matcher.
    pub pattern: String,
    /// Decision to apply when this rule matches.
    pub action: BashPolicyActionConfig,
    /// Optional user-facing explanation.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyMatcherConfig {
    /// Rust `regex` matched against the full shell command string.
    #[default]
    Regex,
    /// Case-sensitive substring match against the full command string.
    Contains,
    /// Case-sensitive prefix match after trimming leading whitespace.
    StartsWith,
    /// Match the leading program name (after leading env assignments), e.g.
    /// `git`, `cargo`, `kubectl`.
    Program,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BashPolicyActionConfig {
    /// Let the command proceed to the normal permission broker.
    Allow,
    /// Require a one-off human confirmation, ignoring any broad `bash *` allow.
    Confirm,
    /// Refuse the command before spawning a shell.
    #[default]
    Deny,
}

/// One declarative permission rule from `[permissions]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleConfig {
    /// Tool name (e.g. `"execute_command"`, `"read_text"`, `"mcp__fs__read"`).
    pub tool: String,
    /// Permission scope. `"*"` matches every call to the tool. Any other value
    /// must match the call's scope *exactly* (e.g. a full path, or the exact
    /// command string for `bash`) — there is no prefix/substring matching.
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "*".to_string()
}

/// Capability metadata overlaid from a provider's live `GET /models` response
/// for one model id the client registry does not know. Persisted in the
/// remote catalog cache (`remote_catalog.json`) so the metadata survives
/// restarts: a background sync refreshes it, and a failed
/// fetch leaves the last good values in place. Only instances created from a
/// preset whose `RemoteCatalogSource` is an endpoint that returns capability
/// fields (trusted official endpoints) ever carry this.
/// See `nuo_contracts::model::FittedModel`.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct FittedModelInfo {
    /// Advertised context window in tokens (`0` = the endpoint did not say).
    #[serde(default)]
    pub context_window: usize,
    /// The endpoint advertises reasoning (e.g. a `reasoning_content` stream).
    #[serde(default)]
    pub reasoning: bool,
    /// The endpoint advertises image inputs. `None` (also what an older cache
    /// entry without the field deserializes to) means *undeclared*: the
    /// endpoint said nothing, and the route must not be treated as
    /// text-only (ADR-0230).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Advertised reasoning-effort tiers, as named by the provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
}

/// Per-(instance, model) reasoning overrides — the user's own per-route
/// choices (set from the model `e` editor), persisted in **state** via
/// [`crate::route_settings::RouteSettingsStore`] (keyed
/// `providers[<instance_id>][<model_id>]`). Unlike the *derived* capability
/// fields ([`FittedModelInfo`]), these are not rebuildable: the entry's
/// presence opts the model in to reasoning on Anthropic-protocol routes,
/// `thinking` defaulting **on** unless explicitly `false` (ADR-0046).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteSettings {
    /// Reasoning depth: `"none"`/`"minimal"`/`"low"`/`"medium"`/`"high"`/
    /// `"xhigh"`/`"max"`, clamped at request time to the model's levels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Whether extended thinking is on (`true`) or off (`false`) once the
    /// route is opted in. Defaults to on when the entry exists; set `false`
    /// to reason with depth only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// Explicit capability overrides -- the **top layer** of the capability
    /// resolution order (ADR-0149). `None`/empty means "no opinion": the
    /// effective capabilities fall through to remote metadata, then the
    /// static baseline. Unlike the derived `FittedModelInfo`, these are the
    /// user's own per-route choices and are never rebuilt from an endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_overrides: Option<nuo_contracts::CapabilityOverrides>,
    /// Prompt-cache behavior requested for this concrete route. The exact
    /// mode and retention must be advertised by the resolved route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<nuo_contracts::PromptCachePreference>,
}

impl RouteSettings {
    /// Whether the entry carries any explicit knob. An entry with neither
    /// field set still opts the model in to thinking on Anthropic routes.
    /// Capability overrides (ADR-0149 layer 1) count as a knob: a record that
    /// only carries them must not be pruned as empty.
    pub fn is_empty(&self) -> bool {
        self.effort.is_none()
            && self.thinking.is_none()
            && self
                .capability_overrides
                .as_ref()
                .is_none_or(nuo_contracts::CapabilityOverrides::is_empty)
            && self.prompt_cache.is_none()
    }
}

/// Provider API keys split out of `config.toml` into their own
/// `credentials.toml` (written `rw-------` via [`crate::fsutil`]). This is the
/// **secret** half of provider configuration: `config.toml` holds the
/// *behavior*, `providers.toml` the instance *declarations*, and this file the
/// *keys* — so the other two files can be shared, screenshotted, or
/// version-controlled without leaking credentials.
///
/// Credentials are keyed by **provider instance**, never by route — a route
/// is a derived model path, not a security master. One instance has
/// exactly one API-key credential:
///
/// ```toml
/// [providers]
/// deepseek = "sk-..."
/// ```
///
/// OAuth logins do **not** live here; their access/refresh token sets are
/// runtime state in `auth.toml` (`[tokens.<connection-id>]`), also keyed by
/// exact connection id.
///
/// Resolution precedence is **`api_key_env` env var > credentials.toml**: the
/// instance declares an optional `api_key_env` (a variable *name*) in
/// `providers.toml`; the catalog resolves env-first, then this file. The map
/// is a `BTreeMap` for stable, diff-friendly serialisation. Unknown tables
/// (e.g. the pre-refactor `[builtins.<id>]` / `[user.<id>]` sections) are
/// tolerated and ignored so a not-yet-migrated file keeps loading.
/// Credentials split out of `config.toml` into their own `credentials.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// API keys keyed by connection id.
    #[serde(default, alias = "providers")]
    pub connections: BTreeMap<String, SecretString>,
    /// Credentials keyed by compiled web provider id, not connection id.
    #[serde(
        default,
        alias = "websearch",
        skip_serializing_if = "WebCredentials::is_empty"
    )]
    pub web: WebCredentials,
    /// One-shot migration markers. These prevent preserved legacy secrets from
    /// being re-imported after a user deliberately clears their new value.
    #[serde(default, skip_serializing_if = "CredentialMigrations::is_empty")]
    pub migrations: CredentialMigrations,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CredentialMigrations {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web_connections_v1: bool,
}

impl CredentialMigrations {
    fn is_empty(&self) -> bool {
        !self.web_connections_v1
    }
}

/// Web credentials. Flat fields are accepted only as legacy migration input;
/// serialization emits the search and reader maps.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebCredentials {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub search: BTreeMap<String, SecretString>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reader: BTreeMap<String, SecretString>,
    #[serde(default, skip_serializing)]
    pub exa_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub parallel_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub tavily_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub bocha_api_key: Option<SecretString>,
    #[serde(default, skip_serializing)]
    pub jina_api_key: Option<SecretString>,
}

impl WebCredentials {
    pub fn is_empty(&self) -> bool {
        self.search.is_empty()
            && self.reader.is_empty()
            && self.exa_api_key.is_none()
            && self.parallel_api_key.is_none()
            && self.tavily_api_key.is_none()
            && self.bocha_api_key.is_none()
            && self.jina_api_key.is_none()
    }

    fn normalize_legacy(&mut self) -> bool {
        let mut changed = false;
        for (id, secret) in [
            ("exa", self.exa_api_key.take()),
            ("parallel", self.parallel_api_key.take()),
            ("tavily", self.tavily_api_key.take()),
            ("bocha", self.bocha_api_key.take()),
        ] {
            if let Some(secret) = secret {
                self.search.entry(id.to_string()).or_insert(secret);
                changed = true;
            }
        }
        if let Some(secret) = self.jina_api_key.take() {
            self.reader.entry("jina".to_string()).or_insert(secret);
            changed = true;
        }
        changed
    }
}

impl Credentials {
    fn path() -> PathBuf {
        paths::get().credentials_file()
    }

    /// Read `credentials.toml`, returning an empty (not erroring) value when
    /// the file is missing or unparseable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&content) {
            Ok(mut credentials) => {
                let mut changed = credentials.web.normalize_legacy();
                if !credentials.migrations.web_connections_v1
                    && crate::web_migration::migrate_connection_credentials(&mut credentials)
                        .is_some()
                {
                    credentials.migrations.web_connections_v1 = true;
                    changed = true;
                }
                if changed && let Err(error) = credentials.save() {
                    tracing::warn!(%error, "could not persist migrated web credentials");
                }
                credentials
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse credentials file; ignoring",
                );
                Self::default()
            }
        }
    }

    /// Persist atomically with owner-only permissions (0600) via
    /// [`crate::fsutil::atomic_write_bytes`].
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// The credential for `connection_id`, if set and non-empty.
    pub fn api_key(&self, connection_id: &str) -> Option<&SecretString> {
        self.connections
            .get(connection_id)
            .filter(|k| !k.expose_secret().trim().is_empty())
    }

    pub fn web_credential(
        &self,
        axis: WebProviderAxis,
        provider_id: &str,
    ) -> Option<&SecretString> {
        let credentials = match axis {
            WebProviderAxis::Search => &self.web.search,
            WebProviderAxis::Reader => &self.web.reader,
        };
        credentials
            .get(provider_id)
            .filter(|secret| !secret.expose_secret().trim().is_empty())
    }

    pub fn set_web_credential(
        &mut self,
        axis: WebProviderAxis,
        provider_id: &str,
        secret: Option<SecretString>,
    ) {
        let credentials = match axis {
            WebProviderAxis::Search => &mut self.web.search,
            WebProviderAxis::Reader => &mut self.web.reader,
        };
        match secret {
            Some(secret) if !secret.expose_secret().trim().is_empty() => {
                credentials.insert(provider_id.to_string(), secret);
            }
            _ => {
                credentials.remove(provider_id);
            }
        }
    }

    /// Set (or clear) the credential for `connection_id`.
    pub fn set_api_key(&mut self, connection_id: &str, key: Option<SecretString>) {
        match key {
            Some(key) if !key.expose_secret().trim().is_empty() => {
                self.connections.insert(connection_id.to_string(), key);
            }
            _ => {
                self.connections.remove(connection_id);
            }
        }
    }

    /// Remove the credential for `connection_id`, if any.
    pub fn remove_api_key(&mut self, connection_id: &str) {
        self.connections.remove(connection_id);
    }
}

pub struct ResolvedWebConfig {
    pub runtime: nuo_contracts::WebRuntimeConfig,
    pub search_credential: nuo_contracts::WebCredentialStatus,
    pub reader_credential: nuo_contracts::WebCredentialStatus,
}

pub fn resolve_web_config(config: &WebConfig, credentials: &Credentials) -> ResolvedWebConfig {
    use nuo_contracts::{WebCredentialRequirement as Requirement, WebCredentialStatus as Status};

    fn resolve(
        axis: WebProviderAxis,
        provider_id: &str,
        requirement: Requirement,
        env_name: Option<&str>,
        credentials: &Credentials,
    ) -> (Option<SecretString>, Status) {
        if requirement == Requirement::None {
            return (None, Status::NotRequired);
        }
        if let Some(env_name) = env_name
            && let Ok(value) = std::env::var(env_name)
            && !value.trim().is_empty()
        {
            return (Some(SecretString::new(value)), Status::Environment);
        }
        if let Some(secret) = credentials.web_credential(axis, provider_id) {
            return (Some(secret.clone()), Status::Stored);
        }
        let status = match requirement {
            Requirement::Required => Status::RequiredMissing,
            Requirement::Optional => Status::OptionalMissing,
            Requirement::None => Status::NotRequired,
        };
        (None, status)
    }

    let search = config.provider.capability();
    let reader = config.reader.capability();
    let (search_credential, search_status) = search
        .as_ref()
        .map(|capability| {
            resolve(
                WebProviderAxis::Search,
                &capability.id,
                capability.credential,
                capability.default_env_var.as_deref(),
                credentials,
            )
        })
        .unwrap_or((None, Status::NotRequired));
    let (reader_credential, reader_status) = reader
        .as_ref()
        .map(|capability| {
            resolve(
                WebProviderAxis::Reader,
                &capability.id,
                capability.credential,
                capability.default_env_var.as_deref(),
                credentials,
            )
        })
        .unwrap_or((None, Status::NotRequired));

    ResolvedWebConfig {
        runtime: nuo_contracts::WebRuntimeConfig {
            behavior: config.clone(),
            search_credential,
            reader_credential,
        },
        search_credential: search_status,
        reader_credential: reader_status,
    }
}

/// Discovered model lists and fitted capabilities, cached under
/// `$XDG_STATE_HOME/muta/remote_catalog.json`.
///
/// Lives under state (not cache) since the contents — discovered model ids,
/// ETag revalidation metadata, advertised capability fields — are
/// program-generated state the user expects to survive restarts rather than
/// regenerable-from-scratch cache data. A pre-0.43 build wrote this file under
/// `$XDG_CACHE_HOME/muta/remote_catalog.json`; that legacy copy is read once
/// on first load after upgrade and adopted into the state path (see
/// [`Self::load`]).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteCatalogCache {
    /// Cached discovered model lists, keyed by connection id:
    /// connection_id -> model ids (in catalog order).
    #[serde(default)]
    pub connection_models: BTreeMap<String, Vec<String>>,
    /// Fitted capability metadata, keyed by connection id then model id.
    #[serde(default)]
    pub fitted_models: BTreeMap<String, BTreeMap<String, FittedModelInfo>>,
    /// Trusted per-(connection, model) capability metadata advertised by the
    /// connection's live `GET /models` (endpoint, thinking, effort tiers …).
    #[serde(default)]
    pub remote_metadata: BTreeMap<String, BTreeMap<String, RemoteModelMetadata>>,
    /// Revalidation metadata for live catalogs, keyed by connection id. The
    /// actual model/capability payload remains in the maps above so older cache
    /// files continue to deserialize without migration.
    #[serde(default)]
    pub model_lists: BTreeMap<String, ModelListCacheState>,
}

/// Freshness and validator state for one connection's remote model catalog.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelListCacheState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default)]
    pub client_version: String,
    /// Stable identity of the catalog request that produced this validator.
    /// An ETag is reusable only for the same source, endpoint, protocol, and
    /// client emulation profile.
    #[serde(default)]
    pub source_identity: String,
    #[serde(default)]
    pub refreshed_at_ms: i64,
    /// Whether the most recent refresh attempt for this connection **failed**.
    /// A failed refresh deliberately keeps the previous payload
    /// (`[INV-CATALOG-03]`), so this is the only record that the retained
    /// availability verdicts were observed before a failure and may already be
    /// out of date (ADR-0273). Cleared by any successful refresh.
    #[serde(default)]
    pub refresh_failed: bool,
}

impl RemoteCatalogCache {
    pub fn file_path() -> PathBuf {
        paths::get().remote_catalog_cache_file()
    }

    /// Read `remote_catalog.json`, returning an empty value if missing or unparseable.
    ///
    /// Clean break (ADR-0203 §29): no cache-dir migration shim. A file left at
    /// the retired `$XDG_CACHE_HOME/muta/remote_catalog.json` location is
    /// ignored and re-derived — the catalog is program-generated state, so a
    /// missing read costs one refresh, not correctness.
    pub fn load() -> Self {
        let path = Self::file_path();
        if let Ok(content) = fs::read_to_string(&path) {
            return serde_json::from_str(&content).unwrap_or_default();
        }
        Self::default()
    }

    /// Persist atomically to `$XDG_STATE_HOME/muta/remote_catalog.json`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_json::to_vec_pretty(self)?;
        fsutil::atomic_write_bytes(&Self::file_path(), &bytes)?;
        Ok(())
    }

    /// Remove the per-connection records for `connection_id` (used on connection deletion).
    pub fn remove_connection(&mut self, connection_id: &str) {
        self.connection_models.remove(connection_id);
        self.fitted_models.remove(connection_id);
        self.remote_metadata.remove(connection_id);
        self.model_lists.remove(connection_id);
    }

    /// Re-key every cached entry from `from` to `to` (connection rename).
    ///
    /// A rename changes the connection's *name*, not what it fetches:
    /// [`crate::RemoteCatalogCache`]'s `model_lists` validator fingerprint is
    /// derived from the fetch attributes (shape, base URL, client profile,
    /// dimensions), never from the name, so the cached ETag stays valid for the
    /// renamed connection and the next sync revalidates instead of refetching.
    /// Discarding the entry here would throw away a live validator and force a
    /// full catalog fetch for no reason — which is why rename carries rather
    /// than deletes.
    pub fn rename_connection(&mut self, from: &str, to: &str) {
        if from.eq_ignore_ascii_case(to) {
            return;
        }
        if let Some(models) = self.connection_models.remove(from) {
            self.connection_models.insert(to.to_string(), models);
        }
        if let Some(fitted) = self.fitted_models.remove(from) {
            self.fitted_models.insert(to.to_string(), fitted);
        }
        if let Some(metadata) = self.remote_metadata.remove(from) {
            self.remote_metadata.insert(to.to_string(), metadata);
        }
        if let Some(state) = self.model_lists.remove(from) {
            self.model_lists.insert(to.to_string(), state);
        }
    }

    /// The trusted per-(connection, model) metadata, if set.
    pub fn remote_metadata_for(
        &self,
        connection_id: &str,
        model_id: &str,
    ) -> Option<&RemoteModelMetadata> {
        self.remote_metadata
            .get(connection_id)
            .and_then(|models| models.get(model_id))
    }

    /// Acquire the cross-process lock on `remote_catalog.json` and load the latest state under the lock.
    pub async fn lock() -> Result<LockedRemoteCatalogCache, String> {
        let path = Self::file_path();
        let lock = tokio::task::spawn_blocking(move || fsutil::FileLock::acquire(&path))
            .await
            .map_err(|error| format!("remote-catalog lock task failed: {error}"))?
            .map_err(|error| format!("could not lock remote-catalog cache: {error}"))?;
        let cache = Self::load();
        Ok(LockedRemoteCatalogCache { cache, _lock: lock })
    }

    /// Execute a transactional mutation on `RemoteCatalogCache` under the cross-process lock and save atomically.
    pub async fn modify<F, R>(f: F) -> Result<R, String>
    where
        F: FnOnce(&mut RemoteCatalogCache) -> R,
    {
        let mut locked = Self::lock().await?;
        let result = f(&mut locked);
        locked.save().map_err(|e| e.to_string())?;
        Ok(result)
    }
}

/// An exclusive cross-process lock over `remote_catalog.json`.
pub struct LockedRemoteCatalogCache {
    cache: RemoteCatalogCache,
    _lock: fsutil::FileLock,
}

impl std::ops::Deref for LockedRemoteCatalogCache {
    type Target = RemoteCatalogCache;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl std::ops::DerefMut for LockedRemoteCatalogCache {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cache
    }
}

impl LockedRemoteCatalogCache {
    pub fn cache(&self) -> &RemoteCatalogCache {
        &self.cache
    }

    pub fn cache_mut(&mut self) -> &mut RemoteCatalogCache {
        &mut self.cache
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.cache.save()
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Config {
    #[serde(alias = "default_provider")]
    pub default_connection: String,
    pub mcp: HashMap<String, McpServerConfig>,
    /// Versioned context lifecycle and admission policy (ADR-0280).
    pub context: nuo_contracts::context_lifecycle::ContextPolicy,
    /// Deprecated context-compaction thresholds, skipped on serialization (ADR-0280).
    #[serde(skip)]
    pub compaction: CompactionPolicy,
    /// Maximum number of attempts for a single model request when the connection returns a
    /// transient error (HTTP 408/429/5xx, connection, timeout). The initial try
    /// counts as the first attempt, so this is the *total* attempts, not extra
    /// retries. Clamped to `[1, 60]` at the call site.
    #[serde(alias = "provider_retry_max_attempts")]
    pub connection_retry_max_attempts: usize,
    /// Base delay (ms) for the bounded stepped backoff between retries:
    /// `1s -> 2s -> 5s -> 10s -> 10s ...` (scaled by `base_ms`), capped by `connection_retry_max_ms`.
    #[serde(alias = "provider_retry_base_ms")]
    pub connection_retry_base_ms: u64,
    /// Hard cap (ms) on a single backoff delay, including the exponential growth.
    /// A server-supplied `Retry-After`/`retry-after-ms` header still wins but is
    /// itself capped at this value.
    #[serde(alias = "provider_retry_max_ms")]
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
    /// Daemon lifecycle knobs (ADR-0101): the `[daemon]` table of
    /// `config.toml`. Controls how the session daemon exits — its shutdown
    /// grace budget and its idle-empty auto-exit.
    #[serde(default)]
    pub daemon: DaemonConfig,
}

/// Daemon lifecycle configuration, deserialized from the `[daemon]` table of
/// `config.toml` (ADR-0101).
///
/// ```toml
/// [daemon]
/// shutdown_grace_secs = 10      # graceful-teardown budget before forced exit
/// idle_exit_minutes = 5         # auto-exit after N minutes of zero sessions
///                                # and zero attached clients; 0 = never
/// local_auth = true             # bearer-token the loopback listener too
///                                # (ADR-0105); false = trust local processes
/// ///                                # at boot (ADR-0125); false = cold start
/// ```
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct DaemonConfig {
    /// Total budget for graceful shutdown (listeners close → connections
    /// drain → sessions tear down with `SessionEnd` hooks). When the budget
    /// expires — or a second signal arrives — remaining tasks are aborted and
    /// the process exits anyway, so a hung external hook can never pin the
    /// daemon open. A always-on/service deployment should set this at or
    /// above the supervisor's stop timeout (e.g. systemd's `TimeoutStopSec`).
    pub shutdown_grace_secs: u64,
    /// Auto-exit after this many continuous minutes hosting **zero sessions
    /// with zero attached clients** (ADR-0100 rule 3): the daemon becomes
    /// born-on-demand, gone-when-useless. `0` disables idle exit for
    /// always-on deployments.
    pub idle_exit_minutes: u64,
    /// Require a bearer token even on the loopback TCP listener (ADR-0105).
    /// The token is generated per daemon start and published in the
    /// owner-only (0600) discovery record, so co-located CLI/TUI clients
    /// authenticate transparently while other local processes, other users
    /// on a shared machine, and drive-by browser pages cannot drive the
    /// control plane. `false` restores the pre-0105 trust-the-loopback
    /// posture; the UDS listener is always exempt (filesystem permissions
    /// are its boundary). Default: true.
    pub local_auth: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            shutdown_grace_secs: 10,
            idle_exit_minutes: 5,
            local_auth: true,
        }
    }
}

/// Per-model tool-variant selection, deserialized from the `[tool_variants]`
/// section of `config.toml`. Maps a model id to a `capability → variant_id`
/// map. A capability is realized by the named variant (a genuinely different
/// implementation/schema/description), not a re-worded copy of one impl.
///
/// ```toml
/// [tool_variants."glm-5.2"]       # model id (quoted: has dots)
/// read_text        = "terse"    # capability = variant id
/// execute_command  = "workspace"
/// ```
///
/// Capabilities and models not listed use their default variant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolVariantsConfig(pub HashMap<String, ModelToolVariants>);

/// One model's variant selection: a transparent wrapper around the
/// `capability → variant_id` map so it serializes directly as a TOML table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelToolVariants(pub VariantSelection);

impl ToolVariantsConfig {
    /// Look up the variant selection for `model_id`, if any. Returns an empty
    /// map (not `None`) for unknown models so callers can always borrow
    /// `&VariantSelection`.
    pub fn for_model(&self, model_id: &str) -> &VariantSelection {
        self.0
            .get(model_id)
            .map(|m| &m.0)
            .unwrap_or_else(|| nuo_contracts::empty_variant_selection())
    }
}

/// One lifecycle event hook entry (ADR-0025). Deserialized from a `[[hooks]]`
/// table in `config.toml`:
///
/// ```toml
/// [[hooks]]
/// event   = "PostToolUse"          # a [`HookEventKind`] variant
/// matcher = "Write|Edit"           # optional; tool-name `|`-list or regex
/// command = ".nuo/hooks/lint.sh"
/// ```
///
/// The command receives the [`nuo_contracts::HookContext`] as JSON on stdin and
/// communicates a decision via exit code / stdout JSON (see the CLI subagent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpec {
    /// When this hook fires.
    pub event: HookEventKind,
    /// Tool-name filter. `None` (or unset) matches every event; only tool
    /// events (`PreToolUse` / `PostToolUse` / `PostToolUseFailure`) honour it.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Shell command run when the event matches. Executed with the project
    /// root as cwd and the hook context as JSON on stdin.
    pub command: String,
    /// Runtime-only origin marker. Project-defined hooks carry their exact
    /// workspace root and execute read-only/offline inside the workspace
    /// sandbox. Global user hooks leave this unset.
    #[serde(skip)]
    pub sandbox_root: Option<std::path::PathBuf>,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default, alias = "default_provider")]
    default_connection: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    mcp: Option<HashMap<String, McpServerConfig>>,
    #[serde(default)]
    context: Option<nuo_contracts::context_lifecycle::ContextPolicy>,
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
    #[serde(default, alias = "provider_retry_max_attempts")]
    connection_retry_max_attempts: Option<usize>,
    #[serde(default, alias = "provider_retry_base_ms")]
    connection_retry_base_ms: Option<u64>,
    #[serde(default, alias = "provider_retry_max_ms")]
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
    websearch: Option<WebConfig>,
    #[serde(default)]
    agent: Option<AgentConfig>,
    #[serde(default)]
    hooks: Option<Vec<HookSpec>>,
    #[serde(default)]
    tool_variants: Option<ToolVariantsConfig>,
    #[serde(default)]
    daemon: Option<DaemonConfig>,
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
                "legacy 'compaction.*' configuration is retired under ADR-0280 [INV-POLICY-01]; run `muta context migrate` to convert to versioned `context.*` policy",
            ));
        }
        if let Some(ctx) = raw.context {
            ctx.validate().map_err(serde::de::Error::custom)?;
            cfg.context = ctx;
        }
        if let Some(a) = raw.connection_retry_max_attempts {
            cfg.connection_retry_max_attempts = a;
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
        if let Some(web) = raw.web.or(raw.websearch) {
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
        if let Some(d) = raw.daemon {
            cfg.daemon = d;
        }
        Ok(cfg)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_connection: String::new(),
            mcp: HashMap::new(),
            context: nuo_contracts::context_lifecycle::ContextPolicy::default(),
            compaction: CompactionPolicy::default(),
            connection_retry_max_attempts: 30,
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
            daemon: DaemonConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let config_path = Self::config_file_path();
        match fs::read_to_string(&config_path) {
            Ok(content) => {
                match toml::from_str(&crate::web_migration::migrate_config_source(&content)) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        // A corrupt config must never block startup, but falling
                        // back to defaults *silently* would discard the user's
                        // entire setup with no trace of why. Warn loudly (the
                        // log carries the file and the error) so a typo'd
                        // config.toml is diagnosable instead of reading as
                        // "muta forgot my settings".
                        tracing::error!(
                            path = %config_path.display(),
                            %error,
                            "config.toml is unparseable; continuing with defaults \
                             (fix the syntax error to restore the saved configuration)"
                        );
                        Config::default()
                    }
                }
            }
            // Absent is the normal first-run condition; nothing to report.
            Err(_) => Config::default(),
        }
    }

    /// Load only the `[mcp.*]` table from a project-local `.nuo/config.toml`
    /// (ADR-0085 §2/§3). Returns an empty map when the file or table is absent.
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
        // Muta's `read_only` and `enabled` policy fields. A JSON definition with
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
            /// Config-time tool scoping (ADR-0085 follow-up): original-name
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

    /// Load role-scoped MCP servers from `$XDG_CONFIG_HOME/muta/roles/<role>/mcp.json` (ADR-0253).
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

    /// Merge a role-scoped MCP server set into this config (ADR-0253).
    pub fn merge_role_mcp(&mut self, role_mcp: HashMap<String, McpServerConfig>) {
        for (name, server) in role_mcp {
            self.mcp.insert(name, server);
        }
    }

    /// Merge a project-local MCP server set into this (global-origin) config.
    /// A project entry with the same name as a global entry **replaces** it
    /// wholesale (ADR-0085 §4); project entries with new names are added. The
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
    /// The in-memory `Config` is loaded once at process start (ADR-0209) and
    /// lives on for the daemon's whole lifetime, while the file can change
    /// under it at any moment. A naive whole-file rewrite therefore resurrects
    /// anything the user deleted — the reported bug: removing
    /// `[workspace].additional_roots` from `~/.config/muta/config.toml` while a
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
    ///   mutations of user-owned tables (web settings, favorites, `muta mcp
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
        // Serialise against other `muta` instances so concurrent config
        // writes do not lost-update each other (ADR-0018 pattern). The lock is
        // held on the companion `.lock` file (not the data file, which is
        // rewritten via temp + rename and swaps inodes) for the whole RMW.
        let config_path = Self::config_file_path();
        let _lock = fsutil::FileLock::acquire(&config_path)
            .map_err(|e| format!("could not lock config file: {e}"))?;

        // The on-disk state as of *right now*, under the lock. An absent or
        // unparseable file contributes the default document: the ordinary
        // load path already warns about corruption, and a first run simply
        // has nothing to preserve.
        let on_disk: Config = fs::read_to_string(&config_path)
            .ok()
            .and_then(|content| toml::from_str(&content).ok())
            .unwrap_or_default();

        // Runtime-owned: the selection pair. When preserving, the on-disk
        // value wins so another process's write survives; the snapshot value
        // is only the fallback for an empty/never-set disk default.
        let (default_connection, default_model) = if preserve_connection_selection {
            let connection = if on_disk.default_connection.is_empty() {
                // On-disk default is gone (or never set): keep this writer's
                // selection so the file never silently loses it.
                self.default_connection.clone()
            } else {
                on_disk.default_connection.clone()
            };
            (connection, on_disk.default_model.clone())
        } else {
            (self.default_connection.clone(), self.default_model.clone())
        };

        // User-owned: start from the disk document (hand edits and all) and
        // overlay only the runtime-owned pair.
        let mut out = on_disk;
        out.default_connection = default_connection;
        out.default_model = default_model;

        // config.toml = behavior only
        // Secrets live in `credentials.toml`, connections in `connections.toml`.
        let bytes = toml::to_string_pretty(&out)?.into_bytes();
        fsutil::atomic_write_bytes(&config_path, &bytes)?;
        Ok(())
    }

    pub fn config_file_path() -> PathBuf {
        paths::get().config_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_table_round_trips_through_toml() {
        // The `[agent]` table must round-trip: partial TOML keeps defaults,
        // full TOML preserves explicit overrides.
        let toml_canonical = r#"
            [agent]
            hard_stop_turns = 40
        "#;
        let cfg: Config = toml::from_str(toml_canonical).unwrap();
        assert_eq!(cfg.agent.hard_stop_turns, 40);

        // Missing `[agent]` table → defaults match the documented values.
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.agent.hard_stop_turns, 0);

        // Round-trip through save+load format (serialize then parse).
        let mut cfg = Config::default();
        cfg.agent.hard_stop_turns = 99;
        let serialised = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&serialised).unwrap();
        assert_eq!(parsed.agent.hard_stop_turns, 99);
    }

    #[test]
    fn compaction_round_count_writes_canonical_key_and_drops_legacy_key() {
        // ADR-0120 / ADR-0280: the pre-ADR-0047 key is not aliased. It parses as
        // an unknown key (warned and ignored) and the field stays at its
        // default — the stale value must not carry through.
        let legacy: Config = toml::from_str("compaction_preserve_turns = 9").unwrap();
        assert_eq!(
            legacy.context.preferred_recent_rounds,
            Config::default().context.preferred_recent_rounds
        );

        let serialized = toml::to_string(&legacy).unwrap();
        assert!(serialized.contains("preferred_recent_rounds ="));
        assert!(!serialized.contains("compaction_preserve_turns ="));
        assert!(!serialized.contains("[compaction]"));
    }

    #[test]
    fn legacy_compaction_keys_are_refused_under_inv_policy_01() {
        let err = toml::from_str::<Config>("[compaction]\nutilization = 0.85\n").unwrap_err();
        assert!(err.to_string().contains("retired under ADR-0280 [INV-POLICY-01]"));

        let err2 = toml::from_str::<Config>("compaction_preserve_rounds = 6\n").unwrap_err();
        assert!(err2.to_string().contains("retired under ADR-0280 [INV-POLICY-01]"));
    }

    #[test]
    fn trajectory_guard_table_writes_canonical_key() {
        let canonical: Config =
            toml::from_str("[agent.trajectory_guard]\nenabled = false\nwindow = 24\n").unwrap();
        assert!(!canonical.agent.trajectory_guard.enabled);
        assert_eq!(canonical.agent.trajectory_guard.window, 24);

        let serialized = toml::to_string(&canonical).unwrap();
        assert!(
            serialized.contains("[agent.trajectory_guard]"),
            "got: {serialized}"
        );
    }

    #[test]
    fn tool_variants_table_parses_and_resolves_per_model() {
        // The table name mirrors the Config field name (`tool_variants`), as
        // serde maps struct fields to TOML keys verbatim. The model id is
        // quoted because it contains dots/hyphens. Each entry maps a capability
        // name to the variant id chosen for that model.
        let toml_src = r#"
            [tool_variants."kimi-k2.7-code"]
            read_text = "terse"
            execute_command = "workspace"

            [tool_variants."glm-5.2"]
            read_text = "verbose"
        "#;
        let cfg: Config = toml::from_str(toml_src).unwrap();

        // Known model → its map; unlisted capability within a known model → absent.
        let kimi = cfg.tool_variants.for_model("kimi-k2.7-code");
        assert_eq!(kimi.get("read_text").map(String::as_str), Some("terse"));
        assert_eq!(
            kimi.get("execute_command").map(String::as_str),
            Some("workspace")
        );
        assert!(kimi.get("grep").is_none());

        // A different model gets its own independent map.
        let glm = cfg.tool_variants.for_model("glm-5.2");
        assert_eq!(glm.get("read_text").map(String::as_str), Some("verbose"));
        assert!(glm.get("execute_command").is_none());

        // Unknown model → empty (but borrowable without an Option).
        assert!(cfg.tool_variants.for_model("does-not-exist").is_empty());

        // Absent table entirely → empty config, every lookup is empty.
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.tool_variants.for_model("kimi-k2.7-code").is_empty());
    }

    #[test]
    fn tool_variants_round_trip_through_serialise() {
        let mut cfg = Config::default();
        let mut sel = nuo_contracts::VariantSelection::new();
        sel.insert("read_text".to_string(), "terse".to_string());
        sel.insert("bash".to_string(), "strict".to_string());
        cfg.tool_variants
            .0
            .insert("kimi-k2.7-code".to_string(), ModelToolVariants(sel));
        let serialised = toml::to_string(&cfg).unwrap();
        let parsed: Config = toml::from_str(&serialised).unwrap();
        let resolved = parsed.tool_variants.for_model("kimi-k2.7-code");
        assert_eq!(resolved.get("read_text").map(String::as_str), Some("terse"));
        assert_eq!(resolved.get("bash").map(String::as_str), Some("strict"));
    }

    /// Tests that mutate the process-wide paths override (`set_test_default`)
    /// and read/write the throwaway config/credentials/cache files must
    /// serialise against each other so the parallel subagent never observes
    /// another test's Dirs. Mirrors the `ENV_GUARD` pattern in `paths.rs`.
    static PATHS_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fresh throwaway directory + the test paths override installed against
    /// it. Drop the returned guards (both the module lock and the crate-wide
    /// override lock) to restore the default paths so the next test starts
    /// clean.
    ///
    /// The crate-wide `paths::TEST_OVERRIDE_GUARD` is what actually serialises
    /// against `session`'s override-touching tests; the per-module `PATHS_GUARD`
    /// is kept for any intra-module shared state.
    fn sandbox_config_dir() -> (
        std::path::PathBuf,
        std::sync::MutexGuard<'static, ()>,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let override_guard = paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let guard = PATHS_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!("muta-creds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let dirs = paths::Dirs {
            config_dir: tmp.clone(),
            data_dir: tmp.join("data"),
            state_dir: tmp.join("state"),
            cache_dir: tmp.join("cache"),
            runtime_dir: None,
        };
        paths::set_test_default(Some(dirs));
        (tmp, guard, override_guard)
    }

    #[test]
    fn credentials_round_trip_through_toml() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        let mut creds = Credentials::default();
        creds.set_api_key("deepseek", Some("sk-ds".into()));
        creds.set_api_key("relay", Some("relay-secret".into()));
        // Empty / whitespace keys never materialise an entry.
        creds.set_api_key("keyless", Some("   ".into()));
        creds.save().unwrap();

        let on_disk = std::fs::read_to_string(tmp.join("credentials.toml")).unwrap();
        assert!(on_disk.contains("sk-ds"));
        assert!(on_disk.contains("relay-secret"));
        assert!(!on_disk.contains("keyless"), "empty key must not persist");

        let mut reloaded = Credentials::load();
        assert_eq!(
            reloaded
                .api_key("deepseek")
                .map(SecretString::expose_secret),
            Some("sk-ds")
        );
        assert_eq!(
            reloaded.api_key("relay").map(SecretString::expose_secret),
            Some("relay-secret")
        );
        assert!(reloaded.api_key("keyless").is_none());
        assert!(reloaded.api_key("missing").is_none());

        reloaded.remove_api_key("deepseek");
        reloaded.save().unwrap();
        assert!(Credentials::load().api_key("deepseek").is_none());

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn credentials_ignore_legacy_sections_and_read_providers() {
        // The pre-refactor credentials layout (`[builtins.<id>]` /
        // `[user.<id>]`) is superseded by `[providers.<id>]`. Reading an old
        // file must not fail and must not surface the old sections.
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("credentials.toml"),
            r#"[builtins]
openai = "old-builtin"
[user.my-relay]
api_key = "old-user"
[providers]
deepseek = "new-key"
"#,
        )
        .unwrap();
        let creds = Credentials::load();
        assert!(
            creds.api_key("openai").is_none(),
            "builtins section is gone"
        );
        assert!(creds.api_key("my-relay").is_none(), "user section is gone");
        assert_eq!(
            creds.api_key("deepseek").map(SecretString::expose_secret),
            Some("new-key")
        );

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn catalog_cache_and_remote_round_trip() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        let mut cache = RemoteCatalogCache::default();
        cache.connection_models.insert(
            "deepseek".to_string(),
            vec!["deepseek-v4-flash".to_string()],
        );
        cache.fitted_models.insert("kimi".to_string(), {
            let mut m = std::collections::BTreeMap::new();
            m.insert(
                "kimi-for-coding".to_string(),
                FittedModelInfo {
                    context_window: 262_144,
                    reasoning: true,
                    vision: Some(true),
                    efforts: vec!["max".to_string()],
                },
            );
            m
        });
        cache.model_lists.insert(
            "deepseek".to_string(),
            ModelListCacheState {
                etag: Some("\"models-v1\"".to_string()),
                client_version: "0.1.0".to_string(),
                source_identity: "models-dev:deepseek".to_string(),
                refreshed_at_ms: 1234,
                refresh_failed: false,
            },
        );
        cache.save().unwrap();

        let mut reloaded = RemoteCatalogCache::load();
        assert_eq!(
            reloaded.fitted_models["kimi"]["kimi-for-coding"].context_window,
            262_144
        );
        assert!(reloaded.remote_metadata_for("deepseek", "nope").is_none());
        assert_eq!(
            reloaded.model_lists["deepseek"].etag.as_deref(),
            Some("\"models-v1\"")
        );

        reloaded.remove_connection("deepseek");
        assert!(reloaded.connection_models.is_empty());
        assert!(reloaded.model_lists.is_empty());
        reloaded.save().unwrap();
        assert!(RemoteCatalogCache::load().connection_models.is_empty());

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn config_save_is_behavior_only_and_tolerates_legacy_provider_tables() {
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("config.toml"),
            r#"default_connection = "deepseek"
deepseek_api_key = "legacy-key"
[[providers]]
id = "deepseek"
name = "DeepSeek"
"#,
        )
        .unwrap();
        let loaded = Config::load();
        assert_eq!(loaded.default_connection, "deepseek");
        let mut cfg = loaded;
        cfg.default_connection = "zai".to_string();
        cfg.save().unwrap();
        let on_disk = std::fs::read_to_string(tmp.join("config.toml")).unwrap();
        assert!(on_disk.contains("default_connection = \"zai\""));
        assert!(
            !on_disk.contains("[[providers]]"),
            "legacy provider tables must not be re-emitted"
        );
        assert!(
            !on_disk.contains("legacy-key"),
            "legacy key fields must not be re-emitted"
        );
        // The old credentials layout is untouched by a behavior-only save.
        let creds_text =
            std::fs::read_to_string(tmp.join("credentials.toml")).unwrap_or_else(|_| String::new());
        assert!(creds_text.is_empty() || !creds_text.contains("legacy-key"));

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn save_does_not_resurrect_user_deleted_workspace_roots() {
        // The reported lost update: a session holds a snapshot with
        // `additional_roots = ["../x"]`, the user deletes the entry from
        // `config.toml` while the session runs, then any save must not write
        // the deleted value back. Disk is the newer truth for user-owned
        // tables.
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("config.toml"),
            "default_connection = \"deepseek\"\n\
             [workspace]\n\
             additional_roots = [\"../x\", \"../y\"]\n",
        )
        .unwrap();

        // Simulate the session's long-lived snapshot: loaded at startup.
        let snapshot = Config::load();
        assert_eq!(
            snapshot.workspace.additional_roots,
            vec!["../x", "../y"],
            "precondition: snapshot saw the roots"
        );

        // The user hand-edits the file while the session is live.
        std::fs::write(
            tmp.join("config.toml"),
            "default_connection = \"deepseek\"\n",
        )
        .unwrap();

        // Any save (e.g. a `/models` switch) flushes the stale snapshot —
        // the deletion must survive.
        snapshot.save().unwrap();
        let after = Config::load();
        assert!(
            after.workspace.additional_roots.is_empty(),
            "user-deleted additional_roots must stay deleted, got {:?}",
            after.workspace.additional_roots
        );
        // …and the runtime-owned field still flushes.
        assert_eq!(after.default_connection, "deepseek");

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn save_preserves_user_edited_unrelated_tables() {
        // Broader lost-update case: the user changes any user-owned table
        // (`[agent]` here) while the daemon runs; a snapshot-driven save must
        // not roll it back to the loaded-at-startup value.
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(tmp.join("config.toml"), "default_model = \"m1\"\n").unwrap();
        let snapshot = Config::load();

        std::fs::write(
            tmp.join("config.toml"),
            "default_model = \"m1\"\n[agent]\nhard_stop_turns = 42\n",
        )
        .unwrap();

        snapshot.save_preserving_connection_selection().unwrap();
        let after = Config::load();
        assert_eq!(after.agent.hard_stop_turns, 42, "user edit must survive");
        // Preserved selection still comes from disk.
        assert_eq!(after.default_model.as_deref(), Some("m1"));

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn save_still_flushes_runtime_changed_selection_and_leaves_roots_to_disk() {
        // The flip side of disk-wins: fields the runtime deliberately changed
        // (selection) must reach disk — and the persisted
        // `[workspace].additional_roots` stays purely user-declared: a
        // project-trust merge in the snapshot must NOT leak into the global
        // file.
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("config.toml"),
            "default_connection = \"old\"\n\
             [workspace]\n\
             additional_roots = [\"../user-root\"]\n",
        )
        .unwrap();

        let mut snapshot = Config::load();
        snapshot.default_connection = "new".to_string();
        snapshot.default_model = Some("m2".to_string());
        // Runtime merges a trusted project root on top of the loaded config.
        snapshot.merge_project_additional_roots(vec!["../project-root".to_string()]);
        assert_eq!(
            snapshot.workspace.additional_roots,
            vec!["../user-root", "../project-root"]
        );

        snapshot.save().unwrap();
        let after = Config::load();
        assert_eq!(after.default_connection, "new");
        assert_eq!(after.default_model.as_deref(), Some("m2"));
        assert_eq!(
            after.workspace.additional_roots,
            vec!["../user-root"],
            "project-merged roots are runtime view state, never persisted"
        );

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn save_preserving_connection_selection_keeps_disk_default() {
        // The existing preserve semantics must keep working under the
        // merge-save rewrite: a snapshot carrying a resumed session's pin does
        // not overwrite the on-disk default.
        let (tmp, _guard, _override_guard) = sandbox_config_dir();
        std::fs::write(
            tmp.join("config.toml"),
            "default_connection = \"disk-default\"\n\
             default_model = \"disk-model\"\n",
        )
        .unwrap();

        let mut snapshot = Config::load();
        snapshot.default_connection = "session-pin".to_string();
        snapshot.default_model = Some("session-model".to_string());
        snapshot.save_preserving_connection_selection().unwrap();

        let after = Config::load();
        assert_eq!(after.default_connection, "disk-default");
        assert_eq!(after.default_model.as_deref(), Some("disk-model"));

        paths::set_test_default(None);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn route_settings_presence_opts_in_and_is_empty_semantics() {
        // ADR-0046: entry presence opts in; a bare entry (no knobs) still
        // counts as configured, while an entry that carries no fields after
        // mutation reports empty so callers can prune it.
        let empty = RouteSettings::default();
        assert!(empty.is_empty());
        let bare = RouteSettings {
            effort: None,
            thinking: None,
            capability_overrides: None,
            prompt_cache: None,
        };
        assert!(bare.is_empty());
        let with_effort = RouteSettings {
            effort: Some("high".to_string()),
            thinking: None,
            capability_overrides: None,
            prompt_cache: None,
        };
        assert!(!with_effort.is_empty());
        let with_thinking = RouteSettings {
            effort: None,
            thinking: Some(false),
            capability_overrides: None,
            prompt_cache: None,
        };
        assert!(!with_thinking.is_empty());
    }

    // project-scope MCP merge (ADR-0085 §2/§3)

    struct ScratchProject(tempfile::TempDir);

    impl std::ops::Deref for ScratchProject {
        type Target = std::path::Path;

        fn deref(&self) -> &Self::Target {
            self.0.path()
        }
    }

    fn scratch_project_root() -> ScratchProject {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".nuo")).unwrap();
        ScratchProject(dir)
    }

    #[test]
    fn resolve_workspace_additional_roots_empty_when_table_absent() {
        let root = scratch_project_root();
        assert!(
            Config::default()
                .resolve_workspace_additional_roots(&root)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn resolve_workspace_additional_roots_resolves_relative_and_absolute_entries() {
        let root = scratch_project_root();
        let sibling =
            std::env::temp_dir().join(format!("muta-additional-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&sibling).unwrap();
        let mut config = Config::default();
        config.workspace.additional_roots = vec![
            format!("../{}", sibling.file_name().unwrap().to_string_lossy()),
            sibling.canonicalize().unwrap().display().to_string(),
        ];
        let roots = config.resolve_workspace_additional_roots(&root).unwrap();
        // Both spellings resolve to the same canonical sibling directory.
        assert_eq!(roots, vec![sibling.canonicalize().unwrap()]);
    }

    #[test]
    fn resolve_workspace_additional_roots_skips_missing_and_nested_entries() {
        let root = scratch_project_root();
        let sibling =
            std::env::temp_dir().join(format!("muta-additional-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::create_dir_all(root.join("nested")).unwrap();

        let mut config = Config::default();
        config.workspace.additional_roots = vec![
            "../does-not-exist-anywhere".to_string(),
            "nested".to_string(),
            root.canonicalize().unwrap().display().to_string(),
            sibling.display().to_string(),
        ];
        let detailed = config
            .resolve_workspace_additional_roots_detailed(&root)
            .unwrap();
        assert_eq!(detailed.admitted, vec![sibling.canonicalize().unwrap()]);
        assert_eq!(detailed.skipped.len(), 3);
        assert!(detailed.skipped[0].1.contains("does not exist"));
        assert!(detailed.skipped[1].1.contains("inside workspace"));
        assert!(detailed.skipped[2].1.contains("workspace root itself"));
    }

    #[test]
    fn project_config_cannot_widen_workspace_roots() {
        let root = scratch_project_root();
        let sibling =
            std::env::temp_dir().join(format!("muta-additional-root-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(
            root.join(".nuo/config.toml"),
            format!(
                r#"
                    [workspace]
                    additional_roots = ["{}"]
                "#,
                sibling.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        let roots = Config::default()
            .resolve_workspace_additional_roots(&root)
            .unwrap();
        assert!(roots.is_empty());
    }

    #[test]
    fn load_project_mcp_reads_muta_config_table() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".nuo/config.toml"),
            r#"
                [mcp.project-db]
                command = ["./bin/db-mcp"]
                enabled = true
            "#,
        )
        .unwrap();

        let mcp = Config::load_project_mcp(&root);
        assert_eq!(mcp.len(), 1);
        assert_eq!(mcp["project-db"].command, vec!["./bin/db-mcp".to_string()]);
        assert!(mcp["project-db"].enabled);
        assert_eq!(
            mcp["project-db"].sandbox_root.as_deref(),
            Some(root.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn load_project_mcp_reads_json_and_json_overrides_toml() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".nuo/config.toml"),
            "[mcp.shared]\ncommand = [\"toml-server\"]\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".nuo/mcp.json"),
            r#"{
                "mcpServers": {
                    "shared": {
                        "command": "json-server",
                        "args": ["--stdio"],
                        "env": {"MODE": "project"},
                        "read_only": true
                    }
                }
            }"#,
        )
        .unwrap();

        let mcp = Config::load_project_mcp(&root);
        assert_eq!(
            mcp["shared"].command,
            vec!["json-server".to_string(), "--stdio".to_string()]
        );
        assert_eq!(mcp["shared"].environment["MODE"], "project");
        assert!(mcp["shared"].enabled);
        assert!(mcp["shared"].read_only);
        assert_eq!(
            mcp["shared"].sandbox_root.as_deref(),
            Some(root.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn load_project_mcp_is_empty_when_file_absent() {
        let root = scratch_project_root();
        // No config.toml written.
        let mcp = Config::load_project_mcp(&root);
        assert!(mcp.is_empty());
    }

    #[test]
    fn load_project_mcp_ignores_unrelated_keys_and_bad_toml() {
        let root = scratch_project_root();
        // A project file may carry non-mcp tables; only [mcp.*] is projected,
        // and an invalid [mcp.*] makes the whole table drop (warn + empty).
        std::fs::write(
            root.join(".nuo/config.toml"),
            r#"
                [agent]
                hard_stop_turns = 7

                [mcp.ok]
                command = ["x"]
            "#,
        )
        .unwrap();
        let mcp = Config::load_project_mcp(&root);
        assert_eq!(mcp.len(), 1, "agent ignored, mcp.ok projected");

        // A structurally invalid TOML → empty (never panics).
        let root2 = scratch_project_root();
        std::fs::write(root2.join(".nuo/config.toml"), "this is = = not toml").unwrap();
        assert!(Config::load_project_mcp(&root2).is_empty());
    }

    #[test]
    fn merge_project_mcp_overrides_same_name_adds_new() {
        let mut global = Config::default();
        global.mcp.insert(
            "shared".to_string(),
            McpServerConfig {
                command: vec!["global-cmd".into()],
                enabled: true,
                ..McpServerConfig::default()
            },
        );
        global
            .mcp
            .insert("only-global".to_string(), McpServerConfig::default());

        let mut project = HashMap::new();
        // Same name → wholesale override (new command).
        project.insert(
            "shared".to_string(),
            McpServerConfig {
                command: vec!["project-cmd".into()],
                enabled: false,
                ..McpServerConfig::default()
            },
        );
        // New name → added.
        project.insert("only-project".to_string(), McpServerConfig::default());

        global.merge_project_mcp(project);

        // Override took effect.
        assert_eq!(
            global.mcp["shared"].command,
            vec!["project-cmd".to_string()]
        );
        assert!(!global.mcp["shared"].enabled);
        // Both pre-existing and added survive.
        assert!(global.mcp.contains_key("only-global"));
        assert!(global.mcp.contains_key("only-project"));
        assert_eq!(global.mcp.len(), 3);
    }

    #[test]
    fn load_project_hooks_reads_hooks_array() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".nuo/config.toml"),
            r#"
                [[hooks]]
                event   = "PostToolUse"
                matcher = "Write|Edit"
                command = ".nuo/hooks/lint.sh"

                [[hooks]]
                event   = "Stop"
                command = ".nuo/hooks/notify.sh"
            "#,
        )
        .unwrap();

        let hooks = Config::load_project_hooks(&root);
        let canonical = root.0.path().canonicalize().unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].event, HookEventKind::PostToolUse);
        assert_eq!(hooks[0].command, ".nuo/hooks/lint.sh");
        assert_eq!(hooks[1].event, HookEventKind::Stop);
        assert!(
            hooks
                .iter()
                .all(|hook| hook.sandbox_root.as_deref() == Some(canonical.as_path()))
        );
    }

    #[test]
    fn load_project_hooks_is_empty_when_file_absent() {
        let root = scratch_project_root();
        // No config.toml written.
        assert!(Config::load_project_hooks(&root).is_empty());
    }

    #[test]
    fn load_project_hooks_ignores_unrelated_keys_and_bad_toml() {
        let root = scratch_project_root();
        // A project file may carry non-hooks tables; only [[hooks]] projects.
        std::fs::write(
            root.join(".nuo/config.toml"),
            r#"
                [mcp.something]
                command = ["x"]

                [[hooks]]
                event = "Stop"
                command = "echo done"
            "#,
        )
        .unwrap();
        let hooks = Config::load_project_hooks(&root);
        assert_eq!(hooks.len(), 1, "mcp ignored, one hook projected");

        // Structurally invalid TOML → empty (never panics).
        let root2 = scratch_project_root();
        std::fs::write(root2.join(".nuo/config.toml"), "this is = = not toml").unwrap();
        assert!(Config::load_project_hooks(&root2).is_empty());
    }

    #[test]
    fn merge_project_hooks_appends_to_global() {
        let mut global = Config::default();
        global.hooks.push(HookSpec {
            event: HookEventKind::Stop,
            matcher: None,
            command: "global-notify.sh".to_string(),
            sandbox_root: None,
        });
        let project_hooks = vec![HookSpec {
            event: HookEventKind::PostToolUse,
            matcher: Some("Write".to_string()),
            command: ".nuo/hooks/lint.sh".to_string(),
            sandbox_root: Some(std::path::PathBuf::from("/project")),
        }];
        global.merge_project_hooks(project_hooks);
        assert_eq!(global.hooks.len(), 2, "global + project appended");
        // Global hook ordering preserved; project hooks come after.
        assert_eq!(global.hooks[0].command, "global-notify.sh");
        assert_eq!(global.hooks[1].command, ".nuo/hooks/lint.sh");
    }

    #[test]
    fn load_project_additional_roots_reads_workspace_table() {
        let root = scratch_project_root();
        std::fs::write(
            root.join(".nuo/config.toml"),
            r#"
                [workspace]
                additional_roots = ["../optics", "~/shared/design"]
            "#,
        )
        .unwrap();

        let roots = Config::load_project_additional_roots(&root);
        assert_eq!(roots, vec!["../optics", "~/shared/design"]);
    }

    #[test]
    fn load_project_additional_roots_empty_when_absent() {
        let root = scratch_project_root();
        assert!(Config::load_project_additional_roots(&root).is_empty());
    }

    #[test]
    fn merge_project_additional_roots_deduplicates_and_appends() {
        let mut global = Config::default();
        global.workspace.additional_roots = vec!["../optics".to_string()];
        global.merge_project_additional_roots(vec![
            "../optics".to_string(),
            "../backend".to_string(),
        ]);
        assert_eq!(
            global.workspace.additional_roots,
            vec!["../optics", "../backend"]
        );
    }

    fn catalog_cache_with(connection: &str) -> RemoteCatalogCache {
        let mut cache = RemoteCatalogCache::default();
        cache
            .connection_models
            .insert(connection.into(), vec!["qfmodel".into()]);
        cache.fitted_models.insert(
            connection.into(),
            [(
                "qfmodel".to_string(),
                FittedModelInfo {
                    context_window: 200_000,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        cache.remote_metadata.insert(
            connection.into(),
            [(
                "qfmodel".to_string(),
                RemoteModelMetadata {
                    context_window: Some(200_000),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        cache.model_lists.insert(
            connection.into(),
            ModelListCacheState {
                etag: Some("W/\"catalog-etag\"".into()),
                source_identity: "sha256:deadbeef".into(),
                client_version: "1.1.58".into(),
                refreshed_at_ms: 1_700_000_000_000,
                refresh_failed: false,
            },
        );
        cache
    }

    /// A rename re-keys all four maps, so the renamed connection keeps its
    /// cached models, fitted metadata, advertised metadata, and — critically —
    /// its ETag validator. Nothing is left under the dead name.
    #[test]
    fn rename_connection_carries_every_map_including_the_validator() {
        let mut cache = catalog_cache_with("old");
        cache.rename_connection("old", "new");

        assert!(
            !cache.connection_models.contains_key("old"),
            "no entry may be stranded under the old name"
        );
        assert_eq!(cache.connection_models["new"], ["qfmodel".to_string()]);
        assert_eq!(
            cache.fitted_models["new"]["qfmodel"].context_window,
            200_000
        );
        assert_eq!(
            cache
                .remote_metadata_for("new", "qfmodel")
                .and_then(|m| m.context_window),
            Some(200_000)
        );
        assert_eq!(
            cache.model_lists["new"].etag.as_deref(),
            Some("W/\"catalog-etag\""),
            "the validator must survive so the next sync revalidates, not refetches"
        );
        assert_eq!(cache.model_lists["new"].source_identity, "sha256:deadbeef");
    }

    /// A case-only rename is the same connection and must not move anything:
    /// the key is case-insensitive for identity (ADR-0201) but the stored key
    /// is exact, so a case flip that re-keyed would drop the entry.
    #[test]
    fn case_only_rename_leaves_the_entry_in_place() {
        let mut cache = catalog_cache_with("qod");
        cache.rename_connection("qod", "QOD");
        assert!(cache.model_lists.contains_key("qod"), "unchanged");
        assert!(!cache.model_lists.contains_key("QOD"));
    }

    /// Renaming a connection with no cached entry is a no-op that must not
    /// create one — an empty entry would make `route_models` treat the
    /// connection as "catalog answered with nothing" rather than "never
    /// fetched", which changes the fallback behaviour.
    #[test]
    fn rename_connection_without_an_entry_creates_nothing() {
        let mut cache = RemoteCatalogCache::default();
        cache.rename_connection("ghost", "renamed");
        assert!(cache.connection_models.is_empty());
        assert!(cache.model_lists.is_empty());
        assert!(cache.remote_metadata.is_empty());
        assert!(cache.fitted_models.is_empty());
    }
}
