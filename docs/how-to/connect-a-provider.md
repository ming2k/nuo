# How to Connect a Model Provider

Authenticate a model provider so Nuo can route agent turns to it, then verify the connection is live.

---

## Before You Start

Choose the invitation lane your provider account uses (see [ADR-0014](../../docs/adr/0014-model-provider-invocation-schemes-oauth-subscription-lane-and-api-key-byok-lane.md)):

- **Local Direct (OAuth) / subscription lane**: authenticate through the provider's login flow.
- **Local Direct (static) / API key (BYOK) lane**: you hold a provider-issued key.

The lanes are strictly exclusive per connection: pasting an API key into a subscription connection downgrades it to a static-key connection.

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

Configuration itself is declarative. Provider entries live in the domain-separated configuration files (`server.toml`, `client.toml`, `agent.toml` under `$XDG_CONFIG_HOME/nuo/`); the `/connections` scene authors those entries for you.

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
