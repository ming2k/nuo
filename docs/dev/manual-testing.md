# Manual Testing & Full-Stack Verification Guide

This document defines the comprehensive manual testing procedures, exploratory testing playbooks, and release walk-through runbooks for **Nuo** — the unified AI session daemon, control plane, and semantic terminal client.

It ensures that developers, QA engineers, and release gatekeepers can verify end-to-end functionality, user experience ergonomics, interactive approval workflows, and fault recovery from a clean cold start without synthetic test harnesses or internal backdoor bypasses.

---

## 1. Principles & Testing Philosophy

Manual verification complements automated testing (`cargo test --workspace`, snapshot tests) by focusing on cognitive and perceptual dimensions that machines cannot evaluate:

1. **`[INV-VAL-01]` Real User Journey Parity**:
   All manual verifications must be performed through the public unified CLI interface (`nuo`) and standard HTTP/IPC endpoints from a cold start. Using private test harness fixtures or mock overrides is strictly prohibited during manual sign-off.
2. **`[INV-CLI-01]` / `[INV-CLI-02]` Unified Binary Singleton**:
   The workspace produces a single public executable: `nuo`. Interactive terminal sessions, headless scripts, and server daemons are all accessed through this single binary entrypoint.
3. **Ergonomics & Visual Polish**:
   Validate terminal layout responsiveness, border integrity, CJK wide-character alignment, ANSI color fidelity, breathing animations, and cursor state restoration powered by `nuo-tui` and `nuotc`.
4. **Interactive Safety & Approval Barriers**:
   Verify that high-hazard operations (destructive bash commands, file modifications) pause cleanly for explicit human confirmation and never execute prematurely.
5. **Host Isolation Invariant**:
   Manual test runs must never pollute the operator's host configuration (`~/.config/nuo`) or collide with running production daemons on default ports. Every test session must explicitly run inside an isolated sandbox directory using `NUO_HOME` and `NUO_PORT`.

---

## 2. Environment Preparation & Sandbox Setup

### Prerequisites

- **Rust Toolchain**: Rust 1.85+ (pinned via `Cargo.toml`).
- **Terminal Emulator**: Modern terminal supporting UTF-8 and truecolor (e.g. Ghostty, Alacritty, iTerm2, WezTerm, or Kitty).
- **Model Provider API Key**: At least one valid API key (e.g. DeepSeek, OpenRouter, Anthropic, or OpenAI).

### Cold-Start Sandbox Setup

Run these commands in your shell to prepare an isolated environment:

```bash
# 1. Compile the unified binary
cargo build -p nuo

# 2. Establish isolated instance root and non-conflicting TCP port
export NUO_HOME=$(mktemp -d /tmp/nuo-manual.XXXXXX)
export NUO_PORT=9820

# 3. Expose debug binary on PATH for canonical command execution
export PATH="$PWD/target/debug:$PATH"

echo "Sandbox initialized at: $NUO_HOME (Port: $NUO_PORT)"
```

To clean up after testing:

```bash
nuo stop >/dev/null 2>&1
rm -rf "$NUO_HOME"
```

---

## 3. Step-by-Step Verification Scenarios

### Suite 1: Build & CLI Baseline Verification

#### Scenario 1.1: Binary Execution & Help Surface
- **Action**:
  ```bash
  nuo --help
  nuo --version
  nuo run --help
  nuo serve --help
  ```
- **Expected Outcome**:
  - Help text renders cleanly with accurate usage patterns, subcommands, and environment variable documentation (`NUO_HOME`, `NUO_PORT`).
  - Version strings print matching the workspace version (e.g., `0.0.4`).
  - Commands exit with status `0`.

#### Scenario 1.2: Shell Completions Generation
- **Action**:
  ```bash
  nuo completions bash | head -n 15
  nuo completions zsh | head -n 15
  nuo completions fish | head -n 15
  ```
- **Expected Outcome**:
  - Valid completion scripts for `bash`, `zsh`, and `fish` print to stdout without panic.
  - Exit code `0`.

---

### Suite 2: Daemon Lifecycle & Server Control Plane

#### Scenario 2.1: Foreground Server Container Execution (`nuo serve` / `nuo start --fg`)
- **Action**:
  1. In Terminal A, start the daemon service in foreground mode:
     ```bash
     nuo serve --port "$NUO_PORT"
     ```
  2. In Terminal B, query status:
     ```bash
     nuo status
     ```
  3. Send `Ctrl+C` (SIGINT) to Terminal A.
