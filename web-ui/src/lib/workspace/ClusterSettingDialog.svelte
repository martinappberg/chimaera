<script lang="ts">
  /**
   * The cluster page's two per-cluster settings, each a small dialog:
   * - "startup": the cluster-default startup commands (every workspace job
   *   on this cluster runs them first; stored on the cluster).
   * - "rules": what agents in this cluster's workspaces are told about the
   *   cluster's rules — a file on the cluster and/or pasted text.
   */
  import { clusterSetAgentRules, clusterSetStartup, type AgentRules } from "../net/native";
  import { modalFocus } from "../shared/modalFocus";
  import { focusOnMount } from "../shared/focusOnMount";

  interface Props {
    alias: string;
    mode: "startup" | "rules";
    /** Current cluster-default startup commands ("startup" mode). */
    startup?: string;
    /** Current rules ("rules" mode). */
    rules?: AgentRules;
    /** Saved — the page refetches. */
    onSaved: () => void;
    onClose: () => void;
  }

  let { alias, mode, startup = "", rules = { text: "" }, onSaved, onClose }: Props = $props();

  // The dialog mounts fresh per open: the props' values are the baseline.
  // svelte-ignore state_referenced_locally
  let text = $state(mode === "startup" ? startup : rules.text);
  // svelte-ignore state_referenced_locally
  let file = $state(rules.file ?? "");
  let fileError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function save(): Promise<void> {
    if (busy) return;
    error = null;
    fileError = null;
    busy = true;
    try {
      if (mode === "startup") {
        await clusterSetStartup(alias, null, text);
      } else {
        const f = file.trim();
        if (f !== "" && !f.startsWith("/")) {
          fileError = "A full path on the cluster, starting with /.";
          busy = false;
          return;
        }
        await clusterSetAgentRules(alias, { file: f === "" ? null : f, text });
      }
      onSaved();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape" && !busy) {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="overlay">
  <button class="scrim" aria-label="Close" tabindex="-1" onclick={() => !busy && onClose()}></button>
  <div
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label={mode === "startup" ? `Startup commands on ${alias}` : `Rules for agents on ${alias}`}
    tabindex="-1"
    use:modalFocus
  >
    <form
      class="body"
      onsubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <div class="title">
        {mode === "startup" ? "Startup commands" : "Rules for agents"}
        <span class="host">on {alias}</span>
      </div>
      {#if mode === "startup"}
        <p class="lede">
          Every workspace job on {alias} runs these first, before every shell and agent in it —
          then the workspace's own, then a run's.
        </p>
        <textarea
          class="in mono"
          bind:value={text}
          rows="6"
          spellcheck="false"
          placeholder={"module load …\nexport …"}
          use:focusOnMount
        ></textarea>
      {:else}
        <p class="lede">
          Agents in this cluster's workspaces are told these rules. Without any, they get a short
          generic set: explicit time limits, polite queue checks, nothing left running on login
          nodes.
        </p>
        <label class="field">
          <span class="lab">A file on the cluster</span>
          <input
            class="in mono"
            bind:value={file}
            placeholder="/full/path/to/rules-for-agents.md"
            spellcheck="false"
            autocomplete="off"
            oninput={() => (fileError = null)}
            use:focusOnMount
          />
          {#if fileError !== null}
            <span class="err">{fileError}</span>
          {:else}
            <span class="hint">Some clusters publish one; agents read it before significant work.</span>
          {/if}
        </label>
        <label class="field">
          <span class="lab">Text</span>
          <textarea
            class="in"
            bind:value={text}
            rows="6"
            spellcheck="true"
            placeholder="Paste the rules, or write your own."
          ></textarea>
        </label>
      {/if}
      {#if error !== null}
        <div class="err">{error}</div>
      {/if}
      <div class="acts">
        <button type="button" class="quiet" disabled={busy} onclick={onClose}>Cancel</button>
        <button type="submit" class="cta" disabled={busy}>{busy ? "Saving…" : "Save"}</button>
      </div>
    </form>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    animation: fade 0.1s ease-out;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }

  .scrim {
    position: absolute;
    inset: 0;
    appearance: none;
    border: none;
    padding: 0;
    background: var(--scrim);
    cursor: default;
  }

  .panel {
    position: relative;
    width: min(500px, calc(100vw - 2rem));
    margin: 14vh auto 0;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 9px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 18px 14px;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 600;
  }

  .host {
    font-weight: 400;
    font-size: var(--text-xs);
    color: var(--muted);
    margin-left: 4px;
  }

  .lede {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .lab {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--muted);
  }

  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .err {
    font-size: var(--text-xs);
    color: var(--err);
    white-space: pre-wrap;
  }

  .in {
    min-width: 0;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 6px 10px;
    outline: none;
    line-height: 1.45;
  }

  textarea.in {
    resize: vertical;
  }

  .in:focus {
    border-color: var(--focus-ring);
  }

  .in::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .in.mono {
    font-family: var(--mono);
  }

  .acts {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 2px;
  }

  .quiet {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
  }

  .quiet:hover:enabled {
    color: var(--fg);
  }

  .cta {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 5px 14px;
    border-radius: 6px;
    cursor: pointer;
  }

  .cta:hover:enabled {
    border-color: var(--accent);
  }

  .cta:disabled {
    opacity: 0.55;
    cursor: default;
  }
</style>
