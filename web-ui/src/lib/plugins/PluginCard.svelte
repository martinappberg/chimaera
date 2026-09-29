<script lang="ts">
  /**
   * One chimaera plugin, as a card (the Plugins segment of the Extensions
   * tab). Top to bottom:
   *  - the head: tile · name (a link to the plugin's own page) · the
   *    maintainers' check (first-party) · version · "local build"; on the
   *    right the ONE primary control — Install for a plugin not on this
   *    host, else the switch for THIS workspace with its state in words —
   *    and the quiet "…" menu holding everything secondary (Check for
   *    updates, Use previous, Set up, Open on GitHub, Remove);
   *  - the summary, and the author's description (two lines, "more");
   *  - what it adds ("For you", "For agents") and what it found here;
   *  - the agent-side plugin box (requirementsModel.ts), only for agents
   *    installed on this host, each row one state and at most one action;
   *  - what it can do ("Can": the daemon's words for its capabilities, the
   *    same list the trust prompt shows);
   *  - an update, a fault (with Reinstall when that mends it), a hold (why
   *    it can't run on this host: waiting for trust, blocked, the policy —
   *    with Review and trust, or Use anyway for a soft block), then one
   *    outcome line and the quiet "checked …" after a check.
   * An install, update, switch or Use previous the daemon answers with a
   * trust prompt (`TrustNeeded`) opens `TrustDialog`; confirming repeats it
   * with the capability digest shown.
   * A plugin not installed yet is the head and its summary, and the whole
   * top of the card is one button (a chevron beside the summary shows it)
   * that opens it in place: the same body, from what its release says
   * (`/details`, fetched once), the description in full, the agent-side
   * box with each agent's state but no actions (they need the plugin
   * installed), then where Install downloads from and a second Install. A
   * repository previewed from the form (`preview`) is that opened body in
   * a card of its own: "not installed" where the switch would be, its own
   * Install at the bottom, a close button.
   * Plain words throughout; anything technical is a tooltip on a button.
   * Versions and verification come from the daemon; agent state from the
   * agents.
   */
  import ConfirmDialog from "../shared/ConfirmDialog.svelte";
  import Switch from "../shared/Switch.svelte";
  import ActivityDialog from "./ActivityDialog.svelte";
  import TrustDialog from "./TrustDialog.svelte";
  import PluginSettings from "./PluginSettings.svelte";
  import PluginScreen from "./ui/PluginScreen.svelte";
  import { openPluginView } from "./platform";
  import { contextMenu, type ContextMenuEntry } from "../shared/contextMenu.svelte";
  import { isWebUrl, openInSystemBrowser } from "../shared/urlOpen";
  import {
    canReinstall,
    checkedWords,
    hereLine,
    holdWords,
    installedOutcome,
    installLine,
    installTitle,
    repoUrl,
    stateWords,
    tileLetters,
    trustAskFor,
    updatedOutcome,
    type Outcome,
  } from "./installCopy";
  import { agentSideBlocks, installationDetected, requirementsModel, type AgentsState, type RequirementRow } from "./requirementsModel";
  import {
    agentInstallContinuation,
    changeWorkbenchPlugin,
    checkedAt,
    detailsKey,
    expandedPlugins,
    installFirstPartyPlugin,
    installPlugin,
    installWorkbenchPlugin,
    isMissingRoute,
    pluginDetails,
    setWorkspacePluginOn,
    toggleExpanded,
    TrustNeeded,
    trustChange,
    type AgentId,
    type AgentPlugins,
    type PluginChange,
    type PluginDetails,
    type PluginUpdate,
    type TrustAsk,
    type WorkspacePlugin,
    workspacePlugins,
  } from "./store";

  interface Props {
    plugin: WorkspacePlugin | PluginDetails;
    agentPlugins: AgentPlugins | null;
    agentState: AgentsState;
    agentError: string | null;
    /** The Knowledge counts, when this workspace has a provider. */
    counts: { findings: number; decisions: number } | null;
    wsId: string | null;
    /** A clock the view ticks while shown (the "checked …" line ages). */
    now: number;
    onAttach: (pluginId: string) => void;
    onOpenSession: (id: string) => void;
    onRefresh: () => void;
    onRemove?: (p: WorkspacePlugin) => void;
    /** A change is in flight elsewhere (the Remove dialog). */
    removing?: boolean;
    /** A release previewed from the repository form (`plugin` is what it
     *  described): its own Install, and closing it. */
    preview?: { install: (trust?: string) => Promise<PluginChange>; close: () => void } | null;
  }

  let {
    plugin: p,
    agentPlugins,
    agentState,
    agentError,
    counts,
    wsId,
    now,
    onAttach,
    onOpenSession,
    onRefresh,
    onRemove = () => {},
    removing = false,
    preview = null,
  }: Props = $props();

  type Change = "install" | "update" | "rollback" | "check" | "reinstall";

  let switching = $state(false);
  let working = $state<Change | null>(null);
  /** The agent whose own install is starting. */
  let agentBusy = $state<string | null>(null);
  let note = $state<Outcome | null>(null);
  let error = $state<string | null>(null);
  /** The installed card's description, unclamped ("more"). */
  let descOpen = $state(false);
  let overflowing = $state(false);
  let moreBtn = $state<HTMLButtonElement | null>(null);
  /** An open trust prompt: what it asks, and what confirming does with the
   *  digest shown (and, for an update that asks for more, Skip). */
  let asking = $state<{
    ask: TrustAsk;
    mode: "install" | "update" | "trust";
    retry: (caps: string) => Promise<void>;
    skip: (() => Promise<void>) | null;
  } | null>(null);
  let askBusy = $state(false);
  let askError = $state<string | null>(null);
  let showActivity = $state(false);
  let untrusting = $state(false);
  let untrustBusy = $state(false);
  let untrustError = $state<string | null>(null);

  const previewing = $derived(preview !== null);
  const available = $derived(p.source === "available");
  /** An available card in the list (not a preview): it opens in place. */
  const opens = $derived(available && !previewing);
  const open = $derived(previewing || (opens && $expandedPlugins.has(p.id)));
  const fetched = $derived(opens ? $pluginDetails.get(detailsKey(p.id, p.version)) : undefined);
  /** What the body describes: an installed card its own entry, a preview
   *  what its release said, an opened card its fetched details (null while
   *  they come, or when they couldn't be read). */
  const shown = $derived<WorkspacePlugin | PluginDetails | null>(
    !available || previewing ? p : fetched?.state === "ok" ? fetched.plugin : null,
  );
  const described = $derived(shown !== null && "download" in shown ? shown : null);
  const busy = $derived(working !== null || removing);
  const homepage = $derived((shown ?? p).homepage);
  const home = $derived(homepage !== null && isWebUrl(homepage) ? homepage : null);
  const here = $derived(hereLine(p, counts));
  const model = $derived.by(() => {
    const x = shown ?? p;
    return requirementsModel({
      requires: x.requires,
      recommends: x.recommends,
      knowledge: x.provides.knowledge,
      report: agentPlugins,
      state: agentState,
    });
  });
  const blocks = $derived.by(() => {
    const x = shown ?? p;
    return agentSideBlocks(model, {
      name: x.name,
      requires: x.requires,
      recommends: x.recommends,
      requires_summary: x.requires_summary,
      recommends_summary: x.recommends_summary,
      knowledge: x.provides.knowledge,
    });
  });
  const checked = $derived($checkedAt.get(p.id));
  const continuation = $derived(
    $agentInstallContinuation?.workspaceId === wsId && $agentInstallContinuation.pluginId === p.id
      ? $agentInstallContinuation
      : null,
  );
  const installDetected = $derived(
    continuation !== null && installationDetected(model.rows, continuation.agent, continuation.agentPluginId),
  );
  /** A plugin that can't run here: the switch can't turn it on (the daemon
   *  refuses) until the fault is gone. A fault while on is the plugin
   *  failing in this workspace, which switching off and on clears. */
  const blocked = $derived((p.fault !== null && !p.on) || (p.hold !== null && p.hold.kind !== "untrusted"));
  const hold = $derived(holdWords(p));

  const WORKING: Record<Change, string> = {
    install: "installing…",
    update: "updating…",
    rollback: "going back…",
    check: "checking for updates…",
    reinstall: "reinstalling…",
  };

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /** Open the trust prompt the daemon answered with. */
  function ask(
    t: TrustAsk,
    mode: "install" | "update" | "trust",
    retry: (caps: string) => Promise<void>,
    skip: (() => Promise<void>) | null = null,
  ): void {
    askError = null;
    asking = { ask: t, mode, retry, skip };
  }

  /** Confirm: the same change again, with the digest shown. Asked again
   *  (the plugin changed meanwhile) keeps the dialog open on the new list. */
  async function answer(run: (() => Promise<void>) | null = null): Promise<void> {
    if (asking === null || askBusy) return;
    const current = asking;
    askBusy = true;
    askError = null;
    try {
      await (run ?? (() => current.retry(current.ask.caps)))();
      asking = null;
    } catch (e) {
      if (e instanceof TrustNeeded) asking = { ...current, ask: e.ask };
      askError = message(e);
    } finally {
      askBusy = false;
    }
  }

  async function toggle(on: boolean): Promise<void> {
    switching = true;
    error = null;
    try {
      await setWorkspacePluginOn(p.id, on);
    } catch (e) {
      if (e instanceof TrustNeeded) {
        ask(e.ask, "trust", async (caps) => {
          await trustChange("trust", p.id, caps);
          await setWorkspacePluginOn(p.id, true);
        });
      } else {
        error = message(e);
      }
    } finally {
      switching = false;
    }
  }

  /** The card's own "Review and trust" for a build waiting for trust. */
  function openTrust(): void {
    ask(trustAskFor(p), "trust", (caps) => trustChange("trust", p.id, caps));
  }

  /** Use anyway: a build the kill switch turned off (a soft block). */
  async function allowAnyway(): Promise<void> {
    error = null;
    try {
      await trustChange("allow-block", p.id);
    } catch (e) {
      error = message(e);
    }
  }

  async function withdrawTrust(): Promise<void> {
    if (untrustBusy) return;
    untrustBusy = true;
    untrustError = null;
    try {
      await trustChange("untrust", p.id);
      untrusting = false;
    } catch (e) {
      untrustError = message(e);
    } finally {
      untrustBusy = false;
    }
  }

  function outcome(kind: Change, res: PluginChange | { update: PluginUpdate | null }): Outcome | null {
    if (kind === "check") return null; // the callout or the "checked" line says it
    const c = res as PluginChange;
    if (kind === "install") return installedOutcome(c, p.name);
    if (kind === "reinstall") return { text: `reinstalled ${p.name} ${c.version ?? ""}`.trim() };
    if (kind === "rollback") return { text: `back to ${c.version} — Use previous returns to ${c.previous}` };
    return updatedOutcome(c);
  }

  function run(kind: Change, trust?: string): Promise<PluginChange | { update: PluginUpdate | null }> {
    if (kind === "install") return preview !== null ? preview.install(trust) : installFirstPartyPlugin(p.id);
    if (kind === "reinstall") return installWorkbenchPlugin(p.repo ?? "", p.version, trust);
    return changeWorkbenchPlugin(kind, p.id, trust);
  }

  async function change(kind: Change): Promise<void> {
    if (working !== null) return;
    working = kind;
    error = null;
    note = null;
    try {
      note = outcome(kind, await run(kind));
    } catch (e) {
      if (e instanceof TrustNeeded) {
        const t = e.ask;
        ask(
          t,
          kind === "install" || kind === "reinstall" ? "install" : kind === "update" || kind === "rollback" ? "update" : "trust",
          async (caps) => {
            note = outcome(kind, await run(kind, caps));
          },
          kind === "update" ? () => trustChange("skip", p.id, t.version) : null,
        );
      } else {
        error =
          (kind === "install" || kind === "reinstall") && isMissingRoute(e)
            ? "this daemon can't install plugins yet — update chimaera"
            : message(e);
      }
    } finally {
      working = null;
    }
  }

  /** The agent's own install of its plugin, in a visible terminal. */
  async function installForAgent(r: RequirementRow): Promise<void> {
    if (wsId === null || agentBusy !== null) return;
    agentBusy = r.agent;
    error = null;
    try {
      const res = await installPlugin(wsId, p.id, r.agent as AgentId, r.id);
      onOpenSession(res.session_id);
    } catch (e) {
      error = isMissingRoute(e) ? "this daemon can't run installs yet — use the agent's own plugin manager" : message(e);
    } finally {
      agentBusy = null;
    }
  }

  function menuItems(): ContextMenuEntry[] {
    const items: ContextMenuEntry[] = [];
    const hint = "wait for the change in progress";
    if (p.installed && p.repo !== null) {
      items.push({ label: "Check for updates", disabled: busy, hint, onSelect: () => void change("check") });
    }
    if (p.installed && p.previous !== null) {
      items.push({
        label: `Use previous version (${p.previous})`,
        disabled: busy,
        hint,
        onSelect: () => void change("rollback"),
      });
    }
    if (p.installed && p.setup !== null) {
      items.push({ label: "Set up in this workspace…", onSelect: () => onAttach(p.id) });
    }
    const repo = repoUrl(p);
    if (repo !== null) items.push({ label: "Open on GitHub", onSelect: () => openInSystemBrowser(repo) });
    if (p.installed) {
      items.push({ label: "Activity…", onSelect: () => (showActivity = true) });
    }
    if (p.installed) {
      if (items.length > 0) items.push("separator");
      if (p.standing === "trusted") {
        items.push({
          label: "Withdraw trust…",
          disabled: busy,
          hint,
          onSelect: () => {
            untrustError = null;
            untrusting = true;
          },
        });
      }
      items.push({ label: "Remove…", danger: true, disabled: busy, hint, onSelect: () => onRemove(p) });
    }
    return items;
  }

  function openMenu(): void {
    const el = moreBtn;
    if (el === null) return;
    const r = el.getBoundingClientRect();
    contextMenu.openAtPoint(r.right, r.bottom + 4, menuItems(), { alignRight: true });
  }

  function external(e: MouseEvent, url: string): void {
    e.preventDefault();
    openInSystemBrowser(url);
  }

  /** Whether the clamped description hides anything (re-measured on
   *  resize; while open the answer is kept, so "less" stays). */
  function clampWatch(node: HTMLElement) {
    const measure = () => {
      if (!descOpen) overflowing = node.scrollHeight > node.clientHeight + 1;
    };
    const ro = new ResizeObserver(measure);
    ro.observe(node);
    measure();
    return { destroy: () => ro.disconnect() };
  }