- **Expected Outcome**:
  - Terminal A logs startup details, active TCP port `$NUO_PORT`, and domain socket path.
  - Terminal B outputs the active daemon endpoint and zero active sessions.
  - Upon `Ctrl+C`, the daemon performs a graceful drain, shuts down worker threads, removes Unix domain sockets, and exits cleanly.

#### Scenario 2.2: Detached Daemon Lifecycle (`start` / `status` / `token` / `stop`)
- **Action**:
  ```bash
  # 1. Start daemon detached
  nuo start --port "$NUO_PORT"

  # 2. Inspect status
  nuo status --diagnostic

  # 3. Retrieve bearer token
  TOKEN=$(nuo token)
  echo "Daemon token: $TOKEN"

  # 4. Graceful stop
  nuo stop
  ```
- **Expected Outcome**:
  - `start` prints daemon PID and port, then returns control to the shell immediately.
  - `status --diagnostic` reports process PID, socket path, runtime state, and active sessions.
  - `token` outputs a 64-character hexadecimal bearer token.
  - `stop` signals the daemon to drain active connections, removes the lockfile, and exits 0.

#### Scenario 2.3: HTTP Health & Probe Endpoint
- **Action**:
  ```bash
  nuo start --port "$NUO_PORT"

  # Health probe
  curl -i "http://127.0.0.1:$NUO_PORT/healthz"

  # CORS Preflight
  curl -i -X OPTIONS "http://127.0.0.1:$NUO_PORT/healthz"

  # Non-existent route
  curl -i "http://127.0.0.1:$NUO_PORT/invalid-route"

  nuo stop
  ```
- **Expected Outcome**:
  - `/healthz` returns `HTTP/1.1 200 OK`, `Content-Type: application/json`, and body containing `{"version":"...","auth":true}`.
  - `OPTIONS` returns `HTTP/1.1 204 No Content` with `Access-Control-Allow-Origin: *`.
  - `/invalid-route` returns `HTTP/1.1 404 Not Found`.

#### Scenario 2.4: Daemon Double-Start Mutual Exclusion
- **Action**:
  ```bash
  nuo start --port "$NUO_PORT"
  # Attempt second start on the same instance
  nuo start --port "$NUO_PORT"
  nuo stop
  ```
- **Expected Outcome**:
  - The second invocation fails immediately with an informative error (e.g. `daemon lock already held` or already running) and exit code `1`.
  - The first daemon process remains undisturbed.

---

### Suite 3: Configuration & Provider Authentication

#### Scenario 3.1: Configuration Schema Validation
- **Action**:
  ```bash
  nuo config path
  nuo config list
  nuo config check
  ```
- **Expected Outcome**:
  - `config path` prints `$NUO_HOME/nuo/config/config.toml`.
  - `config check` validates configuration against schema with 0 errors.

#### Scenario 3.2: Configuration Mutation (`set` & `get`)
- **Action**:
  ```bash
  nuo config set daemon.shutdown_grace_secs 15
  nuo config get daemon.shutdown_grace_secs
  ```
- **Expected Outcome**:
  - Key `daemon.shutdown_grace_secs` updates to `15`.
  - `config get` outputs `15`.

#### Scenario 3.3: Provider Credentials Setup
- **Action**:
  ```bash
  nuo auth list
  nuo auth set deepseek "sk-test-key-mock-123456"
  nuo auth show deepseek
  ```
- **Expected Outcome**:
  - `auth set` securely stores the credential under the isolated config directory.
  - `auth show deepseek` displays provider configuration with sensitive key characters masked (e.g., `sk-te****3456`).

---

### Suite 4: Semantic Terminal Experience (`nuo` Interactive TUI)

#### Scenario 4.1: Cold-Start Interactive TUI Launch
- **Action**:
  ```bash
  nuo
  ```
- **Expected Outcome**:
  - If `nuo` daemon is not yet running, `nuo` automatically bootstraps the local daemon in the background.
  - Terminal enters alternate screen buffer and raw mode.
  - Header displays active model/role, conversation status, and session indicator.
  - Input composer sits at bottom ready for prompt entry.
  - Typing characters displays smoothly with zero cursor lag.

#### Scenario 4.2: Dialogue Turn & Streaming Rendering
- **Action**:
  1. Inside `nuo`, type a greeting or simple question:
     ```text
     Hello! What is your role and system status?
     ```
  2. Press `Enter`.
- **Expected Outcome**:
  - User message renders as a styled block.
  - Streaming thinking blocks (if reasoning model) or response text stream in real time without screen flicker.
  - Status indicator shifts from `Idle` -> `Thinking` / `Generating` -> `Idle`.
  - Context token usage increments in header/status bar.

