<script lang="ts">
  import type { SubagentExecution, LiveToolExecution } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";

  interface Props {
    tool: LiveToolExecution;
  }

  let { tool }: Props = $props();

  /* UX (borrowed from opencode's session-turn pattern): tools collapse by
     default so long rounds stay scannable — the header row carries a live
     one-line preview while running, and the full transcript opens on demand.
     A tool that is still running, or that failed, opens itself so attention
     goes where it is needed. */
  let expanded = $state(false);
  let subagentExpanded = $state(false);
  let userToggled = $state(false);

  $effect(() => {
    // Auto-open on failure only if the user has not expressed a preference.
    if (!userToggled && tool.status === "failed") expanded = true;
  });

  function toggle() {
    userToggled = true;
    expanded = !expanded;
  }

  let statusLabel = $derived.by(() => {
    switch (tool.status) {
      case "running":
        return `${t("running")}…`;
      case "failed":
        return `${t("failed")} · ${tool.durationMs ?? 0}ms`;
      case "cancelled":
        return t("cancelled");
      default:
        return `${t("done")} · ${tool.durationMs ?? 0}ms`;
    }
  });

  /* One-line preview for the collapsed header: the first non-blank line of
     live output, or of the arguments when nothing has streamed yet. */
  let headPreview = $derived.by(() => {
    const source =
      (tool.status === "running" ? tool.stdout || tool.stderr : "") || tool.arguments;
    const line = source
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0);
    if (!line) return "";
    return line.length > 72 ? `${line.slice(0, 72)}…` : line;
  });

  let livePreview = $derived(
    tool.status === "running" ? tool.stdout || tool.stderr : "",
  );

  function subagentSummary(subagent: SubagentExecution): string {
    const parts: string[] = [];
    if (subagent.profile) parts.push(subagent.profile);
    if (subagent.activity) parts.push(subagent.activity);
    const running = subagent.tools.filter((t2) => t2.status === "running").length;
    if (running > 0) parts.push(t("toolsRunning")(running));
    return parts.join(" · ") || t("working");
  }
</script>

