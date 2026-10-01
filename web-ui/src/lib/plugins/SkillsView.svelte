<script lang="ts">
  /**
   * Skills — "what can my agents do here?" (design §6.4): every skill any
   * agent can use in this workspace on this host, in the order you'd look
   * for it — this project's own, then each plugin's (a plugin is what you
   * install, so it is what the list is read by), then yours, then what is
   * built into claude and into codex, each as one quiet flow of chips in
   * that agent's own syntax (`/name`, `$name`). Each row names the skill,
   * says what it does, and carries a badge only for the agents that can use
   * it; a click opens the row in place: how each agent calls it (copyable),
   * its SKILL.md, and any load error. Truth comes from the agents; codex's
   * load errors are shown, not hidden.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import { copyText } from "../shared/clipboard";
  import {
    filterSkills,
    groupSkills,
    invokeSyntax,
    pluginSections,
    shortName,
    type SkillFilter,
    type SkillGroup,
  } from "./skillsModel";
  import { agentName, type AgentId, type Skill, type SkillsReport } from "./store";

  interface Props {
    report: SkillsReport | null;
    status: "idle" | "loading" | "ok" | "unavailable" | "error";
    error: string | null;
    host: string;
    wsRoot: string | null;
    onOpenFile: (absPath: string) => void;
    onRefresh: () => void;
  }

  let { report, status, error, host, wsRoot, onOpenFile, onRefresh }: Props = $props();

  let filter = $state<SkillFilter>("all");
  let query = $state("");
  /** The one row opened in place (by skill name). */
  let openName = $state<string | null>(null);
  /** "name:agent" of the invocation just copied — the button says so briefly. */
  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => () => {
    if (copiedTimer !== null) clearTimeout(copiedTimer);
  });
  /** The built-in groups showing all their chips ("+ 12 more" opened). */
  let builtinsAll = $state(new Set<string>());
  const BUILTIN_SHOWN = 18;

  const AGENTS = $derived(Object.keys(report?.agents ?? {}));

  const all = $derived(report?.skills ?? []);
  const shown = $derived(filterSkills(all, filter, query));
  const groups = $derived(groupSkills(shown, host));
  /** claude reports its built-ins only while one of its chats runs: say so
   *  where they would be, and only then. */
  const claudeQuiet = $derived(
    report !== null && report.agents.claude?.available && !report.agents.claude.live && (filter === "all" || filter === "claude") && query.trim() === "",
  );
  const hasClaudeBuiltins = $derived(groups.some((g) => g.key === "builtin-claude"));

  function builtinAgent(g: SkillGroup): AgentId {
    return g.key.slice("builtin-".length);
  }

  function abs(p: string): string {
    return p.startsWith("/") || p.startsWith("~") || wsRoot === null ? p : `${wsRoot}/${p}`;
  }
  function skillFile(s: Skill): string | null {
    const p = Object.values(s.paths).find(Boolean) ?? null;
    if (p === null) return null;
    return p.endsWith("SKILL.md") ? p : `${p.replace(/\/$/, "")}/SKILL.md`;
  }
  /** `path` is the skill's own file or dir, or lies inside that dir — a
   *  sibling that merely shares a prefix ("pdf" vs "pdf-tools") is not. */
  function within(path: string, skillPath: string): boolean {
    return path === skillPath || path.startsWith(skillPath.endsWith("/") ? skillPath : `${skillPath}/`);
  }
  function errorsFor(s: Skill): string[] {
    if (report === null) return [];
    const paths = Object.values(s.paths).filter((p): p is string => !!p);
    return report.errors
      .filter((e) => e.path !== undefined && paths.some((p) => within(e.path!, p)))
      .map((e) => `${agentName(e.agent)}: ${e.message}`);
  }
  /** Load errors no listed skill claims (a SKILL.md too broken to list). */
  const strayErrors = $derived.by(() => {
    if (report === null) return [];
    const claimed = all.flatMap((s) => Object.values(s.paths).filter((p): p is string => !!p));
    return report.errors.filter(
      (e) => (filter === "all" || e.agent === filter) &&
        (e.path === undefined || !claimed.some((p) => within(e.path!, p))),
    );
  });

  async function copy(s: Skill, agent: AgentId): Promise<void> {
    if (await copyText(invokeSyntax(s, agent))) {
      copied = `${s.name}:${agent}`;
      if (copiedTimer !== null) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = null), 1400);
    }
  }
