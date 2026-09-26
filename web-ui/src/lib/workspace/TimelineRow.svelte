<script lang="ts">
  /**
   * One Timeline row — the ONE anatomy the dashboard's "Since you left" and
   * the Timeline view share (design §4/§9): a kind glyph, the session name
   * in muted mono, the user's own prompt as the headline, time/duration on
   * the right; a second line with the agent's first real sentence ("→ …");
   * an evidence line (files · tools, what got recorded). Ids and metadata
   * step back into mono; colour never travels without a word.
   *
   * Everything the row shows is agent- or file-influenced text: one-liners
   * go through inlineMarkdown (escape-then-format, safe by construction),
   * never raw.
   */
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import type { Session } from "./sessions";
  import type { TimelineEntry } from "./timeline.svelte";
  import {
    commandHead,
    commandLabel,
    formatClock,
    formatDuration,
    groupDurationMs,
    groupEvidence,
    jobFailed,
    type TimelineGroup,
  } from "./timelineModel";
  import { relPath } from "../dashboard/dash";

  interface Props {
    group: TimelineGroup;
    sessions: Map<string, Session>;
    names: Map<string, string>;
    wsRoot: string | null;
    /** Open/focus a live session (rail-click semantics); rows whose session
     *  is gone render the name without a door. */
    onOpenSession: (id: string) => void;
    /** Open a file the episode touched (workspace-relative on the wire). */
    onOpenFile: (absPath: string) => void;
    /** Knowledge rows link into the Knowledge view. */
    onOpenKnowledge?: () => void;
    /** Agent notes: hand the note to its addressee as a real message. */
    onDeliver?: (entry: TimelineEntry) => void;
    /** The row's own delivery state text (the caller owns the request). */
    deliverState?: string | null;
  }

  let {
    group,
    sessions,
    names,
    wsRoot,
    onOpenSession,
    onOpenFile,
    onOpenKnowledge,
    onDeliver,
    deliverState = null,
  }: Props = $props();

  const first = $derived(group.first);
  const last = $derived(group.last);
  const sid = $derived(first.sid ?? null);
  const live = $derived(sid !== null && sessions.has(sid));
  const name = $derived(
    sid !== null ? (names.get(sid) ?? sessions.get(sid)?.name ?? first.name ?? sid) : (first.name ?? ""),
  );
  const evidence = $derived(groupEvidence(group));
  const durationMs = $derived(groupDurationMs(group));

  /** Absolute path for a workspace-relative evidence path. */
  function abs(p: string): string {
    return p.startsWith("/") || wsRoot === null ? p : `${wsRoot}/${p}`;
  }
  function base(p: string): string {
    const i = p.lastIndexOf("/");
    return i >= 0 ? p.slice(i + 1) : p;
  }

  /** The glyph column: shape + tone, always paired with a word in the line. */
  const glyph = $derived.by((): { mark: string; tone: "accent" | "err" | "muted" | "warn"; title: string } => {
    switch (first.kind) {
      case "episode": {
        const end = last.end ?? "unknown";
        if (end === "errored") return { mark: "✕", tone: "err", title: "the turn errored" };
        if (end === "finished") return { mark: "●", tone: "accent", title: "finished" };
        if (end === "interrupted") return { mark: "○", tone: "muted", title: "interrupted" };
        if (end === "exited") return { mark: "○", tone: "muted", title: "the session exited" };
        return { mark: "○", tone: "muted", title: "outcome unknown" };
      }
      case "command":
        return first.command?.exit !== undefined && first.command.exit !== 0
          ? { mark: "✕", tone: "err", title: `exit ${first.command.exit}` }
          : { mark: "✓", tone: "accent", title: "finished" };
      case "job":
        return jobFailed(first.job?.state)
          ? { mark: "✕", tone: "err", title: first.job?.state ?? "failed" }
          : first.job?.state === "COMPLETED"
            ? { mark: "✓", tone: "accent", title: "COMPLETED" }
            : { mark: "○", tone: "muted", title: first.job?.state ?? "ended" };
      case "knowledge":
        return first.knowledge?.to === "contradicted"
          ? { mark: "◇", tone: "err", title: "contradicted" }
          : first.knowledge?.to === "preliminary"
            ? { mark: "◆", tone: "muted", title: first.knowledge.to }
            : { mark: "◆", tone: "accent", title: first.knowledge?.to ?? "recorded" };
      case "session":
        return { mark: "✕", tone: "err", title: "the session ended unexpectedly" };
      case "note":
        return { mark: "✉", tone: "muted", title: "a note" };
      default:
        return { mark: "·", tone: "muted", title: first.kind };
    }
  });

  /** Right column: episodes carry duration · clock; the rest a clock. */
  const when = $derived.by(() => {
    const clock = formatClock(group.ts);
    if (first.kind === "episode" && durationMs >= 5_000) return `${formatDuration(durationMs)} · ${clock}`;
    return clock;
  });

  /** Where a note is addressed: a session name, the Mastermind, or everyone. */
  const noteTo = $derived.by(() => {
    const to = first.note?.to;
    if (to === undefined) return "everyone";
    if (to === "mastermind") return "Mastermind";
    return names.get(to) ?? sessions.get(to)?.name ?? to;
  });

  const jobVerb = $derived.by(() => {
    const j = first.job;
    if (j === undefined) return "";
    const el = j.elapsed !== undefined && j.elapsed !== "" ? ` in ${j.elapsed}` : "";
    if (j.state === "COMPLETED") return `completed${el}`;
    if (jobFailed(j.state)) return `${j.state.toLowerCase().replace(/_/g, " ")}${el.replace(" in ", " after ")}`;
    return `ended${el}`;
  });

  const tierTitle = $derived(
    first.tier === "hooks" ? "seen through Claude Code hooks — coarse but honest" : null,
  );
