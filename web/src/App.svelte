<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "./lib/stores/daemon.svelte.js";
  import { resolveInitialTheme, toggleTheme, type Theme } from "./lib/theme.js";
  import { t } from "./lib/i18n.svelte.js";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import ChatHeader from "./lib/components/ChatHeader.svelte";
  import MessageItem from "./lib/components/MessageItem.svelte";
  import CommandBlock from "./lib/components/CommandBlock.svelte";
  import InterruptMarker from "./lib/components/InterruptMarker.svelte";
  import RetryMarker from "./lib/components/RetryMarker.svelte";
  import ToolCard from "./lib/components/ToolCard.svelte";
  import Composer from "./lib/components/Composer.svelte";
  import PermissionBanner from "./lib/components/PermissionBanner.svelte";
  import ToastStack from "./lib/components/ToastStack.svelte";
  import TodoPanel from "./lib/components/TodoPanel.svelte";
  import ModelPicker from "./lib/components/ModelPicker.svelte";
  import ConnectionDialog from "./lib/components/ConnectionDialog.svelte";
  import WebSearchDialog from "./lib/components/WebSearchDialog.svelte";

  let transcriptEl: HTMLElement;
  let autoScroll = $state(true);
  let sidebarOpen = $state(false);
  let modelsOpen = $state(false);
  let connectionOpen = $state(false);
  let webSearchOpen = $state(false);
  let theme = $state<Theme>(resolveInitialTheme());

  onMount(() => {
    // Surface unexpected client errors instead of dying silently.
    const onError = (event: Event) => {
      const message =
        event instanceof ErrorEvent
          ? event.message
          : (event as PromiseRejectionEvent).reason?.toString?.() ?? "unknown error";
      daemon.pushToast("error", t("clientError"), message);
    };
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onError);

    daemon.init();
    // First-impression credential check (ADR-0105): probe the daemon's
    // `/healthz` immediately. When the daemon requires a bearer token and we
    // have none, do not wait out the "maybe it will connect" timer — open the
    // connection dialog right away with the auth hint visible. A stored
    // token that the daemon has since rotated (its restart regenerates the
    // token) still surfaces here as disconnected+auth-required: the probe
    // says what the socket cannot.
    void daemon.probe().then((probe) => {
      if (probe?.auth && !daemon.token) connectionOpen = true;
    });

    // Open the connection dialog when there is nothing to talk to yet.
    const openTimer = window.setTimeout(() => {
      if (daemon.connection !== "connected") connectionOpen = true;
    }, 4000);

    return () => {
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onError);
      window.clearTimeout(openTimer);
    };
  });

  $effect(() => {
    // First-impression credential challenge (ADR-0105): the store raises
    // `credentialChallenge` when the daemon requires a token and the present
    // one was rejected — open the connection dialog immediately so the user
    // never stares at a silently reconnecting badge.
    if (daemon.credentialChallenge) connectionOpen = true;
  });

  $effect(() => {
    // Track transcript growth (feed length + streaming length) for
    // auto-scroll, cleaning up any in-flight scroll on re-run.
    void daemon.feed.length;
    void daemon.streamingAssistantText.length;
    void Object.keys(daemon.liveTools).length;
    const el = transcriptEl;
    if (!el || !autoScroll) return;
    const timer = window.setTimeout(() => {
      el.scrollTop = el.scrollHeight;
    }, 10);
    return () => window.clearTimeout(timer);
  });

  function handleScroll() {
    if (!transcriptEl) return;
    autoScroll =
      transcriptEl.scrollHeight - transcriptEl.scrollTop - transcriptEl.clientHeight < 80;
  }

  /** Theme is resolved before mount; onMount just re-syncs in case it drifted. */
  $effect(() => {
    document.documentElement.dataset.theme = theme;
  });
</script>

