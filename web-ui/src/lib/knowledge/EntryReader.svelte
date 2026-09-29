<script lang="ts">
  /**
   * One entry, read in full: where it sits and who wrote it, its standing
   * (corrected, superseded — only what the text says), its status as the
   * agent wrote it, what it cites, its body as written, its follow-ups, and
   * what refers to it. Every id is a chip; every file opens at its line.
   */
  import { fsValidate } from "../previews/files";
  import { inlineMarkdown } from "../shared/inlineMarkdown";
  import type { Knowledge, KnowledgeLabels } from "../workspace/knowledge";
  import type { RefMatcher, ReferenceChips } from "../shared/references";
  import Badge from "../shared/ui/Badge.svelte";
  import Callout from "../shared/ui/Callout.svelte";
  import FileCard from "../shared/ui/FileCard.svelte";
  import { qualifiedId, resolveRef, type Entry, type KnowledgeIndex } from "./entries";
  import EntryBody from "./EntryBody.svelte";
  import { kindWord, priorityTone, shortStatusNote, stateLabel, TODO_GROUP_LABEL, todoGroup } from "./overview";
  import StatusMark from "./StatusMark.svelte";

  interface Props {
    entry: Entry;
    k: Knowledge;
    idx: KnowledgeIndex;
    matcher: RefMatcher | null;
    labels: KnowledgeLabels;
    chips: ReferenceChips;
    wsRoot: string | null;
    wsId: string | null;
    canBack: boolean;
    canForward: boolean;
    onBack: () => void;
    onForward: () => void;
    /** Show another entry here (history grows). */
    onOpen: (ekey: string) => void;
    onOpenFile: (path: string, line: number, end: number) => void;
    onOpenSession: (sid: string) => void;
    /** Narrow panes: back to the list. */
    onClose?: () => void;
  }

  let {
    entry,
    k,
    idx,
    matcher,
    labels,
    chips,
    wsRoot,
    wsId,
    canBack,
    canForward,
    onBack,
    onForward,
    onOpen,
    onOpenFile,
    onOpenSession,
    onClose,
  }: Props = $props();

  const word = (kind: string): string => kindWord(labels, kind);

  const e = $derived(entry);
  const shownId = $derived(qualifiedId(idx, e));
  const standing = $derived(stateLabel(e, (id) => id));
  const stateBy = $derived.by(() => {
    if (e.state === null || e.state.by === "") return [];
    return resolveRef(idx, { kind: "", id: e.state.by }, e);
  });
  const amends = $derived(
    e.amends.map((a) => ({ a, targets: resolveRef(idx, { kind: "", id: a.id }, e) })),
  );
  /** Other entries that carry this id (a reused finding id). */
  const namesakes = $derived(
    e.id !== "" ? (idx.byId.get(e.id.toUpperCase()) ?? []).filter((x) => x !== e && x.kind === e.kind) : [],
  );
  const backlinks = $derived(idx.backlinks.get(e.ekey) ?? []);
  const refs = $derived(
    e.refs.map((r) => ({ r, found: resolveRef(idx, r, e) })).filter((x) => !e.amends.some((a) => a.id === x.r.id)),
  );
  /** The handoff's asks name this entry's id (whole, not inside D-157). */
  const handoffCites = $derived.by(() => {
    if (k.left_off === null || e.id === "") return false;
    const id = e.id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const re = new RegExp(`(?<![\\w-])${id}(?![\\w-])`, "i");
    return k.asks.some((a) => a.source.kind === "handoff" && re.test(a.text));
  });

  const FILE_KINDS = new Set(["script", "data", "figure", "doc", "path"]);
  /** Cited paths that resolve in the workspace (only those open). */
  let validCites = $state<Record<string, string>>({});
  $effect(() => {
    const paths = e.cites.filter((c) => FILE_KINDS.has(c.kind)).map((c) => c.text);
    validCites = {};
    if (paths.length === 0 || wsRoot === null) return;
    let cancelled = false;
    fsValidate(paths, wsRoot, wsId).then(
      (r) => {
        if (cancelled) return;
        const out: Record<string, string> = {};
        for (const p of paths) {
          const hit = r.valid[p];
          if (hit !== undefined && hit.kind === "file") out[p] = hit.path;
        }
        validCites = out;
      },
      () => {},
    );
    return () => {
      cancelled = true;
    };
  });

  const fallback = $derived.by(() => {
    const f = e.finding;
    if (f !== undefined) {
      const parts: string[] = [];
      if (f.implications) parts.push(`**So what.** ${f.implications}`);
      if (f.ledger.length > 0) {
        parts.push(
          [
            "| Date | Run | Dataset | Result | Direction |",
            "|---|---|---|---|---|",
            ...f.ledger.map((r) => `| ${[r.date, r.run, r.dataset, r.result, r.direction].map((c) => c.replace(/\|/g, "\\|")).join(" | ")} |`),
          ].join("\n"),
        );
      }
      if (f.questions.length > 0) parts.push(`**Open questions**\n\n${f.questions.map((q) => `- ${q}`).join("\n")}`);
      return parts.join("\n\n");
    }
    const d = e.decision;
    if (d !== undefined) {
      const parts: string[] = [];
      if (d.context) parts.push(`**Context.** ${d.context}`);
      if (d.decision) parts.push(`**Decision.** ${d.decision}`);
      if (d.alternatives.length > 0) parts.push(`**Instead of**\n\n${d.alternatives.map((a) => `- ${a}`).join("\n")}`);
      if (d.rationale) parts.push(`**Why.** ${d.rationale}`);
      if (d.consequences) parts.push(`**Consequences.** ${d.consequences}`);
      return parts.join("\n\n");
    }
    const l = e.learning;
    if (l !== undefined) {
      const parts: string[] = [];
      if (l.what) parts.push(`**What happened.** ${l.what}`);
      if (l.why) parts.push(`**Why it matters.** ${l.why}`);
      if (l.resolution) parts.push(`**Resolution.** ${l.resolution}`);
      return parts.join("\n\n");
    }
    const t = e.todo;
    if (t !== undefined) return t.item;
    const s = e.session;
    if (s !== undefined) return [s.summary, s.outputs && `**Outputs.** ${s.outputs}`].filter(Boolean).join("\n\n");
    return "";
  });
  /** Nothing more to show than the title: say so instead of repeating it. */
  const bodyless = $derived(e.span === null && (fallback.trim() === "" || fallback.trim() === e.title.trim()));

  const crumb = $derived(e.kind === "finding" ? e.topic : word(e.kind));
  /** The note beside a status: the provider's first sentence (the legend
   *  carries all of it). */
  const shortNote = $derived(shortStatusNote(labels.status_note));
  /** A follow-up written inside the finding's own lines is already in its
   *  body: listed, not drawn twice. */
  function insideEntry(sp: { path: string; line: number; end_line: number } | null): boolean {
    const own = e.span;
    return sp !== null && own !== null && sp.path === own.path && sp.line >= own.line && sp.end_line <= own.end_line;
  }
