/**
 * Daemon connection store — the Web app's client for the muta
 * session-daemon WebSocket protocol.
 *
 * Contract: `docs/reference/server-api.md` + `docs/reference/server.asyncapi.yaml`
 * (mirrored in `../types.ts`). Two logical channels over one endpoint:
 *
 * - a Monitor connection (`Select{monitor}` → snapshot + diffs) driving the
 *   session list, and
 * - a per-attached-session connection (`Select{attach}` → `Welcome` +
 *   `Response` stream) driving the transcript.
 *
 * Everything here serializes/deserializes the daemon's serde shapes exactly:
 * flattened `Request`/`Response` payloads, `Monitor` frames tagged `kind`,
 * bare-string unit variants inside `Round.event`, and `Chat.images` always
 * present (the Rust field has no `#[serde(default)]`).
 */

import type {
  AgentNotice,
  AgentRequest,
  AgentResponse,
  AttachAction,
  CommandRecord,
  CommandCatalog,
  CommandResult,
  SubagentEvent,
  ImagePart,
  ComposerCompletion,
  ComposerCompletion as InputCompletion,
  StdinRequest,
  StdinRequest as InputRequest,
  Message,
  MonitorFrame,
  MonitoredSession,
  PermissionDecision,
  PermissionRequest,
  ProviderPickerSnapshot,
  QueuedMessage,
  QueuedUserInput,
  RetryResolution,
  RoundEvent,
  RoundInterrupt,
  RoundSummary,
  TodoList,
  UserQuestionRequest,
  WebConfigUpdate,
  WebConfigView,
  Wire,
} from "../types.js";

/** User-facing label for a round-interrupt reason (C11), mirroring the Rust
 * `RoundInterruptReason::label` so the panel and TUI agree verbatim. */
export function interruptLabel(reason: RoundInterrupt["reason"]): string {
  switch (reason) {
    case "user":
      return "Esc Esc";
    case "superseded":
      return "new message";
    case "terminated":
      return "process exited";
    case "error":
      return "error";
  }
}

/**
 * Client build identifier for the handshake. Advisory identity since
 * ADR-0134 (the protocol number below is the compatibility gate), but still
 * enforced against pre-protocol daemons, which judge it by exact equality
 * against their own `CARGO_PKG_VERSION` — so this must be the plain
 * workspace version (e.g. "0.24.0") with no client prefix. Injected at
 * build time from `package.json` by `vite.config.ts`
 * (`__MUTA_CLIENT_VERSION__`); CI refuses a drift between the two. Empty
 * (tests / non-vite runtimes) omits the field, which the daemon tolerates.
 */
const CLIENT_VERSION: string =
  typeof __MUTA_CLIENT_VERSION__ === "string" ? __MUTA_CLIENT_VERSION__ : "";

/**
 * The wire protocol number this client speaks (ADR-0134). Must equal
 * `PROTOCOL_VERSION` in `crates/muta-contracts/src/wire.rs` — the
 * `protocol_mirror` test in `muta-contracts` refuses a drift between the
 * two (ts-rs cannot export constants, so this is the one hand-maintained
 * mirror of that value). The daemon serves any number in its window;
 * sending it is what opts this client into protocol-number negotiation
 * instead of product-version equality.
 */
const PROTOCOL_VERSION = 14;

/** Reconnect base delay for both channels; doubles per failure, capped. */
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15_000;

/** Default daemon endpoint when nothing is configured. */
const DEFAULT_WS_URL = "ws://127.0.0.1:9800";

/** localStorage keys for the connection settings. */
const WS_URL_STORAGE_KEY = "muta.ws-url";
const PROJECT_STORAGE_KEY = "muta.project";
const TOKEN_STORAGE_KEY = "muta.ws-token";

/** Connection state, distinct from any session's status. */
export type ConnectionState = "connecting" | "connected" | "disconnected";

/** Where a blocking request originated (top-level agent or a subagent). */
export interface RequestOrigin {
  /** The subagent's parent tool-call id; `null` for top-level requests. */
  parentCallId: string | null;
  /** Display label, e.g. the subagent profile name. */
  label: string | null;
}

const TOP_LEVEL_ORIGIN: RequestOrigin = { parentCallId: null, label: null };

/** A blocking request the operator must answer before the round proceeds. */
export interface PendingPermission {
  request: PermissionRequest;
  origin: RequestOrigin;
}

export interface PendingQuestion {
  request: UserQuestionRequest;
  origin: RequestOrigin;
}

export interface PendingStdin {
  request: StdinRequest;
  origin: RequestOrigin;
}

export type PendingInput = PendingStdin;

/** One transient user-visible notice/error line. */
export interface Toast {
  id: number;
  severity: "info" | "warning" | "error";
  title: string;
  body?: string;
}

/** A tool run by a subagent, rendered nested inside the parent tool card. */
export interface SubagentTool {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "completed";
  output?: string;
  durationMs?: number;
}

/** UI-model subagent execution, folded from `RoundEvent::SubagentStep` sub-events. */
export interface SubagentExecution {
  profile: string | null;
  activity: string | null;
  /** Completed subagent response text (accumulated across `StreamEnd`s). */
  text: string;
  streamingText: string;
  /** Completed subagent reasoning traces (accumulated across `StreamReasoningEnd`s). */
  reasoning: string[];
  streamingReasoning: string;
  tools: SubagentTool[];
}

/** UI-model tool execution, folded from ToolCall/ToolStream/ToolResult events. */
export interface LiveToolExecution {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "completed" | "failed" | "cancelled";
  stdout: string;
  stderr: string;
  output?: string;
  durationMs?: number;
  /** Nested subagent activity when this tool is a `task` spawn (ADR-0029). */
  subagent?: SubagentExecution;
}

/**
 * The transcript feed: dialogue messages plus slash-command blocks (ADR-0091)
 * plus round-interrupt markers (C11), in arrival order. `key` is a stable
 * per-session-ui id for keyed each blocks.
 */
export type FeedItem =
  | { kind: "message"; key: string; message: Message }
  | { kind: "command"; key: string; record: CommandRecord }
  | { kind: "interrupt"; key: string; record: RoundInterrupt }
  | { kind: "retry_resolution"; key: string; record: RetryResolution }
  | {
      kind: "retry_scheduled";
      key: string;
      attempt: number;
      max_attempts: number;
      delay_ms: number;
      message: string;
    };

/** Resolved connection settings for the daemon endpoint. */
export interface DaemonConfig {
  wsUrl: string;
  /** Caller project path for the `Select` frame; `null` omits the field. */
  project: string | null;
  /**
   * Bearer token for daemons with auth on (ADR-0105: always on `--public`,
   * default on loopback via `[daemon] local_auth`). Browsers cannot set
   * headers on a WebSocket, so it travels as the `bearer.<token>` subprotocol.
   */
  token: string | null;
}

/** Minimal storage contract so `resolveConfig` stays testable without a DOM. */
export interface ConfigStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function sanitizeWsUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (trimmed.startsWith("ws://") || trimmed.startsWith("wss://")) return trimmed;
  // Bare host[:port] input is common in a settings field — assume plain ws.
  if (/^[\w.-]+(:\d+)?(\/\S*)?$/.test(trimmed)) return `ws://${trimmed}`;
  return null;
}