<div class="layout">
  <Sidebar
    open={sidebarOpen}
    onClose={() => (sidebarOpen = false)}
    onOpenConnection={() => (connectionOpen = true)}
  />

  <main class="main">
    <ChatHeader
      onToggleSidebar={() => (sidebarOpen = !sidebarOpen)}
      onOpenModels={() => (modelsOpen = true)}
      onOpenWebSearch={() => (webSearchOpen = true)}
      theme={theme}
      onToggleTheme={() => (theme = toggleTheme(theme))}
    />

    <section class="transcript" bind:this={transcriptEl} onscroll={handleScroll}>
      {#if daemon.feed.length === 0 && !daemon.streamingAssistantText && Object.keys(daemon.liveTools).length === 0}
        <div class="empty-hero">
          <!-- 朱砂印 + 远山: the seal above a distant-ridge line, pure 留白 around. -->
          <img class="hero-seal" src="/logo-seal.png" alt="" draggable="false" />
          <svg class="ridges" viewBox="0 0 320 120" fill="none" aria-hidden="true">
            <path
              class="ridge far"
              d="M0 96 C 48 84 76 58 108 58 C 142 58 158 82 190 78 C 226 74 240 40 272 40 C 292 40 306 54 320 62 L 320 120 L 0 120 Z"
            />
            <path
              class="ridge near"
              d="M0 112 C 40 108 70 92 104 94 C 146 96 168 108 208 104 C 248 100 268 82 320 88 L 320 120 L 0 120 Z"
            />
          </svg>
          <h3 class="hero-title">Muta</h3>
          <p class="hero-line">
            {#if daemon.connection !== "connected"}
              {t("connecting")}
            {:else if daemon.sessionAttached}
              {t("startPrompting")}
            {:else}
              {t("selectSession")}
            {/if}
          </p>
          {#if daemon.sessionError}
            <p class="error-line">{daemon.sessionError}</p>
          {/if}
        </div>
      {:else}
        <div class="thread">
          {#each daemon.feed as item (item.key)}
            {#if item.kind === "message"}
              <MessageItem message={item.message} />
            {:else if item.kind === "interrupt"}
              <InterruptMarker record={item.record} />
            {:else if item.kind === "retry_resolution"}
              <RetryMarker record={item.record} />
            {:else if item.kind === "retry_scheduled"}
              <div class="retry-live" role="status">
                <span class="glyph">↻</span>
                <span class="label">{t("retrying")}</span>
                <span class="detail"
                  >{item.attempt}/{item.max_attempts} · {item.message}</span
                >
              </div>
            {:else}
              <CommandBlock record={item.record} />
            {/if}
          {/each}

          <!-- Active Tool Executions -->
          {#each Object.values(daemon.liveTools) as tool (tool.id)}
            <ToolCard {tool} />
          {/each}

          <!-- Streaming Assistant Text -->
          {#if daemon.streamingAssistantText || daemon.streamingReasoningText}
            <div class="streaming-block">
              {#if daemon.streamingReasoningText}
                <details class="reasoning">
                  <summary>thinking…</summary>
                  <pre>{daemon.streamingReasoningText}</pre>
                </details>
              {/if}
              {#if daemon.streamingAssistantText}
                <div class="stream-text">{daemon.streamingAssistantText}</div>
              {/if}
            </div>
          {/if}
        </div>
      {/if}
    </section>

    <TodoPanel />

    <PermissionBanner />

    <Composer />
  </main>
</div>

{#if modelsOpen}
  <ModelPicker onclose={() => (modelsOpen = false)} />
{/if}

{#if connectionOpen}
  <ConnectionDialog onclose={() => (connectionOpen = false)} />
{/if}

{#if webSearchOpen}
  <WebSearchDialog onclose={() => (webSearchOpen = false)} />
{/if}

<ToastStack />

<style>
  /* ————— 骨架 ————— */
  .layout {
    display: flex;
    height: 100vh;
    height: 100dvh;
    width: 100vw;
    background-color: var(--bg-app);
  }

  .main {
    flex: 1;
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    min-width: 0;
  }

  /* The transcript is the 留白: a narrow centered column floating on the
     paper, generous padding top and bottom. */
  .transcript {
    flex: 1;
    overflow-y: auto;
    padding: 3rem var(--pad-x) 4rem;
    display: flex;
    flex-direction: column;
    scroll-behavior: smooth;
  }

  .thread {
    width: min(var(--measure), 100%);
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    flex: 1;
  }

  /* ————— 空态 · 远山 ————— */
  .empty-hero {
    margin: auto;
    text-align: center;
    max-width: 420px;
    padding-bottom: 8vh;
  }

  .hero-seal {
    width: 64px;
    height: 64px;
    object-fit: contain;
    margin: 0 auto 0.75rem;
    display: block;
    user-select: none;
    opacity: 0.92;
  }

  .ridges {
    width: min(300px, 70%);
    margin: 0 auto 1.25rem;
    display: block;
    overflow: visible;
  }

  .ridge {
    stroke-linecap: round;
    fill: none;
    transition: stroke var(--t-slow);
  }

  .ridge.far {
    stroke: var(--border-strong);
    stroke-width: 1.5;
  }

  .ridge.near {
    stroke: var(--text-muted);
    stroke-width: 2;
    opacity: 0.7;
  }

  .hero-title {
    font-family: var(--font-brush);
    font-size: 1.6rem;
    font-weight: 600;
    letter-spacing: 0.35em;
    /* Drop the trailing letter-spacing so the word centers optically. */
    margin-right: -0.35em;
    margin-bottom: 0.75rem;
    color: var(--text-primary);
  }

  .hero-line {
    font-size: 0.85rem;
    color: var(--text-muted);
    letter-spacing: 0.08em;
  }

  .hero-line + .error-line {
    margin-top: 0.75rem;
  }

  .error-line {
    color: var(--accent-danger);
    font-family: var(--font-mono);
    font-size: 0.75rem;
    word-break: break-word;
  }

  /* ————— 事件行 (retry-live) ————— */
  .retry-live {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0.3rem 0;
    font-size: 0.82rem;
    color: var(--text-muted);
  }

  .retry-live .glyph,
  .retry-live .label {
    color: var(--accent-warning);
    font-weight: 600;
  }

  .retry-live .detail {
    color: var(--text-secondary);
    overflow-wrap: anywhere;
  }

  /* ————— 流式块 ————— */
  .streaming-block {
    width: 100%;
    padding: 0.25rem 0 1rem;
  }

  .reasoning {
    border-left: 2px solid var(--line-strong);
    padding-left: 0.7rem;
    margin-bottom: 0.5rem;
  }

  .reasoning summary {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    cursor: pointer;
    letter-spacing: 0.05em;
  }

  .reasoning pre {
    font-size: 0.72rem;
    color: var(--text-muted);
    white-space: pre-wrap;
    max-height: 160px;
    overflow-y: auto;
    margin: 0.25rem 0 0;
  }

  .stream-text {
    color: var(--text-primary);
    line-height: 1.75;
    font-size: 0.95rem;
    white-space: pre-wrap;
    word-break: break-word;
  }

  @media (max-width: 900px) {
    .transcript {
      padding: 1.5rem 1rem 2rem;
    }

    .hero-title {
      font-size: 1.35rem;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .transcript {
      scroll-behavior: auto;
    }
  }
</style>