</script>

<div class="trow" class:bad={group.bad}>
  <span class="glyph {glyph.tone}" title={glyph.title} aria-hidden="true">{glyph.mark}</span>

  <div class="head">
    {#if first.kind === "episode"}
      {#if live && sid !== null}
        <button class="name link" onclick={() => onOpenSession(sid)} title="open the session">{name}</button>
      {:else}
        <span class="name">{name}</span>
      {/if}
      {#if first.title}
        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
        <span class="title">“{@html inlineMarkdown(first.title)}”</span>
      {:else}
        <span class="title quiet">was active</span>
      {/if}
      {#if group.followUps > 0}
        <span class="follow" title="{group.followUps + 1} turns within ten minutes, folded into one row">
          +{group.followUps} follow-up{group.followUps === 1 ? "" : "s"}
        </span>
      {/if}
    {:else if first.kind === "command"}
      <!-- The terminal's name leads unless it IS the program ("snakemake
           snakemake …" reads as a stutter); the command text is the identity. -->
      {#if name !== "" && name !== commandHead(first.command?.text ?? "")}
        {#if live && sid !== null}
          <button class="name link" onclick={() => onOpenSession(sid)} title="open the terminal">{name}</button>
        {:else}
          <span class="name">{name}</span>
        {/if}
      {/if}
      <span class="cmd" title={first.command?.text}>{commandLabel(first.command?.text ?? "")}</span>
      <span class="title">
        {#if first.command?.exit !== undefined && first.command.exit !== 0}
          failed after {formatDuration(first.command.ms)}
        {:else}
          ran {formatDuration(first.command?.ms ?? 0)}
        {/if}
      </span>
    {:else if first.kind === "job"}
      <span class="name">job {first.job?.id}</span>
      <span class="title"><span class="mono">{first.job?.name}</span> {jobVerb}</span>
    {:else if first.kind === "knowledge"}
      {@const k = first.knowledge}
      <button
        class="name link"
        class:err={k?.to === "contradicted"}
        class:good={k?.to === "supported" || k?.to === "robust"}
        onclick={onOpenKnowledge}
        title="open in Knowledge">{k?.id}</button
      >
      <span class="title">
        {#if k?.change === "new"}recorded{:else if k?.to === "contradicted"}was contradicted{:else}is now {k?.to}{/if}
        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
        — {@html inlineMarkdown(k?.claim ?? "")}
      </span>
    {:else if first.kind === "session"}
      <span class="name">{name}</span>
      <span class="title">{first.end === "errored" ? "crashed" : "exited unexpectedly"}</span>
      {#if first.result}
        <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
        <span class="title quiet">— {@html inlineMarkdown(first.result)}</span>
      {/if}
    {:else if first.kind === "note"}
      {@const n = first.note}
      <span class="name">{n?.from_name ?? name}</span>
      <span class="to">→ {noteTo}</span>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
      <span class="title">{@html inlineMarkdown(n?.text ?? "")}</span>
    {:else}
      <span class="name">{name}</span>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
      <span class="title">{@html inlineMarkdown(first.title ?? first.kind)}</span>
    {/if}
  </div>

  <span class="when">{when}</span>

  {#if first.kind === "episode"}
    {#if last.result}
      <span></span>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
      <div class="result">→ {@html inlineMarkdown(last.result)}</div>
      <span></span>
    {/if}
    {#if evidence.filesN > 0 || evidence.tools > 0 || evidence.recorded !== null || first.via === "mastermind" || tierTitle !== null}
      <span></span>
      <div class="evidence">
        {#if evidence.files.length > 0}
          <span class="files">
            {#each evidence.files.slice(0, 3) as f (f)}
              <button class="file" title={relPath(wsRoot, abs(f))} onclick={() => onOpenFile(abs(f))}>{base(f)}</button>
            {/each}
            {#if evidence.filesN > 3}
              <span class="more" title={evidence.files.join("\n")}>+{evidence.filesN - 3} more</span>
            {/if}
          </span>
        {:else if evidence.filesN > 0}
          <span>{evidence.filesN} file{evidence.filesN === 1 ? "" : "s"}</span>
        {/if}
        {#if evidence.tools > 0}
          <span>{evidence.tools} tool{evidence.tools === 1 ? "" : "s"}</span>
        {/if}
        {#if evidence.turns > 1}
          <span>{evidence.turns} turns</span>
        {/if}
        {#if evidence.recorded !== null}
          {@const r = evidence.recorded}
          <button class="pill" onclick={onOpenKnowledge} title="open in Knowledge">
            recorded
            {#if r.findings.length > 0}<span class="mono">{r.findings.join(" · ")}</span>{/if}
            {#if r.learnings > 0}· {r.learnings} learning{r.learnings === 1 ? "" : "s"}{/if}
            {#if r.decisions > 0}· {r.decisions} decision{r.decisions === 1 ? "" : "s"}{/if}
          </button>
        {/if}
        {#if first.via === "mastermind"}
          <span class="via" title="this prompt was relayed by the workspace Mastermind">via Mastermind</span>
        {/if}
        {#if tierTitle !== null}
          <!-- Fidelity is worn as a quiet mark + tooltip, never as words. -->
          <span class="tier" title={tierTitle} aria-label={tierTitle}>◌</span>
        {/if}
      </div>
      <span></span>
    {/if}
  {:else if first.kind === "command" && first.command}
    {@const c = first.command}
    <span></span>
    <div class="result quiet">
      {#if c.exit !== undefined}exit {c.exit}{/if}
      {#if c.source === "agent"}<span class="dot-sep">·</span> run by an agent{/if}
      {#if live && sid !== null}
        <span class="dot-sep">·</span>
        <button class="inline-link" onclick={() => onOpenSession(sid)}>open terminal</button>
      {/if}
    </div>
    <span></span>
  {:else if first.kind === "knowledge" && first.knowledge}
    {@const k = first.knowledge}
    {#if k.from || name !== ""}
      <span></span>
      <div class="result quiet">
        {#if k.from}was {k.from}{/if}
        {#if name !== ""}{#if k.from}<span class="dot-sep">·</span>{/if}recorded by <span class="mono">{name}</span>{/if}
      </div>
      <span></span>
    {/if}
  {:else if first.kind === "note" && onDeliver !== undefined && first.note?.to !== undefined && first.note.to !== "mastermind"}
    <span></span>
    <div class="result quiet">
      {#if deliverState !== null}
        {deliverState}
      {:else}
        <button class="inline-link" onclick={() => onDeliver(first)} title="send this note to {noteTo} as a real message — your click starts that turn"
          >deliver to {noteTo}</button
        >
      {/if}
    </div>
    <span></span>
  {/if}
</div>

<style>
  .trow {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) auto;
    column-gap: 10px;
    row-gap: 3px;
    padding: 10px 0;
    border-top: 1px solid var(--edge);
    align-items: start;
    min-width: 0;
  }
  .glyph {
    justify-self: center;
    margin-top: 2px;
    font-size: var(--text-xs);
    line-height: 1.3;
    font-weight: 700;
  }
  .glyph.accent {
    color: var(--accent);
  }
  .glyph.err {
    color: var(--err);
  }
  .glyph.warn {
    color: var(--warn);
  }
  .glyph.muted {
    color: var(--muted);
  }

  .head {
    min-width: 0;
    font-size: var(--text-md);
    line-height: 1.4;
    display: inline;
  }
  .name {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    margin-right: 8px;
  }
  button.name {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    cursor: pointer;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }
  button.name:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  button.name.err {
    color: var(--err);
  }
  button.name.good {
    color: var(--accent);
  }
  .title {
    overflow-wrap: anywhere;
  }
  .title.quiet {
    color: var(--muted);
  }
  .title :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .cmd,
  .mono {
    font-family: var(--mono);
    font-size: var(--text-sm);
  }
  .cmd {
    margin-right: 6px;
  }
  .to {
    color: var(--muted);
    margin-right: 6px;
  }
  .follow {
    margin-left: 8px;
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }

  .when {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
    padding-top: 2px;
    font-variant-numeric: tabular-nums;
  }

  .result {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.45;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  .result :global(code) {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .dot-sep {
    margin: 0 4px;
  }
  .inline-link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
  }
  .inline-link:hover {
    text-decoration: underline;
  }

  .evidence {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
    font-size: var(--text-xs);
    color: var(--muted);
    padding-top: 2px;
    min-width: 0;
  }
  .files {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .file {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }
  .file:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  .more {
    opacity: 0.8;
  }
  .pill {
    appearance: none;
    border: none;
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    color: var(--accent);
    padding: 1px 8px;
    border-radius: 999px;
    white-space: nowrap;
  }
  .pill:hover {
    background: color-mix(in srgb, var(--accent) 20%, transparent);
  }
  .pill .mono {
    font-size: var(--text-xs);
  }
  .via {
    font-size: var(--text-xs);
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 0 7px;
    white-space: nowrap;
  }
  .tier {
    color: var(--muted);
    opacity: 0.8;
    cursor: help;
  }
</style>
