<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";

  interface Props {
    onclose: () => void;
  }

  let { onclose }: Props = $props();

  let wsUrl = $state(daemon.wsUrl);
  let project = $state(daemon.project ?? "");
  let token = $state(daemon.token ?? "");

  onMount(() => {
    void daemon.probe();
  });

  let urlValid = $derived(
    /^(wss?:\/\/)?[\w.-]+(:\d+)?(\/\S*)?$/.test(wsUrl.trim()) && wsUrl.trim().length > 0,
  );

  let probeHint = $derived.by(() => {
    const probe = daemon.daemonProbe;
    if (!probe) return null;
    if (probe.auth && !token.trim()) {
      return t("authRequired");
    }
    if (probe.auth) return t("authEnabled");
    return t("daemonReachable")(probe.version);
  });

  function save() {
    if (!urlValid) return;
    const trimmed = wsUrl.trim();
    daemon.applyConfig({
      wsUrl: trimmed.startsWith("ws") ? trimmed : `ws://${trimmed}`,
      project: project.trim() || null,
      token: token.trim() || null,
    });
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
  <div class="modal" role="dialog" aria-label="Connection settings">
    <div class="modal-header">
      <h3>Connection</h3>
      <button class="close" aria-label="Close" onclick={onclose}>×</button>
    </div>

    <div class="modal-body">
      <label class="field">
        <span class="label">Daemon WebSocket URL</span>
        <input
          type="text"
          bind:value={wsUrl}
          placeholder="ws://127.0.0.1:9800"
          spellcheck="false"
          class:invalid={!urlValid}
        />
        <span class="hint">
          The daemon listens on <code>ws://127.0.0.1:9800</code> by default (falling back to an
          ephemeral port, recorded in the discovery file, when 9800 is taken).
        </span>
      </label>

      <label class="field">
        <span class="label">Bearer token</span>
        <input
          type="password"
          bind:value={token}
          placeholder="required when the daemon has auth on"
          spellcheck="false"
          autocomplete="off"
        />
        <span class="hint">
          Daemons started with default settings require a token (local_auth). Find it in the
          discovery file <code>$XDG_RUNTIME_DIR/muta/daemon.json</code> — it is sent as a
          <code>bearer.</code> subprotocol because browsers cannot set WebSocket headers.
        </span>
      </label>

      <label class="field">
        <span class="label">Project path (optional)</span>
        <input
          type="text"
          bind:value={project}
          placeholder={daemon.daemonProjectRoot || "/path/to/project"}
          spellcheck="false"
        />
        <span class="hint">
          Scopes session creation and monitoring to this project. Empty uses the daemon's
          own project root{daemon.daemonProjectRoot ? ` (${daemon.daemonProjectRoot})` : ""}.
        </span>
      </label>

      <div class="status-row">
        <span class="label">Status</span>
        <span class="value" class:ok={daemon.connection === "connected"}>
          {daemon.connection}
        </span>
      </div>
      {#if probeHint}
        <div class="probe-hint" class:warn={daemon.daemonProbe?.auth && !token.trim()}>
          {probeHint}
        </div>
      {/if}
    </div>

    <div class="modal-footer">
      <button class="btn-secondary" onclick={onclose}>Cancel</button>
      <button class="btn-primary" disabled={!urlValid} onclick={save}>Save & reconnect</button>
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
    padding-top: 14vh;
    z-index: 100;
  }

  .modal {
    width: 480px;
    max-width: calc(100vw - 32px);
    max-height: 82vh;
    overflow-y: auto;
    background-color: var(--bg-surface);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-modal);
    display: flex;
    flex-direction: column;
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
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .label {
    font-family: var(--font-mono);
    font-size: 0.65rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.1em;
  }

  input {
    background-color: var(--input-bg-inactive);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-md);
    padding: 0.5rem 0.6rem;
    color: var(--text-primary);
    font-size: 0.82rem;
    font-family: var(--font-mono);
    outline: none;
  }

  input:focus {
    border-color: var(--border-input-focus);
    background-color: var(--input-bg-active);
  }

  input.invalid {
    border-color: var(--accent-danger);
  }

  .hint {
    font-size: 0.72rem;
    color: var(--text-muted);
    line-height: 1.5;
  }

  .hint code {
    font-family: var(--font-mono);
    background: var(--bg-code);
    border: 1px solid var(--line);
    padding: 0 0.25rem;
    border-radius: var(--radius-sm);
  }

  .status-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .status-row .value {
    font-family: var(--font-mono);
    font-size: 0.75rem;
    color: var(--accent-danger);
    text-transform: uppercase;
  }

  .status-row .value.ok {
    color: var(--accent-info);
  }

  .probe-hint {
    font-size: 0.72rem;
    font-family: var(--font-mono);
    color: var(--text-secondary);
    background: var(--bg-code);
    border: 1px solid var(--line);
    border-radius: var(--radius-md);
    padding: 0.5rem 0.6rem;
    line-height: 1.5;
  }

  .probe-hint.warn {
    color: var(--accent-warning);
    border-color: var(--accent-warning);
  }

  .modal-footer {
    padding: 0.7rem 1rem;
    border-top: 1px solid var(--line);
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
  }

  .btn-primary,
  .btn-secondary {
    font-size: 0.78rem;
    font-weight: 500;
    padding: 0.4rem 0.85rem;
    border-radius: var(--radius-md);
    cursor: pointer;
    border: 1px solid transparent;
  }

  .btn-primary {
    background-color: var(--accent-primary);
    color: var(--bg-app);
    border: none;
  }

  .btn-primary:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .btn-secondary {
    background: transparent;
    border-color: var(--line-strong);
    color: var(--text-secondary);
  }

  .btn-secondary:hover {
    background: var(--bg-surface-hover);
  }
</style>
