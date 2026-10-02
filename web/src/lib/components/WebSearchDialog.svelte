<script lang="ts">
  import { onMount } from "svelte";
  import { daemon } from "../stores/daemon.svelte.js";
  import type {
    WebConfigUpdate,
    WebProviderAxis,
    WebProviderCapability,
    WebReaderProvider,
    WebSearchProvider,
  } from "../types.js";

  interface Props { onclose: () => void }
  let { onclose }: Props = $props();
  let cfg = $derived(daemon.websearchConfig);
  let searchToken = $state("");
  let readerToken = $state("");
  let endpoint = $state<string | null>(null);

  let selectedSearch = $derived(selected("Search"));
  let selectedReader = $derived(selected("Reader"));

  onMount(() => daemon.queryWebSearchConfig());

  function capabilities(axis: WebProviderAxis): WebProviderCapability[] {
    return cfg?.capabilities.filter((item) => item.axis === axis) ?? [];
  }

  function selected(axis: WebProviderAxis): WebProviderCapability | undefined {
    const id = axis === "Search" ? cfg?.provider : cfg?.reader;
    return capabilities(axis).find((item) => item.id === id);
  }

  function patch(update: Omit<WebConfigUpdate, "expected_revision">) {
    if (!cfg) return;
    daemon.updateWebSearchConfig({ expected_revision: cfg.revision, ...update });
  }

  function setSearchProvider(value: string) {
    patch({ provider: value as WebSearchProvider });
  }

  function setReaderProvider(value: string) {
    patch({ reader: value as WebReaderProvider });
  }

  function saveCredential(axis: WebProviderAxis, providerId: string, value: string) {
    patch({ credential: { axis, provider_id: providerId, value: value.trim() } });
    if (axis === "Search") searchToken = "";
    else readerToken = "";
  }

  function statusLabel(status: string): string {
    switch (status) {
      case "Environment": return "Provided by environment";
      case "Stored": return "Stored securely";
      case "RequiredMissing": return "Required · missing";
      case "OptionalMissing": return "Optional · not configured";
      default: return "No token required";
    }
  }

  function handleBackdrop(event: MouseEvent) {
    if (event.target === event.currentTarget) onclose();
  }
</script>

<svelte:window onkeydown={(event) => event.key === "Escape" && onclose()} />

<div class="backdrop" onclick={handleBackdrop} role="presentation">
  <div class="modal" role="dialog" aria-label="Web tools settings">
    <header>
      <div><h3>Web tools</h3><p>One provider per capability. Changes apply immediately.</p></div>
      <button class="icon" aria-label="Close" onclick={onclose}>×</button>
    </header>

    {#if !cfg}
      <main><p class="muted">Loading configuration…</p></main>
    {:else}
      <main>
        <section>
          <div class="section-title"><h4>Search provider</h4><code>search_web</code></div>
          <select value={cfg.provider} onchange={(event) => setSearchProvider(event.currentTarget.value)}>
            {#each capabilities("Search") as provider (provider.id)}
              <option value={provider.id}>{provider.display_name}</option>
            {/each}
            <option value="disabled">Disabled</option>
          </select>
          {#if selectedSearch}
            <p class="muted">{selectedSearch.description}</p>
            {#if selectedSearch.endpoint === "UserSupplied"}
              <label>
                <span>Endpoint</span>
                <input value={endpoint ?? cfg.searxng_url ?? ""} oninput={(event) => endpoint = event.currentTarget.value} placeholder="https://search.example.com/search" />
              </label>
              <button onclick={() => { patch({ searxng_url: (endpoint ?? cfg.searxng_url ?? "").trim() }); endpoint = null; }}>Save endpoint</button>
            {:else if selectedSearch.credential !== "None"}
              <label>
                <span>API token <small>{statusLabel(cfg.search_credential)}</small></span>
                <input type="password" bind:value={searchToken} placeholder="Leave blank to clear" autocomplete="off" />
              </label>
              <button onclick={() => saveCredential("Search", selectedSearch.id, searchToken)}>Save token</button>
            {/if}
          {/if}
        </section>

        <section>
          <div class="section-title"><h4>Reader provider</h4><code>read_url</code></div>
          <select value={cfg.reader} onchange={(event) => setReaderProvider(event.currentTarget.value)}>
            {#each capabilities("Reader") as provider (provider.id)}
              <option value={provider.id}>{provider.display_name}</option>
            {/each}
            <option value="disabled">Disabled</option>
          </select>
          {#if selectedReader}
            <p class="muted">{selectedReader.description}</p>
            {#if selectedReader.credential !== "None"}
              <label>
                <span>API token <small>{statusLabel(cfg.reader_credential)}</small></span>
                <input type="password" bind:value={readerToken} placeholder="Leave blank to clear" autocomplete="off" />
              </label>
              <button onclick={() => saveCredential("Reader", selectedReader.id, readerToken)}>Save token</button>
            {/if}
          {/if}
        </section>

        <section>
          <div class="section-title"><h4>Request timeout</h4><span>{cfg.timeout_secs}s</span></div>
          <input type="range" min="5" max="120" step="5" value={cfg.timeout_secs} onchange={(event) => patch({ timeout_secs: Number(event.currentTarget.value) })} />
        </section>
      </main>
    {/if}

    <footer><span>Credentials are stored separately and never echoed back.</span><button onclick={onclose}>Done</button></footer>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; z-index: 100; display: grid; place-items: center; background: color-mix(in srgb, var(--bg-app) 55%, transparent); }
  .modal { width: min(600px, calc(100vw - 32px)); max-height: calc(100vh - 48px); overflow: auto; color: var(--text-primary); background: var(--bg-surface); border: 1px solid var(--line-strong); border-radius: 12px; box-shadow: 0 20px 60px #0008; }
  header, footer { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 16px 20px; }
  header { border-bottom: 1px solid var(--line-subtle); } footer { border-top: 1px solid var(--line-subtle); color: var(--text-muted); font-size: 12px; }
  h3, h4, p { margin: 0; } header p, .muted { color: var(--text-muted); font-size: 12px; margin-top: 4px; }
  main { display: grid; gap: 18px; padding: 20px; } section { display: grid; gap: 10px; padding: 14px; border: 1px solid var(--line-subtle); border-radius: 8px; }
  .section-title { display: flex; align-items: center; justify-content: space-between; } code, small { color: var(--text-muted); }
  label { display: grid; gap: 6px; font-size: 13px; } label span { display: flex; justify-content: space-between; }
  input, select, button { color: inherit; background: var(--bg-elevated); border: 1px solid var(--line-strong); border-radius: 6px; padding: 8px 10px; }
  button { cursor: pointer; justify-self: end; } .icon { border: 0; background: transparent; font-size: 20px; }
  input[type="range"] { width: 100%; padding: 0; }
</style>