</script>

<article class="reader" aria-label="{word(e.kind)} {shownId}">
  <div class="rnav">
    {#if onClose}<button class="navbtn list" onclick={onClose} aria-label="Back to the list">‹ list</button>{/if}
    <button class="navbtn" onclick={onBack} disabled={!canBack} aria-label="Back" title="Back  [">‹</button>
    <button class="navbtn" onclick={onForward} disabled={!canForward} aria-label="Forward" title="Forward  ]">›</button>
    <span class="crumb mono">{crumb}{e.id !== "" ? ` › ${e.id}` : ""}</span>
    <span class="grow"></span>
    {#if e.file !== ""}
      <button class="link" onclick={() => onOpenFile(e.file, e.line, e.span?.end_line ?? 0)} title="{e.file}{e.line > 0 ? `:${e.line}` : ''}">
        open in file ↗
      </button>
    {/if}
  </div>

  <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
  <h2>{@html inlineMarkdown(e.title || e.id)}</h2>
  <div class="meta">
    {#if e.date !== ""}<span class="mono">{e.date.slice(0, 10)}</span>{/if}
    <span>{word(e.kind)}</span>
    {#if e.recordedBy !== null}
      {@const by = e.recordedBy}
      <span>recorded by <button class="link" onclick={() => onOpenSession(by.sid)}>{by.name} ↗</button></span>
    {/if}
    {#if e.file !== ""}<span class="mono path" title={e.file}>{e.file}{#if e.line > 0}:{e.line}{/if}</span>{/if}
  </div>

  {#if standing !== null}
    <div class="band">
      <Callout tone={standing.tone} title={standing.text.split(" ")[0]}>
        {#if stateBy.length > 0}
          {#each stateBy as b (b.ekey)}
            <button class="rowlink" onclick={() => onOpen(b.ekey)}>
              <span class="mono">{qualifiedId(idx, b)}</span>
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              <span class="btitle">{@html inlineMarkdown(b.title)}</span>
            </button>
          {/each}
        {:else if e.state?.by}
          <span class="mono">{e.state.by}</span>
        {/if}
      </Callout>
    </div>
  {/if}
  {#each amends as { a, targets: t } (a.kind + a.id)}
    <div class="band">
      <Callout tone="warn" title={a.kind}>
        {#if t.length > 0}
          {#each t as b (b.ekey)}
            <button class="rowlink" onclick={() => onOpen(b.ekey)}>
              <span class="mono">{qualifiedId(idx, b)}</span>
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              <span class="btitle">{@html inlineMarkdown(b.title)}</span>
            </button>
          {/each}
        {:else}
          <span class="mono">{a.id}</span>
        {/if}
      </Callout>
    </div>
  {/each}
  {#if namesakes.length > 0}
    <div class="band">
      <Callout tone="neutral">
        <span>
          <span class="mono">{e.id}</span> also names {namesakes.length === 1 ? "a different" : `${namesakes.length} other`}
          {word(e.kind)}{namesakes.length === 1 ? "" : "s"} — a bare “{e.id}” elsewhere is ambiguous:
        </span>
        {#each namesakes as n (n.ekey)}
          <button class="rowlink" onclick={() => onOpen(n.ekey)}>
            <span class="mono">{qualifiedId(idx, n)}</span>
            <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
            <span class="btitle">{@html inlineMarkdown(n.title)}</span>
          </button>
        {/each}
      </Callout>
    </div>
  {/if}

  {#if (e.kind === "finding" || e.kind === "decision") && e.stated.trim() !== ""}
    <section class="sec">
      <h3>Status</h3>
      <div class="status">
        <StatusMark stated={e.stated} {labels} />
        {#if shortNote}<span class="note">{shortNote}</span>{/if}
      </div>
      {#if e.finding !== undefined && e.finding.ledger.length > 0}
        <div class="ledger" title="Evidence ledger rows, as recorded">
          {#each e.finding.ledger as row, i (i)}
            <i class={row.direction}></i>
          {/each}
          <span class="note">
            evidence ledger · {e.finding.ledger.length} row{e.finding.ledger.length === 1 ? "" : "s"}
          </span>
        </div>
      {/if}
    </section>
  {/if}

  {#if e.kind === "todo" && e.todo !== undefined}
    {@const t = e.todo}
    <div class="facts">
      <Badge
        text={t.closed ? TODO_GROUP_LABEL.done : t.status || TODO_GROUP_LABEL[todoGroup(t)]}
        tone={todoGroup(t) === "blocked" ? "warn" : todoGroup(t) === "progress" ? "good" : "neutral"}
      />
      {#if t.priority}<Badge text={t.priority} tone={priorityTone(t.priority)} />{/if}
      {#if t.category}<span class="fact">{t.category}</span>{/if}
      {#if t.author}<span class="fact">by {t.author}</span>{/if}
      {#if t.file !== "" && !t.file.includes(" ")}
        <button class="link" onclick={() => onOpenFile(t.file, 0, 0)}>writeup ↗</button>
      {/if}
    </div>
  {/if}
  {#if e.kind === "session" && e.session !== undefined}
    {@const s = e.session}
    <div class="facts">
      {#if s.status}<Badge text={s.status} />{/if}
      {#if s.branch}<span class="fact mono">{s.branch}</span>{/if}
      {#if s.duration}<span class="fact">{s.duration}</span>{/if}
      {#if s.files}<span class="fact">{s.files} files</span>{/if}
    </div>
  {/if}

  {#if e.cites.length > 0 || refs.length > 0}
    <section class="sec">
      <h3>What backs it</h3>
      <div class="backs">
        {#each e.cites as c, i (i)}
          {#if validCites[c.text] !== undefined}
            {@const p = validCites[c.text]}
            <FileCard title={c.text} subtitle={c.kind} text={p} onclick={() => onOpenFile(p, 0, 0)} />
          {:else}
            <FileCard title={c.text} subtitle={c.kind} />
          {/if}
        {/each}
        {#each refs as { r, found }, i (i)}
          {#if found.length > 0}
            <FileCard title={r.id} subtitle={word(found[0].kind)} text={found[0].title} onclick={() => onOpen(found[0].ekey)} />
          {:else}
            <FileCard title={r.id} text="Not in this project's knowledge" />
          {/if}
        {/each}
      </div>
    </section>
  {/if}

  <section class="sec">
    <h3>{e.kind === "session" ? "Log" : "As written"}</h3>
    {#if bodyless}
      <p class="note">
        The provider sent only this line{#if e.file !== ""} — <button class="link" onclick={() => onOpenFile(e.file, e.line, 0)}
            >open it in its file</button
          > for the rest{/if}.
      </p>
    {:else}
    {#key e.ekey}
      <EntryBody
        span={e.span}
        {fallback}
        from={e}
        {matcher}
        {chips}
        {wsRoot}
        {wsId}
        owner={k}
        dropHeading={e.kind !== "session"}
      />
    {/key}
    {/if}
  </section>

  {#if e.finding !== undefined && e.finding.addenda.length > 0}
    <section class="sec">
      <h3>Follow-ups</h3>
      <div class="thread">
        {#each e.finding.addenda as a, i (i)}
          <div class="fu">
            <div class="fuhead">
              <Badge
                text={a.kind || a.label || "follow-up"}
                tone={a.kind === "correction" ? "bad" : a.kind === "resolution" ? "good" : "neutral"}
              />
              {#if a.date}<span class="mono muted">{a.date}</span>{/if}
              <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
              {#if a.title}<span class="futitle">{@html inlineMarkdown(a.title)}</span>{/if}
              {#if a.stated}<StatusMark stated={a.stated} {labels} short />{/if}
              {#if a.span !== null}
                {@const sp = a.span}
                <button class="link small" onclick={() => onOpenFile(sp.path, sp.line, sp.end_line)}>line {sp.line} ↗</button>
              {/if}
            </div>
            {#if !insideEntry(a.span)}
            {#key e.ekey + ":" + i}
              <EntryBody
                span={a.span}
                fallback={a.text}
                from={e}
                {matcher}
                {chips}
                {wsRoot}
                {wsId}
                owner={k}
              />
            {/key}
            {/if}
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if backlinks.length > 0 || handoffCites}
    <section class="sec">
      <h3>Referenced by</h3>
      <div class="refby">
        {#each backlinks as b (b.ekey)}
          <button class="refrow" onclick={() => onOpen(b.ekey)}>
            <span class="rk">{word(b.kind)}</span>
            <span class="mono rid">{qualifiedId(idx, b)}</span>
            <!-- eslint-disable-next-line svelte/no-at-html-tags -- sanitized in inlineMarkdown -->
            <span class="rt">{@html inlineMarkdown(b.title)}</span>
          </button>
        {/each}
        {#if handoffCites && k.left_off !== null}
          {@const lo = k.left_off}
          <button class="refrow" onclick={() => onOpenFile(lo.path, 0, 0)}>
            <span class="rk">handoff</span>
            <span class="mono rid"></span>
            <span class="rt">the latest session handoff</span>
          </button>
        {/if}
      </div>
    </section>
  {/if}
</article>

<style>
  .reader {
    display: flex;
    flex-direction: column;
    min-width: 0;
    max-width: 78ch;
  }
  .rnav {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: var(--text-xs);
    color: var(--muted);
    margin-bottom: 12px;
  }
  /* In a narrow pane only the crumb gives way; the buttons keep one line. */
  .rnav > :not(.crumb, .grow) {
    flex: none;
    white-space: nowrap;
  }
  .grow {
    flex: 1;
  }
  .navbtn {
    border: 1px solid var(--edge);
    background: var(--overlay-bg);
    color: var(--muted);
    border-radius: 6px;
    min-width: 24px;
    height: 22px;
    padding: 0 6px;
    cursor: pointer;
    font: inherit;
    line-height: 1;
  }
  .navbtn:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .navbtn:not(:disabled):hover {
    color: var(--fg);
  }
  .crumb {
    min-width: 0;
    margin-left: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mono {
    font-family: var(--mono);
    font-size: 0.92em;
  }
  .muted {
    color: var(--muted);
  }
  h2 {
    margin: 0 0 6px;
    font-size: 18px;
    line-height: 1.35;
    font-weight: 600;
    letter-spacing: -0.005em;
    overflow-wrap: anywhere;
  }
  h2 :global(code) {
    font-family: var(--mono);
    font-size: 0.86em;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    align-items: baseline;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .meta .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 100%;
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link.small {
    font-size: var(--text-xs);
  }
  .band {
    margin-top: 12px;
  }
  .rowlink {
    display: inline-flex;
    gap: 8px;
    align-items: baseline;
    border: 0;
    background: none;
    padding: 0;
    color: var(--fg);
    font: inherit;
    text-align: left;
    cursor: pointer;
    min-width: 0;
  }
  .rowlink:hover .btitle {
    text-decoration: underline;
  }
  .btitle {
    overflow-wrap: anywhere;
  }
  .sec {
    margin-top: 20px;
  }
  h3 {
    margin: 0 0 8px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .status {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    align-items: baseline;
    font-size: var(--text-md);
  }
  .note {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .ledger {
    margin-top: 6px;
    display: flex;
    align-items: center;
    gap: 3px;
  }
  .ledger i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
  }
  .ledger i.refines {
    background: linear-gradient(90deg, var(--accent) 50%, transparent 50%);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .ledger i.contradicts {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--err);
  }
  .ledger i.unknown {
    background: var(--muted);
  }
  .ledger .note {
    margin-left: 8px;
  }
  .facts {
    margin-top: 12px;
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    align-items: baseline;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .backs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .thread {
    border-left: 2px solid var(--edge);
    padding-left: 14px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .fuhead {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    align-items: baseline;
    font-size: var(--text-sm);
    margin-bottom: 4px;
  }
  .futitle {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .refby {
    display: flex;
    flex-direction: column;
  }
  .refrow {
    display: grid;
    grid-template-columns: 64px auto minmax(0, 1fr);
    gap: 10px;
    align-items: baseline;
    border: 0;
    background: none;
    padding: 5px 6px;
    margin: 0 -6px;
    border-radius: 6px;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .refrow:hover {
    background: var(--row-hover);
  }
  .rk {
    font-size: var(--text-xs);
    color: var(--muted);
  }
  .rid {
    color: var(--muted);
  }
  .rt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rt :global(code) {
    font-family: var(--mono);
    font-size: 0.9em;
  }
</style>
