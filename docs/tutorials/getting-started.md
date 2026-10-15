# Getting Started with Nuo

In this tutorial you build the workspace from a clean clone, start the session server, and run your first agent turn — first through the interactive terminal interface, then headlessly.

---

## 1. Build the Workspace

Build the single public binary from a clean clone:

```bash
cargo build --release -p nuo
```

The workspace produces exactly one executable, `nuo`, which serves as the terminal interface, headless runner, and server control plane.

---

## 2. Start the Session Server

The session server hosts threads across workspaces. Start it detached, or in the foreground for containers and service managers:

```bash
# Detached background server
./target/release/nuo server start

# Foreground server (containers / systemd)
./target/release/nuo server start --fg
```

Inspect health and active endpoints, then print the bearer token your clients will use:

```bash
./target/release/nuo status
./target/release/nuo server token
```

---

## 3. Launch the Terminal Interface

Open the interactive terminal interface. If no server is running, the client starts and attaches to one automatically:

```bash
./target/release/nuo
```

Open the interactive thread picker to choose from past threads, or attach directly by id:

```bash
./target/release/nuo -t
./target/release/nuo -t <thread_id>
```

---

## 4. Run a Headless Turn

Send a one-shot prompt and stream the result without the terminal interface:

```bash
./target/release/nuo --headless -p "Explain the Nuo project structure"
```

---

## Next Steps

- [How-To Guides](../how-to/index.md): solve a specific task such as connecting a model provider or inspecting quota.
- [Explanation](../explanation/index.md): understand the entity tiers and subsystem boundaries behind what you just ran.
- [Reference](../reference/lifecycle-and-action-semantics.md): the precise lifecycle and action semantics.