/** What `GET /healthz` reports; lets the UI name the failure precisely. */
export interface DaemonProbe {
  version: string;
  auth: boolean;
}

/** Extract one-time token from URL fragment (e.g. `#token=abc`). */
export function extractHashToken(hash: string): string | null {
  if (!hash) return null;
  const cleanHash = hash.startsWith("#") ? hash.slice(1) : hash;
  const params = new URLSearchParams(cleanHash);
  return params.get("token")?.trim() || null;
}

/**
 * Resolve the daemon endpoint and project scope, highest priority first:
 * URL query params (`?ws=` / `?host=`+`?port=` / `?project=`),
 * URL hash fragment `#token=` (one-time credential, scrubbed immediately from address bar),
 * then tab-scoped sessionStorage, then persisted localStorage settings (for endpoint only),
 * then loopback defaults.
 *
 * Security contract:
 * - Query-string `?token=` is deliberately NOT supported to prevent bearer tokens
 *   from leaking into browser history, HTTP referrers, server logs, or copied URLs.
 * - Hash fragments (`#token=...`) are parsed and scrubbed from the address bar via `history.replaceState`.
 * - Tokens are stored only in tab-scoped `sessionStorage` (or memory), NEVER in persistent `localStorage`.
 */
export function resolveConfig(
  search: string,
  storage: ConfigStorage | null,
  hash: string = "",
  sessionStore: ConfigStorage | null = null,
): DaemonConfig {
  const params = new URLSearchParams(search);
  const storedUrl = storage?.getItem(WS_URL_STORAGE_KEY) ?? null;
  const storedProject = storage?.getItem(PROJECT_STORAGE_KEY) ?? null;
  // Tokens are read from sessionStore (or fallback storage for backwards-compatible test environments)
  const storedToken =
    sessionStore?.getItem(TOKEN_STORAGE_KEY) ?? storage?.getItem(TOKEN_STORAGE_KEY) ?? null;

  let wsUrl =
    sanitizeWsUrl(params.get("ws")) ??
    sanitizeWsUrl(
      params.get("host")
        ? `${params.get("host")}${params.get("port") ? `:${params.get("port")}` : ""}`
        : null,
    ) ??
    sanitizeWsUrl(storedUrl) ??
    DEFAULT_WS_URL;

  const project = params.get("project")?.trim() || storedProject?.trim() || null;

  // Prefer hash token (#token=...) over stored session token.
  // Query-param token (?token=) is intentionally ignored for security.
  const hashToken = extractHashToken(hash);
  const token = hashToken || storedToken?.trim() || null;

  if (storage) {
    if (params.get("ws") || params.get("host")) storage.setItem(WS_URL_STORAGE_KEY, wsUrl);
    if (params.get("project")) storage.setItem(PROJECT_STORAGE_KEY, project ?? "");
  }

  // Token is saved into tab-scoped sessionStore, NEVER into persistent localStorage
  if (sessionStore && token) {
    sessionStore.setItem(TOKEN_STORAGE_KEY, token);
  }

  return { wsUrl, project, token };
}

/** Persist connection settings: endpoint config goes to localStorage, token only to sessionStorage. */
export function persistConfig(
  storage: ConfigStorage,
  config: DaemonConfig,
  sessionStore?: ConfigStorage | null,
): void {
  storage.setItem(WS_URL_STORAGE_KEY, config.wsUrl);
  if (config.project) storage.setItem(PROJECT_STORAGE_KEY, config.project);
  else storage.removeItem(PROJECT_STORAGE_KEY);

  // Security: never persist bearer credentials in localStorage
  storage.removeItem(TOKEN_STORAGE_KEY);

  if (sessionStore) {
    if (config.token) sessionStore.setItem(TOKEN_STORAGE_KEY, config.token);
    else sessionStore.removeItem(TOKEN_STORAGE_KEY);
  }
}

/** Derive the HTTP(S) base URL for the daemon's static/health endpoints. */
export function httpBaseUrl(wsUrl: string): string {
  return wsUrl.replace(/^ws:/, "http:").replace(/^wss:/, "https:").replace(/\/+$/, "");
}

/** Probe `GET /healthz`; `null` when nothing answers (or CORS/network fails). */
export async function probeDaemon(wsUrl: string): Promise<DaemonProbe | null> {
  try {
    const resp = await fetch(`${httpBaseUrl(wsUrl)}/healthz`, { cache: "no-store" });
    if (!resp.ok) return null;
    return (await resp.json()) as DaemonProbe;
  } catch {
    return null;
  }
}

function wireEnvelope(action: AttachAction, project: string | null): string {
  const frame: {
    type: "Select";
    action: AttachAction;
    project?: string;
    version?: string;
    protocol?: number;
  } = {
    type: "Select",
    action,
    protocol: PROTOCOL_VERSION,
  };
  if (project !== null) {
    frame.project = project;
  }
  if (CLIENT_VERSION) {
    frame.version = CLIENT_VERSION;
  }
  return JSON.stringify(frame);
}

/** Serialize an AgentRequest into the flattened Wire::Request frame. */
export function requestFrame(req: AgentRequest): string {
  return JSON.stringify({ type: "Request", ...req });
}

/** Millisecond clock for a transcript message (sent_at_ms wins, else unix seconds). */
function messageTimeMs(message: Message): number {
  return message.sent_at_ms ?? (message.timestamp ? message.timestamp * 1000 : 0);
}

export class DaemonStore {
  public connection = $state<ConnectionState>("disconnected");
  /**
   * True when the daemon demonstrably requires a bearer token and the
   * present one was rejected (missing, stale after a daemon restart).
   * The app surface opens the connection dialog on this signal.
   */
  public credentialChallenge = $state<boolean>(false);
  public draining = $state<boolean>(false);
  public sessions = $state<MonitoredSession[]>([]);
  public daemonProjectRoot = $state<string>("");

  public activeSessionId = $state<string | null>(null);
  public sessionAttached = $state<boolean>(false);
  public feed = $state<FeedItem[]>([]);
  public streamingAssistantText = $state<string>("");
  public streamingReasoningText = $state<string>("");
  public liveTools = $state<Record<string, LiveToolExecution>>({});
  public todos = $state<TodoList>({ items: [], next_id: 1, updated_at_round: 0 });
  public contextTokens = $state<number | null>(null);

  /** Harness-reported state (Welcome + HarnessState/UnattendedChanged/ConfinementChanged events). */
  public roundCounter = $state<number>(0);
  public unattended = $state<boolean>(false);
  public confined = $state<boolean>(true);
  /** Last live activity line ("waiting for model", …) from Activity events. */
  public activity = $state<string | null>(null);
  /** 0-indexed model-request position within the round (TurnStarted). */
  public currentTurn = $state<number | null>(null);
  /** Per-round accounting from the last naturally completed round. */
  public lastRound = $state<RoundSummary | null>(null);

