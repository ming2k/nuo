<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";
  import type { ProviderPickerRow } from "../types.js";

  interface Props {
    onclose: () => void;
  }

  let { onclose }: Props = $props();

  interface ModelEntry {
    id: string;
    /** The provider's own label for the model, when it publishes one. */
    name: string | null;
    provider: ProviderPickerRow;
    favorite: boolean;
    effort: string | null;
    thinking: boolean | null;
    active: boolean;
    /**
     * Provider-declared availability (`availability.usable === false`):
     * visible but inert. `null` means undeclared and therefore usable.
     */
    locked: boolean;
    /**
     * The provider's own reason for the verdict, verbatim, when it stated one.
     * Never paraphrased: a provider that declared none leaves this null and the
     * row says only that the model is unavailable.
     */
    lockedReason: string | null;
    /**
     * The provider declared the model unusable but the user's own scope
     * overrode it: the row is usable and must disclose the contradiction.
     */
    overridden: boolean;
  }

  /** Flatten the picker snapshot into one row per served model. */
  let entries = $derived.by((): ModelEntry[] => {
    const snapshot = daemon.providerPicker;
    if (!snapshot) return [];
    const out: ModelEntry[] = [];
    for (const row of snapshot.rows) {
      for (const model of row.models) {
        const info = row.model_info?.find((m) => m.model === model);
        out.push({
          id: model,
          name: info?.name ?? null,
          provider: row,
          favorite: info?.favorite ?? false,
          effort: info?.effort ?? null,
          thinking: info?.thinking ?? null,
          active: row.id === snapshot.default_id && row.model === model,
          locked: info?.availability?.usable === false,
          lockedReason:
            info?.availability?.usable === false
              ? (info.availability.reason ?? null)
              : null,
          overridden: info?.availability_overridden === true,
        });
      }
    }
    // Two-tier ordering, mirroring the TUI's Models picker: the live
    // (provider, model) pair first, favorites next, everything else after —
    // each tier ASCII-sorted by the model id with the provider label as the
    // tiebreaker. ASCII (not localeCompare) keeps the web and TUI lists in
    // the same order.
    return out.sort((a, b) => {
      const weight = (e: ModelEntry) => (e.active ? 2 : e.favorite ? 1 : 0);
      return (
        weight(b) - weight(a) ||
        (a.id < b.id ? -1 : a.id > b.id ? 1 : 0) ||
        (a.provider.name < b.provider.name ? -1 : a.provider.name > b.provider.name ? 1 : 0)
      );
    });
  });

  function choose(entry: ModelEntry) {
    if (!entry.provider.key_ready || entry.locked) return;
    daemon.setDefaultModel(entry.id);
    onclose();
  }

  function handleBackdrop(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" onclick={handleBackdrop} role="presentation">
  <div class="modal" role="dialog" aria-label="Choose model">
    <div class="modal-header">
      <h3>{t("models")}</h3>
      <button class="close" aria-label={t("close")} onclick={onclose}>×</button>
    </div>

    <div class="modal-body">
      {#if !daemon.providerPicker}
        <div class="empty">{t("modelsUnavailable")}</div>
      {:else if entries.length === 0}
        <div class="empty">{t("noProviders")}</div>
      {:else}
        {#each entries as entry (entry.provider.id + ":" + entry.id)}
          <button
            class="model-row"
            class:active={entry.active}
            class:unavailable={!entry.provider.key_ready || entry.locked}
            onclick={() => choose(entry)}
            title={entry.locked
              ? `${entry.provider.name} — unavailable: ${entry.lockedReason ?? "the provider did not state a reason"}`
              : entry.overridden
                ? `${entry.provider.name} — the provider locked this model; you overrode it`
                : entry.provider.key_ready
                  ? `${entry.provider.name} — click to switch`
                  : `${entry.provider.name} — no API key configured`}
          >
            <span class="star" class:favorite={entry.favorite}>{entry.favorite ? "★" : ""}</span>
            <span class="model-name" class:wire={!entry.name}>{entry.name ?? entry.id}</span>
            {#if entry.name}
              <span class="model-id">{entry.id}</span>
            {/if}
            <span class="provider-name">{entry.provider.name}</span>
            <span class="flags">
              {#if entry.effort}
                <span class="flag">{t("effort")}: {entry.effort}</span>
              {/if}
              {#if entry.thinking}
                <span class="flag">{t("thinking")}</span>
              {/if}
              {#if !entry.provider.key_ready}
                <span class="flag no-key">{t("noApiKey")}</span>
              {/if}
              {#if entry.locked}
                <span class="flag locked">
                  {entry.lockedReason ? `${t("locked")}: ${entry.lockedReason}` : t("locked")}
                </span>
              {:else if entry.overridden}
                <span class="flag locked">{t("locked")} · {t("overridden")}</span>
              {/if}
              {#if entry.active}
                <span class="flag current">{t("current")}</span>
              {/if}
            </span>
          </button>
        {/each}
      {/if}
    </div>

    <div class="modal-footer">
      {t("modelsFooter")}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-app) 45%, transparent);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 12vh;
    z-index: 100;
  }

  .modal {
    width: 560px;
    max-width: calc(100vw - 32px);
    max-height: 70vh;
    background-color: var(--bg-surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-modal);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.85rem 1rem;
    border-bottom: 1px solid var(--line);
  }

  .modal-header h3 {
    font-family: var(--font-brush);
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    color: var(--text-primary);
  }

  .close {
    background: transparent;
    border: none;
    color: var(--text-muted);
    font-size: 18px;
    cursor: pointer;
    line-height: 1;
  }

  .close:hover {
    color: var(--text-primary);
  }

  .modal-body {
    overflow-y: auto;
    padding: 8px;
  }

  .empty {
    padding: 24px;
    text-align: center;
    color: var(--text-muted);
    font-size: 12px;
  }

  .model-row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 12px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius-md);
    cursor: pointer;
    text-align: left;
  }

  .model-row:hover {
    background: var(--bg-surface-hover);
  }

  .model-row.active {
    border-color: var(--line-strong);
    background: var(--bg-surface-hover);
  }

  .model-row.unavailable {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .star {
    width: 14px;
    color: var(--accent-warning);
    flex-shrink: 0;
  }

  /* The row's label: the provider's own name for the model when it publishes
     one, else the wire id. Not monospaced when it is a name — it is prose, and
     a name-first list reads wrong in a code font. `.wire` restores the mono
     face for the id-fallback rows so ids keep their familiar shape. */
  .model-name {
    font-size: 13px;
    color: var(--text-primary);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .model-name.wire {
    font-family: var(--font-mono);
  }

  /* The wire id riding behind a name label. The id is what actually goes on the
     wire and into config, so a name-first list must not hide it. */
  .model-id {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .provider-name {
    font-size: 11px;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .flags {
    margin-left: auto;
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }

  .flag {
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: transparent;
    border: 1px solid var(--line);
    color: var(--text-muted);
  }

  .flag.no-key {
    color: var(--accent-danger);
    border-color: var(--accent-danger);
    background: transparent;
  }

  .flag.locked {
    color: var(--text-muted);
    border-color: var(--line-strong);
    background: transparent;
  }

  .flag.current {
    color: var(--accent-info);
    border-color: var(--accent-info);
    background: transparent;
  }

  .modal-footer {
    padding: 0.6rem 1rem;
    border-top: 1px solid var(--line);
    font-size: 0.7rem;
    color: var(--text-muted);
  }
</style>
