<script lang="ts">
  import type { CommandRecord, CommandResult, ReviewVerdict } from "../types.js";
  import { renderMarkdown } from "../markdown.js";

  interface Props {
    record: CommandRecord;
  }

  let { record }: Props = $props();
  let expanded = $state(true);

  /**
   * Text rendering of a `CommandResult`, mirroring `CommandResult::to_text`
   * in `crates/muta-contracts/src/command.rs` — the single text scheme every
   * consumer (live display, resume, export) agrees on.
   */
  function resultToText(result: CommandResult): string {
    if ("Text" in result) return result.Text;
    if ("Error" in result) {
      const { message, detail } = result.Error;
      return detail && detail.trim() ? `Error: ${message}\n${detail}` : `Error: ${message}`;
    }
    if ("Ack" in result) {
      const detail: string[] = result.Ack.detail ?? [];
      const lines = detail.filter((line: string) => line.trim().length > 0);
      return lines.length === 0
        ? result.Ack.title
        : [result.Ack.title, ...lines].join("\n");
    }
    if ("PermissionList" in result) {
      const allowed = result.PermissionList.allowed;
      return allowed.length === 0
        ? "No tools are always allowed for this process."
        : `Always-allowed tools:\n- ${allowed.join("\n- ")}`;
    }
    if ("Search" in result) {
      const { hits } = result.Search;
      if (hits.length === 0) return "No relevant history found.";
      const lines = ["Relevant history (most similar first):"];
      hits.forEach((hit, i) => {
        lines.push(`${i + 1}. [score=${hit.score.toFixed(3)}]\n${hit.text}`);
      });
      return lines.join("\n\n");
    }
    if ("SessionStatus" in result) {
      const s = result.SessionStatus;
      return `Session: ${s.id}\nForked from: ${s.parent_id ?? "none"}\nModel-window messages: ${s.message_count}\nArchived transcript messages: ${s.archived_count}\nLast context projection: ${s.last_projection ?? "none"}`;
    }
    if ("Review" in result) return reviewToText(result.Review.verdicts, result.Review.turns);
    return "";
  }

  function statusLabel(status: string): string {
    switch (status) {
      case "Healthy":
        return "ok";
      case "Watch":
        return "watch";
      case "Stuck":
        return "stuck";
      default:
        return status.toLowerCase();
    }
  }

  /** Mirrors `review_to_text` in `crates/muta-contracts/src/command.rs`. */
  function reviewToText(verdicts: ReviewVerdict[], turns: number): string {
    const turnUnit = turns === 1 ? "turn" : "turns";
    const order: Record<string, number> = { Healthy: 0, Watch: 1, Stuck: 2 };
    const worst = verdicts.reduce<string | null>(
      (acc, v) => (acc === null || order[v.status] > order[acc] ? v.status : acc),
      null,
    );
    let headline: string;
    if (worst === null) {
      return `Session review (~${turns} ${turnUnit}): no review dimensions registered.`;
    } else if (worst === "Healthy") {
      headline = `Session review (~${turns} ${turnUnit}): no concerns found.`;
    } else {
      headline = `Session review (~${turns} ${turnUnit}) — verdict: ${statusLabel(worst)}.`;
    }
    const lines = [headline];
    for (const verdict of verdicts) {
      const detail = verdict.detail.trim();
      lines.push(
        detail
          ? `- ${verdict.dimension} — ${statusLabel(verdict.status)}: ${detail}`
          : `- ${verdict.dimension} — ${statusLabel(verdict.status)}`,
      );
    }
    lines.push("Interrupt the turn with Esc if it looks stuck.");
    return lines.join("\n");
  }

  /**
   * ADR-0106: interaction follows shape. A title-only Ack, or a short
   * single-line Text reply (e.g. `/new`'s confirmation), renders as one flat
   * confirmation row — no disclosure affordance, since expanding would show
   * nothing new. An Ack *with* detail lines keeps the expandable block: the
   * headline leads, and the muted explanation lines follow below it.
   * Multi-line or long replies keep the expandable block too.
   */
  const INLINE_RESULT_MAX_CHARS = 80;
  let ackDetail = $derived(
    record.result && "Ack" in record.result
      ? (record.result.Ack.detail ?? []).filter((line: string) => line.trim().length > 0)
      : null,
  );
  let isInline = $derived(
    record.result !== null &&
      record.result !== undefined &&
      (("Ack" in record.result && (ackDetail?.length ?? 0) === 0) ||
        ("Text" in record.result &&
          !record.result.Text.includes("\n") &&
          record.result.Text.length <= INLINE_RESULT_MAX_CHARS)),
  );
  let resultText = $derived(record.result ? resultToText(record.result) : "");
  let htmlContent = $derived(isInline ? "" : renderMarkdown(resultText));
  let invocation = $derived(
    record.name === "shell"
      ? `!${record.args}`
      : `/${record.name}${record.args ? ` ${record.args}` : ""}`,
  );
