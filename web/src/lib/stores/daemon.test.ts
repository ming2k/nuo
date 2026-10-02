import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DaemonStore, requestFrame, resolveConfig } from "./daemon.svelte.js";
import { FakeWebSocket, installFakeWebSocket } from "../test/fake-websocket.js";

describe("resolveConfig", () => {
  const storage = () => {
    const map = new Map<string, string>();
    return {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
      removeItem: (k: string) => void map.delete(k),
      map,
    };
  };

  it("defaults to the loopback endpoint with no project and no token", () => {
    expect(resolveConfig("", null)).toEqual({
      wsUrl: "ws://127.0.0.1:9800",
      project: null,
      token: null,
    });
  });

  it("prefers the ?ws= query param and persists it", () => {
    const store = storage();
    const cfg = resolveConfig("?ws=ws://10.0.0.2:1234", store);
    expect(cfg.wsUrl).toBe("ws://10.0.0.2:1234");
    expect(store.map.get("muta.ws-url")).toBe("ws://10.0.0.2:1234");
  });

  it("builds a URL from ?host= and ?port=", () => {
    expect(resolveConfig("?host=example.com&port=9000", null).wsUrl).toBe("ws://example.com:9000");
  });

  it("falls back to stored settings", () => {
    const store = storage();
    store.map.set("muta.ws-url", "ws://stored:1");
    store.map.set("muta.project", "/srv/proj");
    store.map.set("muta.ws-token", "stored-token");
    expect(resolveConfig("", store)).toEqual({
      wsUrl: "ws://stored:1",
      project: "/srv/proj",
      token: "stored-token",
    });
  });

  it("rejects non-ws schemes", () => {
    expect(resolveConfig("?ws=http://evil.example", null).wsUrl).toBe("ws://127.0.0.1:9800");
  });

  it("accepts bare host:port in the ws field", () => {
    expect(resolveConfig("?ws=localhost:7777", null).wsUrl).toBe("ws://localhost:7777");
  });

  it("ignores ?token= in query string to prevent history/referrer leakage", () => {
    const local = storage();
    const session = storage();
    const cfg = resolveConfig("?token=abc123", local, "", session);
    expect(cfg.token).toBeNull();
    expect(local.map.get("muta.ws-token")).toBeUndefined();
    expect(session.map.get("muta.ws-token")).toBeUndefined();
  });

  it("reads #token= from hash fragment and stores in sessionStorage only", () => {
    const local = storage();
    const session = storage();
    const cfg = resolveConfig("", local, "#token=secret-hash-token", session);
    expect(cfg.token).toBe("secret-hash-token");
    expect(session.map.get("muta.ws-token")).toBe("secret-hash-token");
    expect(local.map.get("muta.ws-token")).toBeUndefined();
  });
});

