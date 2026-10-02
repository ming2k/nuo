<script lang="ts">
  import type { Message } from "../types.js";
  import { renderMarkdown } from "../markdown.js";
  import { t } from "../i18n.svelte.js";
  import Self from "./MessageItem.svelte";

  interface Props {
    message: Message;
  }

  let { message, compact = false }: { message: Message; compact?: boolean } = $props();

  let htmlContent = $derived(
    message.role === "Tool"
      ? ""
      : renderMarkdown(message.display_content ?? message.content ?? ""),
  );

  let timeLabel = $derived.by(() => {
    const ms = message.sent_at_ms ?? (message.timestamp ? message.timestamp * 1000 : null);
    if (!ms) return null;
    return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  });

  let roleLabel = $derived(
    message.role === "User"
      ? t("roleYou") === "你" ? "问" : "You"
      : message.role === "Tool"
        ? "tool"
        : message.role === "System"
          ? t("roleSystem") === "通知" ? "记" : "•"
          : t("roleAssistant") === "Muta" ? "答" : "Muta",
  );

  let isUser = $derived(message.role === "User");
</script>

{#if message.role === "Tool"}
  <div class="tool-result-message">
    <span class="role-tag mono">{roleLabel}</span>
    <pre class="tool-output">{message.content}</pre>
  </div>
{:else}
  <article class="entry" class:user={isUser} class:compact>
    <header class="entry-head">
      <span class="role-seal" aria-hidden="true">{roleLabel}</span>
      {#if timeLabel}
        <span class="time-tag">{timeLabel}</span>
      {/if}
      <span class="head-rule" aria-hidden="true"></span>
    </header>

    {#if message.reasoning_content}
      <details class="reasoning">
        <summary>thinking</summary>
        <pre>{message.reasoning_content}</pre>
      </details>
    {/if}

    {#if message.images && message.images.length > 0}
      <div class="message-images">
        {#each message.images as image, i (i)}
          <img class="message-image" src="data:{image.mime};base64,{image.data}" alt="attached" />
        {/each}
      </div>
    {/if}

    <div class="message-body markdown-body">
      {@html htmlContent}
    </div>

    {#if message.children && message.children.length > 0}
      <details class="subagent-children">
        <summary>{t("subagentTranscript")(message.children.length)}</summary>
        <div class="subagent-inner">
          {#each message.children as child, i (i)}
            <Self message={child} compact={true} />
          {/each}
        </div>
      </details>
    {/if}
  </article>
{/if}

<style>
  /* ————— Dialogue entries, not bubbles —————
     Each turn is a column of ink on paper: a small brush-character role
     seal, a hairline stretching to the right margin, then the body.
     User turns indent from the right edge with cinnabar accents;
     assistant turns sit flush-left in plain ink. 留白 does the work. */

  .entry {
    display: flex;
    flex-direction: column;
    margin-bottom: 1.75rem;
    content-visibility: auto;
    animation: settle 0.25s var(--ease);
  }

  /* Both voices share one left-aligned column — like a handscroll, question
     and answer run in the same ink line. The role seal alone (cinnabar 问
     vs. muted 答) marks the speaker; indenting would only steal measure
     from code blocks and break the axis of the page. */

  .entry-head {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    margin-bottom: 0.45rem;
  }

  .role-seal {
    font-family: var(--font-brush);
    font-size: 0.8rem;
    line-height: 1;
    width: 1.45rem;
    height: 1.45rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 3px;
    flex-shrink: 0;
    color: var(--text-secondary);
    border: 1px solid var(--line-strong);
  }

  .entry.user .role-seal {
    color: var(--accent-primary);
    border-color: var(--accent-primary);
    background: var(--seal-soft);
  }

  .time-tag {
    font-size: 0.68rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }

  /* The hairline that lets the eye rest: 1px rule fading out to the right. */
  .head-rule {
    flex: 1;
    height: 1px;
    background: linear-gradient(to right, var(--line), transparent);
  }

  .message-body {
    color: var(--text-primary);
    font-size: 0.95rem;
    padding-left: 2rem;
  }

  /* The visitor's question reads a shade quieter than the answer's ink,
     but keeps the same column. */
  .entry.user .message-body {
    color: var(--text-secondary);
  }
  .message-images {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin: 0.25rem 0 0.5rem;
    padding-left: 2rem;
  }

  .message-image {
    max-width: 240px;
    max-height: 180px;
    border-radius: var(--radius-md);
    border: 1px solid var(--line);
    object-fit: cover;
  }

  .reasoning {
    border-left: 2px solid var(--line-strong);
    padding-left: 0.7rem;
    margin: 0 0 0.5rem 2rem;
  }

  .reasoning summary {
    font-size: 0.7rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    cursor: pointer;
  }

  .reasoning pre {
    font-size: 0.72rem;
    color: var(--text-muted);
    white-space: pre-wrap;
    max-height: 160px;
    overflow-y: auto;
    margin: 0.25rem 0 0;
  }

  .tool-result-message {
    margin-bottom: 0.9rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .role-tag.mono {
    font-family: var(--font-mono);
    font-size: 0.65rem;
    color: var(--text-muted);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .tool-output {
    background-color: var(--bg-code);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    padding: 0.6rem 0.75rem;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 200px;
    overflow-y: auto;
    margin: 0;
  }

  .subagent-children {
    margin: 0.5rem 0 0 2rem;
    border-left: 2px solid var(--line);
    padding-left: 0.7rem;
  }

  .subagent-children summary {
    font-size: 0.7rem;
    color: var(--text-muted);
    cursor: pointer;
    font-family: var(--font-mono);
  }

  .subagent-inner {
    padding-top: 0.35rem;
  }

  @keyframes settle {
    from {
      opacity: 0;
      transform: translateY(3px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .entry {
      animation: none;
    }
  }
</style>
