<script lang="ts">
  /**
   * The Environment settings section: the daemon's prelude map — opaque shell
   * text run once at the start of every NEW session, after the user's own rc
   * files and before the shell prompt or agent starts (reconnect reuses the
   * live PTY, so a prelude never re-runs).
   *
   * Bespoke rather than generic SettingRows because it edits a different store
   * than settings.json: /api/v1/environment (env-profiles.json on the daemon),
   * multi-line text with an explicit Save. The PUT replaces the WHOLE map, so
   * save re-fetches and merges — other workspaces' entries must round-trip
   * untouched — which is also why there is no debounced autosave here.
   */
  import { onMount } from "svelte";
  import { ApiError, getActiveWorkspaceId } from "../net/api";
  import { listWorkspaces } from "../workspace/sessions";
  import { getEnvironment, putEnvironment, type EnvironmentMap } from "./environment";

  // The window's workspace identity is fixed for the pane's lifetime
  // (window = workspace), so a mount-time read is enough.
  const wsId = getActiveWorkspaceId();

  let wsName = $state<string | null>(null);
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let saving = $state(false);
  let saveError = $state<string | null>(null);

  let hostText = $state("");
  let wsText = $state("");
  // Last-saved baselines; an editor is dirty when it differs from its baseline.
  let savedHost = $state("");
  let savedWs = $state("");

  const hostDirty = $derived(hostText !== savedHost);
  const wsDirty = $derived(wsId !== null && wsText !== savedWs);
  const dirty = $derived(hostDirty || wsDirty);

  async function load(): Promise<void> {
    loading = true;
    loadError = null;
    try {
      const [env, workspaces] = await Promise.all([
        getEnvironment(),
        wsId !== null ? listWorkspaces() : Promise.resolve([]),
      ]);
      hostText = env.host?.text ?? "";
      wsText = wsId !== null ? (env.workspaces?.[wsId]?.text ?? "") : "";
      savedHost = hostText;
      savedWs = wsText;
      wsName = workspaces.find((w) => w.id === wsId)?.name ?? null;
    } catch (e) {
      loadError = e instanceof Error ? e.message : "failed to load environment";
    } finally {
      loading = false;
    }
  }

  onMount(load);

  async function save(): Promise<void> {
    if (saving || !dirty) return;
    saving = true;
    saveError = null;
    try {
      // Fetch-merge-put: the PUT replaces the whole map, so every other
      // workspace's entry rides along unchanged. An empty editor REMOVES its
      // scope's entry — never persist { text: "" }.
      const fresh = await getEnvironment();
      const workspaces = { ...(fresh.workspaces ?? {}) };
      if (wsId !== null) {
        if (wsText.trim() === "") delete workspaces[wsId];
        else workspaces[wsId] = { text: wsText };
      }
      const next: EnvironmentMap = {};
      if (hostText.trim() !== "") next.host = { text: hostText };
      if (Object.keys(workspaces).length > 0) next.workspaces = workspaces;
      await putEnvironment(next);
      savedHost = hostText;
      savedWs = wsText;
    } catch (e) {
      saveError =
        e instanceof ApiError && e.status === 413
          ? "prelude too large — the daemon caps each scope at 32 KB"
          : e instanceof Error
            ? e.message
            : "failed to save";
    } finally {
      saving = false;
    }
  }
</script>

