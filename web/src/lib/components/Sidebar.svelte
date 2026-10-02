<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";
  import type { MonitoredSession } from "../types.js";

  interface Props {
    open: boolean;
    onClose: () => void;
    onOpenConnection: () => void;
  }

  let { open, onClose, onOpenConnection }: Props = $props();

  /** Session id pending a second click to confirm deletion. */
  let confirmDeleteId = $state<string | null>(null);
  /** Session id whose title is being edited inline. */
  let editingId = $state<string | null>(null);
  let editValue = $state("");

  const statusLabels: Record<string, string> = {
    idle: "idle",
    running: "running",
    needs_approval: "approval",
    needs_input: "input",
    interrupted: "stopped",
    failed: "failed",
  };

  function sessionTitle(s: MonitoredSession): string {
    return s.overview || s.id.slice(0, 8);
  }

  function select(id: string) {
    if (editingId === id) return;
    daemon.attach(id);
    onClose();
  }

  function startRename(s: MonitoredSession) {
    editingId = s.id;
    editValue = s.overview ?? "";
  }

  /** Svelte action: focus the inline rename input on mount. */
  function focusOnMount(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  function commitRename(id: string) {
    const title = editValue.trim();
    if (title) daemon.renameSession(id, title);
    editingId = null;
  }

  function requestDelete(id: string) {
    if (confirmDeleteId === id) {
      daemon.deleteSession(id);
      confirmDeleteId = null;
    } else {
      confirmDeleteId = id;
      window.setTimeout(() => {
        if (confirmDeleteId === id) confirmDeleteId = null;
      }, 4000);
    }
  }
</script>

{#if open}
  <div class="backdrop" onclick={onClose} role="presentation"></div>
{/if}

<aside class="sidebar" class:open>
  <div class="brand-header">
    <div class="brand-logo">
      <!-- 朱砂印: the seal-mark logo (cat & duck in a red round seal). -->
      <img class="seal" src="/logo-seal.png" alt="muta seal" draggable="false" />
      <span class="title">muta</span>
    </div>
    <button
      class="badge"
      class:online={daemon.connection === "connected"}
      onclick={onOpenConnection}
      title={t("connectionSettings")}
    >
      {daemon.connection === "connected"
        ? t("online")
        : daemon.connection === "connecting"
          ? t("connectingShort")
          : t("offline")}
    </button>
  </div>

  <div class="action-bar">
    <button class="btn-new" onclick={() => daemon.newSession()}>
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
        <path d="M12 5v14M5 12h14"/>
      </svg>
      {t("newSession")}
    </button>
  </div>

  <div class="sessions-container">
    <div class="section-title">{t("sessionsTitle")} · {daemon.sessions.length}</div>
    <div class="session-list">
      {#if daemon.sessions.length === 0}
        <div class="empty">
          <span class="empty-mark">空</span>
          <span class="empty-text">{t("noSessions")}</span>
        </div>
      {:else}
        {#each daemon.sessions as s (s.id)}
          <div
            class="session-item"
            class:active={s.id === daemon.activeSessionId}
            role="button"
            tabindex="0"
            onclick={() => select(s.id)}
            onkeydown={(e) => e.key === "Enter" && select(s.id)}
          >
            <div class="session-header">
              {#if editingId === s.id}
                <input
                  class="rename-input"
                  bind:value={editValue}
                  onkeydown={(e) => {
                    if (e.key === "Enter") commitRename(s.id);
                    if (e.key === "Escape") editingId = null;
                    e.stopPropagation();
                  }}
                  onclick={(e) => e.stopPropagation()}
                  onfocusout={() => commitRename(s.id)}
                  aria-label="Rename session"
                  use:focusOnMount
                />
              {:else}
                <span class="session-title">{sessionTitle(s)}</span>
              {/if}
              <span class="status-dot status-{s.status}" title={s.status}>
                {statusLabels[s.status] ?? s.status}
              </span>
            </div>
            <div class="session-meta">
              <span class="activity">{s.current_tool ?? s.activity ?? ""}</span>
              <span class="meta-right">
                {#if s.context_tokens}
                  <span>{s.context_tokens.toLocaleString()} tok</span>
                {/if}
                <button
                  class="icon-action rename-btn"
                  title={t("renameSession")}
                  onclick={(e) => {
                    e.stopPropagation();
                    startRename(s);
                  }}
                >
                  ✎
                </button>
                <button
                  class="icon-action interrupt-btn"
                  title={t("interruptRound")}
                  onclick={(e) => {
                    e.stopPropagation();
                    daemon.interruptSession(s.id);
                  }}
                >
                  ⏹
                </button>
                <button
                  class="icon-action suspend-btn"
                  title={t("suspendSession")}
                  onclick={(e) => {
                    e.stopPropagation();
                    daemon.suspendSession(s.id);
                  }}
                >
                  ⏸
                </button>
                <button
                  class="icon-action end-btn"
                  title={t("endSession")}
                  onclick={(e) => {
                    e.stopPropagation();
                    daemon.endSession(s.id);
                  }}
                >
                  ⏻
                </button>
                <button
                  class="icon-action delete-btn"
                  class:confirm={confirmDeleteId === s.id}
                  title={confirmDeleteId === s.id ? t("confirmDelete") : t("deleteSession")}
                  onclick={(e) => {
                    e.stopPropagation();
                    requestDelete(s.id);
                  }}
                >
                  {confirmDeleteId === s.id ? t("confirmDelete") : "×"}
                </button>
              </span>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  </div>

  <div class="sidebar-footer">
    <span class="footer-text">{daemon.daemonProjectRoot || daemon.wsUrl}</span>
  </div>
</aside>

<style>
  .sidebar {
    width: 264px;
    height: 100%;
    background-color: var(--bg-sidebar);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    flex-shrink: 0;
  }

  .backdrop {
    display: none;
  }

  .brand-header {
    padding: 1.1rem 1.25rem;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .brand-logo {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }

  /* 朱砂印: the round seal mark, image-carried. */
  .seal {
    width: 28px;
    height: 28px;
    object-fit: contain;
    user-select: none;
    /* Light theme print keeps its full cinnabar; night mode: the red seal
       on dark paper needs no filter — the ink already reads. */
  }

  .title {
    font-family: var(--font-brush);
    font-weight: 600;
    font-size: 1.1rem;
    letter-spacing: 0.12em;
    color: var(--text-primary);
  }

  .badge {
    font-family: var(--font-mono);
    font-size: 0.65rem;
    letter-spacing: 0.08em;
    padding: 0.15rem 0.5rem;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--accent-danger);
    border: 1px solid var(--accent-danger);
    text-transform: uppercase;
    cursor: pointer;
    transition: background-color var(--t-fast);
  }

  .badge:hover {
    background: var(--seal-soft);
  }

  .badge.online {
    color: var(--accent-info);
    border-color: var(--accent-info);
  }

  .action-bar {
    padding: 0.9rem 1.1rem 0.4rem;
  }

  .btn-new {
    width: 100%;
    padding: 0.5rem 0.75rem;
    background: transparent;
    color: var(--accent-primary);
    border: 1px solid var(--accent-primary);
    border-radius: var(--radius-md);
    font-size: 0.8rem;
    font-weight: 500;
    letter-spacing: 0.06em;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.4rem;
    cursor: pointer;
    transition: background-color var(--t-fast);
  }

  .btn-new:hover {
    background: var(--seal-soft);
  }

  .sessions-container {
    flex: 1;
    overflow-y: auto;
    padding: 0.5rem 0.7rem;
  }

  .section-title {
    font-size: 0.68rem;
    letter-spacing: 0.15em;
    font-weight: 500;
    color: var(--text-muted);
    margin: 0.6rem 0.3rem;
  }

  .session-list {
    display: flex;
    flex-direction: column;
  }

  .session-item {
    padding: 0.55rem 0.6rem;
    border-left: 2px solid transparent;
    cursor: pointer;
    text-align: left;
    transition: border-color var(--t-fast), background-color var(--t-fast);
  }

  .session-item:hover {
    background: var(--bg-surface-hover);
  }

  /* Selected session marked by a cinnabar rule on the left — a brush
     stroke, not a filled pill. */
  .session-item.active {
    background: var(--bg-surface);
    border-left-color: var(--accent-primary);
  }

  .session-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.15rem;
    gap: 0.4rem;
  }

  .session-title {
    font-weight: 500;
    font-size: 0.82rem;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }

  .status-dot {
    font-family: var(--font-mono);
    font-size: 0.62rem;
    letter-spacing: 0.05em;
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .status-dot::before {
    content: "·";
    margin-right: 0.3em;
    font-weight: 700;
  }

  .status-running {
    color: var(--accent-info);
  }

  .status-needs_approval,
  .status-needs_input {
    color: var(--accent-warning);
  }

  .status-failed {
    color: var(--accent-danger);
  }

  .session-meta {
    font-size: 0.7rem;
    color: var(--text-muted);
    display: flex;
    justify-content: space-between;
    gap: 0.5rem;
    align-items: center;
  }

  .activity {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta-right {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    flex-shrink: 0;
  }

  .icon-action {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-size: 0.8rem;
    line-height: 1;
    cursor: pointer;
    padding: 0 0.1rem;
    opacity: 0;
    transition: opacity var(--t-fast), color var(--t-fast);
  }

  .session-item:hover .icon-action,
  .delete-btn.confirm {
    opacity: 1;
  }

  .icon-action:hover {
    color: var(--text-secondary);
  }

  .end-btn:hover {
    color: var(--accent-warning) !important;
  }

  .interrupt-btn:hover,
  .suspend-btn:hover {
    color: var(--accent-info) !important;
  }

  .delete-btn:hover {
    color: var(--accent-danger) !important;
  }

  .delete-btn.confirm {
    color: var(--accent-danger);
    font-size: 0.62rem;
    font-family: var(--font-mono);
    border: 1px solid var(--accent-danger);
    border-radius: var(--radius-sm);
    padding: 0.05rem 0.25rem;
    opacity: 1;
  }

  .rename-input {
    flex: 1;
    min-width: 0;
    background: var(--input-bg-inactive);
    border: 1px solid var(--border-input-focus);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    font-size: 0.82rem;
    padding: 0.1rem 0.35rem;
    outline: none;
  }

  .empty {
    padding: 2rem 1rem;
    text-align: center;
    color: var(--text-muted);
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .empty-mark {
    font-family: var(--font-brush);
    font-size: 1.6rem;
    color: var(--border-strong);
  }

  .empty-text {
    font-size: 0.72rem;
    letter-spacing: 0.1em;
  }

  .sidebar-footer {
    padding: 0.7rem 1.1rem;
    border-top: 1px solid var(--line);
    font-family: var(--font-mono);
    font-size: 0.68rem;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  @media (max-width: 900px) {
    .sidebar {
      position: fixed;
      top: 0;
      left: 0;
      bottom: 0;
      z-index: 90;
      transform: translateX(-100%);
      transition: transform 0.2s var(--ease);
      box-shadow: 8px 0 32px rgba(0, 0, 0, 0.25);
    }

    .sidebar.open {
      transform: translateX(0);
    }

    .backdrop {
      display: block;
      position: fixed;
      inset: 0;
      background: rgba(0, 0, 0, 0.35);
      z-index: 80;
    }
  }
</style>