  /** Current provider/model from Welcome, updated by ProviderSwitched. */
  public providerInfo = $state<{ provider: string; model: string } | null>(null);
  /** Full provider-picker snapshot, pushed on attach and after mutations. */
  public providerPicker = $state<ProviderPickerSnapshot | null>(null);
  /** Provider key-readiness summary (header surface). */
  public providerKeys = $state<[string, boolean][]>([]);
  /** Slash-command completion/help data published by the daemon. */
  public commandCatalog = $state<CommandCatalog>({
    commands: [],
    aliases: [],
    suggestions: [],
  });
  /** Latest race-checked completion edits produced by the daemon. */
  public composerCompletions = $state<ComposerCompletion[]>([]);
  public get inputCompletions(): ComposerCompletion[] {
    return this.composerCompletions;
  }
  public set inputCompletions(v: ComposerCompletion[]) {
    this.composerCompletions = v;
  }
  private completionRequestId = 0;
  private completionRequestState: { text: string; cursor: number } | null = null;

  /**
   * Effective singleton `[web]` configuration and credential readiness.
   * Refreshed when the settings dialog opens and on every update ack.
   */
  public websearchConfig = $state<WebConfigView | null>(null);

  public pendingPermission = $state<PendingPermission | null>(null);
  public pendingQuestion = $state<PendingQuestion | null>(null);
  public pendingStdin = $state<PendingStdin | null>(null);
  public get pendingInput(): PendingStdin | null {
    return this.pendingStdin;
  }
  public set pendingInput(v: PendingStdin | null) {
    this.pendingStdin = v;
  }

  public toasts = $state<Toast[]>([]);
  public sessionError = $state<string | null>(null);

  /**
   * Draft restored into the composer after `UnsentInput` (the round was
   * interrupted before any output; the prompt never reached the model).
   * One-shot: consumed by the composer via `takeRestoredDraft`. Only set
   * when the composer reports itself idle — the restore is asynchronous and
   * must not clobber in-progress typing (same policy as the TUI).
   */
  public restoredDraft = $state<{ text: string; images: ImagePart[] } | null>(null);

  /**
   * Composer idleness, reported by the composer component so the
   * asynchronous `UnsentInput` restore can adopt only an idle composer
   * (mirrors the TUI's `DraftAdoption::OnlyIfIdle`).
   */
  public composerIdle = true;

  public wsUrl = $state<string>(DEFAULT_WS_URL);
  public project = $state<string | null>(null);
  public token = $state<string | null>(null);
  /** Last `/healthz` probe outcome (null = unreachable or never probed). */
  public daemonProbe = $state<DaemonProbe | null>(null);

  private monitorWs: WebSocket | null = null;
  private sessionWs: WebSocket | null = null;
  /**
   * Whether the in-flight stream turn has already committed its Assistant
   * feed component. One ReAct turn emits at most one `StreamEnd` (text) and
   * one batch of `ToolCall`s; Gemini-style turns often stream reasoning plus
   * function calls with **no** text, so no `StreamEnd` ever arrives — without
   * this flag the buffered thinking would be silently wiped by the next
   * turn's `StreamStart` and never land in the transcript.
   */
  private turnCommitted = false;
  private monitorGeneration = 0;
  private sessionGeneration = 0;
  private reconnectDelay = RECONNECT_BASE_MS;
  private sessionReconnectDelay = RECONNECT_BASE_MS;
  private sessionReconnectTimer: number | null = null;
  private nextToastId = 1;
  private nextFeedKey = 1;
  private newSessionWs: WebSocket | null = null;

  public activeSession = $derived(
    this.sessions.find((s) => s.id === this.activeSessionId) ?? null,
  );

  public isBusy = $derived(
    this.activeSession?.status === "running" ||
      this.streamingAssistantText.length > 0 ||
      Object.keys(this.liveTools).some((id) => this.liveTools[id].status === "running"),
  );