describe("DaemonStore smoke", () => {
  beforeEach(() => {
    installFakeWebSocket();
    FakeWebSocket.reset();
    window.localStorage.clear();
  });

  it("sends a monitor Select on connect, with the handshake version and protocol", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1" });
    const ws = FakeWebSocket.latest();
    expect(ws.url).toBe("ws://test:1");
    // No token configured: no subprotocol offer.
    expect(ws.protocols).toBeUndefined();
    ws.open();
    const select = ws.sentJson(0);
    expect(select.type).toBe("Select");
    expect(select.action).toEqual({ monitor: { watch: true, include_idle: true } });
    // Injected by vite define from package.json — must equal the workspace
    // version or a pre-protocol daemon refuses the connection (ADR-0100).
    expect(typeof select.version).toBe("string");
    expect(select.version).toBe(__MUTA_CLIENT_VERSION__);
    // The wire protocol number (ADR-0134) is the compatibility gate; it
    // must equal PROTOCOL_VERSION in muta-contracts (CI checks).
    expect(typeof select.protocol).toBe("number");
    expect(select.protocol).toBeGreaterThan(0);
  });

  it("carries the token as a bearer. subprotocol (ADR-0105)", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1", token: "sekret" });
    const ws = FakeWebSocket.latest();
    expect(ws.protocols).toEqual(["bearer.sekret"]);
  });

  it("ends a session via the kill_session control verb (ADR-0112)", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1" });
    store.endSession("sess-9");
    const ws = FakeWebSocket.latest();
    ws.open();
    // The Select carries the control verb, not an attach.
    expect(ws.sentJson(0).action).toEqual({
      control: { verb: "kill_session", session_id: "sess-9" },
    });
    // The reply (ok or error) closes the one-shot control connection.
    ws.message({ type: "ControlReply", ok: true, session_id: "sess-9" });
    expect(ws.readyState).toBe(FakeWebSocket.CLOSED);
  });

  it("attaches to the first session from the monitor snapshot and loads the transcript", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1" });
    const monitor = FakeWebSocket.latest();
    monitor.open();
    monitor.message({
      type: "Monitor",
      kind: "snapshot",
      project_root: "/srv/proj",
      daemon_started_at: 1,
      sessions: [
        {
          id: "sess-1",
          overview: "demo",
          created_at: 1,
          updated_at: 2,
          message_count: 1,
          hosting: "hosted",
          status: "idle",
          round: 1,
          output_tokens: 0,
          elapsed_ms: 0,
          project_root: "/srv/proj",
        },
      ],
    });
    expect(store.sessions).toHaveLength(1);

    const session = FakeWebSocket.latest();
    expect(session).not.toBe(monitor);
    session.open();
    expect(session.sentJson(0).action).toEqual({ attach: "sess-1" });

    session.message({
      type: "Welcome",
      session_id: "sess-1",
      round_counter: 3,
      provider: "kimi-code",
      model: "k2",
      messages: [
        { role: "User", content: "hi", timestamp: 100 },
        { role: "System", content: "secret", hidden: true },
        { role: "Assistant", content: "hello", timestamp: 101 },
      ],
    });
    expect(store.sessionAttached).toBe(true);
    expect(store.roundCounter).toBe(3);
    expect(store.providerInfo).toEqual({ provider: "kimi-code", model: "k2" });
    // Hidden harness-injected messages never reach the feed.
    expect(store.feed.map((i) => (i.kind === "message" ? i.message.content : ""))).toEqual([
      "hi",
      "hello",
    ]);
  });
});

// Wire-protocol coverage: one store + scripted fake WS per test, attached to
// "sess-1" via a Welcome replay. Round events go through `roundEvent`.

