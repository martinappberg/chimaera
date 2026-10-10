<script lang="ts">
  import type { AskpassPrompt } from "../net/native";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";
  import { askpassPresentation } from "./askpassPresentation";

  let { prompt, onAnswer }: { prompt: AskpassPrompt; onAnswer(id: number, secret: string | null): void } = $props();
  const askpass = $derived(prompt);
  const presentation = $derived(askpassPresentation(askpass));
  let secretValue = $state("");
  let revealSecret = $state(false);
  $effect(() => {
    // Completion in another window can advance the same mounted dialog.
    askpass.id;
    secretValue = "";
    revealSecret = false;
  });
  function submit(): void {
    if (presentation.type !== "unsupported") {
      onAnswer(askpass.id, presentation.type === "host_key" ? "yes" : secretValue);
    }
  }
  function cancel(): void { onAnswer(askpass.id, null); }
</script>

{#if askpass !== null}
  <!-- Only Escape or the cancel button cancels: a cancel ends the ssh attempt
       (see askpass.rs), so a stray click on the backdrop — on the way to a
       password manager or the Duo app — must not. -->
  <div
    class="askpass-backdrop"
    role="presentation"
    onkeydown={(e) => e.key === "Escape" && cancel()}
  >
    <div
      class="askpass"
      role="dialog"
      aria-modal="true"
      aria-label={presentation?.type === "host_key" ? "Trust SSH host" : "SSH authentication"}
      tabindex="-1"
      use:modalFocus={{ priority: 1 }}
    >
      <!-- Keep one restoration owner across the queue; only prompt controls
           remount so each next prompt gets its safe initial focus. -->
      {#key askpass.id}
        <div class="askpass-head">
          <span class="askpass-glyph" aria-hidden="true">&#128274;</span>
          <span class="askpass-title">
            {presentation?.type === "host_key" ? "Trust this SSH host?" : "Sign in"}{askpass.alias != null ? ` · ${askpass.alias}` : ""}
          </span>
        </div>
        {#if askpass.source?.type === "keeper"}
          <p class="askpass-source">For the connection that stays on while you’re away</p>
        {/if}
        {#if presentation?.type === "host_key"}
          <p class="askpass-trust-note">
            Check this fingerprint with the host’s administrator before trusting it.
            This host will be remembered for SSH connections on this Mac.
          </p>
          <dl class="askpass-trust-details">
            <dt>Host</dt><dd>{presentation.host}</dd>
            <dt>Fingerprint</dt><dd>{presentation.fingerprint}</dd>
          </dl>
        {:else if presentation?.type === "unsupported"}
          <p class="askpass-trust-note">
            This SSH confirmation couldn’t be verified. Cancel and try connecting again.
          </p>
        {:else}
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
              use:focusOnMount
              onkeydown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  submit();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  e.stopPropagation();
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
        {/if}
        <div class="askpass-actions">
          <button
            class="askpass-cancel"
            use:focusOnMount={presentation?.type !== "secret"}
            onclick={cancel}>Cancel</button
          >
          {#if presentation?.type !== "unsupported"}
            <button class="askpass-go" onclick={submit}>
              {presentation?.type === "host_key" ? "Trust and connect" : "Continue"}
            </button>
          {/if}
        </div>
      {/key}
    </div>
  </div>
{/if}

<style>
  .askpass-source {
    margin: -4px 0 12px;
    color: var(--muted);
    font-size: var(--text-sm);
  }

  .askpass-trust-note {
    margin: 0;
    color: var(--muted);
    font-size: var(--text-sm);
    line-height: 1.5;
  }

  .askpass-trust-details { margin: 0; padding: 12px; background: var(--row-hover); border: 1px solid var(--edge); border-radius: 6px; font-size: var(--text-sm); }
  .askpass-trust-details dt { color: var(--muted); margin-top: 10px; }
  .askpass-trust-details dt:first-child { margin-top: 0; }
  .askpass-trust-details dd { margin: 4px 0 0; font-family: var(--mono); overflow-wrap: anywhere; color: var(--fg); }

  .askpass-backdrop {
    position: fixed;
    inset: 0;
    /* SSH is synchronously blocked on this answer. Keep it above every
       ordinary picker, confirm dialog, toast, and reconnect UI. */
    z-index: 230;
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