  /** Read the persisted + query/hash configuration without connecting. */
  public loadConfig(): DaemonConfig {
    const storage = typeof window !== "undefined" ? window.localStorage : null;
    const sessionStore = typeof window !== "undefined" ? window.sessionStorage : null;
    const search = typeof window !== "undefined" ? window.location.search : "";
    const hash = typeof window !== "undefined" ? window.location.hash : "";

    // Scrub token from hash immediately so it does not linger in browser address bar/history
    if (typeof window !== "undefined" && window.location.hash.includes("token=")) {
      const cleanHash = window.location.hash.replace(/#?token=[^&]*&?/, "").replace(/^#$/, "");
      const newUrl =
        window.location.pathname + window.location.search + (cleanHash ? `#${cleanHash}` : "");
      window.history.replaceState(null, "", newUrl);
    }

    // Clean up any legacy token inadvertently left in localStorage from older versions
    if (storage) {
      const legacyToken = storage.getItem(TOKEN_STORAGE_KEY);
      if (legacyToken && sessionStore) {
        sessionStore.setItem(TOKEN_STORAGE_KEY, legacyToken);
      }
      storage.removeItem(TOKEN_STORAGE_KEY);
    }

    return resolveConfig(search, storage, hash, sessionStore);
  }

  public init(overrides?: Partial<DaemonConfig>) {
    const resolved = this.loadConfig();
    this.wsUrl = overrides?.wsUrl ?? resolved.wsUrl;
    this.project = overrides?.project !== undefined ? overrides.project : resolved.project;
    this.token = overrides?.token !== undefined ? overrides.token : resolved.token;
    this.connectMonitor();
  }

  /** Apply new connection settings, persist them, and reconnect everything. */
  public applyConfig(config: DaemonConfig) {
    if (typeof window !== "undefined") {
      persistConfig(window.localStorage, config, window.sessionStorage);
    }
    this.wsUrl = config.wsUrl;
    this.project = config.project;
    this.token = config.token;
    // Reattach from scratch against the new endpoint: drop every live socket
    // (monitor reconnect happens below; the session channel reattaches from
    // the fresh snapshot).
    if (this.sessionWs) {
      this.detachSocketHandlers(this.sessionWs);
      this.sessionWs.close();
      this.sessionWs = null;
    }
    if (this.newSessionWs) {
      this.detachSocketHandlers(this.newSessionWs);
      this.newSessionWs.close();
      this.newSessionWs = null;
    }
    this.clearSessionState();
    this.sessions = [];
    this.daemonProjectRoot = "";
    this.draining = false;
    this.connectMonitor();
  }

  /** Open a control-plane socket, carrying the token as a `bearer.` subprotocol. */
  private openSocket(): WebSocket {
    return new WebSocket(this.wsUrl, this.token ? [`bearer.${this.token}`] : undefined);
  }

  /** Refresh the `/healthz` probe (connection state hints in the dialog). */
  public async probe(): Promise<DaemonProbe | null> {
    this.daemonProbe = await probeDaemon(this.wsUrl);
    return this.daemonProbe;
  }

  // Monitor channel

  private connectMonitor() {
    const generation = ++this.monitorGeneration;

    if (this.monitorWs) {
      this.detachSocketHandlers(this.monitorWs);
      this.monitorWs.close();
      this.monitorWs = null;
    }

    this.connection = "connecting";
    const ws = this.openSocket();
    this.monitorWs = ws;

    ws.onopen = () => {
      if (generation !== this.monitorGeneration) return;
      this.connection = "connected";
      this.credentialChallenge = false;
      this.reconnectDelay = RECONNECT_BASE_MS;
      ws.send(wireEnvelope({ monitor: { watch: true, include_idle: true } }, this.project));
    };

    ws.onmessage = (event) => {
      if (generation !== this.monitorGeneration) return;
      try {
        this.handleFrame(JSON.parse(event.data as string) as Wire);
      } catch (err) {
        console.error("monitor frame parse error:", err, event.data);
      }
    };

    ws.onclose = () => {
      if (generation !== this.monitorGeneration) return;
      this.connection = "disconnected";
      // A failed WS handshake is opaque to browsers; the health probe tells
      // "daemon needs a token" apart from "nothing is listening".
      const hadProbe = this.daemonProbe !== null;
      if (!hadProbe) {
        void this.probe();
      } else {
        // Probe already current: a reconnect loop against an auth-on daemon
        // means the stored token was rejected (e.g. the daemon restarted and
        // rotated it). Raise the credential challenge so the UI can surface
        // the connection dialog immediately instead of silently retrying.
        if (this.daemonProbe?.auth) this.credentialChallenge = true;
      }
      if (!this.draining) {
        window.setTimeout(() => this.connectMonitor(), this.reconnectDelay);
        this.reconnectDelay = Math.min(this.reconnectDelay * 2, RECONNECT_MAX_MS);
      }
    };

    ws.onerror = () => {
      if (generation !== this.monitorGeneration) return;
      this.connection = "disconnected";
    };
  }

  private handleMonitorFrame(frame: MonitorFrame) {
    switch (frame.kind) {
      case "snapshot":
        this.daemonProjectRoot = frame.project_root;
        this.sessions = frame.sessions;
        if (!this.activeSessionId && frame.sessions.length > 0) {
          this.attach(frame.sessions[0].id);
        } else if (
          this.activeSessionId &&
          !this.sessionAttached &&
          this.sessionWs === null &&
          frame.sessions.some((s) => s.id === this.activeSessionId)
        ) {
          // The session channel is down but the daemon still hosts the
          // session — reattach (e.g. after a daemon restart).
          this.attach(this.activeSessionId);
        }
        break;
      case "session_added":
      case "session_updated": {
        const row = frame as MonitoredSession;
        const rest = this.sessions.filter((s) => s.id !== row.id);
        this.sessions =
          frame.kind === "session_added" ? [row, ...rest] : [row, ...rest].sort(
            (a, b) => b.updated_at - a.updated_at,
          );
        break;
      }
      case "session_removed":
        this.sessions = this.sessions.filter((s) => s.id !== frame.session_id);
        if (frame.session_id === this.activeSessionId) {
          this.clearSessionState();
          const next = this.sessions[0];
          if (next) this.attach(next.id);
        }
        break;
      case "daemon_draining":
        this.draining = true;
        this.pushToast("warning", "Daemon draining", "The daemon is shutting down.");
        break;
    }
  }

  // Session channel

  public attach(sessionId: string) {
    if (
      this.activeSessionId === sessionId &&
      this.sessionWs &&
      this.sessionWs.readyState === WebSocket.OPEN
    ) {
      return;
    }

    this.cancelSessionReconnect();
    const generation = ++this.sessionGeneration;
    this.clearSessionState();
    this.activeSessionId = sessionId;

    if (this.sessionWs) {
      this.detachSocketHandlers(this.sessionWs);
      this.sessionWs.close();
      this.sessionWs = null;
    }

    const ws = this.openSocket();
    this.sessionWs = ws;

    ws.onopen = () => {
      if (generation !== this.sessionGeneration) return;
      ws.send(wireEnvelope({ attach: sessionId }, this.project));
    };

    ws.onmessage = (event) => {
      if (generation !== this.sessionGeneration) return;
      try {
        this.handleFrame(JSON.parse(event.data as string) as Wire);
      } catch (err) {
        console.error("session frame parse error:", err, event.data);
      }
    };

    ws.onclose = () => {
      if (generation !== this.sessionGeneration) return;
      this.sessionWs = null;
      const wasAttached = this.sessionAttached;
      this.sessionAttached = false;
      if (this.activeSessionId !== sessionId) return;
      // Reattach with backoff while the daemon still hosts the session; the
      // Welcome replay restores the transcript, so no messages are lost.
      if (wasAttached) {
        this.pushToast("warning", "Session detached", "Reconnecting…");
      }
      this.scheduleSessionReconnect(sessionId);
    };

    ws.onerror = () => {
      if (generation !== this.sessionGeneration) return;
      this.sessionAttached = false;
    };
  }

  private scheduleSessionReconnect(sessionId: string) {
    this.cancelSessionReconnect();
    this.sessionReconnectTimer = window.setTimeout(() => {
      this.sessionReconnectTimer = null;
      if (this.activeSessionId !== sessionId || this.sessionAttached) return;
      // Attach only if the monitor still lists the session (or the monitor is
      // down too — the attach attempt is cheap and fails fast).
      this.attach(sessionId);
    }, this.sessionReconnectDelay);
    this.sessionReconnectDelay = Math.min(this.sessionReconnectDelay * 2, RECONNECT_MAX_MS);
  }

  private cancelSessionReconnect() {
    if (this.sessionReconnectTimer !== null) {
      window.clearTimeout(this.sessionReconnectTimer);
      this.sessionReconnectTimer = null;
    }
  }

  /** Create a new hosted session via the control plane and attach to it. */
  public newSession() {
    if (this.newSessionWs) {
      this.detachSocketHandlers(this.newSessionWs);
      this.newSessionWs.close();
      this.newSessionWs = null;
    }

    const ws = this.openSocket();
    this.newSessionWs = ws;

    ws.onopen = () => {
      ws.send(
        wireEnvelope(
          {
            control: {
              verb: "create_session",
              project: this.project ?? this.daemonProjectRoot ?? "/",
            },
          },
          this.project,
        ),
      );
    };

    ws.onmessage = (event) => {
      try {
        const frame = JSON.parse(event.data as string) as Wire;
        if (frame.type === "ControlReply") {
          if (frame.ok && frame.session_id) {
            this.attach(frame.session_id);
          } else {
            this.pushToast("error", "Could not create session", frame.error ?? "unknown error");
          }
          this.detachSocketHandlers(ws);
          ws.close();
          if (this.newSessionWs === ws) this.newSessionWs = null;
        }
      } catch (err) {
        console.error("control frame parse error:", err, event.data);
      }
    };

    ws.onclose = () => {
      if (this.newSessionWs === ws) this.newSessionWs = null;
    };
  }

  /**
   * End a hosted session (ADR-0112): the panel-side counterpart of the TUI's
   * `/exit` — "I am done with this session", not "detach". Reuses the
   * `kill_session` control verb (one-shot control connection, mirroring
   * `newSession`), which tears the session down server-side and publishes
   * `SessionRemoved`; the monitor stream then drops the row and, if it was
   * the active session, `handleMonitorFrame` clears the view. Disk history
   * is kept — ending is not deleting.
   */
  public endSession(id: string) {
    const ws = this.openSocket();
    ws.onopen = () => {
      ws.send(
        wireEnvelope(
          {
            control: {
              verb: "kill_session",
              session_id: id,
            },
          },
          this.project,
        ),
      );
    };
    ws.onmessage = (event) => {
      try {
        const frame = JSON.parse(event.data as string) as Wire;
        if (frame.type === "ControlReply" && !frame.ok) {
          this.pushToast("error", "Could not end session", frame.error ?? "unknown error");
        }
      } catch (err) {
        console.error("control frame parse error:", err, event.data);
      }
      this.detachSocketHandlers(ws);
      ws.close();
    };
    ws.onerror = () => {
      this.pushToast("error", "Could not end session", "control connection failed");
    };
  }

  // Frame dispatch

  /**
   * Issue one session-management verb over a one-shot control connection
   * (the shared shape of `endSession` / `interruptSession` /
   * `suspendSession`): send `Select{control}`, surface a failed reply as a
   * toast, close. A session need not be attached to be managed.
   */
  private controlVerb(
    verb: "kill_session" | "interrupt" | "suspend_session",
    id: string,
    failureTitle: string,
  ) {
    const ws = this.openSocket();
    ws.onopen = () => {
      ws.send(
        wireEnvelope({ control: { verb, session_id: id } }, this.project),
      );
    };
    ws.onmessage = (event) => {
      try {
        const frame = JSON.parse(event.data as string) as Wire;
        if (frame.type === "ControlReply" && !frame.ok) {
          this.pushToast("error", failureTitle, frame.error ?? "unknown error");
        }
      } catch (err) {
        console.error("control frame parse error:", err, event.data);
      }
      this.detachSocketHandlers(ws);
      ws.close();
    };
    ws.onerror = () => {
      this.pushToast("error", failureTitle, "control connection failed");
    };
  }

  /**
   * Interrupt any hosted session's current round — the panel-side
   * counterpart of the dashboard's `i` / `/interrupt`. Works on sessions
   * the panel is not attached to (the verb rides the control plane).
   */
  public interruptSession(id: string) {
    this.controlVerb("interrupt", id, "Could not interrupt session");
  }

  /**
   * Suspend a hosted session (park it in memory; the next attach rebuilds
   * it via lazy resume). The daemon refuses a session with an attached
   * client or an active round — the error toast carries the reason.
   */
  public suspendSession(id: string) {
    this.controlVerb("suspend_session", id, "Could not suspend session");
  }

  private handleFrame(frame: Wire) {
    switch (frame.type) {
      case "Welcome":
        this.sessionAttached = true;
        this.sessionReconnectDelay = RECONNECT_BASE_MS;
        this.sessionError = null;
        this.roundCounter = frame.round_counter;
        this.providerInfo = { provider: frame.provider, model: frame.model };
        this.commandCatalog = frame.command_catalog ?? {
          commands: [],
          aliases: [],
          suggestions: [],
        };
        // C11: re-project the durable round-interrupt records into the feed
        // alongside the restored dialogue, merged by timestamp.
        this.feed = this.buildReplacedFeed(
          frame.messages.filter((m) => !m.hidden),
          [],
          frame.round_interrupts ?? [],
          frame.retry_resolutions ?? [],
        );
        break;
      case "Pick":
        this.sessionError =
          "The daemon asked this client to pick a session — not supported by the Web app yet.";
        break;
      case "Error":
        this.sessionError = frame.message;
        if (frame.code === "protocol_mismatch") {
          this.pushToast(
            "error",
            "Client/daemon protocol mismatch",
            "Run `muta stop`, then reload this app — Mutx or another client can restart the new daemon build on demand.",
          );
        } else if (frame.code === "version_mismatch") {
          this.pushToast(
            "error",
            "Client/daemon version mismatch",
            "Run `muta stop`, then reload this app — Mutx or another client can restart the new daemon build on demand.",
          );
        } else {
          this.pushToast("error", "Daemon error", frame.message);
        }
        break;
      case "ControlReply":
        // Handled where issued (newSession); nothing to do on other sockets.
        break;
      case "Monitor":
        this.handleMonitorFrame(frame as MonitorFrame);
        break;
      case "Response":
        this.handleResponse(frame as AgentResponse);
        break;
      case "Request":
      case "Select":
        break;
    }
  }

  private handleResponse(resp: AgentResponse) {
    if ("Round" in resp) {
      this.handleRoundEvent(resp.Round.session_id, resp.Round.event);
    } else if ("ProviderPicker" in resp) {
      this.providerPicker = resp.ProviderPicker;
    } else if ("ProviderKeys" in resp) {
      this.providerKeys = resp.ProviderKeys;
    } else if ("ProviderSwitched" in resp) {
      this.providerInfo = {
        provider: resp.ProviderSwitched.provider,
        model: resp.ProviderSwitched.model,
      };
    } else if ("ComposerCompletions" in resp) {
      const result = resp.ComposerCompletions;
      const state = this.completionRequestState;
      if (
        state !== null &&
        result.request_id === this.completionRequestId &&
        result.text === state.text &&
        result.cursor === state.cursor
      ) {
        this.composerCompletions = result.items;
      }
    } else if ("InputCompletions" in resp) {
      const result = (resp as { InputCompletions: { request_id: number; input: string; cursor: number; items: ComposerCompletion[] } }).InputCompletions;
      const state = this.completionRequestState;
      if (
        state !== null &&
        result.request_id === this.completionRequestId &&
        result.input === state.text &&
        result.cursor === state.cursor
      ) {
        this.composerCompletions = result.items;
      }
    } else if ("ConversationCleared" in resp) {
      // `/new` blanked the transcript: the harness switched this attached
      // connection to a brand-new empty session. The variant is a unit — it
      // carries no id — so the new session id arrives with the next `Round`
      // event (see the handoff in handleRoundEvent) or a monitor row. The
      // socket stays bound, so the composer must stay usable.
      this.clearSessionState();
      this.sessionAttached = true;
    } else if ("ConversationReplaced" in resp) {
      this.activeSessionId = resp.ConversationReplaced.session_id;
      this.streamingAssistantText = "";
      this.streamingReasoningText = "";
      this.liveTools = {};
      this.feed = this.buildReplacedFeed(
        resp.ConversationReplaced.messages.filter((m) => !m.hidden),
        resp.ConversationReplaced.commands ?? [],
        resp.ConversationReplaced.round_interrupts ?? [],
        resp.ConversationReplaced.retry_resolutions ?? [],
      );
    } else if ("CopyToClipboard" in resp) {
      if (typeof navigator !== "undefined" && navigator.clipboard) {
        navigator.clipboard.writeText(resp.CopyToClipboard.text).catch(() => {});
      }
    } else if ("Error" in resp) {
      this.sessionError = resp.Error;
      this.pushToast("error", "Agent error", resp.Error);
    } else if ("WebSearchConfigSnapshot" in resp) {
      this.websearchConfig = resp.WebSearchConfigSnapshot;
    } else if ("WebSearchConfigUpdated" in resp) {
      // Authoritative post-update ack: re-render from persisted state.
      this.websearchConfig = resp.WebSearchConfigUpdated;
      this.pushToast("info", "Web search", "Configuration saved and applied.");
    } else if ("Exit" in resp) {
      this.sessionAttached = false;
    }
  }

  /**
   * Rebuild the feed after a session switch: dialogue messages plus the
   * persisted command ledger plus the round-interrupt records (C11), all
   * merged by timestamp (epoch ms).
   */
  private buildReplacedFeed(
    messages: Message[],
    commands: CommandRecord[],
    interrupts: RoundInterrupt[] = [],
    retryResolutions: RetryResolution[] = [],
  ): FeedItem[] {
    // ADR-0194: Historical retry resolutions are excluded from the main feed;
    // self-healed transport retries are execution details, not dialogue items.
    const items: FeedItem[] = [
      ...messages.map((m) => this.messageItem(m)),
      ...commands.map((c) => this.commandItem(c)),
      ...interrupts.map((r) => this.interruptItem(r)),
    ];
    const time = (item: FeedItem): number =>
      item.kind === "message"
        ? messageTimeMs(item.message)
        : item.kind === "command"
          ? item.record.timestamp
          : "record" in item
            ? item.record.at_ms
            : Date.now();
    return items.sort((a, b) => time(a) - time(b));
  }

  // Round events

  private handleRoundEvent(sessionId: string, event: RoundEvent) {
    if (sessionId !== this.activeSessionId) {
      if (this.activeSessionId !== null) return;
      // Session handoff: after `/new` (`ConversationCleared` carries no id)
      // the daemon rebound this attached connection to the fresh session, and
      // its id first shows up tagging that session's round events. Events for
      // *other* live sessions (e.g. a `/btw` aside streaming alongside) still
      // arrive tagged; only adopt when we hold no current session.
      this.activeSessionId = sessionId;
    }

    if (typeof event === "string") {
      if (event === "StreamStart") {
        this.streamingAssistantText = "";
        this.streamingReasoningText = "";
      } else if (event === "StreamDiscard") {
        this.streamingAssistantText = "";
        this.streamingReasoningText = "";
      }
      return;
    }

    if ("StreamDelta" in event) {
      this.streamingAssistantText += event.StreamDelta;
    } else if ("StreamReasoningDelta" in event) {
      this.streamingReasoningText += event.StreamReasoningDelta;
    } else if ("StreamReasoningEnd" in event) {
      this.streamingReasoningText = event.StreamReasoningEnd;
    } else if ("StreamEnd" in event) {
      this.commitStreamingMessage(event.StreamEnd);
    } else if ("Text" in event) {
      // Non-streamed assistant reply (fallback path, "[Interrupted]",
      // hook-blocked prompts). Emitted only when nothing streamed.
      const text = event.Text;
      if (text.trim().length > 0) {
        this.pushFeed({
          kind: "message",
          key: this.feedKey(),
          message: {
            role: "Assistant",
            content: text,
            timestamp: Math.floor(Date.now() / 1000),
            hidden: false,
          },
        });
      }
    } else if ("CommandResult" in event) {
      const { name, args, result } = event.CommandResult;
      this.pushFeed({
        kind: "command",
        key: this.feedKey(),
        record: {
          name,
          args,
          status: "Error" in result ? "error" : "success",
          result,
          timestamp: Date.now(),
        },
      });
    } else if ("ToolCall" in event) {
      const call = event.ToolCall;
      this.liveTools[call.id] = {
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: "running",
        stdout: "",
        stderr: "",
      };
    } else if ("ToolStream" in event) {
      const t = this.liveTools[event.ToolStream.id];
      if (t) {
        if ("Stdout" in event.ToolStream.stream) t.stdout += event.ToolStream.stream.Stdout;
        else t.stderr += event.ToolStream.stream.Stderr;
      }
    } else if ("ToolResult" in event) {
      const r = event.ToolResult;
      const entry = this.liveTools[r.id];
      if (entry) {
        const failed =
          typeof r.structured === "object" &&
          r.structured !== null &&
          ("Error" in r.structured || "PermissionDenied" in r.structured);
        entry.status = failed ? "failed" : "completed";
        entry.output = r.output;
        entry.durationMs = r.duration_ms;
      }
    } else if ("ToolCancelled" in event) {
      const entry = this.liveTools[event.ToolCancelled.id];
      if (entry) entry.status = "cancelled";
    } else if ("PermissionRequest" in event) {
      this.pendingPermission = { request: event.PermissionRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("UserQuestionRequest" in event) {
      this.pendingQuestion = { request: event.UserQuestionRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("StdinRequest" in event) {
      this.pendingStdin = { request: event.StdinRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("InputRequest" in event) {
      this.pendingStdin = { request: (event as { InputRequest: StdinRequest }).InputRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("TodosUpdated" in event) {
      this.todos = event.TodosUpdated;
    } else if ("ContextTokens" in event) {
      this.contextTokens = event.ContextTokens.tokens;
    } else if ("HarnessState" in event) {
      this.roundCounter = event.HarnessState.round_counter;
      this.unattended = event.HarnessState.unattended ?? false;
      this.confined = event.HarnessState.confined ?? true;
      if (event.HarnessState.loop_status === "idle") {
        this.activity = null;
        this.currentTurn = null;
      }
    } else if ("UnattendedChanged" in event) {
      this.unattended = event.UnattendedChanged;
    } else if ("ConfinementChanged" in event) {
      this.confined = event.ConfinementChanged;
    } else if ("RoundCompleted" in event) {
      this.lastRound = event.RoundCompleted;
      this.roundCounter = event.RoundCompleted.round;
      this.activity = null;
      this.currentTurn = null;
    } else if ("RoundInterrupted" in event) {
      // C11: the round stopped before completing — user interrupt, superseded
      // by newer input, or killed with the process. Append the projection row
      // and surface a toast so the stop is visible even mid-scroll.
      const record = event.RoundInterrupted;
      this.pushFeed({ kind: "interrupt", key: this.feedKey(), record });
      this.pushToast(
        "warning",
        "Round interrupted",
        `Stopped by ${interruptLabel(record.reason)}${
          record.round != null ? ` (round ${record.round})` : ""
        }.`,
      );
      this.activity = null;
      this.currentTurn = null;
    } else if ("TurnStarted" in event) {
      this.currentTurn = event.TurnStarted.turn;
      this.roundCounter = event.TurnStarted.round;
    } else if ("Activity" in event) {
      this.activity = event.Activity;
    } else if ("Compacted" in event) {
      const c = event.Compacted;
      this.pushToast(
        "info",
        "Context compacted",
        `${c.archived_messages} messages archived (${c.window_tokens_before} → ${c.window_tokens_after} tokens).`,
      );
    } else if ("RetryScheduled" in event) {
      const r = event.RetryScheduled;
      // Live retry state rides a dedicated feed row (upserted in place, like
      // the TUI's countdown entry) instead of a toast — a retry that recovers
      // is transcript-worthy, and a toast would vanish mid-scroll.
      const last = this.feed[this.feed.length - 1];
      if (last?.kind === "retry_scheduled") {
        last.attempt = r.attempt;
        last.max_attempts = r.max_attempts;
        last.delay_ms = r.delay_ms;
        last.message = r.message;
      } else {
        this.pushFeed({
          kind: "retry_scheduled",
          key: this.feedKey(),
          attempt: r.attempt,
          max_attempts: r.max_attempts,
          delay_ms: r.delay_ms,
          message: r.message,
        });
      }
    } else if ("RetryResolved" in event) {
      // ADR-0194: The round recovered: clear any live countdown row.
      // Do not inject a permanent resolution notice into the dialogue feed.
      const resolved = this.feed[this.feed.length - 1];
      if (resolved?.kind === "retry_scheduled") {
        this.feed.pop();
      }
    } else if ("SteerAdmitted" in event) {
      this.appendInsertedInput(event.SteerAdmitted);
    } else if ("FollowUpStarted" in event) {
      this.appendInsertedInput(event.FollowUpStarted);
    } else if ("SteerUnavailable" in event) {
      this.pushToast(
        "info",
        "Steer deferred",
        "The round stopped accepting steering input first; it will run next round.",
      );
    } else if ("UserInputInserted" in event) {
      this.appendInsertedInput((event as { UserInputInserted: QueuedMessage }).UserInputInserted);
    } else if ("NextRoundStarted" in event) {
      this.appendInsertedInput((event as { NextRoundStarted: QueuedMessage }).NextRoundStarted);
    } else if ("UserInputUnavailable" in event) {
      this.pushToast(
        "info",
        "Input deferred",
        "The round stopped accepting input first; it will run next round.",
      );
    } else if ("Notice" in event) {
      this.handleNotice(event.Notice);
    } else if ("Error" in event) {
      this.sessionError = event.Error;
      this.pushToast("error", "Turn error", event.Error);
    } else if ("UnsentInput" in event) {
      this.handleUnsentInput(event.UnsentInput);
    } else if ("SubagentStep" in event) {
      this.handleSubagentEvent(event.SubagentStep.parent_call_id, event.SubagentStep.event);
    }
    // UserInputCancelled / UserInputCancelFailed concern queued inserts this
    // client never issues; nothing to surface.
  }

  /**
   * A queued insert crossed the turn boundary and joined the live transcript.
   * Dedupe against the composer's optimistic echo: a Chat we painted moments
   * ago may be re-reported here when the daemon admitted it mid-round.
   */
  private appendInsertedInput(input: QueuedUserInput) {
    const text = input.display_text ?? input.text;
    const now = Date.now();
    const tail = this.feed[this.feed.length - 1];
    if (
      tail?.kind === "message" &&
      tail.message.role === "User" &&
      tail.message.content === text &&
      now - messageTimeMs(tail.message) < 5_000
    ) {
      return;
    }
    this.pushFeed({
      kind: "message",
      key: this.feedKey(),
      message: {
        role: "User",
        content: text,
        images: input.images,
        sent_at_ms: input.sent_at_ms ?? now,
        hidden: false,
      },
    });
  }

  /**
   * The round died before any output: drop the optimistic echo and offer the
   * prompt back for re-editing. Adopting into the composer happens only when
   * the composer is idle — the unsend arrives asynchronously, and in-progress
   * typing must win (same policy as the TUI's `DraftAdoption::OnlyIfIdle`).
   */
  private handleUnsentInput(unsent: { prompt: string; images: ImagePart[] }) {
    // Non-idle case keeps the echoed user message in the feed: with the
    // composer keeping the user's in-progress draft, the echo is the only
    // visible copy of the unsent prompt to copy from. (The harness still
    // reverted the conversation itself; this echo is client-side only.)
    const adopt = this.composerIdle;
    if (adopt) {
      for (let i = this.feed.length - 1; i >= 0; i--) {
        const item = this.feed[i];
        if (item.kind === "message" && item.message.role === "User") {
          if (item.message.content === unsent.prompt) this.feed.splice(i, 1);
          break;
        }
      }
    }
    if (adopt) {
      this.restoredDraft = { text: unsent.prompt, images: unsent.images };
      this.pushToast(
        "warning",
        "Prompt not sent",
        "Interrupted before any output; your prompt was restored to the composer.",
      );
    } else {
      this.pushToast(
        "warning",
        "Prompt not sent",
        "Interrupted before any output; your composer kept your draft — the unsent prompt is in the transcript above.",
      );
    }
  }

  // Subagent events (nested under a parent `task` tool call; ADR-0029)

  private handleSubagentEvent(parentCallId: string, event: SubagentEvent) {
    const parent = this.liveTools[parentCallId];
    const origin: RequestOrigin = {
      parentCallId,
      label: parent?.subagent?.profile ?? null,
    };

    if ("PermissionRequest" in event) {
      this.pendingPermission = { request: event.PermissionRequest, origin };
      return;
    }
    if ("UserQuestionRequest" in event) {
      this.pendingQuestion = { request: event.UserQuestionRequest, origin };
      return;
    }
    if ("StdinRequest" in event) {
      this.pendingStdin = { request: event.StdinRequest, origin };
      return;
    }
    if ("InputRequest" in event) {
      this.pendingStdin = { request: (event as { InputRequest: StdinRequest }).InputRequest, origin };
      return;
    }
    if (!parent) return; // stray subagent event for a tool we never saw
    if (!parent.subagent) {
      parent.subagent = {
        profile: null,
        activity: null,
        text: "",
        streamingText: "",
        reasoning: [],
        streamingReasoning: "",
        tools: [],
      };
    }
    const subagent = parent.subagent;

    if ("Started" in event) {
      subagent.profile = event.Started.profile;
    } else if ("Notice" in event) {
      this.handleNotice(event.Notice);
    } else if ("StreamStart" in event) {
      subagent.streamingText = "";
    } else if ("StreamDelta" in event) {
      subagent.streamingText += event.StreamDelta;
    } else if ("StreamEnd" in event) {
      const finalText = event.StreamEnd || subagent.streamingText;
      subagent.text = subagent.text ? `${subagent.text}\n\n${finalText}` : finalText;
      subagent.streamingText = "";
    } else if ("StreamReasoningStart" in event) {
      subagent.streamingReasoning = "";
    } else if ("StreamReasoningDelta" in event) {
      subagent.streamingReasoning += event.StreamReasoningDelta;
    } else if ("StreamReasoningEnd" in event) {
      const finalReasoning = event.StreamReasoningEnd || subagent.streamingReasoning;
      if (finalReasoning.trim()) {
        subagent.reasoning = [...subagent.reasoning, finalReasoning];
      }
      subagent.streamingReasoning = "";
    } else if ("ToolCall" in event) {
      const call = event.ToolCall;
      subagent.tools.push({
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: "running",
      });
    } else if ("ToolResult" in event) {
      const r = event.ToolResult;
      const tool = subagent.tools.find((t) => t.id === r.id);
      if (tool) {
        tool.status = "completed";
        tool.output = r.output;
        tool.durationMs = r.duration_ms;
      }
    } else if ("Activity" in event) {
      subagent.activity = event.Activity;
    }
  }

  private handleNotice(notice: AgentNotice) {
    const severity =
      notice.severity === "error" ? "error" : notice.severity === "warning" ? "warning" : "info";
    this.pushToast(severity, notice.title, notice.body);
  }

  private commitStreamingMessage(fullText: string) {
    const text = fullText.trim().length > 0 ? fullText : this.streamingAssistantText;
    if (text.trim().length > 0) {
      this.pushFeed({
        kind: "message",
        key: this.feedKey(),
        message: {
          role: "Assistant",
          content: text,
          timestamp: Math.floor(Date.now() / 1000),
          hidden: false,
        },
      });
    }
    this.streamingAssistantText = "";
    this.streamingReasoningText = "";
  }

  // Outgoing requests

  private send(req: AgentRequest) {
    if (this.sessionWs && this.sessionWs.readyState === WebSocket.OPEN) {
      this.sessionWs.send(requestFrame(req));
    } else {
      this.pushToast("warning", "Not attached", "Connect to a session first.");
    }
  }

  /** Request completion from the daemon. `cursorUtf16` is the textarea's
   * native offset; the wire uses Unicode-scalar indices across all clients. */
  public requestComposerCompletions(text: string, cursorUtf16: number) {
    const cursor = Array.from(text.slice(0, cursorUtf16)).length;
    const requestId = ++this.completionRequestId;
    this.completionRequestState = { text, cursor };
    this.composerCompletions = [];
    this.send({ CompleteComposer: { request_id: requestId, text, cursor } });
  }

  /** Legacy alias for `requestComposerCompletions`. */
  public requestInputCompletions(input: string, cursorUtf16: number) {
    this.requestComposerCompletions(input, cursorUtf16);
  }

  public sendPrompt(text: string, images: ImagePart[] = []) {
    const trimmed = text.trim();
    if (!trimmed && images.length === 0) return;
    if (trimmed.startsWith("/")) {
      this.pushFeed({
        kind: "message",
        key: this.feedKey(),
        message: { role: "User", content: trimmed, sent_at_ms: Date.now(), hidden: false },
      });
      this.send({ SlashCommand: trimmed });
      return;
    }
    this.pushFeed({
      kind: "message",
      key: this.feedKey(),
      message: {
        role: "User",
        content: trimmed,
        images: images.length > 0 ? images : undefined,
        sent_at_ms: Date.now(),
        hidden: false,
      },
    });
    // `images` is required on the wire (no serde default on the Rust field).
    this.send({ Prompt: { text: trimmed, images, sent_at_ms: Date.now() } });
  }

  /** Legacy alias for `sendPrompt`. */
  public sendChat(text: string, images: ImagePart[] = []) {
    this.sendPrompt(text, images);
  }

  public interrupt() {
    this.send({ Interrupt: null });
  }

  /** Switch the active model (and persist it as the default). */
  public setDefaultModel(id: string) {
    this.send({ SetDefaultModel: { id } });
  }

  /** Fetch the effective `[web]` configuration and provider catalog. */
  public queryWebSearchConfig() {
    this.send({ QueryWebSearchConfig: null });
  }

  /**
   * PATCH `[web]`. An empty credential value clears it; the daemon persists
   * and hot-applies one resolved snapshot, then returns authoritative state.
   */
  public updateWebSearchConfig(update: WebConfigUpdate) {
    this.send({ UpdateWebSearchConfig: update });
  }

  /** Delete a session (active or archived) by id or short-id prefix. */
  public deleteSession(id: string) {
    this.send({ DeleteSession: { id } });
  }

  /** Set a session's display title; `null` clears back to the AI/first-prompt fallback. */
  public renameSession(id: string, title: string | null) {
    this.send({ RenameSession: { id, title } });
  }

  public resolvePermission(decision: PermissionDecision) {
    if (!this.pendingPermission) return;
    this.send({
      PermissionReply: {
        request_id: this.pendingPermission.request.id,
        decision,
        parent_call_id: this.pendingPermission.origin.parentCallId,
      },
    });
    this.pendingPermission = null;
  }

  public answerQuestion(answers: string[][]) {
    if (!this.pendingQuestion) return;
    this.send({
      UserQuestionReply: {
        request_id: this.pendingQuestion.request.id,
        answers,
        parent_call_id: this.pendingQuestion.origin.parentCallId,
      },
    });
    this.pendingQuestion = null;
  }

  public replyStdin(text: string) {
    if (!this.pendingStdin) return;
    this.send({
      StdinReply: {
        request_id: this.pendingStdin.request.id,
        text,
        parent_call_id: this.pendingStdin.origin.parentCallId,
      },
    });
    this.pendingStdin = null;
  }

  /** Legacy alias for `replyStdin`. */
  public replyInput(text: string) {
    this.replyStdin(text);
  }

  /** Consume the restored composer draft (one-shot). */
  public takeRestoredDraft(): { text: string; images: ImagePart[] } | null {
    const draft = this.restoredDraft;
    this.restoredDraft = null;
    return draft;
  }

  // Misc

  public pushToast(severity: Toast["severity"], title: string, body?: string) {
    const toast: Toast = { id: this.nextToastId++, severity, title, body };
    this.toasts.push(toast);
    const keep = 4;
    if (this.toasts.length > keep) this.toasts.splice(0, this.toasts.length - keep);
    window.setTimeout(() => this.dismissToast(toast.id), 8000);
  }

  public dismissToast(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  private feedKey(): string {
    return `f${this.nextFeedKey++}`;
  }

  private pushFeed(item: FeedItem) {
    this.feed.push(item);
  }

  private messageItem(message: Message): FeedItem {
    return { kind: "message", key: this.feedKey(), message };
  }

  private commandItem(record: CommandRecord): FeedItem {
    return { kind: "command", key: this.feedKey(), record };
  }

  private interruptItem(record: RoundInterrupt): FeedItem {
    return { kind: "interrupt", key: this.feedKey(), record };
  }

  private retryResolutionItem(record: RetryResolution): FeedItem {
    return { kind: "retry_resolution", key: this.feedKey(), record };
  }

  private clearSessionState() {
    this.cancelSessionReconnect();
    this.activeSessionId = null;
    this.sessionAttached = false;
    this.feed = [];
    this.streamingAssistantText = "";
    this.streamingReasoningText = "";
    this.liveTools = {};
    this.todos = { items: [], next_id: 1, updated_at_round: 0 };
    this.contextTokens = null;
    this.roundCounter = 0;
    this.unattended = false;
    this.confined = true;
    this.activity = null;
    this.currentTurn = null;
    this.lastRound = null;
    this.providerInfo = null;
    this.providerPicker = null;
    this.providerKeys = [];
    this.commandCatalog = { commands: [], aliases: [], suggestions: [] };
    this.inputCompletions = [];
    this.completionRequestId += 1;
    this.completionRequestState = null;
    this.pendingPermission = null;
    this.pendingQuestion = null;
    this.pendingInput = null;
    this.sessionError = null;
    this.restoredDraft = null;
  }

  private detachSocketHandlers(ws: WebSocket) {
    ws.onopen = null;
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
  }
}

export const daemon = new DaemonStore();
