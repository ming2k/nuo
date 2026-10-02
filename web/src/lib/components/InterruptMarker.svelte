<script lang="ts">
  import type { RoundInterrupt } from "../types.js";
  import { interruptLabel } from "../stores/daemon.svelte.js";

  interface Props {
    record: RoundInterrupt;
  }

  let { record }: Props = $props();

  /** `HH:MM` local time of the stop — matches the TUI's `sent_time_label`. */
  let timeLabel = $derived.by(() => {
    const d = new Date(record.at_ms);
    const hh = String(d.getHours()).padStart(2, "0");
    const mm = String(d.getMinutes()).padStart(2, "0");
    return `${hh}:${mm}`;
  });

  let title = $derived(
    record.round != null ? `round ${record.round} · ${interruptLabel(record.reason)}` : interruptLabel(record.reason),
  );
</script>

<!--
  Round-interrupt marker (C11): one row per round stopped before completing.
  Mirrors the TUI's `▲ interrupted · HH:MM` entry — a warning-tone event row,
  never a dialogue bubble, so the reader can decide at a glance whether the
  interrupted round's work should be continued.
-->
<div class="interrupt-marker" role="status">
  <span class="glyph">▲</span>
  <span class="label">interrupted</span>
  <span class="detail">{title}</span>
  <span class="time">{timeLabel}</span>
</div>

<style>
  .interrupt-marker {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 6px 2px;
    font-size: 0.82rem;
    color: var(--text-muted);
  }

  .glyph,
  .label {
    color: var(--accent-warning);
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
</style>
