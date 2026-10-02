<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";
  import type { ImagePart, ComposerCompletion } from "../types.js";

  interface PendingImage {
    part: ImagePart;
    /** Object URL for the thumbnail preview. */
    previewUrl: string;
  }

  let draft = $state("");
  let textareaEl: HTMLTextAreaElement;
  let fileInputEl: HTMLInputElement;
  let images = $state<PendingImage[]>([]);
  let completionMatches = $derived(
    daemon.composerCompletions
      .filter(
        (item) =>
          !(
            item.replace_start === 0 &&
            item.label === draft &&
            item.replace_end === Array.from(draft).length
          ),
      )
      .slice(0, 6),
  );

  const MAX_IMAGE_BYTES = 10 * 1024 * 1024;

  // Restore a prompt the daemon reports as never-sent (UnsentInput) so the
  // user can re-edit and re-send instead of retyping. The daemon only sets
  // `restoredDraft` when this composer reported itself idle, so the
  // asynchronous restore never clobbers in-progress typing (mirrors the
  // TUI's `DraftAdoption::OnlyIfIdle`).
  $effect(() => {
    const restored = daemon.restoredDraft;
    if (!restored) return;
    daemon.takeRestoredDraft();
    draft = restored.text;
    clearImages();
    for (const part of restored.images) {
      images.push({ part, previewUrl: `data:${part.mime};base64,${part.data}` });
    }
    resize();
  });

  // Report composer idleness so the daemon's UnsentInput handler can decide
  // between adopting the restored draft and keeping in-progress typing.
  $effect(() => {
    daemon.composerIdle = draft.length === 0 && images.length === 0;
  });

  function resize() {
    if (textareaEl) {
      textareaEl.style.height = "auto";
      textareaEl.style.height = Math.min(textareaEl.scrollHeight, 180) + "px";
    }
  }

  function clearImages() {
    for (const img of images) {
      if (img.previewUrl.startsWith("blob:")) URL.revokeObjectURL(img.previewUrl);
    }
    images = [];
  }

  function handleSend() {
    const text = draft.trim();
    if ((!text && images.length === 0) || !daemon.sessionAttached) return;
    daemon.sendPrompt(
      text,
      images.map((i) => i.part),
    );
    draft = "";
    daemon.requestComposerCompletions("", 0);
    clearImages();
    resize();
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }

  function handleInput() {
    resize();
    daemon.requestComposerCompletions(draft, textareaEl?.selectionStart ?? draft.length);
  }

  function readFile(file: File): Promise<PendingImage | null> {
    return new Promise((resolve) => {
      if (!file.type.startsWith("image/")) return resolve(null);
      if (file.size > MAX_IMAGE_BYTES) {
        daemon.pushToast("warning", t("imageTooLarge"), t("imageTooLargeBody")(file.name));
        return resolve(null);
      }
      const reader = new FileReader();
      reader.onload = () => {
        const dataUrl = reader.result as string;
        const base64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
        resolve({
          part: { mime: file.type, data: base64 },
          previewUrl: URL.createObjectURL(file),
        });
      };
      reader.onerror = () => resolve(null);
      reader.readAsDataURL(file);
    });
  }

  async function addFiles(files: Iterable<File>) {
    for (const file of files) {
      const pending = await readFile(file);
      if (pending) images.push(pending);
    }
  }

  function handlePaste(e: ClipboardEvent) {
    const files = Array.from(e.clipboardData?.files ?? []).filter((f) =>
      f.type.startsWith("image/"),
    );
    if (files.length > 0) {
      e.preventDefault();
      void addFiles(files);
    }
  }

  function handleFilePicked(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    if (input.files) void addFiles(Array.from(input.files));
    input.value = "";
  }

  function removeImage(index: number) {
    const [removed] = images.splice(index, 1);
    if (removed && removed.previewUrl.startsWith("blob:")) {
      URL.revokeObjectURL(removed.previewUrl);
    }
  }

  function insertCommand(cmd: string) {
    draft = cmd;
    daemon.requestComposerCompletions(cmd, cmd.length);
    if (textareaEl) {
      textareaEl.focus();
    }
  }

  function scalarToUtf16(text: string, scalarIndex: number): number {
    return Array.from(text).slice(0, scalarIndex).join("").length;
  }

  function handleBoxClick(e: MouseEvent) {
    const target = e.target as HTMLElement | null;
    if (!target) return;
    if (target.closest("button, input, select, textarea, a, .command-completions")) {
      return;
    }
    textareaEl?.focus();
  }

  function acceptCompletion(item: ComposerCompletion) {
    const start = scalarToUtf16(draft, item.replace_start);
    const end = scalarToUtf16(draft, item.replace_end);
    draft = draft.slice(0, start) + item.insert_text + draft.slice(end);
    const caret = start + item.insert_text.length;
    resize();
    queueMicrotask(() => {
      textareaEl?.focus();
      textareaEl?.setSelectionRange(caret, caret);
      daemon.requestComposerCompletions(draft, caret);
    });
  }