#### Scenario 4.3: Tool Approval Barrier & Permission Sheet
- **Action**:
  1. In `nuo`, ask the assistant to perform a safe filesystem inspection:
     ```text
     Please list the files in the current workspace directory.
     ```
  2. Observe the tool approval card when the agent requests `execute_command` or filesystem tools.
  3. Inspect approval sheet:
     - Use `←` / `→` or `Tab` to toggle between `[Approve Once]`, `[Always Allow]`, and `[Reject]`.
  4. Select `[Approve Once]` and press `Enter`.
- **Expected Outcome**:
  - Tool execution does NOT run until explicit Enter is pressed.
  - Once approved, stdout/stderr stream into an expandable disclosure block.
  - Agent synthesizes the directory listing into its final reply.

#### Scenario 4.4: Slash Commands & In-Session Controls
- **Action**:
  - Type `/help` and press Enter.
  - Type `/models` or press `Ctrl+M` to inspect the model catalog.
  - Type `/settings` to open the settings modal.
  - Type `/exit` or press `Ctrl+C` twice to exit.
- **Expected Outcome**:
  - `/help` renders available commands without invoking the LLM provider.
  - Model selector modal overlays cleanly and dismisses upon `Esc`.
  - `/exit` cleanly exits TUI, restores standard terminal screen buffer, unhides cursor, and returns to shell prompt.

#### Scenario 4.5: Settings → Components Reflects the Declared Registry (ADR-0020)
- **Action**:
  - Open `/settings` and select the **Components** category.
  - Confirm the pane lists one row per declared tool component (`Command
    Execution Logs`, `File Changes (Diffs)`, `File Content Previews`, `Image
    Reads`, `Search & Grep Results`, `Web Article Reads`, `Web Search Results`,
    `Todo & Task Checklists`, `Subagent Delegations`, `Skill Activations`,
    `Clarifying Questions`) followed by `Global Step Density` and
    `Auto-Scroll on Expand`, with `Reasoning Traces (Thinking)` first.
  - Confirm the badges match the declared defaults: `Command Execution Logs`
    and `File Changes (Diffs)` read `[ Expanded ]`; every other component reads
    `[ Collapsed ]`.
  - Toggle `Command Execution Logs`, then run a shell command; confirm the step
    collapses.
  - Run the same shell command under its legacy spelling (a restored session
    persisted as `bash` or `run_command`) and confirm it collapses too.
  - Restart `nuo` and confirm every toggle persisted.
- **Expected Outcome**:
  - The pane's row count and order are derived from `tools::TOOL_COMPONENTS`;
    adding a presenter plus one registry entry makes its row appear with no
    other edit.
  - A component's toggle applies to **every** name that component claims, so an
    alias spelling never ignores the user's choice.
  - Toggling one component never changes another's state.
  - `[default_expanded]` in `$XDG_CONFIG_HOME/nuo/tui.toml` carries one
    entry per alias of each toggled component.

#### Scenario 4.6: Interactive Markers Are Honest
- **Action**:
  - Send a turn that produces thinking, a file edit, a shell command, and a
    web search; hover each summary and press `Ctrl+N` / `Ctrl+P` to walk them
    with the keyboard.
  - Click a compaction card (appears after a context compaction) and press
    `Enter` / `Space` on it while focused.
  - Press `Enter` and `Space` on a focused notice (e.g. a provider-retry entry).
- **Expected Outcome**:
  - Every summary that shows a `+`/`-` marker is focusable, clickable, and
    lights up with the affordance hue on hover/focus (ADR-0174).
  - The compaction card toggles on click and on `Enter`/`Space`; its hint names
    only those chords.
  - Notices and command entries carry no `+`/`-` marker — their body is fully
    disclosed — and neither advertises a folding chord.
  - A click on prose selects text rather than toggling anything.

---

### Suite 5: Headless Execution & CLI Automation (`nuo run` / `nuo -p`)

#### Scenario 5.1: Headless One-Shot Prompt
- **Action**:
  ```bash
  nuo run "Explain the difference between TCP and UDP in 2 sentences"
  ```
  *(or equivalently: `nuo -p "Explain the difference between TCP and UDP in 2 sentences"`)*
- **Expected Outcome**:
  - Runs without launching the interactive TUI screen.
  - Connects to the daemon (auto-starting if necessary).
  - Streams markdown response directly to stdout.
  - Exits with status `0` upon completion.

#### Scenario 5.2: Pipeline & Stdin Ingestion
- **Action**:
  ```bash
  echo "fn calculate_sum(a: i32, b: i32) -> i32 { a + b }" | nuo run "Add doc comments to this Rust function"
  ```