describe("DaemonStore wire protocol", () => {
  beforeEach(() => {
    installFakeWebSocket();
    FakeWebSocket.reset();
    window.localStorage.clear();
  });

  /** Init the store, attach to sess-1, and replay an empty Welcome. */
  function attachSession(store: DaemonStore, sessionId = "sess-1"): FakeWebSocket {
    store.init({ wsUrl: "ws://test:1" });
    FakeWebSocket.latest().open(); // monitor socket
    store.attach(sessionId);
    const session = FakeWebSocket.latest();
    session.open();
    session.message({
      type: "Welcome",
      session_id: sessionId,
      round_counter: 0,
      provider: "kimi-code",
      model: "k2",
      messages: [],
    });
    return session;
  }

  function roundEvent(ws: FakeWebSocket, event: unknown, sessionId = "sess-1") {
    ws.message({ type: "Response", Round: { session_id: sessionId, event } });
  }

  describe("stream folding", () => {
    it("folds StreamDelta×2 + StreamEnd into one Assistant message", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, "StreamStart");
      roundEvent(session, { StreamDelta: "Hello, " });
      roundEvent(session, { StreamDelta: "world" });
      expect(store.streamingAssistantText).toBe("Hello, world");
      expect(store.feed).toHaveLength(0);

      roundEvent(session, { StreamEnd: "Hello, world" });
      expect(store.streamingAssistantText).toBe("");
      expect(store.feed).toHaveLength(1);
      const item = store.feed[0];
      expect(item.kind).toBe("message");
      if (item.kind === "message") {
        expect(item.message.role).toBe("Assistant");
        expect(item.message.content).toBe("Hello, world");
      }
    });

    it("appends a Text event as an Assistant message (non-streamed fallback)", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { Text: "plain reply" });
      expect(store.feed).toHaveLength(1);
      const item = store.feed[0];
      expect(item.kind).toBe("message");
      if (item.kind === "message") {
        expect(item.message.role).toBe("Assistant");
        expect(item.message.content).toBe("plain reply");
      }
      expect(store.streamingAssistantText).toBe("");
    });
  });

  describe("CommandResult", () => {
    it("appends command feed items; Error variant → error, Text/Ack → success", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, {
        CommandResult: { name: "help", args: "", result: { Text: "help text" } },
      });
      roundEvent(session, {
        CommandResult: { name: "bogus", args: "x", result: { Error: { message: "bad" } } },
      });
      roundEvent(session, {
        CommandResult: { name: "compact", args: "", result: { Ack: { title: "Done" } } },
      });

      expect(store.feed.map((i) => i.kind)).toEqual(["command", "command", "command"]);
      const records = store.feed.map((i) => (i.kind === "command" ? i.record : null));
      expect(records[0]).toMatchObject({ name: "help", status: "success" });
      expect(records[1]).toMatchObject({ name: "bogus", status: "error" });
      expect(records[2]).toMatchObject({ name: "compact", status: "success" });
    });
  });

  describe("backend completion", () => {
    it("requests completion from the daemon and keeps only the newest result", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      store.requestComposerCompletions("/mo", 3);
      expect(session.sentJson(session.sent.length - 1)).toEqual({
        type: "Request",
        CompleteComposer: { request_id: 2, text: "/mo", cursor: 3 },
      });
      store.requestComposerCompletions("/mod", 4);
      session.message({
        type: "Response",
        ComposerCompletions: {
          request_id: 2,
          text: "/mo",
          cursor: 3,
          items: [{
            label: "/models",
            description: "Switch model",
            insert_text: "/models",
            replace_start: 0,
            replace_end: 3,
            kind: "slash",
          }],
        },
      });
      expect(store.composerCompletions).toEqual([]);

      session.message({
        type: "Response",
        ComposerCompletions: {
          request_id: 3,
          text: "/mod",
          cursor: 4,
          items: [{
            label: "/models",
            description: "Switch model",
            insert_text: "/models",
            replace_start: 0,
            replace_end: 4,
            kind: "slash",
          }],
        },
      });
      expect(store.composerCompletions.map((item) => item.label)).toEqual(["/models"]);
    });
  });

  describe("RoundInterrupted (C11)", () => {
    it("appends an interrupt feed item and toasts on the live event", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, {
        RoundInterrupted: { reason: "user", at_ms: 1_700_000_000_000, round: 3 },
      });

      expect(store.feed.map((i) => i.kind)).toEqual(["interrupt"]);
      const item = store.feed[0];
      if (item.kind === "interrupt") {
        expect(item.record.reason).toBe("user");
        expect(item.record.round).toBe(3);
      }
      expect(store.toasts.at(-1)?.severity).toBe("warning");
    });

    it("projects persisted records into the restored Welcome feed by timestamp", () => {
      const store = new DaemonStore();
      installFakeWebSocket();
      FakeWebSocket.reset();
      store.init({ wsUrl: "ws://test:1" });
      FakeWebSocket.latest().open();
      store.attach("sess-1");
      const session = FakeWebSocket.latest();
      session.open();
      session.message({
        type: "Welcome",
        session_id: "sess-1",
        round_counter: 2,
        provider: "kimi-code",
        model: "k2",
        messages: [
          { role: "User", content: "first", sent_at_ms: 1_000, hidden: false },
          { role: "Assistant", content: "reply", sent_at_ms: 1_500, hidden: false },
          { role: "User", content: "second", sent_at_ms: 5_000, hidden: false },
        ],
        round_interrupts: [
          { reason: "terminated", at_ms: 9_000, round: 2 },
          { reason: "user", at_ms: 3_000, round: 1 },
        ],
      });

      // Sorted by timestamp: u(1000), a(1500), interrupt(3000), u(5000),
      // interrupt(9000) — the markers land at their seams, not appended.
      expect(store.feed.map((i) => i.kind)).toEqual([
        "message",
        "message",
        "interrupt",
        "message",
        "interrupt",
      ]);
    });
  });

  describe("tool folding", () => {
    it("tracks ToolCall/ToolStream/ToolResult/ToolCancelled in liveTools", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "t1", name: "bash", arguments: "{}" } });
      expect(store.liveTools["t1"]).toMatchObject({
        id: "t1",
        name: "bash",
        status: "running",
        stdout: "",
        stderr: "",
      });

      roundEvent(session, { ToolStream: { id: "t1", stream: { Stdout: "out1 " } } });
      roundEvent(session, { ToolStream: { id: "t1", stream: { Stdout: "out2" } } });
      roundEvent(session, { ToolStream: { id: "t1", stream: { Stderr: "err1" } } });
      expect(store.liveTools["t1"].stdout).toBe("out1 out2");
      expect(store.liveTools["t1"].stderr).toBe("err1");

      roundEvent(session, {
        ToolResult: {
          id: "t1",
          name: "bash",
          output: "done",
          structured: { Text: "done" },
          duration_ms: 5,
        },
      });
      expect(store.liveTools["t1"].status).toBe("completed");
      expect(store.liveTools["t1"].output).toBe("done");
      expect(store.liveTools["t1"].durationMs).toBe(5);

      roundEvent(session, { ToolCall: { id: "t2", name: "bash", arguments: "{}" } });
      roundEvent(session, {
        ToolResult: {
          id: "t2",
          name: "bash",
          output: "",
          structured: { Error: { message: "boom" } },
          duration_ms: 1,
        },
      });
      expect(store.liveTools["t2"].status).toBe("failed");

      roundEvent(session, { ToolCall: { id: "t3", name: "bash", arguments: "{}" } });
      roundEvent(session, { ToolCancelled: { id: "t3", name: "bash" } });
      expect(store.liveTools["t3"].status).toBe("cancelled");
    });
  });

  describe("subagent flow", () => {
    const subagentEvent = (ws: FakeWebSocket, event: unknown, parentCallId = "call-1") =>
      roundEvent(ws, { SubagentStep: { parent_call_id: parentCallId, event } });

    it("folds subagent Started/Stream/Tool events into the parent tool", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      subagentEvent(session, { Started: { profile: "explore" } });
      subagentEvent(session, { StreamDelta: "partial " });
      subagentEvent(session, { StreamDelta: "text" });
      expect(store.liveTools["call-1"].subagent?.streamingText).toBe("partial text");

      subagentEvent(session, { StreamEnd: "partial text" });
      subagentEvent(session, {
        ToolCall: { id: "e1", name: "read", arguments: "{}", round: 1, turn: 0 },
      });
      subagentEvent(session, {
        ToolResult: { id: "e1", name: "read", output: "file contents", duration_ms: 1 },
      });
      subagentEvent(session, { Activity: "reading files" });

      const subagent = store.liveTools["call-1"].subagent;
      expect(subagent?.profile).toBe("explore");
      expect(subagent?.text).toBe("partial text");
      expect(subagent?.streamingText).toBe("");
      expect(subagent?.activity).toBe("reading files");
      expect(subagent?.tools).toHaveLength(1);
      expect(subagent?.tools[0]).toMatchObject({
        id: "e1",
        name: "read",
        status: "completed",
        output: "file contents",
        durationMs: 1,
      });
    });

    it("routes subagent PermissionRequest replies with parent_call_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      subagentEvent(session, { Started: { profile: "explore" } });
      subagentEvent(session, {
        PermissionRequest: {
          id: "p1",
          tool: "bash",
          label: "",
          description: "d",
          arguments: "{}",
          scope: "x",
          elevation: false,
          one_off: false,
        },
      });
      expect(store.pendingPermission?.request.id).toBe("p1");
      expect(store.pendingPermission?.origin.parentCallId).toBe("call-1");
      expect(store.pendingPermission?.origin.label).toBe("explore");

      store.resolvePermission("Once");
      expect(store.pendingPermission).toBeNull();
      // sent[0] is the attach Select frame.
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        PermissionReply: { request_id: "p1", decision: "Once", parent_call_id: "call-1" },
      });
    });

    it("routes top-level PermissionRequest replies with parent_call_id null", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, {
        PermissionRequest: {
          id: "p2",
          tool: "bash",
          label: "",
          description: "d",
          arguments: "{}",
          scope: "x",
          elevation: false,
          one_off: false,
        },
      });
      expect(store.pendingPermission?.origin.parentCallId).toBeNull();

      store.resolvePermission("Once");
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        PermissionReply: { request_id: "p2", decision: "Once", parent_call_id: null },
      });
    });

    it("routes subagent UserQuestionRequest replies with parent_call_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      subagentEvent(session, {
        UserQuestionRequest: {
          id: "q1",
          questions: [{ question: "?", options: [{ label: "a" }], multi_select: false }],
        },
      });
      expect(store.pendingQuestion?.request.id).toBe("q1");
      expect(store.pendingQuestion?.origin.parentCallId).toBe("call-1");

      store.answerQuestion([["a"]]);
      expect(store.pendingQuestion).toBeNull();
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        UserQuestionReply: { request_id: "q1", answers: [["a"]], parent_call_id: "call-1" },
      });
    });

    it("routes subagent InputRequest replies with parent_call_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      subagentEvent(session, {
        StdinRequest: { id: "i1", command: "sudo x", prompt: "password", secret: true },
      });
      expect(store.pendingStdin?.request.id).toBe("i1");
      expect(store.pendingStdin?.origin.parentCallId).toBe("call-1");

      store.replyStdin("x");
      expect(store.pendingStdin).toBeNull();
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        StdinReply: { request_id: "i1", text: "x", parent_call_id: "call-1" },
      });
    });
  });

  describe("input recovery", () => {
    it("UnsentInput drops the optimistic echo and restores the draft once", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      store.sendChat("hello");
      expect(store.feed).toHaveLength(1);

      roundEvent(session, { UnsentInput: { prompt: "hello", images: [] } });
      expect(store.feed).toHaveLength(0);
      expect(store.restoredDraft).toEqual({ text: "hello", images: [] });

      expect(store.takeRestoredDraft()).toEqual({ text: "hello", images: [] });
      expect(store.takeRestoredDraft()).toBeNull();
    });

    it("UnsentInput keeps the echo and the in-progress draft when the composer is busy", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      store.sendChat("hello");
      expect(store.feed).toHaveLength(1);

      // The user was mid-composition when the interrupt landed.
      store.composerIdle = false;

      roundEvent(session, { UnsentInput: { prompt: "hello", images: [] } });
      // The optimistic echo stays: with the composer keeping the user's
      // draft, the echo is the only visible copy of the unsent prompt.
      expect(store.feed).toHaveLength(1);
      expect(store.feed[0]).toMatchObject({
        kind: "message",
        message: { role: "User", content: "hello" },
      });
      expect(store.restoredDraft).toBeNull();
    });

    it("SteerAdmitted dedupes the optimistic echo but appends new text", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      store.sendPrompt("hello");
      roundEvent(session, {
        SteerAdmitted: { id: "q1", text: "hello", images: [], sent_at_ms: Date.now() },
      });
      expect(store.feed).toHaveLength(1); // deduped against the echo

      roundEvent(session, {
        SteerAdmitted: { id: "q2", text: "world", images: [], sent_at_ms: Date.now() },
      });
      expect(store.feed).toHaveLength(2);
      const item = store.feed[1];
      expect(item.kind).toBe("message");
      if (item.kind === "message") {
        expect(item.message.role).toBe("User");
        expect(item.message.content).toBe("world");
      }
    });
  });

  describe("reconnect", () => {
    afterEach(() => {
      vi.useRealTimers();
    });

    it("reattaches with backoff after a server-side close", () => {
      vi.useFakeTimers();
      try {
        const store = new DaemonStore();
        const session = attachSession(store);
        expect(store.sessionAttached).toBe(true);

        const socketsBefore = FakeWebSocket.instances.length;
        session.serverClose();
        expect(store.sessionAttached).toBe(false);
        // Backoff: no immediate reattach.
        expect(FakeWebSocket.instances.length).toBe(socketsBefore);

        vi.advanceTimersByTime(1000);
        const reattached = FakeWebSocket.latest();
        expect(reattached).not.toBe(session);
        reattached.open();
        expect(reattached.sentJson(0).type).toBe("Select");
        expect(reattached.sentJson(0).action).toEqual({ attach: "sess-1" });
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe("state events", () => {
    it("folds HarnessState/UnattendedChanged/RoundCompleted/Activity/Compacted", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { Activity: "waiting for model" });
      expect(store.activity).toBe("waiting for model");

      roundEvent(session, {
        HarnessState: { loop_status: "idle", round_counter: 2, unattended: true, confined: true, retry_pending: false },
      });
      expect(store.roundCounter).toBe(2);
      expect(store.unattended).toBe(true);
      expect(store.confined).toBe(true);
      expect(store.activity).toBeNull(); // idle clears the activity line

      roundEvent(session, { UnattendedChanged: false });
      expect(store.unattended).toBe(false);

      roundEvent(session, { ConfinementChanged: false });
      expect(store.confined).toBe(false);

      roundEvent(session, { Activity: "thinking" });
      roundEvent(session, {
        RoundCompleted: {
          round: 1,
          output_tokens: 100,
          duration_ms: 2000,
          paused_ms: 0,
          generation_ms: 1500,
        },
      });
      expect(store.lastRound).toEqual({
        round: 1,
        output_tokens: 100,
        duration_ms: 2000,
        paused_ms: 0,
        generation_ms: 1500,
      });
      expect(store.roundCounter).toBe(1);
      expect(store.activity).toBeNull();

      roundEvent(session, {
        Compacted: { archived_messages: 3, before_chars: 1000, after_chars: 200 },
      });
      expect(store.toasts.length).toBeGreaterThan(0);
      const toast = store.toasts[store.toasts.length - 1];
      expect(toast.severity).toBe("info");
      expect(toast.title).toBe("Context compacted");
    });
  });

  describe("global responses", () => {
    it("stores ProviderPicker/ProviderKeys and applies ProviderSwitched", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      session.message({
        type: "Response",
        ProviderPicker: {
          default_id: "kimi-code",
          rows: [
            {
              id: "kimi-code",
              name: "Kimi Code",
              model: "k2",
              models: ["k2"],
              builtin: true,
              protocol: "openai",
              base_url: "https://api.example",
              key_ready: true,
            },
          ],
        },
      });
      expect(store.providerPicker?.default_id).toBe("kimi-code");
      expect(store.providerPicker?.rows).toHaveLength(1);

      session.message({ type: "Response", ProviderSwitched: { provider: "p", model: "m" } });
      expect(store.providerInfo).toEqual({ provider: "p", model: "m" });

      session.message({ type: "Response", ProviderKeys: [["kimi-code", true]] });
      expect(store.providerKeys).toEqual([["kimi-code", true]]);
    });

    it("stores the websearch config snapshot and update ack, never a secret", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      session.message({
        type: "Response",
        WebSearchConfigSnapshot: {
          revision: 4,
          provider: "exa",
          reader: "jina",
          timeout_secs: 20,
          search_credential: "OptionalMissing",
          reader_credential: "Stored",
          capabilities: [
            {
              axis: "Search",
              id: "exa",
              display_name: "Exa",
              description: "Hosted semantic search",
              credential: "Optional",
              endpoint: "Fixed",
              default_endpoint: "https://mcp.exa.ai",
              default_env_var: "EXA_API_KEY",
            },
            {
              axis: "Reader",
              id: "jina",
              display_name: "Jina Reader",
              description: "Rendered page extraction to Markdown",
              credential: "Optional",
              endpoint: "Fixed",
              default_endpoint: "https://r.jina.ai",
              default_env_var: "JINA_API_KEY",
            },
          ],
        },
      });
      expect(store.websearchConfig?.provider).toBe("exa");
      // Readiness metadata, never key material — the view has no secret field.
      expect(store.websearchConfig?.reader_credential).toBe("Stored");
      expect(store.websearchConfig?.capabilities.map((item) => item.id)).toEqual(["exa", "jina"]);

      session.message({
        type: "Response",
        WebSearchConfigUpdated: {
          revision: 5,
          provider: "tavily",
          reader: "jina",
          timeout_secs: 30,
          search_credential: "Stored",
          reader_credential: "Stored",
          capabilities: [],
        },
      });
      expect(store.websearchConfig?.provider).toBe("tavily");
      expect(store.websearchConfig?.reader).toBe("jina");
      expect(store.websearchConfig?.revision).toBe(5);
    });

    it("ConversationReplaced merges messages and commands sorted by timestamp", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      session.message({
        type: "Response",
        ConversationReplaced: {
          session_id: "sess-2",
          messages: [
            // Message timestamps are epoch SECONDS.
            { role: "User", content: "first", timestamp: 100 },
            { role: "Assistant", content: "third", timestamp: 101 },
          ],
          commands: [
            // Command timestamps are epoch MILLISECONDS — between the two.
            {
              name: "help",
              args: "",
              status: "success",
              result: { Text: "help text" },
              timestamp: 100_500,
            },
          ],
        },
      });

      expect(store.activeSessionId).toBe("sess-2");
      expect(
        store.feed.map((item) => {
          if (item.kind === "message") return item.message.content;
          if (item.kind === "command") return `cmd:${item.record.name}`;
          if (item.kind === "retry_scheduled") return `retry:${item.attempt}`;
          if (item.kind === "retry_resolution") return `recovered:${item.record.attempts}`;
          return `interrupt:${item.record.reason}`;
        }),
      ).toEqual(["first", "cmd:help", "third"]);
    });

    it("ConversationCleared blanks state, keeps the attachment, and adopts the handoff id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      // Seed some session-scoped state the switch must wipe.
      roundEvent(session, "StreamStart");
      roundEvent(session, { StreamDelta: "partial" });
      store.roundCounter = 3;

      // The wire variant is a unit — `{"ConversationCleared":null}`; the fresh
      // session's id is NOT carried here (unlike ConversationReplaced).
      session.message({ type: "Response", ConversationCleared: null });

      // `/new` blanked the transcript, zeroed the round counter, and dropped
      // the streaming tail — while the socket stays bound to the harness, so
      // the composer keeps working.
      expect(store.activeSessionId).toBeNull();
      expect(store.feed).toHaveLength(0);
      expect(store.streamingAssistantText).toBe("");
      expect(store.roundCounter).toBe(0);
      expect(store.sessionAttached).toBe(true);

      // The next Round event carries the fresh session's id: the handoff.
      roundEvent(session, { Text: "fresh session reply" }, "sess-fresh");
      expect(store.activeSessionId).toBe("sess-fresh");
      expect(store.feed).toHaveLength(1);
    });
  });

  describe("outgoing frames", () => {
    it("sendChat/interrupt/setDefaultModel/deleteSession send flattened Request frames", () => {
      const store = new DaemonStore();
      const session = attachSession(store);
      // sent[0] is the attach Select frame.

      store.sendPrompt("/help");
      expect(session.sentJson(1)).toEqual({ type: "Request", SlashCommand: "/help" });

      store.sendPrompt("hi", [{ mime: "image/png", data: "AA==" }]);
      expect(session.sentJson(2)).toEqual({
        type: "Request",
        Prompt: {
          text: "hi",
          images: [{ mime: "image/png", data: "AA==" }],
          sent_at_ms: expect.any(Number),
        },
      });

      store.interrupt();
      expect(session.sentJson(3)).toEqual({ type: "Request", Interrupt: null });

      store.setDefaultModel("k2");
      expect(session.sentJson(4)).toEqual({ type: "Request", SetDefaultModel: { id: "k2" } });

      store.deleteSession("sess-1");
      expect(session.sentJson(5)).toEqual({ type: "Request", DeleteSession: { id: "sess-1" } });
    });

    it("requestFrame flattens the request next to the type tag", () => {
      const frame = requestFrame({ Prompt: { text: "hi", images: [] } });
      expect(frame.startsWith('{"type":"Request","Prompt"')).toBe(true);
      const parsed = JSON.parse(frame) as Record<string, unknown>;
      expect(parsed).not.toHaveProperty("request");
      expect(parsed.Prompt).toEqual({ text: "hi", images: [] });
    });
  });

  describe("session routing", () => {
    it("ignores Round events for a different session_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { Text: "stray" }, "other");
      expect(store.feed).toHaveLength(0);

      roundEvent(session, { Text: "mine" });
      expect(store.feed).toHaveLength(1);
    });
  });
});