</script>

<footer class="composer-container">
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="composer-box" onclick={handleBoxClick}>
    {#if images.length > 0}
      <div class="image-chips">
        {#each images as img, i (img.previewUrl)}
          <span class="chip">
            <img src={img.previewUrl} alt="attachment" />
            <button class="remove" aria-label={t("removeImage")} onclick={() => removeImage(i)}>×</button>
          </span>
        {/each}
      </div>
    {/if}

    <textarea
      bind:this={textareaEl}
      bind:value={draft}
      onkeydown={handleKeyDown}
      oninput={handleInput}
      onpaste={handlePaste}
      placeholder={t("composerPlaceholder")}
      rows="1"
      disabled={!daemon.sessionAttached}
    ></textarea>

    {#if completionMatches.length > 0}
      <div class="command-completions" aria-label={t("commandCompletions")}>
        {#each completionMatches as item (`${item.kind}:${item.label}`)}
          <button
            type="button"
            class:alias-row={item.kind === 'slash_alias' || (item.alias_of !== undefined && item.alias_of !== null)}
            onclick={() => acceptCompletion(item)}
          >
            <code>{item.label}</code>
            {#if item.alias_of}
              <span class="alias-target" title={`Accepting submits ${item.alias_of}`}
                >↝ {item.alias_of}</span
              >
            {:else}
              <span>{item.description}</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}

    <div class="toolbar">
      <div class="hints">
        <button class="hint-pill" onclick={() => insertCommand("/help")}>/help</button>
        <button class="hint-pill" onclick={() => insertCommand("/status")}>/status</button>
        <button class="hint-pill" onclick={() => insertCommand("/mcp")}>/mcp</button>
      </div>

      <div class="actions">
        <input
          bind:this={fileInputEl}
          type="file"
          accept="image/*"
          multiple
          class="file-input"
          onchange={handleFilePicked}
        />
        <button
          class="attach-btn"
          aria-label={t("attachImage")}
          title={t("attachImage")}
          disabled={!daemon.sessionAttached}
          onclick={() => fileInputEl?.click()}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
            <path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l8.57-8.57A4 4 0 1118 8.84l-8.59 8.57a2 2 0 01-2.83-2.83l8.49-8.48"/>
          </svg>
        </button>
        <button
          class="send-btn"
          aria-label={t("sendMessage")}
          disabled={(!draft.trim() && images.length === 0) || !daemon.sessionAttached}
          onclick={handleSend}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z"/>
          </svg>
        </button>
      </div>
    </div>
  </div>
</footer>

<style>
  .composer-container {
    padding: 0.5rem var(--pad-x) 1.4rem;
    background: linear-gradient(to top, var(--bg-app) 78%, transparent);
  }

  /* The composer floats on the paper like a blank scroll awaiting the
     brush: a single raised surface, hairline border, no heavy shadow. */
  .composer-box {
    width: min(var(--measure), 100%);
    margin: 0 auto;
    background-color: var(--input-bg-inactive);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-lg);
    padding: 0.7rem 1rem 0.5rem;
    display: flex;
    flex-direction: column;
    transition: background-color var(--t-fast), border-color var(--t-fast);
  }

  /* Focused paper brightens — the only "lifted" state in the whole UI. */
  .composer-box:focus-within {
    background-color: var(--input-bg-active);
    border-color: var(--border-input-focus);
  }

  textarea {
    background: transparent;
    border: none;
    color: var(--text-primary);
    font-family: var(--font-sans);
    font-size: 0.92rem;
    line-height: 1.6;
    resize: none;
    outline: none;
    min-height: 1.6em;
    max-height: 180px;
  }

  textarea::placeholder {
    color: var(--text-muted);
    opacity: 0.7;
  }

  textarea:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .command-completions {
    display: grid;
    gap: 2px;
    margin: 0.35rem -0.35rem 0.1rem;
    padding-top: 0.35rem;
    border-top: 1px solid var(--line);
  }

  .command-completions button {
    display: grid;
    grid-template-columns: minmax(8rem, auto) 1fr;
    gap: 0.75rem;
    align-items: baseline;
    padding: 0.35rem 0.4rem;
    border: 0;
    border-radius: var(--radius-sm);
    color: var(--text-secondary);
    background: transparent;
    text-align: left;
    cursor: pointer;
    font-size: 0.8rem;
  }

  .command-completions button:hover,
  .command-completions button:focus-visible {
    color: var(--text-primary);
    background: var(--bg-surface-hover);
    outline: none;
  }

  .command-completions code {
    color: var(--accent-primary);
    font-family: var(--font-mono);
    font-size: 0.78rem;
  }

  /* Alias candidates are a secondary tier: the alias keeps the primary slot
     but is visually quieter, and the secondary column shows the canonical
     command accepting will submit instead of the target's summary. */
  .command-completions button.alias-row code {
    color: var(--text-secondary);
    font-weight: normal;
  }

  .command-completions .alias-target {
    color: var(--accent-primary);
    opacity: 0.75;
    font-family: var(--font-mono);
    font-size: 0.72rem;
  }

  .image-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-bottom: 0.5rem;
  }

  .chip {
    position: relative;
    width: 48px;
    height: 48px;
    border-radius: var(--radius-md);
    overflow: hidden;
    border: 1px solid var(--line-strong);
  }

  .chip img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }

  .chip .remove {
    position: absolute;
    top: 2px;
    right: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: none;
    background: rgba(0, 0, 0, 0.65);
    color: #fff;
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .toolbar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-top: 0.5rem;
  }

  .hints {
    display: flex;
    gap: 0.35rem;
  }

  .hint-pill {
    font-family: var(--font-mono);
    font-size: 0.68rem;
    padding: 0.12rem 0.4rem;
    border-radius: var(--radius-sm);
    background-color: transparent;
    color: var(--text-muted);
    border: 1px solid var(--line);
    cursor: pointer;
    transition: color var(--t-fast), border-color var(--t-fast);
  }

  .hint-pill:hover {
    color: var(--text-secondary);
    border-color: var(--line-strong);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .file-input {
    display: none;
  }

  .attach-btn {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    background: transparent;
    color: var(--text-muted);
    border: 1px solid var(--line);
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: color var(--t-fast), border-color var(--t-fast);
  }

  .attach-btn:not(:disabled):hover {
    color: var(--text-secondary);
    border-color: var(--line-strong);
  }

  .attach-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  /* 朱砂 send: the one filled element in the entire interface. */
  .send-btn {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    background-color: var(--accent-primary);
    color: var(--bg-app);
    border: none;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition: opacity var(--t-fast), transform var(--t-fast);
  }

  .send-btn:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }

  .send-btn:not(:disabled):hover {
    opacity: 0.88;
    transform: translateY(-1px);
  }

  @media (prefers-reduced-motion: reduce) {
    .send-btn:not(:disabled):hover {
      transform: none;
    }
  }
</style>
