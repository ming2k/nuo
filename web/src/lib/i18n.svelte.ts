/**
 * Minimal i18n for the 山水 web client.
 *
 * Two locales: `en` (default, the lingua franca) and `zh` (中文). The
 * initial locale resolves from localStorage, then the browser language;
 * the header offers a manual toggle.
 *
 * Deliberately NOT translated: seal glyphs (问/答/危/许/入/记/空), theme
 * proper names (宣纸/夜山), and the brand mark — they are design language,
 * not copy, the way Japanese design carries kanji. Where a glyph needs to
 * be understood rather than admired, an accessible label or tooltip from
 * the table accompanies it.
 *
 * `t()` keys are flat; values are exact strings (no printf — interpolations
 * use template functions where needed).
 */

export type Locale = "en" | "zh";

const STORAGE_KEY = "muta.locale";

const en = {
  // App / empty state
  connecting: "Connecting to the session daemon…",
  startPrompting: "Write below to begin a turn.",
  selectSession: "Pick or create a session from the sidebar.",
  retrying: "retrying",

  // Sidebar
  newSession: "New Session",
  sessionsTitle: "Sessions",
  noSessions: "No active sessions",
  renameSession: "Rename session",
  interruptRound: "Interrupt the current round",
  suspendSession: "Suspend session (park in memory; attaching again resumes it)",
  endSession: "End session (keeps history; removes it from the daemon)",
  deleteSession: "Delete session",
  confirmDelete: "Confirm?",
  connectionSettings: "Connection settings",
  online: "online",
  connectingShort: "…",
  offline: "offline",

  // Header
  noActiveSession: "No active session",
  round: "round",
  turn: "turn",
  ctx: "ctx",
  unattended: "unattended",
  unattendedTip: "Unattended: permission prompts are bypassed",
  unconfined: "unconfined",
  unconfinedTip: "Unconfined: host-wide file access enabled",
  switchModel: "Switch model",
  webSearchSettings: "Web search backend & reader settings",
  toLightTheme: "Switch to light theme (宣纸)",
  toDarkTheme: "Switch to dark theme (夜山)",
  interrupt: "Interrupt",
  toEnglish: "Switch to English",
  toChinese: "切换到中文",

  // Composer
  composerPlaceholder: "Write… (Enter to send · Shift+Enter for a newline)",
  attachImage: "Attach image (or paste)",
  sendMessage: "Send message",
  removeImage: "Remove image",
  commandCompletions: "Command completions",

  // Permission / question / input banners
  permissionElevated: "elevated",
  permissionAsk: "ask",
  permissionTitle: "permission",
  inputTitle: "input",
  agentNeedsAnswer: "The agent needs your answer",
  otherOption: "Other…",
  otherPlaceholder: "Type a free-text answer",
  answer: "Answer",
  cancel: "Cancel",
  send: "Send",
  secretInput: "secret input",
  commandInput: "input for the command",
  questionNeedsAnswer: (n: number) =>
    n === 1
      ? "Question 1 needs an answer — pick an option, write an Other answer, or cancel."
      : "Questions need answers — pick options, write Other answers, or cancel.",
  allowOnce: "Allow once",
  alwaysAllow: "Always allow",
  reject: "Reject",

  // Tool cards
  running: "running",
  failed: "failed",
  cancelled: "cancelled",
  done: "done",
  arguments: "Arguments",
  liveOutput: "Live output",
  result: "Result",
  subagent: "subagent",
  working: "working…",
  toolsRunning: (n: number) => `${n} tool${n > 1 ? "s" : ""} running`,
  thinking: "thinking",

  // Command blocks
  error: "error",

  // Message roles (accessible labels for the seal glyphs)
  roleYou: "You",
  roleAssistant: "Muta",
  roleSystem: "notification",
  roleTool: "tool",
  subagentTranscript: (n: number) => `subagent transcript (${n})`,

  // Todos
  tasks: "Tasks",

  // Toasts
  dismiss: "Dismiss",
  clientError: "Client error",
  imageTooLarge: "Image too large",
  imageTooLargeBody: (name: string) => `${name} exceeds 10 MB.`,
  dismissToast: "Dismiss",

  // Model picker
  models: "Models",
  modelsUnavailable: "Model list not available — attach to a session first.",
  noProviders: "No providers configured.",
  noApiKey: "no key",
  locked: "locked",
  overridden: "overridden by you",
  current: "current",
  effort: "effort",
  modelsFooter: "Switching sets the default model, mirroring the TUI's Models picker.",
  close: "Close",

  // Connection dialog
  connection: "Connection",
  daemonUrl: "Daemon WebSocket URL",
  daemonUrlHint:
    "The daemon listens on ws://127.0.0.1:9800 by default (falling back to an ephemeral port, recorded in the discovery file, when 9800 is taken).",
  bearerToken: "Bearer token",
  bearerTokenPlaceholder: "required when the daemon has auth on",
  bearerTokenHint:
    "Daemons started with default settings require a token (local_auth). Find it in the discovery file $XDG_RUNTIME_DIR/muta/daemon.json — it is sent as a bearer. subprotocol because browsers cannot set WebSocket headers.",
  projectPath: "Project path (optional)",
  projectPathHint: (root: string) =>
    `Scopes session creation and monitoring to this project. Empty uses the daemon's own project root${root ? ` (${root})` : ""}.`,
  projectPathPlaceholder: "/path/to/project",
  status: "Status",
  authRequired: "This daemon requires a bearer token — read it from the discovery file (see below).",
  authEnabled: "Daemon reachable; auth enabled.",
  daemonReachable: (v: string) => `Daemon reachable (v${v}); no auth required.`,
  cancelBtn: "Cancel",
  saveReconnect: "Save & reconnect",

  // Web search dialog
  webSearch: "Web search",
  loadingConfig: "Loading configuration…",
  searchBackend: "Search backend",
  searchBackendHint:
    "Used by the websearch tool. Changes apply live and persist to config.toml.",
  pageReader: "Page reader",
  pageReaderHint:
    "How read_url converts HTML pages to text using Jina Reader (server-side JS rendering and readability extraction).",
  timeout: "Timeout",
  searxngEndpoint: "SearXNG endpoint",
  searxngUrlLabel: "JSON search URL",
  searxngRequired: "Required — a backend is set to searxng.",
  searxngOptional: "Only used when a backend is searxng.",
  apiKeys: "API keys",
  apiKeysHint:
    "Persist to credentials.toml (never config.toml). Existing keys are never echoed back — only whether they are set. Submit an empty field as a no-op; use the ✕ button to clear a stored key.",
  keySet: "set",
  keyRequiredWhenSelected: "required when selected",
  keyUnchanged: "(unchanged)",
  keyNotSet: "not set",
  clearStoredKey: "Clear the stored key",
  webSearchFooterNote: "Backend/reader/timeout apply immediately; text fields need Save.",
  save: "Save",
  backendTag: "websearch",
  readerTag: "read_url",
};

