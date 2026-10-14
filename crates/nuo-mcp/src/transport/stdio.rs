use crate::error::{McpError, Result};
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type PendingRequests = Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>;

/// Asynchronous Stdio process transport for MCP.
///
/// Communicates with an external MCP server over newline-delimited JSON-RPC 2.0.
pub struct StdioTransport {
    _child: Arc<Mutex<Child>>,
    stdin_tx: mpsc::Sender<String>,
    pending: PendingRequests,
    next_id: AtomicU64,
    cancel_token: CancellationToken,
}

impl StdioTransport {
    /// Spawns an MCP server subprocess and connects standard I/O pipes.
    pub async fn spawn(
        program: &str,
        args: &[&str],
        working_dir: Option<PathBuf>,
        env_vars: Option<HashMap<String, String>>,
    ) -> Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        if let Some(envs) = env_vars {
            cmd.envs(envs);
        }

        Self::from_command(cmd)
    }

    /// Spawns an MCP server from a pre-configured `tokio::process::Command`.
    pub fn from_command(mut cmd: Command) -> Result<Self> {
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|err| {
            McpError::process(format!("failed to spawn MCP process: {err}"))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::transport("failed to capture child stdin pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::transport("failed to capture child stdout pipe"))?;

        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let cancel_token = CancellationToken::new();

        // Channel for writing to child's stdin
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(128);

        // Stdin pump worker
        tokio::spawn(async move {
            let mut writer = stdin;
            while let Some(line) = stdin_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if writer.flush().await.is_err() {
                    break;
                }
            }
        });

        // Stdout reader worker
        let pending_clone = pending.clone();
        let cancel_clone = cancel_token.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    line_res = reader.next_line() => {
                        match line_res {
                            Ok(Some(line)) => {
                                let line = line.trim();
                                if line.is_empty() {
                                    continue;
                                }
                                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(line)
                                    && let Some(id) = resp.id
                                {
                                    let mut map = pending_clone.lock().await;
                                    if let Some(tx) = map.remove(&id) {
                                        let _ = tx.send(resp);
                                    }
                                }
                            }
                            _ => break,
                        }
                    }
                }
            }
        });

        Ok(Self {
            _child: Arc::new(Mutex::new(child)),
            stdin_tx,
            pending,
            next_id: AtomicU64::new(1),
            cancel_token,
        })
    }

    /// Sends a JSON-RPC request and awaits the matched response.
    pub async fn send_request(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<JsonRpcResponse> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest::new(id, method, params);
        let mut json_str =
            serde_json::to_string(&req).map_err(|e| McpError::serialization(e.to_string()))?;
        json_str.push('\n');

        let (resp_tx, resp_rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, resp_tx);
        }

        if self.stdin_tx.send(json_str).await.is_err() {
            let mut map = self.pending.lock().await;
            map.remove(&id);
            return Err(McpError::Closed);
        }

        resp_rx.await.map_err(|_| McpError::Closed)
    }

    /// Sends a fire-and-forget JSON-RPC notification.
    pub async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notif = JsonRpcNotification::new(method, params);
        let mut json_str =
            serde_json::to_string(&notif).map_err(|e| McpError::serialization(e.to_string()))?;
        json_str.push('\n');

        self.stdin_tx
            .send(json_str)
            .await
            .map_err(|_| McpError::Closed)
    }

    /// Terminates the transport pump.
    pub fn close(&self) {
        self.cancel_token.cancel();
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.close();
    }
}
