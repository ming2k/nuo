# Nuo Web

Browser client for the Nuo session daemon's WebSocket control plane. Built with Svelte 5, TypeScript, and Vite as a zero-backend static SPA.

## Capabilities

- **Fleet Monitoring**: Real-time sidebar tracking all hosted sessions with live status (`idle`, `running`, `needs_approval`), active tools, and token metrics.
- **Interactive Chat**: Replays session transcripts, streams assistant tokens (`StreamDelta`), renders live tool stdout/stderr, and supports slash commands.
- **Approval Barriers**: Inline interactive approvals for hazardous tool executions (`Once`, `Always`, `Reject`), user prompts (`ask_user`), and subagent delegation.
- **Subagent Telemetry**: Visualizes nested child agent steps, activity profiles, and reasoning traces without blocking parent context.
- **Model Switcher**: Dynamic provider and model selection with capability badges (reasoning effort, context limits).
- **Session Resilience**: Automatic exponential-backoff WebSocket reconnection with replay hydration.

## Connecting to Nuo

The web client communicates directly with a running `nuo` daemon over WebSockets:

1. **Start the Nuo daemon**:
   ```bash
   nuo start
   ```

2. **Retrieve local authentication token**:
   ```bash
   nuo token
   ```

3. **Launch the web development server**:
   ```bash
   pnpm install
   pnpm run dev
   ```

4. **Connect**:
   Open the browser interface, click the connection badge, and enter the WebSocket endpoint (`ws://127.0.0.1:9800`) and the bearer token.

## Development

```bash
pnpm install     # Install dependencies
pnpm run dev     # Start Vite development server
pnpm run check   # Run svelte-check and TypeScript validation
pnpm run test    # Run vitest test suite
pnpm run build   # Compile production static bundle to dist/
```

## Protocol Coupling

The frontend exchanges typed JSON envelopes over WebSockets with `nuo`'s WebSocket server engine, adhering to the client-daemon wire specifications defined in `nuo-client`.
