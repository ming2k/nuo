# How to Connect a Model Provider

Authenticate a model provider so Nuo can route agent turns to it, then verify the connection is live.

---

## Before You Start

Choose the invitation lane your provider account uses (see [ADR-0014](../adr/0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)):

- **Local Direct (OAuth)**: authenticate through the provider's OAuth login flow; Nuo holds the resulting token and calls the vendor host directly.
- **Local Direct (static)**: you hold a provider-issued API key (BYOK).
- **Subscription Proxy**: the control plane holds vendor credentials and bills subscription credits; the client never sees a vendor key.

The lanes are strictly exclusive per connection: pasting an API key into a subscription connection downgrades it to a static-key connection (`[INV-LANE-01]`).

---

## 1. Open the Connections Surface

Launch the interactive interface and open the connection manager from the slash-command palette:

```bash
nuo
```

Then, inside the interface, run:

```text
/connections
```

This scene lists existing provider connections, their lane, and their live status.

---

## 2. Add or Authenticate a Provider

Add a new connection from the same scene. For OAuth-lane providers, the client reports the authorization URL, completes the login, and stores the resulting token in the state store (never as plaintext in a config file). For static-key providers, supply the key through the same form.

### Provider-Specific Login Shapes

Each curated OAuth lane surfaces the vendor's *own* login protocol, not a generic form:

| Provider | Login surface | What you'll see |
| :--- | :--- | :--- |
| `chatgpt-plan` | Browser PKCE | Loopback callback binds `127.0.0.1:1455`; browser opens, Nuo receives the redirect locally. |
| `github-copilot` | Device code (RFC 8628) | A URL plus a short code; enter it on any device signed into GitHub. |
| `xai` | Browser PKCE | Loopback callback binds `127.0.0.1:56121`. |
| `qoder` | Vendor device flow | A browser URL; approval on the web side unblocks the poll. |
| `codebuddy` | Browser web-handoff | A URL opens on `${codebuddy account line}`; Nuo polls the login session until sign-in completes (no local callback binds). |

Pasting an API key on any of these downgrades the connection to the static-key (BYOK) lane (`[INV-LANE-01]`, `[INV-CB-01]`).

Configuration itself is declarative. Provider **connections** persist in `connections.toml` under the state directory (`$XDG_STATE_HOME/nuo/connections.toml`), while behavior settings live in the domain-separated config files (`server.toml`, `client.toml`, `terminal.toml`, `agent.toml` under `$XDG_CONFIG_HOME/nuo/`). Credentials never enter either as plaintext: OAuth token sets live in `auth.toml` and API keys in `credentials.toml`, both under the state directory with mode `0600` (`[INV-AUTH-02]`). The `/connections` scene authors the connection entry for you.

---

## 3. Verify

Confirm the connection is discoverable and reports the expected models:

```bash
nuo status
```

The provider should appear with a live connection. You can also switch the active model with the `/models` command.

---

## Related

- [Manage Threads and Sessions](manage-threads.md)
- [Inspect Token Usage](inspect-usage.md)
- [CodeBuddy wire reference (OAuth + inference)](../reference/openapi/codebuddy.yml)
- [ADR-0094: CodeBuddy Subscription Lane](../adr/0094-codebuddy-browser-web-handoff-oauth-subscription-lane.md)
