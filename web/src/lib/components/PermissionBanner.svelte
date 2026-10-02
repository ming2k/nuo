<script lang="ts">
  import { daemon } from "../stores/daemon.svelte.js";
  import { t } from "../i18n.svelte.js";

  let inputText = $state("");
  let selected: number[][] = $state([]);
  // ADR-0141 web/TUI parity: free-text "Other" per question, mirroring the
  // TUI picker. An "other" answer replaces the option selection: the wire
  // carries the raw text as that question's single answer label.
  let otherText: string[] = $state([]);
  let otherActive: boolean[] = $state([]);
  let answerError = $state("");

  $effect(() => {
    // Reset the local answer state whenever a new question arrives.
    if (daemon.pendingQuestion) {
      const n = daemon.pendingQuestion.request.questions.length;
      selected = daemon.pendingQuestion.request.questions.map(() => []);
      otherText = Array.from({ length: n }, () => "");
      otherActive = Array.from({ length: n }, () => false);
      answerError = "";
    }
    if (daemon.pendingInput) {
      inputText = "";
    }
  });

  function toggleOption(qi: number, oi: number) {
    const q = daemon.pendingQuestion?.request.questions[qi];
    if (!q) return;
    const multi = q.multi_select;
    const current = selected[qi] ?? [];
    selected[qi] = multi
      ? current.includes(oi)
        ? current.filter((i) => i !== oi)
        : [...current, oi]
      : current.includes(oi)
        ? []
        : [oi];
    selected = [...selected];
  }

  function toggleOther(qi: number) {
    otherActive[qi] = !otherActive[qi];
    otherActive = [...otherActive];
    if (otherActive[qi]) selected[qi] = [];
    selected = [...selected];
  }

  /** A question is answered iff it has ≥1 selected option or non-blank Other. */
  function answered(qi: number): boolean {
    const picks = selected[qi] ?? [];
    if (otherActive[qi] && (otherText[qi] ?? "").trim() !== "") return true;
    return picks.length > 0;
  }

  function submitQuestion() {
    const req = daemon.pendingQuestion?.request;
    if (!req) return;
    // Zero-selection validation (ADR-0141): a non-empty outer array with
    // empty inner arrays is undefined in the parked-question protocol — the
    // reserved cancellation shape is an empty OUTER array. Refuse to send
    // an undefined payload; the operator must answer or explicitly cancel.
    const missing = req.questions
      .map((_, qi) => qi)
      .filter((qi) => !answered(qi));
    if (missing.length > 0) {
      answerError = t("questionNeedsAnswer")(missing.length);
      return;
    }
    answerError = "";
    const answers = req.questions.map((_, qi) =>
      otherActive[qi] && (otherText[qi] ?? "").trim() !== ""
        ? [otherText[qi].trim()]
        : (selected[qi] ?? []).map((oi) => req.questions[qi].options[oi]?.label ?? ""),
    );
    daemon.answerQuestion(answers);
  }

  function cancelQuestion() {
    // The reserved cancellation shape: empty OUTER array. Settles the
    // parked question as cancelled; the model is told the operator declined
    // to answer.
    answerError = "";
    daemon.answerQuestion([]);
  }

  function submitInput() {
    daemon.replyInput(inputText);
    inputText = "";
  }
</script>

