<script lang="ts">
  /**
   * The Documents settings section: opt-in ways to teach agents launched
   * OUTSIDE Chimaera the portable markdown dialect. Sessions inside Chimaera
   * already get it from the chimaera MCP server (`document_guide`,
   * `check_document`), so nothing here is automatic: each write is a click
   * behind a dialog that shows the exact text (fetched from the daemon, which
   * owns it).
   *
   * Bespoke rather than schema rows: it drives /api/v1/agent-docs, not
   * settings.json.
   */
  import { onMount } from "svelte";
  import { getActiveWorkspaceId } from "../net/api";
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import {
    getAgentDocs,
    installAgentDocs,
    type AgentDocsEntry,
    type AgentDocsState,
    type AgentDocsTarget,
  } from "./agentDocs";

  // The window's workspace identity is fixed for the pane's lifetime.
  const wsId = getActiveWorkspaceId();

  let entries = $state<AgentDocsEntry[]>([]);
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let confirming = $state<AgentDocsEntry | null>(null);
  let installing = $state(false);
  let installError = $state<string | null>(null);
  /** The last install's outcome, per target ("added", "already current"). */
  let done = $state<Partial<Record<AgentDocsTarget, string>>>({});

  const agentsMd = $derived(entries.find((e) => e.target === "agents_md") ?? null);
  const skill = $derived(entries.find((e) => e.target === "claude_skill") ?? null);

  async function load(): Promise<void> {
    loadError = null;
    try {
      entries = await getAgentDocs(wsId);
    } catch (e) {
      loadError = e instanceof Error ? e.message : "failed to load";
    } finally {
      loading = false;
    }
  }

  onMount(load);

  const STATE_LABEL: Record<AgentDocsState, string> = {
    installed: "installed",
    outdated: "an older version is installed",
    absent: "not installed",
    no_file: "no AGENTS.md yet",
    broken: "unmatched chimaera:docs marker; fix it by hand",
    unreadable: "can't be read",
  };

  function stateLabel(entry: AgentDocsEntry): string {
    if (entry.target === "agents_md" && entry.state === "absent") return "no documents section";
    return STATE_LABEL[entry.state] ?? entry.state;
  }

  function actionLabel(entry: AgentDocsEntry): string | null {
    switch (entry.state) {
      case "installed":
      case "broken":
      case "unreadable":
        return null;
      case "outdated":
        return "Update…";
      default:
        return entry.target === "agents_md" ? "Add section…" : "Install…";
    }
  }

  const dialog = $derived.by(() => {
    const entry = confirming;
    if (entry === null) return null;
    if (entry.target === "agents_md") {
      return {
        title: entry.state === "no_file" ? "Create AGENTS.md" : "Add a documents section to AGENTS.md",
        body:
          `Writes this block to ${entry.path}` +
          (entry.state === "no_file" ? " (a new file)" : "") +
          ". Running it again replaces only the text between the markers.",
        confirm: entry.state === "outdated" ? "update" : "write",
      };
    }
    return {
      title: "Install the Claude Code skill",
      body: `Writes ${entry.path}. Claude Code sessions started outside Chimaera load it when they write documents.`,
      confirm: entry.state === "outdated" ? "update" : "install",
    };
  });

  async function confirmInstall(): Promise<void> {
    const entry = confirming;
    if (entry === null || installing) return;
    installing = true;
    installError = null;
    try {
      const out = await installAgentDocs(entry.target, wsId);
      done = {
        ...done,
        [entry.target]: out.created ? "created just now" : out.changed ? "updated just now" : "already current",
      };
      confirming = null;
      await load();
    } catch (e) {
      installError = e instanceof Error ? e.message : "failed to write";
    } finally {
      installing = false;
    }
  }
</script>

<section class="docs">
  <h2 class="cat">Documents</h2>
  <p class="intro">
    Agents running in Chimaera already know the portable markdown dialect and check their documents
    with <code>check_document</code>. To teach agents you start elsewhere, write it where they look.
    Nothing is written until you confirm.
  </p>

  {#if loading}
    <p class="state">loading…</p>
  {:else if loadError !== null}
    <div class="err" role="alert">
      <span>{loadError}</span>
      <button class="btn" onclick={() => void load()}>retry</button>
    </div>
  {:else}
    {#if agentsMd !== null}
      {@render row(agentsMd, "This workspace's AGENTS.md", "read by Codex, and by Claude through @AGENTS.md")}
    {:else}
      <div class="item">
        <div class="head"><span class="name">This workspace's AGENTS.md</span></div>
        <p class="none">No workspace is open in this window: open a folder to add a section.</p>
      </div>
    {/if}
    {#if skill !== null}
      {@render row(skill, "Claude Code skill", "for Claude Code sessions outside Chimaera")}
    {/if}
  {/if}
</section>

{#snippet row(entry: AgentDocsEntry, name: string, hint: string)}
  {@const action = actionLabel(entry)}
  <div class="item">
    <div class="head">
      <span class="name">{name}</span>
      <span class="hint">{hint}</span>
    </div>
    <div class="line">
      <code class="path" title={entry.path}>{entry.path}</code>
    </div>
    <div class="line">
      <span
        class="badge"
        class:ok={entry.state === "installed"}
        class:bad={entry.state === "broken" || entry.state === "unreadable"}
      >
        {stateLabel(entry)}
      </span>
      {#if done[entry.target] !== undefined && entry.state === "installed"}
        <span class="done">{done[entry.target]}</span>
      {/if}
      {#if action !== null}
        <button
          class="btn primary"
          onclick={() => {
            installError = null;
            confirming = entry;
          }}>{action}</button
        >
      {/if}
    </div>
  </div>
{/snippet}

{#if confirming !== null && dialog !== null}
  <ConfirmDialog
    title={dialog.title}
    body={dialog.body}
    detail={confirming.text}
    confirmLabel={installing ? "writing…" : dialog.confirm}
    error={installError}
    onConfirm={() => void confirmInstall()}
    onCancel={() => {
      if (!installing) confirming = null;
    }}
  />
{/if}

<style>
  /* The shared settings grammar (see EnvironmentSettings): an uppercase
     category header, an intro, then SettingRow-shaped blocks. */
  .docs {
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
  }

  .state {
    margin: 0;
    padding: 8px 14px;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .item {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 18px 14px 41px;
    border-radius: 8px;
    transition: background-color 0.12s ease;
  }

  .item:hover {
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .head {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px 8px;
    min-width: 0;
  }

  .name {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }

  .hint {
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .line {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    min-width: 0;
  }

  .path {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .badge {
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 1px 7px;
    border: 1px solid var(--edge);
    border-radius: 999px;
  }

  .badge.ok {
    color: color-mix(in srgb, var(--accent) 80%, var(--fg));
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .badge.bad {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--edge));
  }

  .done {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .none {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
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

  .btn:hover {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  .btn.primary {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .btn.primary:hover {
    background: color-mix(in srgb, var(--accent) 10%, transparent);
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

  @container settings (max-width: 640px) {
    .item {
      padding-left: 22px;
      padding-right: 14px;
    }
  }
</style>
