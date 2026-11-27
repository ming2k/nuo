//! Isolated `NUO_HOME` and workspace fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

use crate::Result;
use crate::eventually;

/// The subset of `server.json` E2E tests assert on.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DiscoveryRecord {
    pub pid: u32,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub token: Option<String>,
}

/// A private instance root plus a project workspace.
///
/// Layout:
///
/// ```text
/// <root>/
///   home/nuo/{config,state,data,cache,instance}
///   workspace/
/// ```
///
/// `command` exports `NUO_HOME` and removes any ambient per-category
/// `NUO_*_DIR` overrides, so a developer's shell configuration cannot leak
/// real state into a test.
pub struct Sandbox {
    root: TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

impl Sandbox {
    /// Create a fresh sandbox.
    pub fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        for dir in [
            home.join("nuo/config"),
            home.join("nuo/state"),
            home.join("nuo/data"),
            home.join("nuo/cache"),
            home.join("nuo/instance"),
        ] {
            fs::create_dir_all(&dir)?;
        }
        fs::create_dir_all(&workspace)?;
        Ok(Self {
            root,
            home,
            workspace,
        })
    }

    /// Sandbox root.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// Instance root exported as `NUO_HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The project directory the CLI runs in.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Resolved `<NUO_HOME>/nuo/config` directory.
    pub fn config_dir(&self) -> PathBuf {
        self.home.join("nuo/config")
    }

    /// Resolved `<NUO_HOME>/nuo/state` directory.
    pub fn state_dir(&self) -> PathBuf {
        self.home.join("nuo/state")
    }

    /// Resolved `<NUO_HOME>/nuo/data` directory.
    pub fn data_dir(&self) -> PathBuf {
        self.home.join("nuo/data")
    }

    /// Server discovery record.
    pub fn discovery_path(&self) -> PathBuf {
        self.home.join("nuo/instance/server.json")
    }

    /// Build a `nuo` command with the sandbox environment and workspace cwd.
    pub fn command(&self, bin: &Path) -> Command {
        let mut command = Command::new(bin);
        command
            .env("NUO_HOME", &self.home)
            // These take precedence over NUO_HOME and are interpreted as
            // `nuo`-suffixed roots, not category directories. Remove them
            // rather than trying to translate an ambient value.
            .env_remove("NUO_CONFIG_DIR")
            .env_remove("NUO_STATE_DIR")
            .env_remove("NUO_DATA_DIR")
            .env_remove("NUO_CACHE_DIR")
            .env("HOME", self.root.path())
            .env("NUO_PORT", "0")
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .current_dir(&self.workspace)
            .stdin(Stdio::null());
        command
    }

    /// Write a file under the sandbox, creating parent directories.
    pub fn write_file(&self, path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
        Ok(())
    }

    /// Configure one user-declared OpenAI-compatible provider and make it the
    /// default connection.
    ///
    /// Writes the four files a `--headless` turn needs:
    /// `model_providers.toml`, `connections.toml`, `credentials.toml`, and
    /// `client.toml`. The provider's catalog is static (`source = "none"`), so
    /// no live `/models` request is made.
    pub fn configure_openai_mock(
        &self,
        provider: &str,
        connection: &str,
        model: &str,
        base_url: &str,
    ) -> Result<()> {
        atom(provider, "provider id")?;
        atom(connection, "connection name")?;
        string_value(model, "model id")?;
        string_value(base_url, "base url")?;

        let api_root = format!("{}/v1", base_url.trim_end_matches('/'));
        let providers = format!(
            "version = 1\n\
             \n\
             [providers.{provider}]\n\
             root_url = \"{api_root}\"\n\
             default_protocol = \"openai-chat-completions\"\n\
             catalog = {{ source = \"none\" }}\n\
             \n\
             [[model_providers.{provider}.inject]]\n\
             id = \"{model}\"\n"
        );
        self.write_file(&self.config_dir().join("model_providers.toml"), providers)?;

        let connections = format!(
            "[[connections]]\n\
             name = \"{connection}\"\n\
             provider = \"{provider}\"\n"
        );
        self.write_file(&self.state_dir().join("connections.toml"), connections)?;

        let credentials = format!("[connections]\n{connection} = \"test-key\"\n");
        self.write_file(&self.state_dir().join("credentials.toml"), credentials)?;

        let client = format!(
            "[connection]\n\
             default_connection = \"{connection}\"\n\
             default_model = \"{model}\"\n"
        );
        self.write_file(&self.config_dir().join("client.toml"), client)?;
        Ok(())
    }