</script>

{#snippet head()}
  <header class="head">
    <span class="tile" class:on={p.active} aria-hidden="true">{tileLetters(p.id)}</span>
    <div class="ident">
      <h3 class="name" id="pc-{p.id}{previewing ? '-preview' : ''}">
        {#if home !== null}
          <a
            href={home}
            target="_blank"
            rel="noopener noreferrer"
            title="Open {p.name}'s page"
            onclick={(e) => external(e, home)}>{p.name}</a
          >
        {:else}
          {p.name}
        {/if}
      </h3>
      {#if p.first_party}
        <!-- `first_party`: in Chimaera's curated list (the lock), available
             or installed. The checksum check never shows here. -->
        <span class="badge" role="img" aria-label="Verified by the Chimaera maintainers" title="Verified by the Chimaera maintainers">
          <svg viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
            <path d="M3.5 8.5l3 3 6-6.5" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </span>
      {/if}
      {#if p.version !== ""}
        <span
          class="ver"
          title={p.installed && p.first_party && p.pinned_version !== null && p.pinned_version !== p.version
            ? `chimaera pins ${p.pinned_version}`
            : undefined}>{p.version}</span
        >
      {/if}
      {#if p.local_path !== null}
        <span class="tag" title="Installed from {p.local_path}">local build</span>
      {/if}
      {#if p.tier === "privileged"}
        <span class="tag warn" title="It can start programs on this host; its card lists which">runs programs</span>
      {/if}
    </div>
    {#if previewing}
      <span class="state">not installed</span>
    {:else if !available}
      <span class="state" class:good={p.active}>
        {#if p.active}<span class="dot" aria-hidden="true"></span>{/if}{stateWords(p)}
      </span>
    {/if}
    <div class="ctrl">
      {#if preview !== null}
        {@const close = preview.close}
        <button class="more" aria-label="Close the preview of {p.name}" title="Close" onclick={close}>
          <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        </button>
      {:else}
        {#if available}
          <button class="opt primary" disabled={busy} title={installTitle(p)} onclick={() => void change("install")}>
            {working === "install" ? "Installing…" : "Install"}
          </button>
        {:else}
          <span title={blocked ? "It can't run until the problem below is fixed" : undefined}>
            <Switch
              on={p.on}
              label="{p.name} {p.on ? 'on' : 'off'} in this workspace"
              disabled={switching || blocked}
              onToggle={(next) => void toggle(next)}
            />
          </span>
        {/if}
        <button
          class="more"
          bind:this={moreBtn}
          aria-haspopup="menu"
          aria-label="More for {p.name}"
          title="More"
          onclick={openMenu}
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <circle cx="3.5" cy="8" r="1.3" fill="currentColor" />
            <circle cx="8" cy="8" r="1.3" fill="currentColor" />
            <circle cx="12.5" cy="8" r="1.3" fill="currentColor" />
          </svg>
        </button>
      {/if}
    </div>
  </header>
{/snippet}

<!-- What it adds and what it found here: the same list on every card. -->
<!-- What a 0.2 plugin adds to its card: its tab views to open, its card
     sections (`slot = "card"`), and its settings. -->
{#snippet platformBlock()}
  {@const tabs = p.installed ? p.platform.views.filter((v) => v.slot === "tab") : []}
  {@const cards = p.installed && p.active ? p.platform.views.filter((v) => v.slot === "card") : []}
  {#if p.active && wsId !== null && tabs.length > 0}
    <div class="opens">
      {#each tabs as v (v.id)}
        <button class="opt small" onclick={() => openPluginView(p.id, v.id)}>Open {v.title}</button>
      {/each}
    </div>
  {/if}
  {#if wsId !== null}
    {#each cards as v (v.id)}
      <div class="card-section" aria-label={v.title}>
        <PluginScreen ws={wsId} wsRoot={$workspacePlugins?.root ?? null} plugin={p.id} view={v.id} compact />
      </div>
    {/each}
  {/if}
  {#if p.installed && p.platform.settings.length > 0}
    <details class="card-settings">
      <summary>Settings</summary>
      <PluginSettings plugin={p.id} name={p.name} {wsId} />
    </details>
  {/if}
{/snippet}

{#snippet facts(x: WorkspacePlugin, found: ReturnType<typeof hereLine>)}
  {#if x.adds.ui.length > 0 || x.adds.agents.length > 0 || x.can.length > 0 || found !== null}
    <dl class="facts">
      {#if x.adds.ui.length > 0}
        <dt>For you</dt>
        <dd>{#each x.adds.ui as line, i (i)}<span>{line}</span>{/each}</dd>
      {/if}
      {#if x.adds.agents.length > 0}
        <dt>For agents</dt>
        <dd>{#each x.adds.agents as line, i (i)}<span>{line}</span>{/each}</dd>
      {/if}
      {#if x.can.length > 0}
        <!-- The daemon's own words for what it can do: the trust prompt's list. -->
        <dt>Can</dt>
        <dd>{#each x.can as line, i (i)}<span class:priv={line.privileged}>{line.text}</span>{/each}</dd>
      {/if}
      {#if found !== null}
        <dt>Here</dt>
        <dd>
          <span class:muted={found.kind !== "using"}
            >{found.text}{#if found.kind === "setup"}{" "}— <button class="link" onclick={() => onAttach(p.id)}>Set it up</button
              >{/if}</span
          >
        </dd>
      {/if}
    </dl>
  {/if}
{/snippet}

<!-- The agent-side plugin box: each agent's state, and — once the plugin is
     installed — at most one action per row. -->
{#snippet sides(actions: boolean)}
  {#each blocks as b (b.kind)}
    <section class="side" aria-label={b.title}>
      <div class="side-head">
        <span class="side-title">{b.title}</span>
        {#if b.link !== null}
          {@const link = b.link}
          <a class="side-link" href={link.url} target="_blank" rel="noopener noreferrer" onclick={(e) => external(e, link.url)}
            >{link.label}<span aria-hidden="true"> ↗</span></a
          >
        {/if}
      </div>
      <p class="side-sum">{b.summary}</p>
      {#if b.notice !== null}
        <p class="side-note">{b.notice}</p>
      {/if}
      {#if b.rows.length > 0}
        <ul class="arows">
          {#each b.rows as r (`${r.agent}:${r.id}`)}
            <li class="arow">
              <span class="aname">{r.agent}</span>
              <span class="astate {r.tone}" title={r.scope !== null ? `${r.id} · ${r.scope} scope` : r.id}>{r.state}</span>
              {#if actions && r.action === "install"}
                <button
                  class="opt small"
                  disabled={agentBusy !== null}
                  title="Runs {r.agent}'s own plugin install in a terminal you can watch"
                  onclick={() => void installForAgent(r)}
                >
                  {agentBusy === r.agent ? "Starting…" : "Install"}
                </button>
              {:else if actions && r.action === "review"}
                <button class="opt small" title="See exactly what {r.agent} will run, then trust it" onclick={() => onAttach(p.id)}
                  >Review</button
                >
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if model.phase === "error" && agentError !== null}
        <p class="side-note err">
          Couldn't ask the agents: {agentError}
          <button class="link" onclick={onRefresh}>Try again</button>
        </p>
      {/if}
    </section>
  {/each}
{/snippet}

{#snippet faultCallout(x: WorkspacePlugin, reinstall: boolean)}
  {#if x.fault !== null}
    <div class="callout fault" role="note">
      <span class="ctext">
        {x.fault}{#if x.on}{" "}Switching it off and on starts it again.{/if}
      </span>
      {#if reinstall && canReinstall(x)}
        <button
          class="opt small"
          disabled={busy}
          title="Downloads {x.name} {x.version} again from its release"
          onclick={() => void change("reinstall")}
        >
          {working === "reinstall" ? "Reinstalling…" : "Reinstall"}
        </button>
      {/if}
    </div>
  {/if}
{/snippet}

{#snippet holdCallout()}
  {#if hold !== null}
    <!-- Waiting for trust, or a soft block, is a question for the user (the
         update callout's tone); a hard block or the policy is a fault. -->
    <div class="callout" class:update={hold.action !== null} class:fault={hold.action === null} role="note">
      <span class="ctext">{hold.text}</span>
      {#if hold.action === "trust"}
        <button class="opt primary small" disabled={busy} title="See what it can do, then trust it" onclick={openTrust}
          >Review and trust</button
        >
      {:else if hold.action === "allow"}
        <button class="opt small" disabled={busy} title="Switch it back on, despite why it was turned off" onclick={() => void allowAnyway()}
          >Use anyway</button
        >
      {/if}
    </div>
  {/if}
{/snippet}

<!-- A plugin not installed yet, opened: everything an installed card shows,
     from its release, then where Install downloads from and Install again. -->
{#snippet beforeInstall(x: WorkspacePlugin | PluginDetails)}
  {@render facts(x, null)}
  {@render sides(false)}
  {@render faultCallout(x, false)}
  <div class="install-again">
    <p class="install-line">
      {installLine(x.repo, described?.download?.wasm_bytes ?? null)}
    </p>
    <button class="opt primary" disabled={busy} onclick={() => void change("install")}>
      {working === "install" ? "Installing…" : "Install"}
    </button>
  </div>
{/snippet}

{#snippet statusLines()}
  {#if working !== null && working !== "install" && working !== "update" && working !== "reinstall"}
    <p class="status" role="status">{WORKING[working]}</p>
  {:else if error !== null}
    <p class="status err" role="alert">{error}</p>
  {:else if continuation !== null}
    <p class="status" role="status">
      {#if installDetected}
        Installed for {continuation.agent} —
      {:else}
        Installation opened for {continuation.agent}. When it finishes,
      {/if}
      <button class="link" onclick={() => onAttach(p.id)}>continue setup</button>.
    </p>
  {:else if note !== null}
    <p class="status" role="status">{note.text}</p>
  {/if}
  {#if checked !== undefined && working !== "check"}
    <p class="status">{p.update === null ? "No newer version · " : ""}{checkedWords(checked, now)}</p>
  {/if}
{/snippet}

<article
  class="card"
  class:active={p.active}
  class:available
  class:preview={previewing}
  aria-labelledby="pc-{p.id}{previewing ? '-preview' : ''}"
>
  {#if opens}
    <!-- The whole top is one button: the chevron's `::before` covers it, the
         Install and "…" buttons (and the name's link) sit above that. -->
    <div class="top" class:open class:follows={open || error !== null || note !== null}>
      {@render head()}
      <div class="lead">
        <p class="summary">{p.summary}</p>
        <button
          class="expander"
          aria-expanded={open}
          aria-controls="pc-more-{p.id}"
          aria-label="More about {p.name}"
          title={open ? "Show less" : "See what it adds before you install it"}
          onclick={() => toggleExpanded(p.id, p.version)}
        >
          <svg class="chev" viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
            <path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
          </svg>
        </button>
      </div>
    </div>
    {#if open}
      <div class="body" class:tight={shown !== null && shown.description !== null} id="pc-more-{p.id}">
        {#if fetched === undefined || fetched.state === "loading"}
          <p class="status" role="status">loading…</p>
        {:else if fetched.state === "error"}
          <p class="status err" role="alert">
            {fetched.missingRoute ? "this daemon can't show more yet — update chimaera" : fetched.message}
          </p>
        {:else if shown !== null}
          {#if shown.description !== null}
            <p class="desc">{shown.description}</p>
          {/if}
          {@render beforeInstall(shown)}
        {/if}
        {@render statusLines()}
      </div>
    {:else if error !== null || note !== null}
      <div class="body">{@render statusLines()}</div>
    {/if}
  {:else}
    {@render head()}
    <div class="body">
      <div class="prose">
        <p class="summary">{p.summary}</p>
        {#if p.description !== null}
          {#if previewing}
            <p class="desc">{p.description}</p>
          {:else}
            <p class="desc" class:clamped={!descOpen} use:clampWatch>{p.description}</p>
            {#if overflowing}
              <button class="toggle-more" aria-expanded={descOpen} onclick={() => (descOpen = !descOpen)}>
                {descOpen ? "less" : "more"}
              </button>
            {/if}
          {/if}
        {/if}
      </div>

      {#if previewing}
        {@render beforeInstall(p)}
      {:else}
        {@render facts(p, here)}
        {@render sides(true)}
        {@render platformBlock()}

        {#if p.installed && p.update !== null}
          {@const u = p.update}
          <div class="callout update">
            <span class="ctext"><b>{u.version}</b> is available</span>
            {#if u.url !== "" && isWebUrl(u.url)}
              <a class="link" href={u.url} target="_blank" rel="noopener noreferrer" onclick={(e) => external(e, u.url)}>what changed</a>
            {/if}
            <button
              class="opt primary small"
              disabled={busy}
              title="Downloads {u.version} from its release and switches to it; the version you have now stays as the previous one"
              onclick={() => void change("update")}
            >
              {working === "update" ? "Updating…" : "Update"}
            </button>
          </div>
        {/if}

        {@render faultCallout(p, true)}
        {@render holdCallout()}
      {/if}

      {@render statusLines()}
    </div>
  {/if}
</article>

{#if asking !== null}
  {@const a = asking}
  <TrustDialog
    ask={a.ask}
    mode={a.mode}
    busy={askBusy}
    error={askError}
    onConfirm={() => void answer()}
    onCancel={() => {
      if (!askBusy) asking = null;
    }}
    onSkip={a.skip === null ? null : () => void answer(a.skip)}
  />
{/if}

{#if showActivity}
  <ActivityDialog pluginId={p.id} name={p.name} onClose={() => (showActivity = false)} />
{/if}

{#if untrusting}
  <ConfirmDialog
    title="Withdraw your trust in {p.name}?"
    body="It goes off in every workspace at once, and stays installed. Trusting it again brings it back."
    confirmLabel={untrustBusy ? "Withdrawing…" : "Withdraw trust"}
    danger
    error={untrustError}
    onConfirm={() => void withdrawTrust()}
    onCancel={() => {
      if (!untrustBusy) untrusting = false;
    }}
  />
{/if}

<style>
  /* Its layout follows the view's width (PluginsView's scroller is the
     container), not the window's, so a split pane gets the narrow layout. */
  .card {
    --pad-y: 18px;
    --pad-x: 20px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    padding: var(--pad-y) var(--pad-x);
    display: flex;
    flex-direction: column;
    gap: 12px;
    transition: border-color 0.12s ease;
  }
  .card.active {
    border-color: color-mix(in srgb, var(--accent) 35%, var(--edge));
  }

  /* --- the head: tile · name line · state · the control and the menu ------ */
  .head {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto auto;
    grid-template-areas: "tile ident state ctrl";
    align-items: center;
    column-gap: 12px;
    row-gap: 2px;
    min-width: 0;
  }
  .tile {
    grid-area: tile;
    width: 34px;
    height: 34px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    color: var(--muted);
    display: flex;
    align-items: center;
    justify-content: center;
    font-family: var(--mono);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .tile.on {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
  }
  .ident {
    grid-area: ident;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 8px;
    min-width: 0;
  }
  .name {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 1.3;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .name a {
    color: inherit;
    text-decoration: none;
    border-radius: 3px;
  }
  .name a:hover {
    text-decoration: underline;
    text-decoration-color: color-mix(in srgb, var(--fg) 40%, transparent);
    text-underline-offset: 3px;
  }
  /* The maintainers' check: AttachSheet's done-step recipe (accent wash,
     accent check), small, beside the name. */
  .badge {
    flex: none;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: color-mix(in srgb, var(--accent) 14%, transparent);
    color: var(--accent);
  }
  .ver {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--muted);
  }
  .tag {
    font-size: 11px;
    line-height: 1.5;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 7px;
    white-space: nowrap;
  }
  .tag.warn {
    color: var(--warn);
    border-color: color-mix(in srgb, var(--warn) 45%, var(--edge));
  }
  /* A Can line only a plugin that runs programs has. */
  .facts dd .priv {
    color: var(--warn);
  }
  .state {
    grid-area: state;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }
  .state.good {
    color: var(--accent);
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
  }
  .ctrl {
    grid-area: ctrl;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .ctrl > span {
    display: inline-flex;
  }
  .ctrl .opt {
    white-space: nowrap;
  }
  .more {
    appearance: none;
    border: 1px solid transparent;
    background: none;
    color: var(--muted);
    width: 28px;
    height: 28px;
    border-radius: 7px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }
  .more:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  /* --- the body, aligned with the name (past the tile) --------------------- */
  .body {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding-left: 46px;
    min-width: 0;
  }
  .body p {
    margin: 0;
  }
  .prose {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .summary {
    font-size: var(--text-md);
    line-height: 1.5;
    color: var(--fg);
  }
  .desc {
    font-size: var(--text-sm);
    line-height: 1.55;
    color: var(--muted);
    max-width: 74ch;
  }
  .desc.clamped {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
  }
  .toggle-more {
    align-self: flex-start;
    appearance: none;
    border: none;
    background: none;
    padding: 0 2px;
    margin: 0 0 0 -2px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    border-radius: 3px;
  }
  .toggle-more:hover {
    color: var(--accent);
  }

  /* What it adds and what it found: labels in the muted label style, the
     values in body text, one column line for all three. */
  /* --- a 0.2 plugin's own additions -------------------------------------- */
  .opens {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .card-section {
    border-top: 1px solid var(--edge);
    padding-top: 12px;
  }
  .card-settings > summary {
    cursor: pointer;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
    padding-top: 12px;
    border-top: 1px solid var(--edge);
  }
  .facts {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    align-items: baseline;
    gap: 7px 18px;
    margin: 2px 0 0;
    padding-top: 12px;
    border-top: 1px solid var(--edge);
  }
  .facts dt {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
    white-space: nowrap;
  }
  .facts dd {
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--fg);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted);
  }

  /* --- the agent-side plugin ---------------------------------------------- */
  .side {
    border: 1px solid var(--edge);
    border-radius: 10px;
    padding: 12px 14px 6px;
    background: color-mix(in srgb, var(--fg) 2.5%, transparent);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .side-head {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  .side-title {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .side-link {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--accent);
    text-decoration: none;
    white-space: nowrap;
    border-radius: 3px;
  }
  .side-link:hover {
    text-decoration: underline;
  }
  .side-sum {
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }
  .side-note {
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
    padding-bottom: 6px;
  }
  .arows {
    list-style: none;
    margin: 2px 0 0;
    padding: 0;
  }
  .arow {
    display: grid;
    grid-template-columns: 58px minmax(0, 1fr) auto;
    align-items: center;
    column-gap: 10px;
    min-height: 34px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-sm);
  }
  .aname {
    font-family: var(--mono);
    font-size: 12.5px;
    color: var(--fg);
  }
  .astate {
    color: var(--muted);
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .astate.good {
    color: var(--fg);
  }
  .astate.warn {
    color: var(--warn);
  }
  .opt.small {
    font-size: var(--text-xs);
    padding: 2px 10px;
  }

  /* --- a plugin not installed yet: its top opens it in place -------------- */
  .top {
    position: relative;
    isolation: isolate;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-width: 0;
  }
  /* Above the expander's cover: the controls, the name's link, and the
     badge (for its tooltip). */
  .top .ctrl,
  .top .name a,
  .top .badge {
    position: relative;
    z-index: 1;
  }
  .lead {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding-left: 46px;
    min-width: 0;
  }
  .lead .summary {
    flex: 1 1 auto;
    min-width: 0;
    margin: 0;
  }
  .expander {
    flex: none;
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    width: 22px;
    height: 20px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    cursor: pointer;
    border-radius: 5px;
    transition: color 0.12s ease;
  }
  /* The cover: the card's whole top, out to its padded edge (to half the
     gap when something follows). It is the button, so a click anywhere up
     there opens the card, and the focus ring rings all of it. */
  .expander::before {
    content: "";
    position: absolute;
    top: calc(-1 * var(--pad-y));
    left: calc(-1 * var(--pad-x));
    right: calc(-1 * var(--pad-x));
    bottom: calc(-1 * var(--pad-y));
    border-radius: 11px;
  }
  .top.follows .expander::before {
    bottom: -6px;
    border-radius: 11px 11px 0 0;
  }
  .expander:hover,
  .expander:focus-visible {
    color: var(--fg);
  }
  .expander:focus-visible {
    outline: none;
  }
  .expander:focus-visible::before {
    outline: 2px solid var(--focus-ring);
    outline-offset: -3px;
  }
  .chev {
    transition: transform 0.15s ease;
  }
  .top.open .chev {
    transform: rotate(180deg);
  }
  @media (prefers-reduced-motion: reduce) {
    .chev {
      transition: none;
    }
  }
  /* The description sits under the summary as it does on an installed card. */
  .body.tight {
    margin-top: -9px;
  }
  .install-again {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 16px;
    padding-top: 12px;
    border-top: 1px solid var(--edge);
  }
  .install-line {
    flex: 1 1 260px;
    min-width: 0;
    font-size: var(--text-xs);
    line-height: 1.5;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .install-again .opt {
    margin-left: auto;
    white-space: nowrap;
  }

  /* --- an update, a fault ------------------------------------------------- */
  .callout {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
    padding: 8px 10px 8px 12px;
    border-radius: 8px;
    font-size: var(--text-sm);
    line-height: 1.45;
  }
  .callout .opt {
    margin-left: auto;
  }
  .callout.update {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent) 28%, var(--edge));
  }
  .callout.update b {
    font-family: var(--mono);
    font-weight: 600;
    font-size: 0.95em;
  }
  .callout.fault {
    background: color-mix(in srgb, var(--err) 7%, transparent);
    border: 1px solid color-mix(in srgb, var(--err) 30%, var(--edge));
  }
  .ctext {
    display: inline-block;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .callout.fault .ctext {
    color: var(--fg);
  }
  /* The daemon's fault is a sentence fragment; the callout reads it as a
     sentence. */
  .callout.fault .ctext::first-letter {
    text-transform: uppercase;
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
    text-decoration: none;
    border-radius: 3px;
  }
  .link:hover {
    text-decoration: underline;
  }
  .status {
    font-size: var(--text-xs);
    line-height: 1.45;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .status.err,
  .err {
    color: var(--err);
  }

  /* Narrow: the state words go under the name (the control keeps the right
     edge), the body uses the full width. */
  @container (max-width: 600px) {
    .card {
      --pad-y: 16px;
      --pad-x: 16px;
    }
    .head {
      grid-template-columns: auto minmax(0, 1fr) auto;
      grid-template-areas:
        "tile ident ctrl"
        "tile state ctrl";
    }
    .state {
      justify-self: start;
    }
    .body,
    .lead {
      padding-left: 0;
    }
  }
</style>
