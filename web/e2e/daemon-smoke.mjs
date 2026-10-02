#!/usr/bin/env node
/**
 * End-to-end smoke against a live daemon (ADR-0105/0136): the exact flow a
 * browser app drives, over the exact transports it uses.
 *
 *   1. GET /healthz                → generic version/auth probe
 *   2. WS monitor with token as `bearer.` subprotocol → snapshot
 *   3. Control create_session      → ControlReply with session_id
 *   4. Attach that session         → Welcome
 *   5. Backend composer completion → ready-to-apply edit
 *   6. WS without credentials      → must fail (when auth is on)
 *   7. Version-skewed Select       → Error frame with code version_mismatch
 *
 * Env: DAEMON_URL (default http://127.0.0.1:9800), DAEMON_TOKEN (required
 * when the daemon has local_auth on), CLIENT_VERSION (defaults to the Web
 * package version and remains handshake diagnostic metadata under protocol
 * negotiation).
 * Exits non-zero on the first failed step.
 */

import { readFileSync } from "node:fs";

const BASE = (process.env.DAEMON_URL ?? "http://127.0.0.1:9800").replace(/\/+$/, "");
const WS_URL = BASE.replace(/^http:/, "ws:").replace(/^https:/, "wss:");
const TOKEN = process.env.DAEMON_TOKEN ?? "";
const packageJson = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const VERSION = process.env.CLIENT_VERSION ?? packageJson.version;
const wireRs = readFileSync(new URL("../../../crates/muta-contracts/src/wire.rs", import.meta.url), "utf8");
const protocolMatch = wireRs.match(/pub const PROTOCOL_VERSION:\s*u32\s*=\s*(\d+);/);
const minProtocolMatch = wireRs.match(/pub const MIN_PROTOCOL_VERSION:\s*u32\s*=\s*(\d+);/);
const PROTOCOL_VERSION = protocolMatch ? parseInt(protocolMatch[1], 10) : 13;
const MIN_PROTOCOL_VERSION = minProtocolMatch ? parseInt(minProtocolMatch[1], 10) : 12;

let step = "startup";
const timer = setTimeout(() => fail(step, "timeout"), 30_000);

function fail(where, why) {
  console.error(`FAIL [${where}]: ${why}`);
  process.exit(1);
}

function ok(msg) {
  console.log(`ok: ${msg}`);
}

function wsConnect(action, { token = TOKEN, version = VERSION, protocol } = {}) {
  return new Promise((resolvePromise, reject) => {
    const ws = new WebSocket(WS_URL, token ? [`bearer.${token}`] : []);
    const rejectOnce = () => reject(new Error("handshake refused"));
    ws.onerror = rejectOnce;
    ws.onclose = (e) => {
      if (!e.wasClean) rejectOnce();
    };
    ws.onopen = () => {
      const select = { type: "Select", action, version };
      if (protocol !== undefined) select.protocol = protocol;
      ws.send(JSON.stringify(select));
      resolvePromise(ws);
    };
  });
}

function nextFrame(ws, predicate, label) {
  return new Promise((resolvePromise, reject) => {
    const onMessage = (e) => {
      const frame = JSON.parse(e.data);
      if (predicate(frame)) {
        ws.removeEventListener("message", onMessage);
        resolvePromise(frame);
      } else if (frame.type === "Error") {
        reject(new Error(`${label}: error frame: ${frame.message}`));
      }
    };
    ws.addEventListener("message", onMessage);
  });
}

