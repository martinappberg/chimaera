<script lang="ts">
  /**
   * The Agents settings section: one row per agent CLI showing what chimaera
   * resolves for it (yours / chimaera-managed / a path you set / not found) with
   * the version and resolved path, an explicit-path override, and — for a
   * chimaera-managed binary — Update (when a newer release is known) and
   * Uninstall buttons. A newer release for the user's OWN binary renders as
   * quiet information only: chimaera never touches an install it doesn't own.
   *
   * Bespoke rather than generic SettingRows because it fuses live daemon state
   * (GET /api/v1/agents) with the `agents.<id>.path` settings and imperative
   * update/uninstall. The path override still persists as a normal setting (so
   * the JSON editor and the store stay in sync); only the presentation is custom.
   */
  import { onDestroy, onMount } from "svelte";
  import { getActiveWorkspaceId } from "../net/api";
  import {
    listAgents,
    pollAgents,
    relativeAge,
    uninstallAgent,
    updateAgent,
    type AgentInfo,
  } from "../workspace/launcher";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { flushSettings, getSetting, isModified, setSetting } from "./store.svelte";

  // The four per-agent path keys are all string-typed, so this cast is sound.
  type AgentPathKey =
    | "agents.claude.path"
    | "agents.codex.path"
    | "agents.gemini.path"
    | "agents.agy.path";
  const pathKey = (id: string): AgentPathKey => `agents.${id}.path` as AgentPathKey;

  // The window's workspace identity is fixed for the pane's lifetime
  // (window = workspace) — where an update session spawns.
  const wsId = getActiveWorkspaceId();

  let agents = $state<AgentInfo[]>([]);
  let loading = $state(true);
  let loadError = $state<string | null>(null);
  // Per-agent editable path field + busy/error state, keyed by agent id.
  let inputs = $state<Record<string, string>>({});
  let saving = $state<Record<string, boolean>>({});
  let removing = $state<Record<string, boolean>>({});
  let updating = $state<Record<string, boolean>>({});
  let rowError = $state<Record<string, string | null>>({});

  async function load(check = false): Promise<void> {
    loading = true;
    loadError = null;
    try {
      // refresh=true so the rows reflect the current settings + managed state
      // (the daemon busts its detection cache on an install/uninstall/edit);
      // check=true (the re-check button) also probes upstream for each
      // agent's latest release before answering.
      const list = await listAgents(true, check);
      agents = list;
      const next: Record<string, string> = {};
      for (const a of list) next[a.id] = getSetting(pathKey(a.id));
      inputs = next;
    } catch (e) {
      loadError = e instanceof Error ? e.message : "failed to list agents";
    } finally {
      loading = false;
    }
  }

  onMount(() => void load());

  function provenance(a: AgentInfo): { label: string; cls: string; title: string } {
    if (!a.installed)
      return { label: "not found", cls: "missing", title: "not resolvable — set a path below" };
    if (a.explicit)
      return { label: "set path", cls: "set", title: "resolved from the path you set below" };
    if (a.managed)
      return { label: "chimaera", cls: "managed", title: "installed by chimaera in ~/.chimaera/agents" };
    return { label: "yours", cls: "yours", title: "your own install, resolved from your login shell" };
  }

  async function save(id: string): Promise<void> {
    if (saving[id]) return;
    saving = { ...saving, [id]: true };
    rowError = { ...rowError, [id]: null };
    try {
      setSetting(pathKey(id), (inputs[id] ?? "").trim());
      await flushSettings();
      await load();
    } catch (e) {
      rowError = { ...rowError, [id]: e instanceof Error ? e.message : "failed to save" };
    } finally {
      saving = { ...saving, [id]: false };
    }
  }

  /** Start the daemon's curated update for a MANAGED binary: it streams into
   *  an "update <agent>" terminal (visible in the rail), and a bounded poll
   *  clears the row's affordance when the new build lands. */
  async function startUpdate(a: AgentInfo): Promise<void> {
    if (updating[a.id] || wsId === null) return;
    updating = { ...updating, [a.id]: true };
    rowError = { ...rowError, [a.id]: null };
    try {
      await updateAgent(a.id, wsId);
      ensurePoll();
    } catch (e) {
      updating = { ...updating, [a.id]: false };
      rowError = {
        ...rowError,
        [a.id]: e instanceof Error ? e.message : "failed to start the update",
      };
    }
  }

  // While an update session runs, re-probe the catalog every few seconds so
  // the row reconciles itself (the affordance disappears once the swap
  // lands). Bounded: a wedged download stops the poll after ~2 minutes and
  // re-enables the button — the daemon 409s a double-start anyway.
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let pollTicks = 0;

  function ensurePoll(): void {
    if (pollTimer !== null) return;
    pollTicks = 0;
    pollTimer = setInterval(() => {
      pollTicks += 1;
      if (pollTicks > 24) {
        stopPoll();
        updating = {};
        return;
      }
      void refreshRows();
    }, 5000);
  }

  function stopPoll(): void {
    if (pollTimer !== null) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  onDestroy(stopPoll);

  /** Refresh only the daemon-state side of the rows (never `inputs` — a
   *  path edit in progress must not be clobbered by a background poll).
   *  pollAgents, not listAgents(true): the daemon's install watcher already
   *  re-detected when the update session ended — forcing `refresh` here
   *  would spawn four login-shell re-resolutions every 5s tick. */
  async function refreshRows(): Promise<void> {
    try {
      const list = await pollAgents();
      agents = list;
      const next = { ...updating };
      for (const a of list) if (!a.updateAvailable) next[a.id] = false;
      updating = next;
      if (!list.some((a) => updating[a.id])) stopPoll();
    } catch {
      // transient fetch hiccup; the next tick retries
    }
  }

  async function uninstall(a: AgentInfo): Promise<void> {
    if (removing[a.id]) return;
    if (
      !confirm(
        `Uninstall the chimaera-managed ${a.name}?\n\n` +
          `This removes only chimaera's own copy under ~/.chimaera/agents. ` +
          `Your own install, if any, is left untouched.`,
      )
    )
      return;
    removing = { ...removing, [a.id]: true };
    rowError = { ...rowError, [a.id]: null };
    try {
      await uninstallAgent(a.id);
      await load();
    } catch (e) {
      rowError = { ...rowError, [a.id]: e instanceof Error ? e.message : "failed to uninstall" };
    } finally {
      removing = { ...removing, [a.id]: false };
    }
  }

  const versionNumber = (v: string): string =>
    v.split(" ").find((t) => /^\d/.test(t)) ?? v.split(" ")[0];

  /** Version tooltip: the full --version line plus the newest upstream
   *  release the daemon knows (and how fresh that knowledge is) — "am I
   *  current?" answerable without adding a visible element. */
  function verTitle(a: AgentInfo): string {
    let title = a.version ?? "";
    if (a.latestVersion !== null) {
      const age = a.latestCheckedAt !== null ? relativeAge(a.latestCheckedAt) : null;
      const checked = age === null ? "" : age === "now" ? ", checked just now" : `, checked ${age} ago`;
      title += ` — latest: ${a.latestVersion}${checked}`;
    }
    return title;
  }
</script>

<section class="agents">
  <div class="cat-row">
    <h2 class="cat">Agents</h2>
    <button
      class="recheck"
      title="re-detect installed agents and check upstream for their latest releases"
      onclick={() => void load(true)}
      disabled={loading}
    >
      {loading ? "checking…" : "re-check"}
    </button>
  </div>
  <p class="intro">
    Which binary chimaera runs for each agent — for both a launched agent and when
    you type its name in a chimaera terminal. Leave a path empty to resolve it
    from your login shell, then a chimaera-managed install. chimaera only shadows
    your own binary when it manages one or you set a path here.
  </p>

  {#if loadError !== null}
    <div class="err" role="alert">{loadError}</div>
  {/if}

  {#each agents as a (a.id)}
    {@const p = provenance(a)}
    {@const modified = isModified(pathKey(a.id))}
    <div class="row" class:modified>
      <div class="gutter" title={modified ? "path override set" : undefined}></div>
      <div class="text">
        <div class="head">
          <span class="glyph"><SessionGlyph kind="agent" agentKind={a.id} size={13} title={a.name} /></span>
          <span class="title">{a.name}</span>
          <span class="badge {p.cls}" title={p.title}>{p.label}</span>
          {#if a.version}<span class="ver" title={verTitle(a)}>{versionNumber(a.version)}</span>{/if}
          {#if a.outdated}<span class="badge missing" title="installed but too old to run usefully">outdated</span>{/if}
          {#if a.updateAvailable && a.latestVersion !== null && !a.managed}
            <!-- The user's own binary: a newer release is information, never
                 an action — chimaera doesn't touch installs it doesn't own. -->
            <span
              class="newver"
              title="{a.latestVersion} is out — this is your own install; update it your way"
              >{a.latestVersion} available</span
            >
          {/if}
        </div>
        <p class="desc" title={a.path ?? undefined}>
          {a.path ?? "not on your PATH — set a path below, or install it and re-check"}
        </p>
        <code class="id" title="settings.json key">{pathKey(a.id)}</code>
        {#if rowError[a.id]}<div class="err row-err" role="alert">{rowError[a.id]}</div>{/if}
      </div>

      <div class="control">
        <input
          class="textbox"
          bind:value={inputs[a.id]}
          placeholder="resolve from login shell"
          spellcheck="false"
          autocapitalize="off"
          autocorrect="off"
          disabled={saving[a.id]}
          onkeydown={(e) => {
            if (e.key === "Enter") void save(a.id);
          }}
        />
        <button
          class="btn"
          disabled={saving[a.id] || (inputs[a.id] ?? "") === getSetting(pathKey(a.id))}
          onclick={() => void save(a.id)}
        >
          {saving[a.id] ? "saving…" : "save"}
        </button>
        {#if a.managed && a.updateAvailable && a.latestVersion !== null}
          <button
            class="btn update"
            disabled={updating[a.id] || wsId === null}
            title="downloads the official {a.name} build into ~/.chimaera/agents — runs in a terminal you can watch"
            onclick={() => void startUpdate(a)}
          >
            {updating[a.id] ? "updating…" : `update → ${a.latestVersion}`}
          </button>
        {/if}
        {#if a.managed}
          <button
            class="btn danger"
            disabled={removing[a.id]}
            title="remove chimaera's managed copy (your own install is untouched)"
            onclick={() => void uninstall(a)}
          >
            {removing[a.id] ? "removing…" : "uninstall"}
          </button>
        {/if}
      </div>
    </div>
  {/each}
</section>

<style>
  /* Matches the shared settings grammar: an uppercase category header, then a
     flat two-column row per agent (label + detail left, controls right) with
     the modified-gutter, hover-revealed key, and container-query stacking —
     the same recipe as SettingRow, so this section reads as one system. */
  .agents {
    display: flex;
    flex-direction: column;
  }

  .cat-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin: 18px 0 4px;
    padding: 0 14px;
  }
  .cat {
    margin: 0;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .recheck {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0 4px;
    border-radius: 4px;
    transition: color 0.12s ease;
  }
  .recheck:hover:not(:disabled) {
    color: var(--fg);
  }
  .recheck:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .intro {
    margin: 0 0 6px;
    padding: 0 14px;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 60ch;
  }

  /* --- one agent, as a SettingRow-shaped row --- */
  .row {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 24px;
    padding: 14px 18px 14px 41px;
    border-radius: 8px;
    transition: background-color 0.12s ease;
  }
  .row:hover {
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
  .row.modified .gutter {
    background: color-mix(in srgb, var(--accent) 70%, transparent);
  }

  .text {
    flex: 1;
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .glyph {
    align-self: center;
    display: flex;
    color: var(--muted);
  }
  .title {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }

  /* Provenance / outdated badges: the quiet bordered-mono idiom of the shared
     "daemon" scope tag, tinted by state (accent = resolvable, warn = not). */
  .badge {
    font-family: var(--mono);
    font-size: var(--text-xs);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 5px;
  }
  .badge.yours,
  .badge.managed,
  .badge.set {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }
  .badge.missing {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--edge));
  }

  .ver {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  /* A newer release for the user's OWN binary: quiet information (their
     updater is theirs — no action here), tinted just enough to be found. */
  .newver {
    font-family: var(--mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: color-mix(in srgb, var(--accent) 80%, var(--muted));
    white-space: nowrap;
  }

  .desc {
    margin: 3px 0 0;
    font-family: var(--mono);
    font-size: var(--text-xs);
    line-height: 1.45;
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .id {
    display: inline-block;
    max-width: 100%;
    overflow-wrap: anywhere;
    margin-top: 5px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    opacity: 0;
    transition: opacity 0.12s ease;
    user-select: all;
  }
  .row:hover .id {
    opacity: 0.75;
  }

  .control {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 24px;
    padding-top: 2px;
  }

  .textbox {
    width: 200px;
    min-width: 0;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 8px;
  }
  .textbox:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
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
  .btn.danger:hover:not(:disabled) {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--edge));
    background: color-mix(in srgb, var(--warn) 8%, transparent);
  }

  /* The one-click managed update: accent = the row's single actionable
     affordance, present only while a strictly newer release is known. */
  .btn.update {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 40%, var(--edge));
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .btn.update:hover:not(:disabled) {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 65%, var(--edge));
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .err {
    margin: 4px 14px;
    font-size: var(--text-sm);
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    padding: 4px 8px;
    border-radius: 5px;
  }
  .err.row-err {
    margin: 5px 0 0;
  }

  /* Narrow pane: stack the controls under the label, matching SettingRow. */
  @container settings (max-width: 640px) {
    .row {
      flex-direction: column;
      align-items: stretch;
      gap: 10px;
      padding-left: 22px;
      padding-right: 14px;
    }
    .control {
      padding-top: 0;
      min-height: 0;
    }
    .textbox {
      flex: 1;
      width: auto;
    }
  }
</style>