/** Dictionary shape: template entries widened so locale implementations
 *  only need to return strings, not literal unions. */
type TemplateKeys =
  | "questionNeedsAnswer"
  | "toolsRunning"
  | "subagentTranscript"
  | "imageTooLargeBody"
  | "projectPathHint"
  | "daemonReachable";

type WidenTemplates = {
  [K in keyof typeof en]: K extends TemplateKeys
    ? typeof en[K] extends (...args: infer A) => unknown
      ? (...args: A) => string
      : typeof en[K]
    : typeof en[K];
};

export type Dict = WidenTemplates;

const zh: Dict = {
  connecting: "正在连接 session daemon…",
  startPrompting: "于下方落笔，开始一轮对话。",
  selectSession: "从侧栏选择或新建一个会话。",
  retrying: "重试中",

  newSession: "新会话",
  sessionsTitle: "会话",
  noSessions: "尚无活动会话",
  renameSession: "重命名会话",
  interruptRound: "中断当前轮次",
  suspendSession: "挂起会话（驻留内存；重新附加即恢复）",
  endSession: "结束会话（保留历史；从 daemon 移除）",
  deleteSession: "删除会话",
  confirmDelete: "确认？",
  connectionSettings: "连接设置",
  online: "在线",
  connectingShort: "…",
  offline: "离线",

  noActiveSession: "无活动会话",
  round: "轮",
  turn: "回合",
  ctx: "上下文",
  unattended: "无人值守",
  unattendedTip: "无人值守：权限请求将被自动放行",
  unconfined: "无沙箱",
  unconfinedTip: "无沙箱：已启用全主机文件访问",
  switchModel: "切换模型",
  webSearchSettings: "网页搜索后端与阅读器设置",
  toLightTheme: "切换到亮色主题（宣纸）",
  toDarkTheme: "切换到暗色主题（夜山）",
  interrupt: "中断",
  toEnglish: "Switch to English",
  toChinese: "切换到中文",

  composerPlaceholder: "落笔…（Enter 发送 · Shift+Enter 换行）",
  attachImage: "附加图片（或直接粘贴）",
  sendMessage: "发送消息",
  removeImage: "移除图片",
  commandCompletions: "命令补全",

  permissionElevated: "提权",
  permissionAsk: "请求",
  permissionTitle: "权限",
  inputTitle: "输入",
  agentNeedsAnswer: "代理需要你的回答",
  otherOption: "其他…",
  otherPlaceholder: "输入自定义回答",
  answer: "回答",
  cancel: "取消",
  send: "发送",
  secretInput: "机密输入",
  commandInput: "命令输入",
  questionNeedsAnswer: (n: number): string =>
    n === 1
      ? "第 1 个问题尚未回答——请选择选项、填写“其他”，或取消。"
      : "尚有问题未回答——请选择选项、填写“其他”，或取消。",
  allowOnce: "允许一次",
  alwaysAllow: "始终允许",
  reject: "拒绝",

  running: "运行中",
  failed: "失败",
  cancelled: "已取消",
  done: "完成",
  arguments: "参数",
  liveOutput: "实时输出",
  result: "结果",
  subagent: "subagent",
  working: "工作中…",
  toolsRunning: (n: number) => `${n} 个工具运行中`,
  thinking: "思考中",

  error: "错误",

  roleYou: "你",
  roleAssistant: "Muta",
  roleSystem: "通知",
  roleTool: "工具",
  subagentTranscript: (n: number) => `subagent 记录（${n}）`,

  tasks: "任务",

  dismiss: "关闭",
  clientError: "客户端错误",
  imageTooLarge: "图片过大",
  imageTooLargeBody: (name: string) => `${name} 超过 10 MB。`,
  dismissToast: "关闭",

  models: "模型",
  modelsUnavailable: "模型列表不可用——请先附加到一个会话。",
  noProviders: "未配置任何提供方。",
  noApiKey: "无密钥",
  locked: "已锁定",
  overridden: "已被你覆盖",
  current: "当前",
  effort: "强度",
  modelsFooter: "切换将设置默认模型，与 TUI 的模型选择器一致。",
  close: "关闭",

  connection: "连接",
  daemonUrl: "Daemon WebSocket 地址",
  daemonUrlHint:
    "daemon 默认监听 ws://127.0.0.1:9800（端口被占用时回退到临时端口，记录在发现文件中）。",
  bearerToken: "Bearer 令牌",
  bearerTokenPlaceholder: "daemon 开启认证时必填",
  bearerTokenHint:
    "默认配置启动的 daemon 要求令牌（local_auth）。请查阅发现文件 $XDG_RUNTIME_DIR/muta/daemon.json——浏览器无法设置 WebSocket 头，令牌以 bearer. 子协议发送。",
  projectPath: "项目路径（可选）",
  projectPathHint: (root: string) =>
    `限定会话创建与监控的项目范围。留空使用 daemon 自身的项目根目录${root ? `（${root}）` : ""}。`,
  projectPathPlaceholder: "/path/to/project",
  status: "状态",
  authRequired: "此 daemon 要求 bearer 令牌——请从发现文件中读取（见下）。",
  authEnabled: "daemon 可达；已启用认证。",
  daemonReachable: (v: string) => `daemon 可达（v${v}）；无需认证。`,
  cancelBtn: "取消",
  saveReconnect: "保存并重连",

  webSearch: "网页搜索",
  loadingConfig: "正在加载配置…",
  searchBackend: "搜索后端",
  searchBackendHint: "由 websearch 工具使用。更改即时生效并持久化到 config.toml。",
  pageReader: "网页阅读器",
  pageReaderHint:
    "read_url 使用 Jina Reader 将 HTML 页面转换为文本（服务端 JS 渲染与正文抽取）。",
  timeout: "超时",
  searxngEndpoint: "SearXNG 端点",
  searxngUrlLabel: "JSON 搜索地址",
  searxngRequired: "必填——当前后端为 searxng。",
  searxngOptional: "仅在后端为 searxng 时使用。",
  apiKeys: "API 密钥",
  apiKeysHint:
    "持久化到 credentials.toml（绝不写入 config.toml）。已存的密钥不会回显——只显示是否已设置。留空提交表示不修改；用 ✕ 按钮清除已存密钥。",
  keySet: "已设置",
  keyRequiredWhenSelected: "选中时必填",
  keyUnchanged: "（未修改）",
  keyNotSet: "未设置",
  clearStoredKey: "清除已存密钥",
  webSearchFooterNote: "后端/阅读器/超时立即生效；文本字段需保存。",
  save: "保存",
  backendTag: "websearch",
  readerTag: "read_url",
};

const tables: Record<Locale, Dict> = { en, zh };

/** Reactive locale — a rune, so any component reading `t()` re-renders. */
let current: Locale = $state("en");

export function resolveInitialLocale(): Locale {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "en" || stored === "zh") return stored;
  } catch {
    // No storage — fall through to the browser language.
  }
  return navigator.language?.toLowerCase().startsWith("zh") ? "zh" : "en";
}

export function getLocale(): Locale {
  return current;
}

export function setLocale(locale: Locale): void {
  if (locale === current) return;
  current = locale;
  document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
  try {
    localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // Best effort; the in-memory locale already took effect.
  }
}

export function toggleLocale(): Locale {
  const next: Locale = current === "en" ? "zh" : "en";
  setLocale(next);
  return next;
}

/** Translate: `t("key")` or, for parameterized entries, `t("key")(arg)`.
 *  Reads the reactive `current`, so callers re-render on locale change. */
export function t<K extends keyof Dict>(key: K): Dict[K] {
  return tables[current][key];
}

/** Called once before mount (main.ts) to pin <html lang> and the locale. */
export function initLocale(): void {
  current = resolveInitialLocale();
  document.documentElement.lang = current === "zh" ? "zh-CN" : "en";
}