- **Expected Outcome**:
  - Input from stdin is concatenated into the agent context prompt.
  - Agent produces documentation comments for the code snippet.
  - Exits code `0`.

#### Scenario 5.3: Unattended Mode (`--unattended`)
- **Action**:
  ```bash
  nuo run --unattended "Read Cargo.toml and output the workspace version"
  ```
- **Expected Outcome**:
  - Tool approvals within the safe execution sandbox execute without prompting for keyboard confirmation.
  - Output is printed and process terminates cleanly.

#### Scenario 5.4: Structured JSON Output (`--json`)
- **Action**:
  ```bash
  nuo run --json "List 3 programming languages"
  ```
- **Expected Outcome**:
  - Output is emitted as structured JSON stream / envelope.
  - Easily parsable with `jq`.

---

### Suite 6: Multi-Session, Remote Attach & Dashboard

#### Scenario 6.1: Detached Session & Fleet Monitoring
- **Action**:
  ```bash
  # 1. Start daemon
  nuo start --port "$NUO_PORT"

  # 2. Spawn a long-running prompt in headless or background
  nuo run "List all crates in the workspace and explain their dependencies" &

  # 3. Monitor daemon sessions
  nuo status --watch
  ```
- **Expected Outcome**:
  - `nuo status --watch` streams real-time session state transitions (`running`, `active`, `idle`).
  - Press `Ctrl+C` to exit monitor stream.

#### Scenario 6.2: Remote Daemon Connection over TCP + Token
- **Action**:
  ```bash
  TOKEN=$(nuo token)
  nuo --remote "127.0.0.1:$NUO_PORT" --token "$TOKEN" run "Respond with 'PONG'"
  ```
- **Expected Outcome**:
  - `nuo` connects over TCP using the bearer token rather than local Unix domain sockets.
  - Output streams successfully; daemon processes the request on the remote port.

#### Scenario 6.3: Session Attach & Picker (`nuo attach`)
- **Action**:
  ```bash
  # Launch attach picker
  nuo attach
  ```
- **Expected Outcome**:
  - If multiple sessions exist, displays an interactive picker listing session IDs, titles, and creation timestamps.
  - Selecting a session attaches the TUI to that session and replays recent message history.

#### Scenario 6.4: Full-Screen Session Dashboard (`nuo dashboard`)
- **Action**:
  ```bash
  nuo dashboard
  ```
- **Expected Outcome**:
  - Renders interactive full-screen session table with status, duration, model, and memory footprint.
  - Navigation keys (`↑` / `↓` / `Enter` to attach, `d` to delete, `Esc` to quit) function as documented.

#### Scenario 6.5: Explicit Session Termination (`nuo session rm`)
- **Action**:
  ```bash
  SESSION_ID=$(nuo status --json | grep -o '"id":"[^"]*' | head -n 1 | cut -d'"' -f4)
  if [ -n "$SESSION_ID" ]; then
    nuo session rm "$SESSION_ID"
    nuo status
  fi
  ```
- **Expected Outcome**:
  - Session is terminated immediately and evicted from the active sessions table.

---

### Suite 7: Extensibility & System Diagnostics (MCP, Skills & Doctor)

#### Scenario 7.1: Skills Discovery & Inspection
- **Action**:
  ```bash
  nuo skill ls
  ```
- **Expected Outcome**:
  - Lists built-in and workspace-discovered skills with namespace, version, and description.
- **Action**:
  ```bash
  # Inspect specific skill
  nuo skill info "sample" || true
  nuo skill show "sample" || true
  ```

#### Scenario 7.2: Skill Scaffolding (`nuo skill init`)
- **Action**:
  ```bash
  nuo skill init "smoke-test-skill"
  ls -la "smoke-test-skill"
  rm -rf "smoke-test-skill"
  ```
- **Expected Outcome**:
  - Scaffolds a new skill directory with standard skill templates and metadata.

#### Scenario 7.3: MCP Server Inspection & Probe
- **Action**:
  ```bash
  nuo mcp ls
  ```
- **Expected Outcome**:
  - Lists configured Model Context Protocol (MCP) servers defined in `config.toml`.
  - If a server (e.g. `filesystem` or `fetch`) is configured, `nuo mcp probe <server>` connects and enumerates advertised tool schemas.

#### Scenario 7.4: Storage Integrity & Doctor (`nuo doctor`)
- **Action**:
  ```bash
  nuo doctor
  ```
- **Expected Outcome**:
  - Verifies local session database schemas, storage directories, and index consistency.
  - Reports zero corruption or issues.

