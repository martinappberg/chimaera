<script lang="ts">
  /**
   * Skills — "what can my agents do here?" (design §6.4): every skill any
   * agent can use in this workspace on this host, in the order you'd look
   * for it — this project's own, then each plugin's (a plugin is what you
   * install, so it is what the list is read by), then yours, then the
   * commands built into the agents as one quiet line. Each row names the
   * skill, says what it does, and carries a badge only for the agents that
   * can use it; a click opens the row in place: how each agent calls it
   * (copyable, in the agent's own syntax), its SKILL.md, and any load error.
   * Truth comes from the agents; codex's load errors are shown, not hidden.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import { copyText } from "../shared/clipboard";
  import {
    filterSkills,
    groupSkills,
    invokeSyntax,
    pluginSections,
    shortName,
    skillCounts,
    usable,
    type SkillFilter,
  } from "./skillsModel";
  import type { AgentId, Skill, SkillsReport } from "./store";

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
  let builtinsAll = $state(false);
  const BUILTIN_SHOWN = 18;

  const AGENTS: AgentId[] = ["claude", "codex"];

  const all = $derived(report?.skills ?? []);
  const counts = $derived(skillCounts(all));
  const shown = $derived(filterSkills(all, filter, query));
  const groups = $derived(groupSkills(shown, host));

  function abs(p: string): string {
    return p.startsWith("/") || p.startsWith("~") || wsRoot === null ? p : `${wsRoot}/${p}`;
  }
  function skillFile(s: Skill): string | null {
    const p = s.paths.claude ?? s.paths.codex ?? null;
    if (p === null) return null;
    return p.endsWith("SKILL.md") ? p : `${p.replace(/\/$/, "")}/SKILL.md`;
  }
  function errorsFor(s: Skill): string[] {
    if (report === null) return [];
    const paths = [s.paths.claude, s.paths.codex].filter((p): p is string => !!p);
    return report.errors
      .filter((e) => e.path !== undefined && paths.some((p) => e.path === p || e.path!.startsWith(p)))
      .map((e) => `${e.agent}: ${e.message}`);
  }
  /** Load errors no listed skill claims (a SKILL.md too broken to list). */
  const strayErrors = $derived.by(() => {
    if (report === null) return [];
    const claimed = all.flatMap((s) => [s.paths.claude, s.paths.codex].filter((p): p is string => !!p));
    return report.errors.filter(
      (e) => e.path === undefined || !claimed.some((p) => e.path === p || e.path!.startsWith(p)),
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
  {@const expanded = openName === s.name}
  {@const errs = errorsFor(s)}
  <div class="skill" class:expanded>
    <button class="shead" aria-expanded={expanded} onclick={() => (openName = expanded ? null : s.name)}>
      <span class="sname" title={s.name}>{name}</span>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
      <span class="sdesc">{@html inlineMarkdown(s.description)}</span>
      <span class="agents">
        {#if errs.length > 0}<span class="warnmark" title={errs.join("\n")}>!</span>{/if}
        {#each AGENTS as a (a)}
          {@const st = s.agents[a]}
          {#if st.state === "available"}
            <span class="agent">{a}</span>
          {:else if st.state === "off"}
            <span class="agent off" title="{a}: {st.reason ?? 'present but not usable'}">{a}</span>
          {/if}
        {/each}
      </span>
      <svg class="chev" viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
        <path d="M6 3.5L10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    {#if expanded}
      {@const file = skillFile(s)}
      <div class="sbody">
        <div class="uses">
          {#each AGENTS as a (a)}
            {@const st = s.agents[a]}
            {#if st.state === "available"}
              <button class="use" title="copy — then type it in {a}" onclick={() => copy(s, a)}>
                <span class="uagent">{a}</span>
                <span class="uinv">{invokeSyntax(s, a)}</span>
                <span class="ucopy">{copied === `${s.name}:${a}` ? "copied" : "copy"}</span>
              </button>
            {:else if st.state === "off"}
              <span class="useoff"><span class="uagent">{a}</span>{st.reason ?? "present but not usable"}</span>
            {/if}
          {/each}
        </div>
        {#if file !== null}
          <div class="fileline">
            <button class="link" onclick={() => onOpenFile(abs(file))}>open SKILL.md</button>
            <span class="fpath" title={abs(file)}>{file}</span>
          </div>
        {/if}
        {#each errs as e, i (i)}
          <div class="errline">{e}</div>
        {/each}
      </div>
    {/if}
  </div>
{/snippet}

{#if status === "unavailable"}
  <p class="empty">This daemon can't list skills yet — update chimaera.</p>
{:else if status === "error" && report === null}
  <p class="empty err">{error} <button class="link" onclick={onRefresh}>retry</button></p>
{:else if report === null}
  <p class="empty">asking claude and codex…</p>
{:else}
  <div class="bar">
    <span class="count">
      <b>{counts.total}</b> skill{counts.total === 1 ? "" : "s"}
      <span class="muted">· claude {counts.claude} · codex {counts.codex}</span>
    </span>
    <div class="filters" role="group" aria-label="Show skills for">
      {#each [
        { v: "all", l: "All" },
        { v: "claude", l: "claude" },
        { v: "codex", l: "codex" },
      ] as f (f.v)}
        <button
          class="fchip"
          class:on={filter === f.v}
          aria-pressed={filter === f.v}
          onclick={() => (filter = f.v as SkillFilter)}>{f.l}</button
        >
      {/each}
    </div>
    <label class="search">
      <span class="sr">Search skills</span>
      <input type="search" placeholder="Search skills…" bind:value={query} spellcheck="false" />
    </label>
  </div>

  {#if !report.agents.claude.available && !report.agents.codex.available}
    <p class="empty">Neither claude nor codex is installed on {host}, so there are no skills to list.</p>
  {:else if shown.length === 0}
    <p class="empty">Nothing matches.</p>
  {/if}

  {#each groups as g (g.key)}
    <section class="group">
      <h3 class="ghead">
        <span class="lbl">{g.key === "builtin" ? "Built into the agents" : g.label}</span>
        {#if g.key === "builtin"}
          <span class="hint"
            >the commands they ship with{!report.agents.claude.live
              ? " · start a claude chat session to see claude's"
              : ""}</span
          >
        {:else if g.key !== "plugin" && g.hint}
          <span class="hint">{g.hint}</span>
        {/if}
      </h3>

      {#if g.key === "plugin"}
        {#each pluginSections(g.skills) as p (p.plugin)}
          <div class="plugin">
            <div class="phead">
              <span class="pname">{p.plugin}</span>
              <span class="hint"
                >{p.skills.length} skill{p.skills.length === 1 ? "" : "s"}{p.agents.length > 0
                  ? ` · ${p.agents.join(" + ")}`
                  : ""}</span
              >
            </div>
            <div class="list">
              {#each p.skills as s (s.name)}
                {@render row(s, shortName(s))}
              {/each}
            </div>
          </div>
        {/each}
      {:else if g.key === "builtin"}
        <!-- One quiet line: the agents' own commands, in their own syntax. -->
        <div class="flow">
          {#each builtinsAll ? g.skills : g.skills.slice(0, BUILTIN_SHOWN) as s (s.name)}
            {@const agent = usable(s, "claude") ? "claude" : "codex"}
            <span class="bchip" title="{agent}: {s.description}">{invokeSyntax(s, agent)}</span>
          {/each}
          {#if g.skills.length > BUILTIN_SHOWN}
            <button class="link" onclick={() => (builtinsAll = !builtinsAll)}>
              {builtinsAll ? "fewer" : `+ ${g.skills.length - BUILTIN_SHOWN} more`}
            </button>
          {/if}
        </div>
      {:else}
        <div class="list">
          {#each g.skills as s (s.name)}
            {@render row(s, s.name)}
          {/each}
        </div>
      {/if}
    </section>
  {/each}

  {#if strayErrors.length > 0}
    <section class="group">
      <h3 class="ghead"><span class="lbl">Couldn't load</span><span class="hint">as the agents report it</span></h3>
      {#each strayErrors as e, i (i)}
        <div class="errline">{e.agent}: {e.message}{#if e.path}<span class="fpath"> · {e.path}</span>{/if}</div>
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
  .muted {
    color: var(--muted);
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
  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--accent);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }

  /* --- the bar: count · who · search ---------------------------------------- */
  .bar {
    display: flex;
    align-items: center;
    gap: 14px;
    flex-wrap: wrap;
    margin-bottom: 22px;
  }
  .count {
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  .filters {
    display: flex;
    gap: 4px;
  }
  .fchip {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 2px 10px;
    border-radius: 999px;
    cursor: pointer;
    transition:
      color 0.12s ease,
      border-color 0.12s ease,
      background-color 0.12s ease;
  }
  .fchip:hover {
    color: var(--fg);
  }
  .fchip.on {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .search {
    margin-left: auto;
  }
  .search input {
    width: 220px;
    max-width: 40vw;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 5px 10px;
    outline: none;
  }
  .search input:focus {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  /* --- groups ---------------------------------------------------------------- */
  .group {
    margin-bottom: 26px;
  }
  .ghead {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin: 0 0 10px;
    font: inherit;
  }
  .plugin {
    margin-bottom: 14px;
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
    background: var(--bg);
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
  /* Same columns as the row head, so everything lines up under the
     description. */
  .sbody {
    display: grid;
    grid-template-columns: minmax(120px, 210px) minmax(0, 1fr) auto 12px;
    column-gap: 16px;
    row-gap: 8px;
    padding: 2px 14px 12px;
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .sbody > :global(*) {
    grid-column: 2 / 4;
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
    border-radius: 8px;
    padding: 4px 6px 4px 10px;
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
    padding: 4px 0;
  }
  .fileline {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
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
  }

  /* --- built-ins: one quiet flow -------------------------------------------- */
  .flow {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 8px;
  }
  .bchip {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    padding: 1px 8px;
    border: 1px solid var(--edge);
    border-radius: 999px;
    cursor: default;
  }

  @media (max-width: 720px) {
    .shead {
      grid-template-columns: minmax(0, 1fr) auto 12px;
    }
    .sdesc {
      grid-column: 1 / -1;
      grid-row: 2;
    }
    .sbody {
      grid-template-columns: minmax(0, 1fr);
    }
    .sbody > :global(*) {
      grid-column: 1;
    }
  }
</style>
