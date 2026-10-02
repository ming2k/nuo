<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";

  let collapsed = $state(false);

  let items = $derived(daemon.todos.items);
  let doneCount = $derived(
    items.filter((i) => i.status === "completed" || i.status === "cancelled").length,
  );

  function glyph(status: string): string {
    switch (status) {
      case "completed":
        return "✓";
      case "in_progress":
        return "◐";
      case "cancelled":
        return "✕";
      default:
        return "○";
    }
  }
</script>

{#if items.length > 0}
  <div class="todo-panel">
    <button class="todo-header" onclick={() => (collapsed = !collapsed)}>
      <span class="title">{t("tasks")}</span>
      <span class="progress">{doneCount}/{items.length}</span>
      <span class="chevron">{collapsed ? "+" : "-"}</span>
    </button>
    {#if !collapsed}
      <ul class="todo-list">
        {#each items as item (item.id)}
          <li class="todo status-{item.status}">
            <span class="glyph">{glyph(item.status)}</span>
            <span class="content">{item.content}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .todo-panel {
    width: min(var(--measure), 100%);
    margin: 0 auto 0.6rem;
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    background-color: var(--bg-surface);
    overflow: hidden;
    flex-shrink: 0;
  }

  .todo-header {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.75rem;
    background: transparent;
    border: none;
    cursor: pointer;
    font-family: var(--font-brush);
    font-size: 0.78rem;
    letter-spacing: 0.15em;
    color: var(--text-secondary);
  }

  .todo-header .title {
    font-weight: 600;
  }

  .todo-header .progress {
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 0.68rem;
  }

  .chevron {
    margin-left: auto;
    color: var(--text-muted);
    font-size: 0.65rem;
    font-family: var(--font-mono);
  }

  .todo-list {
    list-style: none;
    max-height: 160px;
    overflow-y: auto;
    border-top: 1px solid var(--line);
    padding: 0.35rem 0;
  }

  .todo {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0.18rem 0.75rem;
    font-size: 0.78rem;
    color: var(--text-primary);
  }

  .glyph {
    font-family: var(--font-mono);
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .status-completed .glyph {
    color: var(--accent-info);
  }

  .status-completed .content {
    color: var(--text-muted);
    text-decoration: none;
    opacity: 0.65;
  }

  .status-in_progress .glyph {
    color: var(--accent-warning);
  }

  .status-cancelled .content {
    color: var(--text-muted);
    text-decoration: line-through;
    opacity: 0.65;
  }

  .content {
    word-break: break-word;
  }
</style>
