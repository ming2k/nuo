/// Build-time constants injected by `vite.config.ts` via `define`.

/**
 * The web client's version, identical to `package.json`'s `version` at build
 * time. Sent as the `version` field of the `Select` frame; the daemon
 * enforces exact equality with its own `CARGO_PKG_VERSION` (ADR-0100 rule 4),
 * so the two must stay in lockstep (CI checks this).
 */
declare const __MUTA_CLIENT_VERSION__: string;