<section class="env">
  <h2 class="cat">Environment</h2>
  <p class="intro">
    Prelude commands run once when a new session starts — after your own rc files, before the
    shell prompt or agent. Plain POSIX (bash) lines, run verbatim:
    <code>ml bcftools</code>, <code>micromamba activate env</code>, <code>export FOO=bar</code>.
    The host prelude runs first, then the workspace's.
  </p>

  {#if loading}
    <p class="state">loading…</p>
  {:else if loadError !== null}
    <div class="err" role="alert">
      <span>{loadError}</span>
      <button class="btn" onclick={() => void load()}>retry</button>
    </div>
  {:else}
    <div class="scope" class:dirty={hostDirty}>
      <div class="gutter" title={hostDirty ? "unsaved changes" : undefined}></div>
      <div class="head">
        <span class="name">This machine</span>
        <span class="hint">runs for every session on this daemon</span>
      </div>
      <textarea
        class="prelude"
        bind:value={hostText}
        placeholder={"ml bcftools\nexport FOO=bar"}
        rows="4"
        spellcheck="false"
        autocapitalize="off"
        disabled={saving}
        aria-label="host prelude"
      ></textarea>
    </div>

    <div class="scope" class:dirty={wsDirty}>
      <div class="gutter" title={wsDirty ? "unsaved changes" : undefined}></div>
      <div class="head">
        <span class="name">{wsName ?? "This workspace"}</span>
        {#if wsId !== null}
          <span class="hint">also runs for sessions in this workspace</span>
        {/if}
      </div>
      {#if wsId !== null}
        <textarea
          class="prelude"
          bind:value={wsText}
          placeholder={"micromamba activate env"}
          rows="4"
          spellcheck="false"
          autocapitalize="off"
          disabled={saving}
          aria-label="workspace prelude"
        ></textarea>
      {:else}
        <p class="none">No workspace is open in this window — open a folder to set a workspace prelude.</p>
      {/if}
    </div>

    <div class="actions">
      <button class="btn save" disabled={!dirty || saving} onclick={() => void save()}>
        {saving ? "saving…" : "Save"}
      </button>
      {#if dirty && !saving}
        <span class="unsaved">unsaved changes</span>
      {/if}
    </div>
    {#if saveError !== null}
      <div class="err" role="alert">{saveError}</div>
    {/if}
  {/if}
</section>

<style>
  /* Matches the shared settings grammar (see AgentsSettings): an uppercase
     category header, an intro, then SettingRow-shaped scope blocks with the
     modified-gutter idiom (here: unsaved-changes) and container-query
     narrowing, so the section reads as one system. */
  .env {
    display: flex;
    flex-direction: column;
  }

  .cat {
    margin: 18px 0 4px;
    padding: 0 14px;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .intro {
    margin: 0 0 6px;
    padding: 0 14px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 60ch;
  }

  .intro code {
    font-family: var(--mono);
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
  }

  .state {
    margin: 0;
    padding: 8px 14px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  /* --- one prelude scope, as a SettingRow-shaped block --- */
  .scope {
    position: relative;
    padding: 14px 18px 14px 41px;
    border-radius: 8px;
    transition: background-color 0.12s ease;
  }

  .scope:hover {
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .gutter {
    position: absolute;
    left: 14px;
    top: 14px;
    bottom: 14px;
    width: 3px;
    border-radius: 2px;
    background: transparent;
  }

  .scope.dirty .gutter {
    background: color-mix(in srgb, var(--accent) 70%, transparent);
  }

  .head {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px 8px;
    min-width: 0;
    margin-bottom: 8px;
  }

  .name {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  .hint {
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .prelude {
    display: block;
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    min-height: 84px;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    line-height: 1.5;
    tab-size: 4;
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 8px 10px;
  }

  .prelude:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .prelude::placeholder {
    color: var(--muted);
    opacity: 0.6;
  }

  .prelude:disabled {
    opacity: 0.6;
  }

  .none {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .actions {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    padding: 2px 18px 14px 41px;
  }

  .btn {
    appearance: none;
    flex: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 3px 9px;
    border-radius: 6px;
    transition:
      color 0.12s ease,
      border-color 0.12s ease,
      background-color 0.12s ease;
  }

  .btn:hover:not(:disabled) {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  /* The save button carries the accent while there is something to save. */
  .btn.save:not(:disabled) {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .btn.save:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }

  .unsaved {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .err {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    margin: 4px 14px;
    font-size: var(--text-sm);
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    padding: 4px 8px;
    border-radius: 5px;
  }

  /* Narrow pane: pull the blocks in, matching SettingRow / AgentsSettings. */
  @container settings (max-width: 640px) {
    .scope {
      padding-left: 22px;
      padding-right: 14px;
    }

    .actions {
      padding-left: 22px;
      padding-right: 14px;
    }
  }
</style>
