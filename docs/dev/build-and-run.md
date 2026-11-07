# Build & Run Guide

This guide describes how to build, run, and verify **Nuo** directly from source.

Nuo requires no synthetic harnesses, test runners, or multi-step ceremony for manual testing: **compile the unified binary with `cargo`, inject your provider environment variables, and run.**

---

## 1. Quick Start: Build & Run

### Interactive Terminal (TUI) Mode

```bash
# Pass your provider key and launch the interactive terminal
DEEPSEEK_API_KEY="sk-..." cargo run -p nuo
```

> **First Run Note (Connection Setup)**:
> Nuo is connection-oriented ([ADR-0087](../adr/0087-model-wire-aperture-purification-and-provider-connection-orthogonality.md)). When launching inside a clean sandbox for the first time:
> 1. Type `/connections` in the prompt composer to open the connection manager.
> 2. Pick your provider (e.g. `deepseek`) and press `Enter` to activate it.
> 3. Your `DEEPSEEK_API_KEY` from the environment is automatically bound, and the connection persists in `$NUO_HOME/nuo/state/connections.toml`.
> 4. After this initial one-time selection, subsequent runs immediately resume dialogue.

### Headless (One-Shot Prompt) Mode

Once a provider connection exists (or if pre-seeding `connections.toml` in your sandbox state directory):

```bash
# Execute non-interactively and stream to stdout
DEEPSEEK_API_KEY="sk-..." cargo run -p nuo -- --headless -p "Explain the workspace structure in 2 sentences"
```

To allow tools to execute without interactive keyboard confirmation:

```bash
cargo run -p nuo -- --headless --unattended -p "Read Cargo.toml and print the workspace version"
```

---

## 2. Environment Variables Reference

### Provider API Keys

Nuo reads credentials directly from environment variables. Export any of the following:

| Provider | Environment Variable | Notes |
| :--- | :--- | :--- |
| **DeepSeek** | `DEEPSEEK_API_KEY` | Official DeepSeek Chat & Reasoner |
| **OpenAI** | `OPENAI_API_KEY` | OpenAI GPT-4o / o1 / o3 models |
| **Anthropic** | `ANTHROPIC_API_KEY` | Claude 3.5 Sonnet / Haiku / Opus |
| **OpenRouter** | `OPENROUTER_API_KEY` | Aggregated multi-provider router |
| **Qianwen (Aliyun)** | `QIANWEN_API_KEY` | Qwen Turbo / Plus / Max models |
| **Kimi (Moonshot)** | `MOONSHOT_API_KEY` | Moonshot / Kimi models |
| **xAI** | `XAI_API_KEY` | Grok models |
| **CodeBuddy** | `CODEBUDDY_API_KEY` | Tencent CodeBuddy relay |
| **OpenCode** | `OPENCODE_API_KEY` | OpenCode relay models |

### Development & Sandbox Overrides

By default, debug builds (`cargo run` without `--release`) automatically isolate state to `target/nuo-dev`, preventing manual tests from polluting your host system (`~/.config/nuo` or production databases).

| Variable | Default (Debug) | Purpose |
| :--- | :--- | :--- |
| `NUO_DEV` | `1` (on debug) | Enables automatic sandbox isolation under `target/nuo-dev`. Set `NUO_NO_DEV=1` to disable. |
| `NUO_HOME` | `target/nuo-dev` | Explicitly override the instance root (holds db, logs, and sockets). E.g. `NUO_HOME=$(mktemp -d)`. |
| `NUO_PORT` | `9800` | Port for standalone server mode. Useful if port 9800 is already in use. |
| `NUO_LOG` | `info` | Log level for tracing output (`error`, `warn`, `info`, `debug`, `trace`, or `off`). |
| `NUO_BIN` | Sibling binary | Path to `nuo` executable if overriding client-server binary discovery. |

---

## 3. Manual Testing Workflows

### Workflow A: Interactive Terminal Interface (TUI)

Launch the interface:

```bash
cargo run -p nuo
```

**Key verification points:**
1. **Cold Start & Layout**: Alternate screen activates cleanly, status bar shows model and thread status, composer is focused.
2. **Conversation & Streaming**: Type a prompt, press `Enter`. Verify tokens stream smoothly, thinking/reasoning blocks fold/unfold, and markdown renders with correct ANSI colors.
3. **Tool Approval Barrier**: Request a filesystem or command operation (e.g. "List files in the current folder"). Verify an approval card appears and does **not** execute until explicitly approved (`Enter` on `[Approve Once]`).
4. **In-Session Slash Commands**:
   - `/help`: Print built-in commands.
   - `/models` (or `Ctrl+M`): Open the interactive model selector.
   - `/connections`: View and verify configured provider lanes.
   - `/exit` (or `Ctrl+C` twice): Restore standard terminal cleanly, unhide cursor, exit 0.
5. **Terminal Resizing**: Resize terminal window while streaming. Layout should dynamically reflow without panics or border clipping.

### Workflow B: Headless Execution & Pipelines

Test scriptable, non-interactive execution:

```bash
# Single prompt
cargo run -p nuo -- --headless -p "Say hello"

# Stdin pipeline
echo "fn add(a: i32, b: i32) -> i32 { a + b }" | cargo run -p nuo -- --headless -p "Add rustdoc"

# Unattended execution (tools auto-approved)
cargo run -p nuo -- --headless --unattended -p "Check git status of this repository"
```

### Workflow C: Server Daemon & Lifecycle Verification

Test standalone server lifecycle and diagnostics:

```bash
# 1. Start server in foreground (view live daemon logs)
cargo run -p nuo -- server start --fg --port 9820

# 2. In another terminal, inspect health and diagnostics
cargo run -p nuo -- status
cargo run -p nuo -- status --diagnostic

# 3. Retrieve bearer token
cargo run -p nuo -- server token

# 4. Attach client to the running server
cargo run -p nuo

# 5. Stop detached or background server
cargo run -p nuo -- server stop
```

### Workflow D: Thread Management & Resumption

Test persistence and session reconnects:

```bash
# List stored threads
cargo run -p nuo -- thread ls

# Resume the most recent thread
cargo run -p nuo -- --resume

# Attach to a specific thread
cargo run -p nuo -- -t <thread_id>

# Delete a thread
cargo run -p nuo -- thread rm <thread_id>
```

---

## 4. Completely Clean Sandbox Testing

If you want a 100% sterile manual test run that touches zero existing state:

```bash
# One-liner disposable sandbox for interactive testing
NUO_HOME=$(mktemp -d) DEEPSEEK_API_KEY="sk-..." cargo run -p nuo
```

To run a headless test in a completely sterile sandbox from scratch, pre-seed a minimal connection:

```bash
SANDBOX=$(mktemp -d)
mkdir -p "$SANDBOX/state"
cat << 'EOF' > "$SANDBOX/state/connections.toml"
[[connections]]
name = "deepseek"
provider = "deepseek"
EOF

NUO_HOME="$SANDBOX" DEEPSEEK_API_KEY="sk-..." cargo run -p nuo -- --headless -p "Say hello"
rm -rf "$SANDBOX"
```