</script>

{#if isInline && record.result}
  <div class="command-ack">
    <span class="tick">✓</span>
    <span class="invocation">{invocation}</span>
    <span class="reply">
      {#if "Ack" in record.result}
        {record.result.Ack.title}
      {:else if "Text" in record.result}
        {record.result.Text}
      {/if}
    </span>
  </div>
{:else if record.result && "Ack" in record.result}
  <div class="command-block">
    <button class="command-header" onclick={() => (expanded = !expanded)}>
      <span class="invocation">{invocation}</span>
      <span class="badges">
        {#if record.duration_ms !== null && record.duration_ms !== undefined}
          <span class="badge">{record.duration_ms}ms</span>
        {/if}
        <span class="chevron">{expanded ? "-" : "+"}</span>
      </span>
    </button>
    {#if expanded && record.result.Ack.title}
      <div class="command-result ack-body">
        <p class="ack-title">{record.result.Ack.title}</p>
        {#each ackDetail ?? [] as line}
          <p class="ack-detail">{line}</p>
        {/each}
      </div>
    {/if}
  </div>
{:else}
  <div class="command-block" class:failed={record.status === "error"}>
    <button class="command-header" onclick={() => (expanded = !expanded)}>
      <span class="invocation">{invocation}</span>
      <span class="badges">
        {#if record.status === "error"}
          <span class="badge error">error</span>
        {/if}
        {#if record.duration_ms !== null && record.duration_ms !== undefined}
          <span class="badge">{record.duration_ms}ms</span>
        {/if}
        <span class="chevron">{expanded ? "-" : "+"}</span>
      </span>
    </button>
    {#if expanded && resultText}
      <div class="command-result markdown-body">
        {@html htmlContent}
      </div>
    {/if}
  </div>
{/if}

<style>
  .command-ack {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.78rem;
    color: var(--text-muted);
    font-family: var(--font-mono);
    margin-bottom: 0.75rem;
  }

  .command-ack .tick {
    color: var(--accent-info);
    flex-shrink: 0;
  }

  .command-ack .invocation {
    color: var(--accent-primary);
    font-weight: 600;
    white-space: nowrap;
  }

  .command-ack .reply {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .command-block {
    border-top: 1px solid var(--line);
    margin-bottom: 0.75rem;
    overflow: hidden;
  }

  .command-block.failed {
    border-color: var(--accent-danger);
  }

  .command-header {
    width: 100%;
    padding: 0.45rem 0.25rem;
    background: transparent;
    border: none;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    text-align: left;
    font-family: var(--font-mono);
    font-size: 0.75rem;
  }

  .invocation {
    color: var(--accent-primary);
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .badges {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }

  .badge {
    font-size: 0.65rem;
    padding: 0.05rem 0.3rem;
    border-radius: var(--radius-sm);
    color: var(--text-muted);
    border: 1px solid var(--line);
  }

  .badge.error {
    color: var(--accent-danger);
    border-color: var(--accent-danger);
  }

  .chevron {
    color: var(--text-muted);
    font-size: 0.7rem;
  }

  .command-result {
    padding: 0.1rem 0.25rem 0.6rem;
    font-size: 0.82rem;
    line-height: 1.6;
    color: var(--text-secondary);
    max-height: 320px;
    overflow-y: auto;
    word-break: break-word;
  }

  /* ADR-0106 two-tone ack: headline prominent, explanation lines muted. */
  .ack-title {
    margin: 0 0 4px;
    color: var(--text-primary);
    font-weight: 600;
  }

  .ack-detail {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.75rem;
  }
</style>
