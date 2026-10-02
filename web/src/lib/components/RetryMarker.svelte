<script lang="ts">
  import type { RetryResolution } from "../types.js";

  interface Props {
    record: RetryResolution;
  }

  let { record }: Props = $props();

  /** `HH:MM` local time of the recovery — matches the TUI's marker rows. */
  let timeLabel = $derived.by(() => {
    const d = new Date(record.at_ms);
    const hh = String(d.getHours()).padStart(2, "0");
    const mm = String(d.getMinutes()).padStart(2, "0");
    return `${hh}:${mm}`;
  });

  let title = $derived(
    `Recovered after ${record.attempts} provider ${record.attempts === 1 ? "retry" : "retries"}` +
      (record.round != null ? ` (round ${record.round})` : ""),
  );
</script>

<!--
  Retry-resolution marker: the success-side twin of the InterruptMarker. One
  row per round that recovered from transient provider faults; the per-attempt
  fault lines ride as the expandable detail. Mirrors the TUI's folded
  "recovered · HH:MM" notice row.
-->
<details class="retry-marker" role="status">
  <summary>
    <span class="glyph">✓</span>
    <span class="label">recovered</span>
    <span class="detail">{title}</span>
    <span class="time">{timeLabel}</span>
  </summary>
  {#if record.faults.length > 0}
    <ul class="faults">
      {#each record.faults as fault}
        <li>{fault}</li>
      {/each}
    </ul>
  {/if}
</details>

<style>
  .retry-marker {
    padding: 6px 2px;
    font-size: 0.82rem;
    color: var(--text-muted);
  }

  .retry-marker summary {
    display: flex;
    align-items: baseline;
    gap: 8px;
    cursor: pointer;
    list-style: none;
  }

  .retry-marker summary::-webkit-details-marker {
    display: none;
  }

  .glyph,
  .label {
    color: var(--accent-info);
    font-weight: 600;
  }

  .detail {
    color: var(--text-secondary);
  }

  .time {
    margin-left: auto;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }

  .faults {
    margin: 6px 0 0;
    padding-left: 24px;
    color: var(--text-muted);
    font-size: 0.75rem;
  }

  .faults li {
    margin: 2px 0;
    overflow-wrap: anywhere;
  }
</style>