</script>

{#snippet row(s: Skill, name: string)}
  {@const expanded = openName === (s.id ?? s.name)}
  {@const errs = errorsFor(s)}
  <div class="skill" class:expanded>
    <button class="shead" aria-expanded={expanded} onclick={() => (openName = expanded ? null : (s.id ?? s.name))}>
      <span class="sname" title={s.name}>{name}</span>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
      <span class="sdesc">{@html inlineMarkdown(s.description)}</span>
      <span class="agents">
        {#if errs.length > 0}<span class="warnmark" title={errs.join("\n")}>!</span>{/if}
        {#each AGENTS as a (a)}
          {@const st = s.agents[a] ?? {state: "absent"}}
          {#if st.state === "available"}
            <span class="agent">{agentName(a)}</span>
          {:else if st.state === "off"}
            <span class="agent off" title="{a}: {st.reason ?? 'present but not usable'}">{agentName(a)}</span>
          {/if}
        {/each}
      </span>
      <svg class="chev" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
        <path d="M6 3.5L10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    {#if expanded}
      {@const file = skillFile(s)}
      <dl class="sbody">
        <dt>Use it</dt>
        <dd class="uses">
          {#each AGENTS as a (a)}
            {@const st = s.agents[a] ?? {state: "absent"}}
            {#if st.state === "available" && invokeSyntax(s, a)}
              <button class="use" title="Copy, then type it in {a}" onclick={() => copy(s, a)}>
                <span class="uagent">{agentName(a)}</span>
                <span class="uinv">{invokeSyntax(s, a)}</span>
                <span class="ucopy">{copied === `${s.name}:${a}` ? "copied" : "copy"}</span>
              </button>
            {:else if st.state === "off" || st.state === "available"}
              <span class="useoff"><span class="uagent">{agentName(a)}</span>{st.reason ?? "present but not usable"}</span>
            {/if}
          {/each}
        </dd>
        {#if file !== null}
          <dt>File</dt>
          <dd class="fileline">
            <button class="link" title={abs(file)} onclick={() => onOpenFile(abs(file))}>open SKILL.md</button>
            <span class="fpath" title={abs(file)}>{file}</span>
          </dd>
        {/if}
        {#if errs.length > 0}
          <dt>Problems</dt>
          <dd>
            {#each errs as e, i (i)}
              <div class="errline">{e}</div>
            {/each}
          </dd>
        {/if}
      </dl>
    {/if}
  </div>
{/snippet}

{#snippet builtins(g: SkillGroup)}
  {@const agent = builtinAgent(g)}
  {@const all = builtinsAll.has(g.key)}
  <div class="flow">
    {#each all ? g.skills : g.skills.slice(0, BUILTIN_SHOWN) as s, i (s.id ?? `${i}:${s.name}`)}
      {#if invokeSyntax(s, agent)}
        <button class="bchip" title="{s.description || s.name} — click to copy" onclick={() => copy(s, agent)}>
          {copied === `${s.name}:${agent}` ? "copied" : invokeSyntax(s, agent)}
        </button>
      {:else}
        <span class="bchip" title={s.agents[agent]?.reason ?? s.description}>{s.name}</span>
      {/if}
    {/each}
    {#if g.skills.length > BUILTIN_SHOWN}
      <button
        class="link small"
        onclick={() => {
          const next = new Set(builtinsAll);
          if (all) next.delete(g.key);
          else next.add(g.key);
          builtinsAll = next;
        }}
      >
        {all ? "fewer" : `${g.skills.length - BUILTIN_SHOWN} more`}
      </button>
    {/if}
  </div>
{/snippet}

{#snippet claudeNote()}
  <section class="group">
    <h3 class="ghead"><span class="lbl">Built into claude</span></h3>
    <p class="note">claude lists its built-in skills only while a claude chat runs.</p>
  </section>
{/snippet}

{#if status === "unavailable"}
  <p class="empty">This daemon can't list skills yet — update chimaera.</p>
{:else if status === "error" && report === null}
  <p class="empty"><span class="err">Couldn't ask the agents: {error}</span> <button class="link" onclick={onRefresh}>Try again</button></p>
{:else if report === null}
  <p class="empty">Asking your agents…</p>
{:else}
  <div class="bar">
    <label class="agent-filter">
      <span class="sr">Show skills for</span>
      <select aria-label="Show skills for" bind:value={filter}>
        <option value="all">All agents</option>
        {#each AGENTS as agent (agent)}<option value={agent}>{agentName(agent)}</option>{/each}
      </select>
    </label>
    <label class="search">
      <span class="sr">Search skills</span>
      <input type="search" placeholder="Search skills" bind:value={query} spellcheck="false" autocomplete="off" />
    </label>
    <span class="count">
      {shown.length} skill{shown.length === 1 ? "" : "s"}

    </span>
  </div>

  {#if !Object.values(report.agents).some(a => a.available)}
    <p class="empty">Install an agent on {host} to see its skills.</p>
  {:else if shown.length === 0}
    <p class="empty">{query.trim() !== "" ? `No skill matches “${query.trim()}”.` : strayErrors.length > 0 ? "The skill list is incomplete." : "No skills here yet."}</p>
  {/if}

  {#each groups as g (g.key)}
    {#if g.key === "builtin-codex" && claudeQuiet && !hasClaudeBuiltins}
      {@render claudeNote()}
    {/if}
    <section class="group">
      <h3 class="ghead">
        <span class="lbl">{g.label}</span>
        {#if g.key !== "plugin" && g.hint}<span class="hint">{g.hint}</span>{/if}
      </h3>

      {#if g.key === "plugin"}
        {#each pluginSections(g.skills) as p (p.plugin)}
          <div class="plugin">
            <div class="phead">
              <span class="pname">{p.plugin}</span>
              <span class="hint"
                >{p.skills.length} skill{p.skills.length === 1 ? "" : "s"}{p.agents.length > 0
                  ? ` · ${p.agents.map(agentName).join(" · ")}`
                  : ""}</span
              >
            </div>
            <div class="list">
              {#each p.skills as s, i (s.id ?? `${i}:${s.name}`)}
                {@render row(s, shortName(s))}
              {/each}
            </div>
          </div>
        {/each}
      {:else if g.key.startsWith("builtin-")}
        {@render builtins(g)}
      {:else}
        <div class="list">
          {#each g.skills as s, i (s.id ?? `${i}:${s.name}`)}
            {@render row(s, s.name)}
          {/each}
        </div>
      {/if}
    </section>
  {/each}
  {#if claudeQuiet && !hasClaudeBuiltins && !groups.some((g) => g.key === "builtin-codex")}
    {@render claudeNote()}
  {/if}

  {#if strayErrors.length > 0}
    <section class="group">
      <h3 class="ghead">
        <span class="lbl">Couldn't load</span>
        <button class="link" disabled={status === "loading"} onclick={onRefresh}>{status === "loading" ? "Checking…" : "Try again"}</button>
      </h3>
      {#each strayErrors as e, i (i)}
        <div class="errline">{agentName(e.agent)}: {e.message}{#if e.path}<span class="fpath"> · {e.path}</span>{/if}</div>
      {/each}
    </section>
  {/if}
{/if}

<style>
  .empty {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.5;
  }
  .err {
    color: var(--err);
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
  .lbl {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
  }
  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .note {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.5;
    color: var(--muted);
  }
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--accent);
    cursor: pointer;
    border-radius: 3px;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.small {
    font-size: var(--text-xs);
  }

  /* --- the controls: who · search · count (Settings' control recipes) ------- */
  /* These blocks sit straight in PluginsView's column, whose gap spaces
     them: no margins of their own. */
  .bar {
    display: flex;
    align-items: center;
    gap: 10px 14px;
    flex-wrap: wrap;
  }
  .agent-filter select {
    max-width: 220px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 4px 28px 4px 10px;
  }
  .search {
    flex: 1 1 220px;
    max-width: 340px;
    display: flex;
  }
  .search input {
    width: 100%;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 4px 10px;
  }
  .search input:focus {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }
  .search input::placeholder {
    color: var(--muted);
  }
  .count {
    margin-left: auto;
    font-size: var(--text-sm);
    white-space: nowrap;
  }

  /* --- groups ---------------------------------------------------------------- */
  .group {
    margin: 0;
  }
  .ghead {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px 10px;
    margin: 0 0 10px;
    font: inherit;
  }
  .plugin + .plugin {
    margin-top: 16px;
  }
  .phead {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin: 0 0 6px 2px;
  }
  .pname {
    font-family: var(--mono);
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }

  /* --- a skill row ---------------------------------------------------------- */
  .list {
    border: 1px solid var(--edge);
    border-radius: 10px;
    overflow: hidden;
    background: var(--overlay-bg);
  }
  .skill + .skill {
    border-top: 1px solid var(--edge);
  }
  .shead {
    appearance: none;
    width: 100%;
    border: none;
    background: none;
    font: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
    display: grid;
    grid-template-columns: minmax(120px, 210px) minmax(0, 1fr) auto 12px;
    align-items: start;
    gap: 16px;
    padding: 10px 14px;
    transition: background-color 0.12s ease;
  }
  .shead:hover {
    background: var(--row-hover);
  }
  .shead:focus-visible {
    outline-offset: -2px;
  }
  .expanded .shead {
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .sname {
    font-family: var(--mono);
    font-size: var(--text-sm);
    font-weight: 500;
    color: var(--fg);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    line-height: 1.45;
  }
  .sdesc {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.45;
    min-width: 0;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
  }
  .expanded .sdesc {
    display: block;
    color: var(--fg);
    line-height: 1.55;
  }
  .agents {
    display: flex;
    gap: 4px;
    align-items: center;
    padding-top: 1px;
  }
  .agent {
    font-family: var(--mono);
    font-size: 10.5px;
    line-height: 1.6;
    padding: 0 7px;
    border-radius: 999px;
    color: var(--fg);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--edge));
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .agent.off {
    color: var(--muted);
    border-style: dashed;
    border-color: var(--edge);
    background: none;
    text-decoration: line-through;
  }
  .warnmark {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    font-size: 10px;
    font-weight: 700;
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 55%, var(--edge));
  }
  .chev {
    color: var(--muted);
    margin-top: 4px;
    transition: transform 0.15s ease;
  }
  .expanded .chev {
    transform: rotate(90deg);
  }

  /* --- the opened row: how to call it, where it lives ------------------------ */
  /* A small definition list under the description: labels in the muted
     label style, aligned in one column. */
  .sbody {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    align-items: baseline;
    gap: 10px 18px;
    margin: 0;
    /* Past the name column (14px padding + its 210px + the 16px gap), so
       it lines up under the description. */
    padding: 4px 14px 14px 240px;
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .sbody dt {
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: 600;
    white-space: nowrap;
  }
  .sbody dd {
    margin: 0;
    min-width: 0;
  }
  .uses {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .use {
    appearance: none;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 3px 6px 3px 10px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }
  .use:hover {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }
  .uagent {
    font-size: var(--text-xs);
    color: var(--muted);
    margin-right: 6px;
  }
  .use .uagent {
    margin-right: 0;
  }
  .uinv {
    font-family: var(--mono);
  }
  .ucopy {
    font-size: 10.5px;
    color: var(--muted);
    border-left: 1px solid var(--edge);
    padding-left: 7px;
  }
  .use:hover .ucopy {
    color: var(--accent);
  }
  .useoff {
    display: inline-flex;
    align-items: center;
    font-size: var(--text-sm);
    color: var(--muted);
    padding: 3px 0;
  }
  .fileline {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  .fileline .link {
    flex: none;
    white-space: nowrap;
  }
  .fpath {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .errline {
    font-size: var(--text-sm);
    color: var(--warn);
    line-height: 1.45;
    overflow-wrap: anywhere;
  }

  /* --- built-ins: one quiet flow per agent, each in its own syntax ---------- */
  .flow {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .bchip {
    appearance: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    background: none;
    padding: 1px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .bchip:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  @container (max-width: 720px) {
    .shead {
      grid-template-columns: minmax(0, 1fr) auto 12px;
      gap: 6px 12px;
    }
    .sdesc {
      grid-column: 1 / -1;
      grid-row: 2;
    }
    .sbody {
      padding-left: 14px;
    }
    .count {
      margin-left: 0;
    }
    .agent-filter select {
    max-width: 220px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 7px;
    padding: 4px 28px 4px 10px;
  }
  .search {
      max-width: none;
      flex-basis: 100%;
      order: 3;
    }
  }
</style>