{#if daemon.pendingPermission}
  <div class="banner permission">
    <div class="head">
      <span class="seal-mark" class:elevated={daemon.pendingPermission.request.elevation}>
        {daemon.pendingPermission.request.elevation ? "危" : "许"}
      </span>
      <span class="title">
        {daemon.pendingPermission.request.label || daemon.pendingPermission.request.tool}
      </span>
      {#if daemon.pendingPermission.origin.label}
        <span class="origin">subagent: {daemon.pendingPermission.origin.label}</span>
      {/if}
    </div>
    {#if daemon.pendingPermission.request.description}
      <p class="desc">{daemon.pendingPermission.request.description}</p>
    {/if}
    <div class="args">
      <span class="label">scope: {daemon.pendingPermission.request.scope}</span>
      <pre>{daemon.pendingPermission.request.arguments}</pre>
    </div>
    <div class="actions">
      <button class="btn primary" onclick={() => daemon.resolvePermission("Once")}>
        {t("allowOnce")}
      </button>
      {#if !daemon.pendingPermission.request.one_off}
        <button class="btn" onclick={() => daemon.resolvePermission("Always")}>
          {t("alwaysAllow")}
        </button>
      {/if}
      <button class="btn danger" onclick={() => daemon.resolvePermission("Reject")}>
        {t("reject")}
      </button>
    </div>
  </div>
{:else if daemon.pendingQuestion}
  <div class="banner question">
    <div class="head">
      <span class="seal-mark">问</span>
      <span class="title">{t("agentNeedsAnswer")}</span>
      {#if daemon.pendingQuestion.origin.label}
        <span class="origin">subagent: {daemon.pendingQuestion.origin.label}</span>
      {/if}
    </div>
    {#each daemon.pendingQuestion.request.questions as q, qi (qi)}
      <div class="question-block">
        {#if q.header}<span class="q-header">{q.header}</span>{/if}
        <p class="q-text">{q.question}</p>
        <div class="options">
          {#each q.options as opt, oi (oi)}
            <button
              class="option"
              class:selected={(selected[qi] ?? []).includes(oi)}
              onclick={() => toggleOption(qi, oi)}
            >
              <span class="opt-label">{opt.label}</span>
              {#if opt.description}
                <span class="opt-desc">{opt.description}</span>
              {/if}
            </button>
          {/each}
        </div>
        <div class="other-row">
          <button
            class="option other-toggle"
            class:selected={otherActive[qi]}
            onclick={() => toggleOther(qi)}
          >{t("otherOption")}</button>
          {#if otherActive[qi]}
            <input
              class="other-input"
              type="text"
              bind:value={otherText[qi]}
              placeholder={t("otherPlaceholder")}
              onkeydown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  submitQuestion();
                }
              }}
            />
          {/if}
        </div>
      </div>
    {/each}
    {#if answerError}<p class="answer-error">{answerError}</p>{/if}
    <div class="actions">
      <button class="btn primary" onclick={submitQuestion}>{t("answer")}</button>
      <button class="btn danger" onclick={cancelQuestion}>{t("cancel")}</button>
    </div>
  </div>
{:else if daemon.pendingInput}
  <div class="banner input">
    <div class="head">
      <span class="seal-mark">入</span>
      <span class="title">{daemon.pendingInput.request.prompt}</span>
      {#if daemon.pendingInput.origin.label}
        <span class="origin">subagent: {daemon.pendingInput.origin.label}</span>
      {/if}
    </div>
    <p class="desc mono">{daemon.pendingInput.request.command}</p>
    <div class="input-row">
      <input
        type={daemon.pendingInput.request.secret ? "password" : "text"}
        bind:value={inputText}
        placeholder={daemon.pendingInput.request.secret ? t("secretInput") : t("commandInput")}
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            submitInput();
          }
        }}
      />
      <button class="btn primary" onclick={submitInput}>{t("send")}</button>
    </div>
  </div>
{/if}

<style>
  .banner {
    width: min(var(--measure), 100%);
    margin: 0 auto 0.6rem;
    border: 1px solid var(--line-strong);
    border-left: 3px solid var(--accent-warning);
    border-radius: var(--radius-md);
    background-color: var(--bg-surface);
    padding: 0.75rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    flex-shrink: 0;
  }

  .banner.permission,
  .banner.input {
    border-left-color: var(--accent-warning);
  }

  .head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  /* Mini seal marking the request class. */
  .seal-mark {
    font-family: var(--font-brush);
    font-size: 0.8rem;
    width: 1.5rem;
    height: 1.5rem;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border-radius: 3px;
    flex-shrink: 0;
    color: var(--accent-warning);
    border: 1px solid var(--accent-warning);
    background: transparent;
  }

  .seal-mark.elevated {
    color: var(--accent-danger);
    border-color: var(--accent-danger);
  }

  .origin {
    font-family: var(--font-mono);
    font-size: 0.62rem;
    padding: 0.05rem 0.35rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--line-strong);
    color: var(--text-muted);
  }

  .title {
    font-weight: 600;
    font-size: 0.88rem;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .desc {
    font-size: 0.78rem;
    color: var(--text-secondary);
    margin: 0;
  }

  .desc.mono {
    font-family: var(--font-mono);
    word-break: break-all;
  }

  .args {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }

  .label {
    font-family: var(--font-mono);
    font-size: 0.62rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  pre {
    background-color: var(--bg-code);
    border: 1px solid var(--line);
    border-radius: var(--radius-sm);
    padding: 0.5rem 0.6rem;
    font-family: var(--font-mono);
    font-size: 0.7rem;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 140px;
    overflow-y: auto;
    margin: 0;
  }

  .actions {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .btn {
    font-size: 0.78rem;
    font-weight: 500;
    padding: 0.35rem 0.8rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--line-strong);
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
    transition: background-color var(--t-fast), border-color var(--t-fast);
  }

  .btn:hover {
    background: var(--bg-surface-hover);
  }

  .btn.primary {
    border-color: var(--accent-info);
    color: var(--accent-info);
  }

  .btn.primary:hover {
    background: color-mix(in srgb, var(--accent-info) 10%, transparent);
  }

  .btn.danger {
    border-color: var(--accent-danger);
    color: var(--accent-danger);
  }

  .btn.danger:hover {
    background: var(--seal-soft);
  }

  .question-block {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }

  .q-header {
    font-family: var(--font-mono);
    font-size: 0.62rem;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  .q-text {
    font-size: 0.85rem;
    color: var(--text-primary);
    margin: 0;
  }

  .options {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  .option {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.1rem;
    text-align: left;
    padding: 0.5rem 0.65rem;
    border-radius: var(--radius-md);
    border: 1px solid var(--line);
    background: transparent;
    cursor: pointer;
    transition: border-color var(--t-fast), background-color var(--t-fast);
  }

  .option:hover {
    background-color: var(--bg-surface-hover);
  }

  /* Selected options take the brush-stroke treatment. */
  .option.selected {
    border-color: var(--accent-primary);
    box-shadow: inset 2px 0 0 var(--accent-primary);
    background: var(--seal-soft);
  }

  .opt-label {
    font-size: 0.82rem;
    color: var(--text-primary);
    font-weight: 500;
  }

  .opt-desc {
    font-size: 0.7rem;
    color: var(--text-muted);
  }

  .answer-error {
    color: var(--accent-danger);
    margin: 0;
    font-size: 0.78rem;
  }

  .other-row {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }

  .other-toggle {
    flex: 0 0 auto;
    font-style: italic;
  }

  .other-input,
  .input-row input {
    flex: 1;
    background-color: var(--input-bg-inactive);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-md);
    padding: 0.45rem 0.6rem;
    color: var(--text-primary);
    font-size: 0.82rem;
    font-family: var(--font-mono);
    outline: none;
  }

  .other-input:focus,
  .input-row input:focus {
    border-color: var(--border-input-focus);
    background-color: var(--input-bg-active);
  }

  .input-row {
    display: flex;
    gap: 0.5rem;
  }
</style>