---

### Suite 8: Failure Injection, Edge Cases & Resilience

#### Scenario 8.1: Occupied Port Handling
- **Action**:
  ```bash
  # Bind a dummy listener to port 9821
  nc -l 9821 &
  NC_PID=$!

  # Attempt to start daemon on occupied port
  nuo start --port 9821 || true

  kill $NC_PID 2>/dev/null || true
  ```
- **Expected Outcome**:
  - The daemon detects port collision gracefully and reports an error, without crashing the shell or corrupting database files.

#### Scenario 8.2: Abrupt Client Disconnect / Reconnect
- **Action**:
  1. Run `nuo` and submit a prompt requiring deep reasoning.
  2. While the model is streaming, forcefully kill the `nuo` process (`kill -9`).
  3. Re-launch `nuo --resume`.
- **Expected Outcome**:
  - Daemon survives client disconnect without panicking.
  - `--resume` reconnects to the existing session, recovers the transcript up to the last persisted turn, and allows continuing the conversation.

#### Scenario 8.3: Rebuilt-Binary Daemon Self-Heal (ADR-0021)
- **Action**:
  ```bash
  # 1. Start a daemon and leave it idle
  nuo start
  nuo status --diagnostic   # note "Core Image" digests; they should match

  # 2. Rebuild the binary under the live daemon (same version, no protocol change)
  cargo build -p nuo

  # 3. Discover it, then start again
  nuo status --diagnostic   # "Daemon sha256" now differs -> "REBUILT/STALE" + drift diagnosis
  nuo                        # must reclaim the idle daemon automatically, no manual `nuo stop`
  ```
- **Expected Outcome**:
  - `nuo status --diagnostic` shows a `Core Image` block naming the installed path and both short digests; after the rebuild the daemon digest is flagged `differs from installed — REBUILT/STALE` and the top-level diagnosis reads `Rebuilt-binary drift`.
  - With **no active sessions and no daemon tasks**, the next `nuo` invocation reclaims the stale daemon and respawns the freshly built image transparently — no `client/daemon binary mismatch` error.
  - With a **live session** running, the same invocation instead refuses with `client/daemon binary mismatch … still hosting N active session(s)` and does **not** interrupt the work.

#### Scenario 8.4: Terminal Resize Stress Testing
- **Action**:
  - In `nuo`, repeatedly rapidly resize the terminal window between 80x24 and 160x50 columns while a response is streaming.
- **Expected Outcome**:
  - Layout recalculates without panicking (`nuotc` flexbox distribution clamps properly).
  - Text reflows cleanly without border artifacts or character truncation.

---

## 4. Release Verification Checklist

Prior to tagging and publishing a new release, verify each item:

| Check Category | Verification Step | Pass Criteria | Status |
| :--- | :--- | :--- | :--- |
| **Clean Build** | `cargo build --release -p nuo` | Zero compilation warnings or errors | [ ] |
| **Doc Governance** | `docgov check` | All protocol invariants pass | [ ] |
| **Unit & E2E Tests** | `cargo test --workspace` | All automated tests pass | [ ] |
| **Cold-Start Service** | `nuo serve --port "$NUO_PORT"` | Foreground daemon starts, binds port, drains gracefully on SIGINT | [ ] |
| **Detached Daemon** | `nuo start` -> `status` -> `token` -> `stop` | Background PID managed cleanly, token generated, clean stop | [ ] |
| **TUI Ergonomics** | Interactive `nuo` prompt and tool approval | Crisp rendering, no CJK glitches, clean exit | [ ] |
| **Headless CLI** | `nuo run "prompt"` and stdin pipe | Non-interactive execution, correct exit code 0 | [ ] |
| **Attach & Fleet** | `nuo attach` and `nuo dashboard` | Interactive picker & full-screen dashboard function smoothly | [ ] |
| **System Integrity** | `nuo doctor` & `nuo config check` | Stored database and configuration check report zero errors | [ ] |
| **Cleanup** | Shell exit & sandbox teardown | Terminal state restored, temporary files removed | [ ] |

---

## 5. Teardown & Environment Cleanup

When testing is complete, tear down the test environment:

```bash
# Terminate any running test daemons
nuo stop >/dev/null 2>&1 || true

# Remove temporary instance directory
if [ -n "$NUO_HOME" ] && [ -d "$NUO_HOME" ]; then
  rm -rf "$NUO_HOME"
  echo "Cleaned up sandbox at: $NUO_HOME"
fi

# Unset environment overrides
unset NUO_HOME
unset NUO_PORT
```
