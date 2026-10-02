<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";
</script>

<div class="toast-stack" role="status" aria-live="polite">
  {#each daemon.toasts as toast (toast.id)}
    <div class="toast {toast.severity}">
      <div class="toast-body">
        <div class="toast-title">{toast.title}</div>
        {#if toast.body}
          <div class="toast-text">{toast.body}</div>
        {/if}
      </div>
      <button class="close" aria-label={t("dismiss")} onclick={() => daemon.dismissToast(toast.id)}>
        ×
      </button>
    </div>
  {/each}
</div>

<style>
  .toast-stack {
    position: fixed;
    bottom: 96px;
    right: 24px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 50;
    max-width: 360px;
  }

  .toast {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    background-color: var(--bg-surface);
    border: 1px solid var(--line-strong);
    border-left: 3px solid var(--text-muted);
    border-radius: var(--radius-md);
    padding: 10px 12px;
    box-shadow: var(--shadow-modal);
    animation: slideIn 0.2s var(--ease);
  }

  .toast.info {
    border-left-color: var(--accent-info);
  }

  .toast.warning {
    border-left-color: var(--accent-warning);
  }

  .toast.error {
    border-left-color: var(--accent-danger);
  }

  .toast-body {
    flex: 1;
  }

  .toast-title {
    font-size: 13px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .toast-text {
    font-size: 12px;
    color: var(--text-secondary);
    margin-top: 2px;
    word-break: break-word;
  }

  .close {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    padding: 0 2px;
  }

  .close:hover {
    color: var(--text-primary);
  }

  @keyframes slideIn {
    from { opacity: 0; transform: translateX(8px); }
    to { opacity: 1; transform: translateX(0); }
  }
</style>