async function main() {
  step = "healthz";
  const health = await fetch(`${BASE}/healthz`).then((r) => r.json());
  if (typeof health.version !== "string" || typeof health.auth !== "boolean") {
    fail(step, `unexpected payload ${JSON.stringify(health)}`);
  }
  if (health.auth && !TOKEN) fail(step, "daemon requires a token but DAEMON_TOKEN is unset");
  ok(`healthz ${JSON.stringify(health)}`);

  step = "monitor";
  const monitor = await wsConnect(
    { monitor: { watch: true, include_idle: true } },
    { protocol: PROTOCOL_VERSION },
  ).catch((e) => fail(step, e.message));
  const snapshot = await nextFrame(
    monitor,
    (f) => f.type === "Monitor" && f.kind === "snapshot",
    "snapshot",
  ).catch((e) => fail(step, e.message));
  ok(`monitor snapshot (sessions=${snapshot.sessions.length})`);

  step = "create_session";
  const control = await wsConnect(
    { control: { verb: "create_session", project: "/" } },
    { protocol: PROTOCOL_VERSION },
  ).catch((e) => fail(step, e.message));
  const reply = await nextFrame(control, (f) => f.type === "ControlReply", "control").catch((e) =>
    fail(step, e.message),
  );
  if (!reply.ok || !reply.session_id) fail(step, reply.error ?? "no session_id");
  ok(`create_session → ${reply.session_id}`);

  step = "attach";
  const session = await wsConnect({ attach: reply.session_id }, { protocol: PROTOCOL_VERSION }).catch((e) =>
    fail(step, e.message),
  );
  const welcome = await nextFrame(session, (f) => f.type === "Welcome", "welcome").catch((e) =>
    fail(step, e.message),
  );
  ok(`Welcome (messages=${welcome.messages.length})`);

  step = "completion";
  session.send(
    JSON.stringify({
      type: "Request",
      CompleteComposer: { request_id: 7, text: "/mod", cursor: 4 },
    }),
  );
  const completion = await nextFrame(
    session,
    (f) => f.type === "Response" && (f.ComposerCompletions?.request_id === 7 || f.InputCompletions?.request_id === 7),
    "completion",
  ).catch((e) => fail(step, e.message));
  const payload = completion.ComposerCompletions ?? completion.InputCompletions;
  const modelItem = payload.items.find((item) => item.label === "/models");
  if (!modelItem || modelItem.insert_text !== "/models" || modelItem.replace_start !== 0) {
    fail(step, `unexpected completion ${JSON.stringify(completion)}`);
  }
  ok("backend completion returned an executable edit");

  if (health.auth) {
    step = "auth-negative";
    let refused = false;
    try {
      await wsConnect({ monitor: { watch: false, include_idle: true } }, { token: "" });
    } catch {
      refused = true;
    }
    if (!refused) fail(step, "credential-less handshake was accepted");
    ok("credential-less handshake refused");
  }

  step = "version-code";
  const skewed = await wsConnect(
    { monitor: { watch: false, include_idle: true } },
    { version: "0.0.0-skew" },
  ).catch((e) => fail(step, e.message));
  const err = await nextFrame(skewed, (f) => f.type === "Error", "skew").catch((e) =>
    fail(step, e.message),
  );
  if (err.code !== "version_mismatch") fail(step, `expected code version_mismatch, got ${err.code}`);
  ok("version skew refused with code=version_mismatch");

  // ADR-0134: the protocol number is the authority when declared. A skew
  // product version alongside a valid protocol number is SERVED (the
  // monitor snapshot arrives, not an Error)…
  step = "protocol-wins-over-version";
  const served = await wsConnect(
    { monitor: { watch: false, include_idle: true } },
    { version: "0.0.0-skew", protocol: MIN_PROTOCOL_VERSION },
  ).catch((e) => fail(step, e.message));
  const protoSnapshot = await nextFrame(served, (f) => f.type === "Monitor", "snapshot").catch(
    (e) => fail(step, e.message),
  );
  if (protoSnapshot.kind !== "snapshot")
    fail(step, `expected a snapshot, got ${protoSnapshot.kind}`);
  ok("declared protocol serves a skewed product version");

  // …and an out-of-window number is refused regardless of the version.
  step = "protocol-code";
  const future = await wsConnect(
    { monitor: { watch: false, include_idle: true } },
    { protocol: PROTOCOL_VERSION + 100 },
  ).catch((e) => fail(step, e.message));
  const perr = await nextFrame(future, (f) => f.type === "Error", "protocol-skew").catch((e) =>
    fail(step, e.message),
  );
  if (perr.code !== "protocol_mismatch")
    fail(step, `expected code protocol_mismatch, got ${perr.code}`);
  ok("out-of-window protocol refused with code=protocol_mismatch");

  clearTimeout(timer);
  console.log("E2E PASS");
  process.exit(0);
}

main().catch((e) => fail(step, e.message ?? String(e)));
