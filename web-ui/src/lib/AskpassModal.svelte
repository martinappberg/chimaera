<script lang="ts">
  // The in-app SSH auth prompt (password / keyboard-interactive 2FA). ssh has
  // no tty in the app, so its prompts arrive via SSH_ASKPASS as a broadcast
  // `ssh-askpass` event (see crates/chimaera-app/src/askpass.rs). Mounted once
  // at the App root so ANY window can answer — a mid-session reconnect on the
  // workbench needs to prompt just as much as the home screen does.
  //
  // Prompts queue rather than replace: ssh asks sequentially (password, then
  // a Duo passcode), and clobbering an unanswered prompt would strand its ssh
  // waiting on an answer that can no longer be given.
  import { onMount } from "svelte";
  import {
    answerAskpass,
    askpassActive,
    listAskpass,
    onAskpass,
    onAskpassDone,
    type AskpassPrompt,
  } from "./native";

  /** Prompts awaiting an answer, oldest first; the head is on screen. */
  let queue = $state<AskpassPrompt[]>([]);
  const askpass = $derived(queue[0] ?? null);
  /** What the user has typed into the field. */
  let secretValue = $state("");
  /** Reveal the typed secret (a passcode is easier to check than a password). */
  let revealSecret = $state(false);

  // Tell the rest of the UI (the reconnect overlay) a prompt is on screen.
  $effect(() => {
    askpassActive.set(askpass !== null);
  });

  function enqueue(p: AskpassPrompt): void {
    if (queue.some((q) => q.id === p.id)) return;
    if (queue.length === 0) {
      secretValue = "";
      revealSecret = false;
    }
    queue = [...queue, p];
  }

  onMount(() => {
    const unlisteners: Array<() => void> = [];
    void onAskpass(enqueue).then((u) => unlisteners.push(u));
    // A prompt resolved elsewhere (another window answered, or ssh gave up
    // waiting) must leave this window's queue too.
    void onAskpassDone((id) => {
      queue = queue.filter((q) => q.id !== id);
    }).then((u) => unlisteners.push(u));
    // Pick up prompts raised before this window existed (startup restore
    // connects before any webview loads; the emit-only path would lose them).
    void listAskpass().then((pending) => pending.forEach(enqueue));
    return () => unlisteners.forEach((u) => u());
  });

  function advance(): void {
    queue = queue.slice(1);
    secretValue = "";
    revealSecret = false;
  }

  function submit(): void {
    const p = askpass;
    if (p === null) return;
    void answerAskpass(p.id, secretValue);
    advance();
  }

  function cancel(): void {
    const p = askpass;
    if (p === null) return;
    void answerAskpass(p.id, null);
    advance();
  }

  /** Focus the field the moment the prompt appears. */
  function focusOnShow(node: HTMLInputElement): void {
    node.focus();
  }
</script>

{#if askpass !== null}
  <div
    class="askpass-backdrop"
    role="presentation"
    onclick={cancel}
    onkeydown={(e) => e.key === "Escape" && cancel()}
  >
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div
      class="askpass"
      role="dialog"
      aria-modal="true"
      aria-label="SSH authentication"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
    >
      <div class="askpass-head">
        <span class="askpass-glyph" aria-hidden="true">&#128274;</span>
        <span class="askpass-title">authenticate</span>
      </div>
      <pre class="askpass-prompt">{askpass.prompt}</pre>
      <div class="askpass-field">
        <input
          class="askpass-input"
          type={revealSecret ? "text" : "password"}
          autocomplete="off"
          autocapitalize="off"
          autocorrect="off"
          spellcheck="false"
          bind:value={secretValue}
          use:focusOnShow
          onkeydown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submit();
            } else if (e.key === "Escape") {
              e.preventDefault();
              cancel();
            }
          }}
        />
        <button
          class="askpass-reveal"
          type="button"
          title={revealSecret ? "hide" : "show"}
          onclick={() => (revealSecret = !revealSecret)}>{revealSecret ? "hide" : "show"}</button
        >
      </div>
      <div class="askpass-actions">
        <button class="askpass-cancel" onclick={cancel}>cancel</button>
        <button class="askpass-go" onclick={submit}>authenticate</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .askpass-backdrop {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--scrim);
    backdrop-filter: blur(2px);
  }

  .askpass {
    width: min(440px, 100%);
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 20px;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 10px;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.35);
  }

  .askpass-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .askpass-glyph {
    font-size: var(--text-md);
  }

  .askpass-title {
    font-size: var(--text-md);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--fg);
  }

  .askpass-prompt {
    margin: 0;
    max-height: 40vh;
    overflow-y: auto;
    padding: 10px 12px;
    background: var(--row-hover);
    border: 1px solid var(--edge);
    border-radius: 6px;
    font-family: var(--mono);
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .askpass-field {
    display: flex;
    align-items: stretch;
    gap: 6px;
  }

  .askpass-input {
    flex: 1;
    min-width: 0;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    color: var(--fg);
    font: inherit;
    font-family: var(--mono);
    padding: 8px 10px;
    outline: none;
  }

  .askpass-input:focus {
    border-color: var(--focus-ring);
  }

  .askpass-reveal {
    appearance: none;
    background: transparent;
    border: 1px solid var(--edge);
    border-radius: 6px;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 0 10px;
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      color 0.12s ease;
  }

  .askpass-reveal:hover {
    border-color: var(--accent);
    color: var(--fg);
  }

  .askpass-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .askpass-cancel,
  .askpass-go {
    appearance: none;
    font: inherit;
    font-size: var(--text-md);
    padding: 7px 16px;
    border-radius: 6px;
    cursor: pointer;
    border: 1px solid var(--edge);
    transition:
      border-color 0.12s ease,
      background 0.12s ease;
  }

  .askpass-cancel {
    background: transparent;
    color: var(--muted);
  }

  .askpass-cancel:hover {
    border-color: var(--accent);
    color: var(--fg);
  }

  .askpass-go {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg);
    font-weight: 600;
  }

  .askpass-go:hover {
    filter: brightness(1.08);
  }
</style>