    /// Read the discovery record if it is complete and parseable.
    pub fn discovery(&self) -> Option<DiscoveryRecord> {
        let bytes = fs::read(self.discovery_path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Wait for the server to publish a discovery record.
    pub fn wait_for_discovery(&self, timeout: Duration) -> Result<DiscoveryRecord> {
        eventually(timeout, || self.discovery())
    }

    /// Start `nuo server start --fg` in this sandbox.
    ///
    /// The returned guard kills the server on drop. Callers that need the
    /// production lifecycle should use `nuo server stop`; the guard is the
    /// panic-safety net.
    pub fn spawn_server_fg(&self, bin: &Path) -> Result<crate::ChildGuard> {
        let mut command = self.command(bin);
        command
            .args(["server", "start", "--fg"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Ok(crate::ChildGuard::new(command.spawn()?))
    }

    /// Stop a server started in this sandbox, if one is running.
    pub fn stop_server(&self, bin: &Path) -> Result<()> {
        let mut command = self.command(bin);
        command
            .args(["server", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let status = command.status()?;
        if status.success() || !self.discovery_path().exists() {
            return Ok(());
        }
        Err(format!("`nuo server stop` failed with {status}").into())
    }

    /// A drop guard that best-effort stops the server in this sandbox.
    pub fn server_cleanup(&self, bin: &Path) -> ServerCleanup {
        ServerCleanup {
            bin: bin.to_path_buf(),
            home: self.home.clone(),
            workspace: self.workspace.clone(),
            discovery: self.discovery_path(),
        }
    }
}

/// Stops a sandboxed server on drop.
pub struct ServerCleanup {
    bin: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    discovery: PathBuf,
}

impl ServerCleanup {
    /// Run the best-effort stop now.
    pub fn run(&self) {
        let output = Command::new(&self.bin)
            .args(["server", "stop"])
            .env("NUO_HOME", &self.home)
            .env_remove("NUO_CONFIG_DIR")
            .env_remove("NUO_STATE_DIR")
            .env_remove("NUO_DATA_DIR")
            .env_remove("NUO_CACHE_DIR")
            .env("HOME", self.workspace.parent().unwrap_or(&self.workspace))
            .env("NUO_PORT", "0")
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();

        let stopped = output.is_ok_and(|output| output.status.success());
        if stopped && !self.discovery.exists() {
            return;
        }
        #[cfg(unix)]
        if let Ok(bytes) = fs::read(&self.discovery)
            && let Ok(record) = serde_json::from_slice::<DiscoveryRecord>(&bytes)
        {
            // SAFETY: the PID came from this sandbox's discovery record; a
            // failed `server stop` is the only path that reaches this kill.
            unsafe {
                libc::kill(record.pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

impl Drop for ServerCleanup {
    fn drop(&mut self) {
        self.run();
    }
}

fn atom(value: &str, field: &str) -> Result<()> {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        Ok(())
    } else {
        Err(
            format!("{field} `{value}` must contain only ASCII letters, digits, `-`, or `_`")
                .into(),
        )
    }
}

fn string_value(value: &str, field: &str) -> Result<()> {
    if value.is_empty() || value.chars().any(|c| matches!(c, '"' | '\\' | '\n' | '\r')) {
        Err(format!("{field} `{value}` cannot be represented in TOML").into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_exports_an_isolated_instance_root() {
        let sandbox = Sandbox::new().unwrap();
        assert!(sandbox.config_dir().is_dir());
        assert!(sandbox.workspace().is_dir());

        let mut command = sandbox.command(Path::new("/bin/true"));
        let environment: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("NUO_HOME"))
                .copied()
                .flatten(),
            Some(sandbox.home().as_os_str())
        );
        let _ = command.args(["--version"]);
    }

    #[test]
    fn openai_mock_configuration_writes_a_complete_default_route() {
        let sandbox = Sandbox::new().unwrap();
        sandbox
            .configure_openai_mock("mock-provider", "mock", "mock-1", "http://127.0.0.1:1")
            .unwrap();

        let providers =
            fs::read_to_string(sandbox.config_dir().join("model_providers.toml")).unwrap();
        assert!(providers.contains("[providers.mock-provider]"));
        assert!(providers.contains("root_url = \"http://127.0.0.1:1/v1\""));
        assert!(providers.contains("[[model_providers.mock-provider.inject]]"));

        let connections = fs::read_to_string(sandbox.state_dir().join("connections.toml")).unwrap();
        assert!(connections.contains("provider = \"mock-provider\""));

        let client = fs::read_to_string(sandbox.config_dir().join("client.toml")).unwrap();
        assert!(client.contains("default_connection = \"mock\""));
        assert!(client.contains("default_model = \"mock-1\""));
    }

    #[test]
    fn invalid_toml_tokens_are_rejected() {
        let sandbox = Sandbox::new().unwrap();
        assert!(
            sandbox
                .configure_openai_mock("bad\nprovider", "mock", "m", "http://127.0.0.1:1")
                .is_err()
        );
        assert!(
            sandbox
                .configure_openai_mock("mock", "mock", "bad\nmodel", "http://127.0.0.1:1")
                .is_err()
        );
    }
}
