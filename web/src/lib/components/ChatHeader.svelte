<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { roundActiveMs, roundTps } from "../types.js";
  import type { Theme } from "../theme.js";
  import { t, toggleLocale } from "../i18n.svelte.js";

  interface Props {
    onToggleSidebar: () => void;
    onOpenModels: () => void;
    onOpenWebSearch: () => void;
    theme: Theme;
    onToggleTheme: () => void;
  }

  let { onToggleSidebar, onOpenModels, onOpenWebSearch, theme, onToggleTheme }: Props =
    $props();

  function formatDuration(ms: number): string {
    if (ms < 1000) return `${ms}ms`;
    return `${(ms / 1000).toFixed(1)}s`;
  }

  let roundLabel = $derived.by(() => {
    const r = daemon.lastRound;
    if (!r) return null;
    const tps = roundTps(r);
    const parts = [
      `${r.output_tokens.toLocaleString()} tok`,
      formatDuration(roundActiveMs(r)),
    ];
    if (tps > 0) parts.push(`${tps.toFixed(1)} tok/s`);
    return parts.join(" · ");
  });
</script>

<header class="header">
  <button class="icon-btn menu-btn" aria-label="Toggle sessions" onclick={onToggleSidebar}>
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
      <path d="M3 6h18M3 12h18M3 18h18"/>
    </svg>
  </button>

  <div class="session-info">
    <h2 class="title">
      {daemon.activeSession?.overview || daemon.activeSessionId || t("noActiveSession")}
    </h2>
    <div class="meta">
      {#if daemon.activeSession}
        {#if daemon.roundCounter > 0}
          <span class="tag">{t("round")} {daemon.roundCounter}</span>
        {/if}
        {#if daemon.currentTurn !== null && daemon.isBusy}
          <span class="tag">{t("turn")} {daemon.currentTurn + 1}</span>
        {/if}
        {#if daemon.contextTokens}
          <span class="tag">{daemon.contextTokens.toLocaleString()} {t("ctx")}</span>
        {/if}
        {#if daemon.activity && daemon.isBusy}
          <span class="tag activity">{daemon.activity}</span>
        {:else if daemon.activeSession.current_tool}
          <span class="tag">{daemon.activeSession.current_tool}</span>
        {/if}
        {#if roundLabel && !daemon.isBusy}
          <span class="tag">{roundLabel}</span>
        {/if}
      {/if}
    </div>
  </div>

  <div class="actions">
    {#if daemon.unattended}
      <span class="badge warn" title={t("unattendedTip")}>
        {t("unattended")}
      </span>
    {/if}
    {#if !daemon.confined}
      <span class="badge warn" title={t("unconfinedTip")}>
        {t("unconfined")}
      </span>
    {/if}
    {#if daemon.providerInfo}
      <button class="chip" title={t("switchModel")} onclick={onOpenModels}>
        <span class="provider">{daemon.providerInfo.provider}</span>
        <span class="model">{daemon.providerInfo.model}</span>
      </button>
    {/if}
    <button
      class="chip"
      title={t("webSearchSettings")}
      onclick={onOpenWebSearch}
    >
      <span class="provider">⌕</span>
      <span class="model">{daemon.websearchConfig?.provider ?? "web"}</span>
    </button>
    <button
      class="icon-btn theme-btn"
      title={theme === "dark" ? t("toLightTheme") : t("toDarkTheme")}
      aria-label="Toggle color theme"
      onclick={onToggleTheme}
    >
      {#if theme === "dark"}
        <!-- 日 -->
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
          <circle cx="12" cy="12" r="4"/>
          <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41"/>
        </svg>
      {:else}
        <!-- 月 -->
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
        </svg>
      {/if}
    </button>
    <button
      class="icon-btn lang-btn"
      title={t("toChinese") === "切换到中文" ? "Switch to English" : t("toChinese")}
      aria-label="Toggle language"
      onclick={toggleLocale}
    >
      <span class="lang-mark">文</span>
    </button>
    {#if daemon.isBusy}
      <button class="stop-btn" onclick={() => daemon.interrupt()}>
        <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor">
          <rect x="6" y="6" width="12" height="12" rx="1"/>
        </svg>
        {t("interrupt")}
      </button>
    {/if}
  </div>
</header>

<style>
  .header {
    padding: 0.85rem var(--pad-x);
    background-color: var(--bg-header);
    border-bottom: 1px solid var(--line);
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.75rem;
  }

  .menu-btn {
    display: none;
  }

  .session-info {
    min-width: 0;
    flex: 1;
  }

  .title {
    font-family: var(--font-brush);
    font-size: 1.05rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    color: var(--text-primary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .meta {
    display: flex;
    gap: 0.65rem;
    margin-top: 0.1rem;
    overflow: hidden;
  }

  .tag {
    font-family: var(--font-mono);
    font-size: 0.68rem;
    color: var(--text-muted);
    white-space: nowrap;
  }

  .tag.activity {
    color: var(--accent-info);
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-shrink: 0;
  }

  .badge {
    font-family: var(--font-mono);
    font-size: 0.65rem;
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--accent-warning);
    border: 1px solid var(--accent-warning);
    opacity: 0.85;
  }

  /* Machine-identity chips: quiet ink, no fill. */
  .chip {
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
    padding: 0.3rem 0.6rem;
    border-radius: var(--radius-md);
    background: transparent;
    border: 1px solid var(--line);
    cursor: pointer;
    max-width: 320px;
    transition: border-color var(--t-fast);
  }

  .chip:hover {
    border-color: var(--line-strong);
  }

  .chip .provider {
    font-size: 0.62rem;
    color: var(--text-muted);
    text-transform: uppercase;
    font-family: var(--font-mono);
  }

  .chip .model {
    font-size: 0.75rem;
    color: var(--text-secondary);
    font-family: var(--font-mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .icon-btn {
    background: transparent;
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    width: 30px;
    height: 30px;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    flex-shrink: 0;
    transition: border-color var(--t-fast), color var(--t-fast);
  }

  .icon-btn:hover {
    border-color: var(--line-strong);
    color: var(--text-primary);
  }

  /* The language toggle carries a brush glyph rather than an abbreviation:
     文 marks "the written word" — tap to switch tongue. */
  .lang-mark {
    font-family: var(--font-brush);
    font-size: 0.85rem;
    line-height: 1;
  }

  .stop-btn {
    padding: 0.32rem 0.7rem;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--accent-danger);
    border: 1px solid var(--accent-danger);
    font-size: 0.75rem;
    font-weight: 500;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    transition: background-color var(--t-fast);
    flex-shrink: 0;
  }

  .stop-btn:hover {
    background: var(--seal-soft);
  }

  @media (max-width: 900px) {
    .menu-btn {
      display: flex;
    }

    .header {
      padding: 0.6rem 1rem;
    }

    .chip .provider {
      display: none;
    }
  }
</style>
