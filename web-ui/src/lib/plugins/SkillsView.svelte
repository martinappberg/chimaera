<script lang="ts">
  /**
   * Skills — "what can my agents do here?" (design §6.4): every skill any
   * agent can use in this workspace on this host, grouped by where it comes
   * from (that is what says who else gets it), one chip per agent (✓
   * available · ◌ present but not usable, reason in words · — absent), and a
   * detail aside with each agent's own invocation syntax. Truth comes from
   * the agents; codex's load errors are shown, not hidden.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import { filterSkills, groupSkills, invokeSyntax, skillCounts, type SkillFilter } from "./skillsModel";
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
  let selectedName = $state<string | null>(null);
  /** Built-ins render as a chip flow; the overflow expands on request. */
  let builtinsAll = $state(false);
  const BUILTIN_SHOWN = 12;

  const all = $derived(report?.skills ?? []);
  const counts = $derived(skillCounts(all));
  const shown = $derived(filterSkills(all, filter, query));
  const groups = $derived(groupSkills(shown, host));
  const selected = $derived(
    all.find((s) => s.name === selectedName) ?? shown[0] ?? null,
  );

  const AGENTS: AgentId[] = ["claude", "codex"];

  function chip(s: Skill, agent: AgentId): { text: string; tone: "good" | "warn" | "faint"; title: string } {
    const st = s.agents[agent];
    if (st.state === "available") return { text: `${agent} ✓`, tone: "good", title: `usable by ${agent}` };
    if (st.state === "off")
      return { text: `${agent} ◌ ${st.reason ?? "off"}`, tone: "warn", title: st.reason ?? "present but not usable" };
    return { text: `${agent} —`, tone: "faint", title: `not available to ${agent}` };
  }

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
  function sourceLine(s: Skill): string {
    switch (s.source) {
      case "project":
        return "in this project";
      case "plugin":
        return `from the ${s.plugin ?? "plugin"} plugin`;
      case "user":
        return `yours — user-level on ${host}`;
      case "builtin":
      case "system":
        return "built into the agent";
      default:
        return s.source;
    }
  }
</script>