<div class="tool-card" class:running={tool.status === "running"}>
  <button class="tool-header" onclick={toggle} aria-expanded={expanded}>
    <span class="glyph" aria-hidden="true">⚡</span>
    <span class="name">{tool.name}</span>
    {#if !expanded && headPreview}
      <span class="preview">{headPreview}</span>
    {/if}
    <span class="spacer"></span>
    <span class="tool-badge status-{tool.status}">{statusLabel}</span>
    <span class="chevron" aria-hidden="true">{expanded ? "−" : "+"}</span>
  </button>

  {#if expanded}
    <div class="tool-content">
      <div class="block">
        <div class="label">{t("arguments")}</div>
        <pre>{tool.arguments}</pre>
      </div>
      {#if tool.status === "running" && livePreview}
        <div class="block">
          <div class="label">{t("liveOutput")}</div>
          <pre class="stream">{livePreview}</pre>
        </div>
      {/if}
      {#if tool.stdout}
        <div class="block">
          <div class="label">stdout</div>
          <pre>{tool.stdout}</pre>
        </div>
      {/if}
      {#if tool.stderr}
        <div class="block">
          <div class="label">stderr</div>
          <pre class="err">{tool.stderr}</pre>
        </div>
      {/if}
      {#if tool.output}
        <div class="block">
          <div class="label">{t("result")}</div>
          <pre class="output">{tool.output}</pre>
        </div>
      {/if}

      {#if tool.subagent}
        {@const subagent = tool.subagent}
        <div class="subagent-block">
          <button class="subagent-header" onclick={() => (subagentExpanded = !subagentExpanded)}>
            <span class="subagent-icon">⎇</span>
            <span class="subagent-title">subagent — {subagentSummary(subagent)}</span>
            <span class="chevron">{subagentExpanded ? "−" : "+"}</span>
          </button>
          {#if subagentExpanded}
            <div class="subagent-content">
              {#each subagent.tools as sub (sub.id)}
                <div class="subagent-tool">
                  <div class="subagent-tool-head">
                    <span class="name sub-name">{sub.name}</span>
                    <span class="sub-status status-{sub.status}">
                      {sub.status === "running" ? `${t("running")}…` : `${t("done")} (${sub.durationMs ?? 0}ms)`}
                    </span>
                  </div>
                  {#if sub.output}
                    <pre>{sub.output}</pre>
                  {/if}
                </div>
              {/each}
              {#if subagent.streamingReasoning}
                <details class="subagent-reasoning" open>
                  <summary>{t("thinking")}…</summary>
                  <pre class="subagent-reasoning-text">{subagent.streamingReasoning}</pre>
                </details>
              {/if}
              {#each subagent.reasoning as trace, i (i)}
                <details class="subagent-reasoning">
                  <summary>{t("thinking")}</summary>
                  <pre class="subagent-reasoning-text">{trace}</pre>
                </details>
              {/each}
              {#if subagent.streamingText}
                <pre class="subagent-stream">{subagent.streamingText}</pre>
              {/if}
              {#if subagent.text}
                <pre class="subagent-text">{subagent.text}</pre>
              {/if}
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* A tool card is a quiet ledger row: no filled card, just a hairline
     above and mono ink below, opening on demand. */
  .tool-card {
    border-top: 1px solid var(--line);
    margin: 0.1rem 0;
    overflow: hidden;
    content-visibility: auto;
  }

  .tool-card.running .glyph {
    animation: breathe 1.6s ease-in-out infinite;
  }

  @keyframes breathe {
    0%,
    100% {
      opacity: 0.45;
    }
    50% {
      opacity: 1;
    }
  }

  .tool-header {
    width: 100%;
    padding: 0.45rem 0.25rem;
    background: transparent;
    border: none;
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    cursor: pointer;
    text-align: left;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    min-width: 0;
  }

  .glyph {
    font-size: 0.7rem;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .name {
    font-weight: 600;
    color: var(--accent-info);
    flex-shrink: 0;
  }

  .preview {
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  .spacer {
    flex: 1;
    min-width: 0.5rem;
  }

  .tool-badge {
    font-size: 0.65rem;
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .status-running {
    color: var(--accent-warning);
  }

  .status-completed {
    color: var(--accent-info);
  }

  .status-failed {
    color: var(--accent-danger);
  }

  .status-cancelled {
    color: var(--text-muted);
  }

  .chevron {
    color: var(--text-muted);
    font-size: 0.7rem;
    flex-shrink: 0;
    width: 0.8em;
  }

  .tool-content {
    padding: 0.4rem 0.25rem 0.6rem;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    max-height: 240px;
    overflow-y: auto;
  }

  .block {
    margin-bottom: 0.5rem;
  }

  .block:last-child {
    margin-bottom: 0;
  }

  .label {
    color: var(--text-muted);
    font-size: 0.62rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    margin-bottom: 0.1rem;
  }

  pre {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--text-secondary);
  }

  pre.err {
    color: var(--accent-danger);
  }

  pre.output {
    color: var(--text-primary);
  }

  .subagent-block {
    margin-top: 0.5rem;
    border-left: 2px solid var(--line-strong);
    padding-left: 0.6rem;
  }

  .subagent-header {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    width: 100%;
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 0.1rem 0;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    text-align: left;
  }

  .subagent-icon {
    color: var(--accent-warning);
  }

  .subagent-title {
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .subagent-content {
    padding: 0.35rem 0 0.1rem;
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .subagent-tool {
    background: var(--bg-surface-hover);
    border-radius: var(--radius-sm);
    padding: 0.35rem 0.5rem;
  }

  .subagent-tool-head {
    display: flex;
    justify-content: space-between;
    gap: 0.5rem;
  }

  .sub-name {
    color: var(--text-secondary);
    font-weight: 600;
  }

  .subagent-tool pre {
    margin-top: 0.25rem;
    max-height: 120px;
    overflow-y: auto;
  }

  .sub-status {
    font-size: 0.62rem;
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .subagent-stream {
    color: var(--text-muted);
    max-height: 140px;
    overflow-y: auto;
  }

  .subagent-reasoning summary {
    cursor: pointer;
    color: var(--text-muted);
    font-size: 0.7rem;
    padding: 0.1rem 0;
    user-select: none;
  }

  .subagent-reasoning-text {
    color: var(--text-muted);
    max-height: 160px;
    overflow-y: auto;
  }

  .subagent-text {
    color: var(--text-secondary);
    max-height: 200px;
    overflow-y: auto;
  }

  @media (prefers-reduced-motion: reduce) {
    .tool-card.running .glyph {
      animation: none;
    }
  }
</style>