{#if status === "unavailable"}
  <p class="empty">This daemon can't list skills yet — update chimaera.</p>
{:else if status === "error" && report === null}
  <p class="empty err">{error} <button class="link" onclick={onRefresh}>retry</button></p>
{:else if report === null}
  <p class="empty">asking claude and codex…</p>
{:else}
  <div class="bar">
    <span class="count"><b>{counts.total} skill{counts.total === 1 ? "" : "s"}</b>
      <span class="muted">· claude {counts.claude} · codex {counts.codex} · both {counts.both}</span></span
    >
    <div class="chips" role="group" aria-label="Show">
      {#each [
        { v: "all", l: "All" },
        { v: "claude", l: "claude" },
        { v: "codex", l: "codex" },
        { v: "one", l: `only one agent · ${counts.onlyOne}` },
      ] as f (f.v)}
        <button class="fchip" class:on={filter === f.v} aria-pressed={filter === f.v} onclick={() => (filter = f.v as SkillFilter)}>{f.l}</button>
      {/each}
    </div>
    <label class="search">
      <span class="sr">Search skills</span>
      <input type="search" placeholder="Search skills…" bind:value={query} spellcheck="false" />
    </label>
  </div>

  {#if !report.agents.claude.available && !report.agents.codex.available}
    <p class="empty">Neither claude nor codex is installed on {host}, so there are no skills to list.</p>
  {/if}

  <div class="grid">
    <div class="groups">
      {#if shown.length === 0}
        <p class="empty">Nothing matches.</p>
      {/if}
      {#each groups as g (g.key)}
        <div class="group">
          <div class="ghead">
            <span class="lbl">{g.label}</span>
            {#if g.hint}<span class="hint">{g.hint}</span>{/if}
            {#if g.key === "builtin" && !report.agents.claude.live}
              <span class="hint">— start a claude chat session to see its built-ins</span>
            {/if}
          </div>
          {#if g.key === "builtin"}
            <div class="flow">
              {#each builtinsAll ? g.skills : g.skills.slice(0, BUILTIN_SHOWN) as s (s.name)}
                {@const agent = s.agents.claude.state === "available" ? "claude" : "codex"}
                <button class="bchip mono" class:on={selected?.name === s.name} onclick={() => (selectedName = s.name)}>
                  {invokeSyntax(s, agent)} <span class="muted">{agent}</span>
                </button>
              {/each}
              {#if g.skills.length > BUILTIN_SHOWN}
                <button class="link" onclick={() => (builtinsAll = !builtinsAll)}>
                  {builtinsAll ? "fewer" : `+ ${g.skills.length - BUILTIN_SHOWN} more`}
                </button>
              {/if}
            </div>
          {:else}
            {#each g.skills as s (s.name)}
              <button class="row" class:on={selected?.name === s.name} onclick={() => (selectedName = s.name)}>
                <span class="mono sname">{s.name}</span>
                <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
                <span class="sdesc">{@html inlineMarkdown(s.description)}</span>
                {#each AGENTS as a (a)}
                  {@const c = chip(s, a)}
                  <span class="achip {c.tone}" title={c.title}>{c.text}</span>
                {/each}
              </button>
            {/each}
          {/if}
        </div>
      {/each}
    </div>

    {#if selected !== null}
      {@const file = skillFile(selected)}
      {@const errs = errorsFor(selected)}
      <aside class="detail" aria-label="Skill details">
        <div class="dhead">
          <span class="mono dname">{selected.name}</span>
          <span class="dsource">{sourceLine(selected)}</span>
        </div>
        {#if selected.description}
          <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
          <p class="ddesc">{@html inlineMarkdown(selected.description)}</p>
        {/if}
        <div class="dgrid">
          {#each AGENTS as a (a)}
            {@const st = selected.agents[a]}
            <span class="muted">{a}</span>
            <span>
              {#if st.state === "available"}
                <span class="good">✓</span> type <span class="mono inv">{invokeSyntax(selected, a)}</span>
              {:else if st.state === "off"}
                <span class="warn">◌</span> {st.reason ?? "present but not usable"}
              {:else}
                <span class="faint">—</span> not available
              {/if}
            </span>
          {/each}
          {#if selected.paths.claude || selected.paths.codex}
            <span class="muted">files</span>
            <span class="mono paths">
              {#if selected.paths.claude}<span title="claude">{selected.paths.claude}</span>{/if}
              {#if selected.paths.codex && selected.paths.codex !== selected.paths.claude}<span title="codex">{selected.paths.codex}</span>{/if}
            </span>
          {/if}
        </div>
        {#if errs.length > 0}
          <div class="derr">
            <span class="lbl">Load errors</span>
            {#each errs as e, i (i)}<div>{e}</div>{/each}
          </div>
        {/if}
        {#if file !== null}
          <button class="link" onclick={() => onOpenFile(abs(file))}>open SKILL.md</button>
        {/if}
      </aside>
    {/if}
  </div>
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
  .mono {
    font-family: var(--mono);
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

  .bar {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .count {
    font-size: var(--text-md);
  }
  .count b {
    font-weight: 600;
  }
  .chips {
    margin-left: 16px;
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .fchip {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 3px 11px;
    border-radius: 999px;
    cursor: pointer;
  }
  .fchip:hover {
    color: var(--fg);
  }
  .fchip.on {
    color: var(--fg);
    border-color: var(--fg);
  }
  .search {
    margin-left: auto;
    display: flex;
  }
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .search input {
    width: 240px;
    max-width: 50vw;
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--fg);
    border-radius: 8px;
    padding: 6px 12px;
    font: inherit;
    font-size: var(--text-sm);
  }

  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 380px;
    gap: 24px;
    align-items: start;
  }
  @container (max-width: 900px) {
    .grid {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  .groups {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-width: 0;
  }
  .group {
    display: flex;
    flex-direction: column;
  }
  .ghead {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding-bottom: 6px;
    flex-wrap: wrap;
  }
  .row {
    appearance: none;
    border: none;
    border-top: 1px solid var(--edge);
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    cursor: pointer;
    display: grid;
    grid-template-columns: 170px minmax(0, 1fr) 92px 92px;
    column-gap: 14px;
    align-items: center;
    padding: 9px 12px;
    font-size: var(--text-sm);
    border-radius: 0;
    transition: background-color 0.12s ease;
  }
  .row:hover {
    background: var(--row-hover);
  }
  .row.on {
    background: var(--row-active);
    border-radius: 8px;
    border-top-color: transparent;
  }
  .row.on + .row {
    border-top-color: transparent;
  }
  @container (max-width: 640px) {
    .row {
      grid-template-columns: minmax(0, 1fr) auto auto;
    }
    .sdesc {
      display: none;
    }
  }
  .sname {
    font-size: 12.5px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sdesc {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sdesc :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .achip {
    font-size: var(--text-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .achip.good,
  .good {
    color: var(--accent);
  }
  .achip.warn,
  .warn {
    color: var(--warn);
  }
  .achip.faint,
  .faint {
    color: color-mix(in srgb, var(--muted) 65%, transparent);
  }

  .flow {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding: 10px 12px;
    border-top: 1px solid var(--edge);
    align-items: center;
    font-size: var(--text-xs);
  }
  .bchip {
    appearance: none;
    border: 1px solid var(--edge);
    background: none;
    color: var(--fg);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    padding: 2px 9px;
    border-radius: 999px;
    cursor: pointer;
  }
  .bchip:hover,
  .bchip.on {
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .detail {
    position: sticky;
    top: 0;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 12px;
    padding: 18px 20px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }
  .dhead {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .dname {
    font-size: var(--text-lg);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .dsource {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .ddesc {
    margin: 0;
    font-size: var(--text-md);
    line-height: 1.55;
  }
  .ddesc :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .dgrid {
    display: grid;
    grid-template-columns: 64px minmax(0, 1fr);
    row-gap: 8px;
    column-gap: 12px;
    font-size: var(--text-sm);
  }
  .inv {
    font-size: 12.5px;
  }
  .paths {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--text-xs);
    overflow-wrap: anywhere;
    color: var(--muted);
  }
  .derr {
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: var(--text-xs);
    color: var(--err);
    border-top: 1px solid var(--edge);
    padding-top: 12px;
  }
</style>
