# Knowledge redesign — a real UI on top of Mycelium

Status: **plan** (2026-09-28, nothing built). The maintainer's decisions
from 2026-09-28 are folded in and listed at the end. It supersedes the
Knowledge parts of
[timeline-knowledge-plugins-plan.md](timeline-knowledge-plugins-plan.md) §5
and the Knowledge rows of its §9 experience bar where they conflict (named
below). It is written to fit the plugin platform plan on
`origin/claude/brave-hamilton-afhtt3` (`docs/plugin-platform-plan.md`, not
yet on main) — see [Knowledge belongs to the plugin](#knowledge-belongs-to-the-plugin).

The ask: the Knowledge tab should be a good, intuitive UI on top of *regular*
Mycelium — it should be obvious where every status, piece of evidence,
to-do and question comes from, it should represent all of Mycelium, not a
template-shaped subset of it, and an id an agent mentions in a chat
(`F-228`) should be something you can hover and click.

Sources: a read of upstream Mycelium at `arjunrajlaboratory/mycelium`
`ab270c3` (v0.7.2); a read of `chimaera-plugin-mycelium` 0.1.3 and of
`web-ui/src/lib/knowledge/`; and a read-only pass over the real
`.living/` of a real, three-month-old research project on the
maintainer's HPC host (the one in the screenshots), where a copy
of the plugin's parser reproduced the UI's numbers exactly.

## What's wrong today

### Why almost everything reads "unrated · no evidence yet"

On that project the tab shows 293 findings, 263 of them "unrated" and 268
with "no evidence yet". Four causes stack:

1. **The host runs plugin 0.1.2.** It counts 35 `### F-NNN addendum:`
   headings as findings of their own — every one unrated and without
   evidence. 0.1.3 (already locked on main) folds them: 258 findings.
   *Fix: update the plugin on that host from Extensions — no code.*
2. **The agents stopped writing Mycelium's template around F-050.** The
   template is `**Status:** preliminary | supported | robust | contradicted`
   plus a `### Evidence Ledger` table. Template use by id range:

   | Ids | Findings | Has a Status in the vocabulary | Has a ledger |
   |---|---|---|---|
   | F-000–049 | 58 | 21 | 15 |
   | F-050–099 | 53 | 4 | 5 |
   | F-100–149 | 55 | 0 | 0 |
   | F-150–199 | 58 | 2 | 0 |
   | F-200–249 | 35 | 3 | 5 |

   Later findings are prose: `**Setup.**`, `**Result.**`,
   `**Why it matters.**`, `**Consequence.**` (period, not colon — the
   parser only knows `**Label:**`). **The evidence is there, written
   inline**: 149 findings cite data files, 138 cite scripts, 139 cite
   decisions (`D-…`), 52 cite conventions (`C-…`), 43 cite Slurm job ids,
   30 carry a "Regenerate from …" line.
3. **19 findings say `**Status:** established …`** — not a Mycelium word,
   so it reads as unknown.
4. **Status is hand-written by the agent, not derived.** Mycelium's
   template describes a rule (one ledger row = preliminary; 2+ agreeing =
   supported; 3+ across datasets or projects = robust; any contradicting
   row = contradicted), but no Mycelium code applies it — the agent types
   the word. Our legend says the status is "set by mycelium from each
   finding's evidence, never by hand" — **that is false**, and the §5
   design line "derived by mycelium from the evidence ledger, never by us"
   rests on the same misreading.

Two presentation choices then turn "not stated" into "failed": the plugin
turns a missing Status into `unknown`, and the UI draws three grey dots plus
"no evidence yet" for it — a grade against a template nobody follows.

### Where to-dos, questions and "where we left off" come from today

- **To-dos** are the rows of the table in `todo/TODO_REGISTRY.md` — and only
  those. The project also keeps 16 to-dos as `##` sections *below* the table
  (`## #50 — … ✅ DONE`, `## T-<Name>`, …), including all six newest; the
  parser stops at the first heading, so the UI never sees them. The
  header's "44 to do" counts **every** row, complete and wont-do included;
  `done 2026-09-23 (…)` isn't recognised as closed. 33 of 44 item cells are
  whole paragraphs (the squashed layout), and 33 File cells are free text
  the UI turns into broken `todo/…` links.
- **Open questions** are only the bullets under findings' `### Open
  Questions` — 22, all from July-era files, several since answered. The
  project's live questions are prose: 26 findings say "Put to the user",
  "user decision" or "not yet decided"; the handoff has "PARKED USER
  DECISION" sections. None of them show.
- **Where we left off** shows `.mycelium/last-session.md` from 12:09 — the
  Stop hook's fallback stub, mostly filtered out as placeholders. The real
  18:29 handoff in `.mycelium/run/claude/<session>/last-session.md` is
  never read (the plugin stops at the shared file), and its numbered
  headings (`## 1. Goal`, `## 2. Done`, `## 3. In flight`, `## 4. Next`)
  wouldn't match anyway.
- **Decisions**: 126 of 169 show undated (the date is `(YYYY-MM-DD)` at the
  end of the heading or a `**Date**:` field), so July sorts first; 94 show
  empty cards (`**Decision.**` / `**Why.**` style); tags after a `·` are
  lost; `## D-108`/`## D-109` aren't entries; `⛔ SUPERSEDED BY D-nnn` is
  just title text.

### Presentation bugs

- To-do column: `.open.two` splits the section in half down to 760px, the
  priority and status columns are fixed, inline `<code>` never wraps — a
  364px code span spills across "open" into the Questions column at every
  two-column width, and `align-items: center` floats "medium" beside a tall
  item. The same `.open` rule also matches an expanded `.lcard.open`
  learning card (it becomes a 28px-gap grid).
- `cell_text` trims backticks off both ends of a table cell, inverting code
  spans ("Fix `a` and `b`" → "Fix `a` and `b" with stray ticks).
- The `**Claim:**` statement is dropped — only the heading survives.
- No open-at-line (the snapshot has `line`; the file opens at the top), no
  link from `recorded_by`, ledger `run` ids or the handoff's session to a
  chat, no links between entries, Timeline and dashboard rows open the tab
  but not the entry, ids in chats are plain text, and 772 entries render
  at once.
- 15 finding ids are reused across topic files for unrelated findings
  (one id three times; another names two unrelated findings in two topic
  files), so a bare id anywhere is ambiguous. Six decision ids are
  duplicated too.

## What Mycelium is (the model the UI must respect)

Condensed from upstream v0.7.2; the full read is in the session that wrote
this plan.

- **Agents write everything** in `.living/`, nudged by hooks
  (PostToolUse after real script runs; Stop blocks when work happened but
  `.living/` wasn't touched). Humans review, answer, and correct. The view
  stays read-only; the user's correction path is the file or an agent.
- **Kinds**: findings (`.living/findings/<topic>.md`, `## F-NNN: claim`,
  Status, Claim, Implications, Tags, Evidence Ledger, Open Questions);
  decisions and learnings (`### [date] title` in one file each — their
  `D-n`/`L-n` are *positional*, renumbered by inserts and migrations);
  conventions (`.living/conventions.md`, free-form `##` sections — the
  reference project numbers them `## C-N`); to-dos (`todo/TODO_REGISTRY.md`
  + item files, at the repo root); session logs (`.living/log/*.md` +
  `LOG_REGISTRY.md`) and data lineage (`.living/log/data-lineage/<sid>.json`:
  scripts, inputs, outputs, sha256s per action); the handoff
  (`.mycelium/last-session.md` and per-session
  `.mycelium/run/<host>/<sid>/last-session.md`, gitignored — local to one
  machine); manifests (`data/DATA_MANIFEST.md`,
  `analysis/ANALYSIS_MANIFEST.md`, …); `MYCELIUM.md` and the managed blocks
  in `AGENTS.md`/`CLAUDE.md`.
- **Status belongs to Mycelium** — in practice, to the agent that writes
  the finding. Chimaera reports it; it never computes, maps or corrects it.
- **Links are conventions, not fields.** The ledger's Run/Session cell is
  meant to hold a session id (the key of the log and lineage) — nothing
  enforces it. Upstream has no addenda, no supersession field (the
  project's `MYCELIUM.md` defines `⛔ SUPERSEDED BY` / `⚠️ SUSPECT`
  markers), no questions file, no finding↔decision link. In practice agents
  cross-reference by writing ids in prose — which is exactly the graph a UI
  can show.
- **Real projects drift from the templates** (the reference project above),
  and upstream has legacy shapes a reader must tolerate (`##` entries before
  0.7.0, `.claude/last-session.md`, numbered and explicit-id headings).

## Principles

1. **Parse the envelope, show the body.** The plugin extracts only what
   the UI navigates by — kind, id, title, date, the status as written,
   tags, state markers, references, and the entry's span in its file. The
   reader renders the entry's own markdown, as written, with our markdown
   renderer (math, callouts, tables). A prose finding then reads exactly as
   the agent wrote it; the template is no longer a precondition for a good
   view.
2. **Show what was recorded — never rate.** The status shown is the word
   the agent wrote, as written ("supported", "established by independent
   realignment"), with Mycelium's ladder glyph only when it is one of
   Mycelium's four words. Neither core nor our plugin computes, maps or
   second-guesses a status. No status means nothing is shown for it — no
   grey ladder, no "unrated" — and the *What backs it* line carries the
   entry instead. Where the entry has an evidence ledger, its rows are drawn
   as written (one mark per row). The legend says who sets the status: the
   agent that recorded the finding.
3. **Everything is linked — including in chats.** `F-`/`D-`/`C-`/`L-`/`T-`
   ids become chips wherever they appear (see
   [Ids everywhere](#ids-everywhere--hover-and-click-from-a-chat)): hover
   previews the entry, click opens it in Knowledge. Every entry lists what
   references it. Paths open the file at the line; a `recorded_by` or
   session id opens the chat turn that wrote it. Colliding ids are
   qualified by topic (`F-177 · topic-a`).
4. **State over status.** Corrected, superseded, retracted and resolved
   come from what agents actually write (`⛔ SUPERSEDED BY D-157`,
   `RETRACTED`, `### F-073 CORRECTION`, `**This CORRECTS F-171.**`,
   `RESOLVED`). A superseded entry dims and points at its replacement; a
   correction threads under its finding. **Bad news leads** — corrections
   and supersessions head *What changed*.
5. **The first screen answers "where are we, what needs me, what
   changed".** Everything else is browsing.
6. **Browse is a list and a reader, not an accordion.** One dense line per
   entry; the selected entry reads in a pane beside the list (full width on
   narrow panes). Keyboard: `j`/`k`, `Enter`, `/`, `[`/`]` back/forward.
7. **One search, everywhere** — every kind, snippet under each hit, an id
   jumps straight to the entry.
8. **Health, quietly; action through agents.** Factual inconsistencies in
   the knowledge (colliding ids, to-dos another entry says are done,
   questions a later entry answers, a clobbered handoff) collect in one
   *Tidy up* list; each offers **Ask an agent**, which drafts the request
   into a chat for the user to send. Knowledge itself never writes
   `.living/`, and Tidy up never judges a status.
9. **All of Mycelium.** Conventions (cited by 52 findings in the reference
   project) and Sessions (the log registry + handoffs, linked to chats)
   join the view. Manifests and lineage come later as evidence behind
   entries, not as their own sections.
10. **The plugin owns the words.** Section names, the legend, the status
    vocabulary and its ladder, the source chip and the attach copy come
    from the plugin; core keeps none of Mycelium's names. Until the
    snapshot carries all of them, the leftovers sit in one adapter module.
    Components take semantic props matching the platform plan's `ui/1`
    (list rows with badges, key–value, callout, empty-with-one-action, file
    cards, tones neutral / accent / good / warn / bad).

## Knowledge belongs to the plugin

**Knowledge exists only while a knowledge plugin is on in the workspace.**
No Mycelium, no Knowledge: no tab, no nav entry, no Knowledge card on the
dashboard, no id chips in chats. The ways in stay where they are — the
dashboard's attach line and Extensions. *Guidance & memory* (AGENTS.md,
CLAUDE.md, claude memory), which today is the whole Knowledge tab when no
plugin is on, isn't Mycelium's: it moves to the dashboard as one row of
links (and `MYCELIUM.md` joins that row while Mycelium is on).

**The plugin says what it is and what it knows; core draws it.** The
plugin declares a knowledge surface (today: `provides.knowledge`), owns its
content, its words (principle 10) and the id shapes it answers for; core
owns the renderer for that kind of surface — the list, the reader, the
chips and previews — the way core owns the PDF viewer a file opens in. A
plugin-drawn screen (the platform plan's generic `ui/1` node trees) can't
do what this needs: previews that appear inside a *chat*, a reader with
history, keyboard navigation, a virtualized list over 800 entries.

This reconciles the two earlier drafts. The 2026-09-28 plugin-views
direction ("Knowledge becomes Mycelium's declarative view, drawn by core")
and the platform plan's §4 ("Knowledge is a data surface core draws,
`knowledge/1`") agree that core draws and the plugin supplies; this plan
takes the platform plan's name for the contract and the plugin-views
draft's ownership: the surface, its tab and its words belong to the
plugin, and switching the plugin off removes all of it. Turn credit stays
in core, keyed by each entry's `span.path` instead of Mycelium's file names.

Fit with the platform plan's phases: nothing in P6 (trust for existing
plugins) touches Knowledge; P7's `ui/1` renderer can reuse this plan's row,
badge, callout and file-card components; P10 ("core stops naming Mycelium")
shrinks to deleting the adapter. The field table below goes to that plan
as the `knowledge/1` draft; the id shapes become a generic contribution
point (`provides.references` or a `references` field on the surface).

## Ids everywhere — hover and click from a chat

When an agent writes "see F-228" in a chat, the id is a chip.

- **Which ids.** The plugin declares the id shapes it answers for (for
  Mycelium: `F-\d+`, `D-\d+`, `C-\d+`, `T-<Name>`, to-do numbers), carried on
  the snapshot. A token in chat text becomes a chip **only if the
  workspace's current snapshot has that id** — so "COVID-19" or a stray
  "D-1" in prose that means something else stays text.
- **Where.** Agent prose, code spans and tool output in chat mode, the
  user's own messages, markdown file previews, Timeline rows and the
  dashboard. The detection sits beside the path-link layer
  (`shared/fileRef.ts` proposes candidates, `chat/paths.ts` resolves them)
  but resolves against the snapshot in the client — no daemon round trip.
  TUI terminals come later, if at all.
- **Hover** shows a preview card straight from the snapshot: kind, id,
  title, date, the status as written, state (*corrected by F-177*), and the
  first lines of the body (fetched via `span` and cached per file mtime).
- **Click** opens the entry in Knowledge — beside the chat when there's
  room (the pane layout's usual "open beside" rules), otherwise in the
  current pane — with the reader on that entry and back/forward history.
  An id that names several entries (collisions) previews all of them with
  their topics; the click picks one.
- **Off means off.** Plugin off → no chips, no previews.

## Information architecture

| Section | The question | Source |
|---|---|---|
| **Overview** | "Where are we, what needs me, what changed?" | the four cards below |
| **Findings** | "What do we know — and on what?" | `.living/findings/*.md` |
| **Decisions** | "Why did we do X — is it still in force?" | `.living/decisions.md` |
| **Watch out for** | "What will bite me?" | `.living/learnings.md` |
| **Conventions** | "What rules do the agents follow here?" | `.living/conventions.md` (+ generated conventions) |
| **To do** | "What's left, and what's blocked?" | `todo/TODO_REGISTRY.md` table **and** sections, item files |
| **Sessions** | "What did each session do?" | `.living/log/LOG_REGISTRY.md`, handoffs, the Timeline |
| *Tidy up* (muted, with a count) | "Is the knowledge itself consistent?" | derived by the plugin |

Section names are the plugin's (principle 10); these are Mycelium's.
Counts in the nav are live counts (open to-dos, not rows). Empty sections
don't render (§9's rule stands).

### Overview

Four cards, top to bottom; each collapses to one honest line when empty.

1. **Where we left off** — the *newest* of Mycelium's handoffs
   (`.mycelium/last-session.md` and every
   `.mycelium/run/<host>/<sid>/last-session.md`, by mtime). Its own
   headings, rendered (Goal · Done · In flight · Next), with its age, the
   session that wrote it (→ chat), and older handoffs behind a disclosure.
   Project-named handoff files are not read (decision 4).
2. **Waiting on you** — decisions put to the user: findings or handoff
   sections that say so ("Put to the user", "PARKED USER DECISION", "Not yet
   decided — options, for the user"), blocked to-dos, and open questions
   that no later entry answers. Each row leads with the question, then the
   source chip and age. This is the card a scientist opens the tab for.
3. **What changed** — the last 7 days, by day: new and edited findings,
   decisions and learnings, with who recorded them (Timeline credit → the
   chat turn). Corrections, supersessions and retractions first, in the
   err/warn tone *with* the word.
4. **Open work** — in progress and blocked, then critical and high open
   to-dos; "23 more" opens To do.

### Browse: list + reader

The list is one line per entry: mono id · title (the heading, one line,
ellipsized) · state chips (`corrects F-171`, `superseded → D-157`,
`resolved`) · date · the status word when one was written · a small
*backs* mark (the ledger's rows when there is a ledger; otherwise a count
of cited files, scripts and jobs). Findings group by topic (topic
description as the group's subtitle); decisions and learnings are newest
first; filter chips above the list (*Changed this week*, *Corrected*, the
status words that occur, a tag).

The reader shows, in order:

- breadcrumb (`topic-slug › F-228`), the title, and one meta line:
  date · recorded by *Main Agent* (→ chat) · **open at line 4152**;
- **Status** — only when the agent wrote one: the word as written (with
  the ladder glyph for Mycelium's four words) and, when the entry has an
  evidence ledger, its rows (one mark per row: filled supports, half
  refines, ring contradicts), labelled "stated by the agent";
- **What backs it** — chips for everything the entry cites: data files,
  scripts, jobs, commits, figures (open at the path), and the entries it
  references;
- the **body**, rendered as written (ids and paths inside it linked);
- **Follow-ups** — addenda, corrections and resolutions, threaded, newest
  last, each with its own date;
- **Referenced by** — every decision, finding, to-do, convention and handoff
  that cites it.

Hovering an id chip anywhere shows the same preview card as in chats; the
reader keeps a back/forward history so following a chain
(F-171 → F-177 → D-157) is cheap.

### To do

Grouped by status — In progress · Blocked · Open (critical → low) · Done
(collapsed, with a count). Each row: priority mark, the item's *first
sentence* as its title, the rest clamped to two lines, its references as
chips, date. The reader shows the full item plus its writeup file when it
has one. Rows from the registry table and to-do sections are one list; an
item another entry marks done (a finding that reports the step COMPLETE
while its to-do is still open) goes to *Tidy up*, never silently closed.

### Tidy up

One list of factual inconsistencies, each row a sentence and an **Ask an
agent** button that drafts a precise request into the workspace's chat (the
user reviews and sends it): *"Finding ids F-079, F-126–129, F-175–178, …
are used twice in different topic files — renumber the later ones and
update references."* / *"The to-do '…' is open, but F-013 reports it
complete."* / *"The Stop hook replaced a hand-written handoff with its
fallback stub."* Never a status judgment.

### Narrow panes

Below ~820px the reader replaces the list (back returns to the list, at
the same scroll position).

## Data: the snapshot additions (a draft `knowledge/1`)

All additive — every current field keeps its meaning and today's fixtures
stay byte-identical where their inputs don't use the new shapes.

| Field | On | Meaning |
|---|---|---|
| `span {path, line, end}` | every entry, addendum, to-do, handoff | where the entry's markdown lives; the reader fetches the file (`/fs/file`) and renders the slice — bodies never ride the snapshot (the reference project's `.living/` text is ~1.8 MB) |
| `stated` | finding, decision | the status exactly as written ("established by independent realignment"); `status` stays the matched Mycelium word or `unknown` |
| `refs[] {kind, id, topic?}` | every entry | ids this entry cites (F/D/C/L/T/to-do), in order |
| `cites[] {kind, text}` | every entry | data files, scripts, jobs, commits, figures named in the body |
| `state {kind, by?}` | every entry | `superseded` / `retracted` / `corrected` / `resolved` / `suspect`, with the id that did it — only from markers the agent wrote |
| `addenda[].kind` | finding | `addendum` / `correction` / `resolution` / `update` |
| `key` | finding | `topic/F-NNN` — unique even when ids collide; `id` stays the bare id |
| `conventions[]` | snapshot | `{id, title, span, refs, cites}` |
| `sessions[]` | snapshot | `{id, date, summary, span, branch, status}` from `LOG_REGISTRY.md` |
| `asks[]` | snapshot | "waiting on you" items: `{text, span, source}` from findings, handoffs, decisions |
| `todos[]` +`closed`, `refs`, `span`, `title` | to-do | closed by the plugin's rules; `title` = the first sentence |
| `left_off.sources[]` | handoff | every handoff found, newest first (`{path, written_ms, session_id?, host?}`) |
| `tidy[]` | snapshot | `{kind, text, refs, ask}` — the Tidy up rows and the request each drafts |
| `id_shapes[] {pattern, kind}` | snapshot | the id shapes the plugin answers for — what chats may turn into chips |
| `labels` | snapshot | the plugin's words: section names, the status vocabulary with its ladder rank, the legend text, the source chip |

Backlinks are computed in the client from `refs` (no extra wire). Deep
links (Timeline, dashboard, chat chips) address an entry by `key`.

### Wire spec — what plugin 0.2.0 adds

Exact shapes, additive to 0.1.3. Omitted means absent-when-empty
(`skip_serializing_if`), so a 0.1.3-shaped input serializes the fields
0.1.3 did plus only what it newly has. Line numbers are 1-based and
inclusive; paths workspace-relative.

- **`Span`** = `{path, line, end}` — the entry's heading line to its last
  line (trailing blank lines excluded). For a to-do table row, `line ==
  end` (the row).
- **`Ref`** = `{kind, id}` — `kind` ∈ `finding | decision | convention |
  learning | todo`; `id` as written (`F-171`, `D-152`, `C-12`, `L-40`,
  `T-DAChromatin`). An entry never lists its own id; order of first
  appearance; ≤ 50.
- **`Cite`** = `{kind, text}` — `kind` ∈ `script` (`.py .R .r .sh .ipynb
  .smk .nf .jl .pl .rs .ts .js .sql`), `data` (`.h5ad .h5 .tsv .csv .txt.gz
  .parquet .yaml .yml .json .bed .bam .vcf .gz .rds .loom .zarr .mtx .npz
  .pkl .xlsx`), `figure` (`.png .pdf .svg .jpg .jpeg`), `doc` (`.md`),
  `path` (any other `/`-containing path-like token in backticks), `job`
  (a Slurm job id: digits after `job`/`jobs`/`JobID`, 5–10 digits),
  `commit` (7–40 hex with a digit and a letter, after
  `commit`/`sha`/`SHA`/`@`). Found in code spans and plain text;
  deduplicated; ≤ 30.
- **`State`** = `{kind, by?}` — `kind` ∈ `superseded | corrected | retracted
  | suspect | resolved`; `by` the id that did it when known. Only from what
  the text says: `⛔ SUPERSEDED BY D-125` / `SUPERSEDED by F-177` (in the
  heading or the entry's first lines) → `superseded`; `RETRACTED` in the
  heading or a Status → `retracted`; `⚠️ SUSPECT` → `suspect`; the newest
  follow-up of kind `resolution` → `resolved`; another entry's `amends`
  of kind `corrects` → `corrected` (inverse, below). An explicit marker on
  the entry wins over an inverse.
- **`Amend`** = `{kind, id}` — `kind` ∈ `corrects | supersedes | retracts`,
  from `This CORRECTS F-171`, `corrects F-171`, `supersedes D-121`,
  `retracts F-037` (any case, `**`/`.` tolerated). The plugin applies the
  inverse `State` to the target (`corrects` → `corrected`, `supersedes` →
  `superseded`, `retracts` → `retracted`, `by` = this entry's id). A target
  id that names several findings resolves to the one in the same topic,
  else to none.

Per kind:

| On | Adds |
|---|---|
| finding | `key` (`<slug>/<id>`, `~2`, `~3` for a repeat in one file), `stated` (the Status field's text, markdown stripped, verbatim otherwise; `""` when none), `date` (a trailing `(YYYY-MM-DD)` in the heading, else `**Date**:`, else `""`), `span`, `refs`, `cites`, `state?`, `amends?` |
| addendum | `kind` (`addendum | correction | resolution | update`), `date`, `stated`, `span` |
| decision | `id` (explicit in the heading — `D-157`, `D1` → `D-1` — else Mycelium's positional `D-<n>` only when no entry in the file has an explicit id, else `""`), `stated`, `span`, `refs`, `cites`, `state?`, `amends?` |
| learning | `id` (same rule, `L-`), `span`, `refs`, `cites` |
| todo | `key` (`todo/<id>` or `todo/r<row>` for an id-less table row, `todo/s<n>` for an id-less section), `id` (`#50`, `T-DAChromatin`, or `""`), `title` (the item's first sentence, markdown stripped, ≤ 160 chars), `closed` (bool), `source` (`table | section`), `span`, `refs` |
| question | `key` (the raising finding's `key`) |
| left_off | `span` (the chosen handoff, whole file), `sources` (every handoff found, newest first: `{path, written_ms, session_id?, host?}`) |
| topic | `date` (frontmatter `last_updated`, else `""`) |
| snapshot | `conventions`, `sessions`, `asks`, `tidy`, `id_shapes`, `labels`; `counts` gains `todos` (open), `questions`, `conventions`, `sessions` |

- **`conventions[]`** = `{key, id, title, status, span, refs, cites}` from
  `.living/conventions.md` `##` sections (`id` = a leading `C-N`, `title`
  the rest) and `.living/generated-conventions/*/convention.md`
  (frontmatter `id`, `title`, `status`); file order, ≤ 400.
- **`sessions[]`** = `{id, date, branch, duration, files, summary, outputs,
  status, log}` from `.living/log/LOG_REGISTRY.md` rows (`log` = the Log
  cell's link, workspace-relative); newest first, ≤ 400.
- **`asks[]`** = `{text, date, source: {kind, id, key}, span}` — sentences
  that put something to the user ("put to the user", "for the user",
  "user decision", "user's call", "PARKED USER DECISION", "not yet
  decided", "awaiting the user") from the chosen handoff (any date) and
  from findings and decisions dated within 14 days of the newest dated
  entry; `text` is the sentence (≤ 300 chars, markdown stripped); newest
  first, ≤ 20.
- **`tidy[]`** = `{kind, text, refs, ask}` — factual inconsistencies only,
  never a status judgment: `duplicate-id` (a finding or explicit decision
  id naming more than one entry — one row per kind listing them), `off-index`
  (to-dos kept as sections outside the registry table; entries Mycelium's
  index can't see, like undated `## D-108`), `handoff-stub` (the shared
  handoff is the Stop hook's fallback while a hand-written run handoff
  exists), `duplicate-todo` (two open to-dos with the same title). `ask` is
  the full request an agent would need, naming files and ids.
- **`id_shapes[]`** = `{kind, pattern}` — the id shapes this snapshot
  answers for, as JavaScript-compatible regex sources without anchors:
  `F-\d{1,4}`, `D-\d{1,4}`, `C-\d{1,3}`, `L-\d{1,4}`, `T-[A-Za-z][A-Za-z0-9]*`.
- **`labels`** = `{source, sections: {left_off, asks, changed, open_work,
  findings, decisions, learnings, conventions, todos, sessions, tidy},
  status_words: [{word, rank, tone}], status_note}` — the plugin's words
  (principle 10); `rank` 1–3 for the ladder, `0` for none; `tone` ∈
  `neutral | good | warn | bad`.

The existing fields keep their meaning; values change only where parsing
improves (more to-dos, real decision dates, the newest handoff).
`status` stays the Mycelium word the Status starts with, else `unknown` —
the Timeline's status moves read it; `stated` is what the UI shows.

## Plugin work — `chimaera-plugin-mycelium` 0.2.0

Parsing that matches what agents write, all with fixtures cut from real
shapes (anonymized):

- `**Label.**` period-style fields and `·`-joined inline fields
  (`**Date**: … · **Status**: … · **Tags**: …`); multi-line `**Tags**`;
  dates from `**Date**:` and a trailing `(YYYY-MM-DD)`; keep `**Claim:**`.
- Headings: `## F-NNN — title (date)`, `### D-157 — title (date)`, dated
  and undated `##` legacy entries with a warning (not a silent drop),
  `### F-020/F-021 addendum:`.
- Follow-up headings as addenda with a kind: `addendum`, `CORRECTION`,
  `RESOLVED`/`RESOLUTION`, `reprocess round N`.
- State markers (`⛔ SUPERSEDED BY`, `RETRACTED`, `⚠️ SUSPECT`,
  `This CORRECTS F-…`), references and citations (paths by extension, Slurm
  job ids, 7–40-hex SHAs, `Regenerate from`).
- Status: `stated` verbatim; no default, no mapping, no rule. Ledger
  directions kept as written.
- To-dos: the registry table **and** the `##` to-do sections under it;
  closed when the status *starts with* a closed word (`done 2026-09-23 …`);
  File cells as refs + at most one real link; stop trimming backticks.
- Handoff: newest by mtime across the shared file and every run dir;
  numbered headings (`## 1. Goal`) map onto the five slots, unknown
  sections kept in order.
- Conventions (`## C-N` / `##` sections), sessions (`LOG_REGISTRY.md`),
  asks, tidy, `id_shapes`, `labels`.
- Keys: `topic/F-NNN`; a collision is a `tidy` row, not a warning line
  above the content.

The 4 MiB snapshot cap stays; the 400-per-kind cap should rise to 1,000
with the snapshot cap as the real bound (the reference project is already
at 310 learnings after three months), measured on the fixture.

## Core work

- **Knowledge only with a provider**: the tab, its nav entry and the
  dashboard card render only while a knowledge plugin is on; *Guidance &
  memory* moves to the dashboard.
- **UI** (`web-ui/src/lib/knowledge/`): the new IA and screens above; an
  entry reader that fetches and renders `span` slices (cached per file
  mtime); an `IdChip` + preview card shared with chats; a virtualized
  list; search over the envelope and loaded bodies; the dashboard's
  `WhereThingsStand` fed by the same Overview model (never printing a raw
  `unknown`).
- **Chat id chips** (`web-ui/src/lib/chat/`, `shared/`): candidates from
  the snapshot's `id_shapes`, confirmed against the snapshot, rendered as
  chips in agent prose, code spans, tool output and user messages; hover
  preview; click opens Knowledge at the entry.
- **Open at line**: `revealLines` (`previews/cm.ts`) already exists — wire
  the file open to take a line.
- **Links to chats**: `recorded_by` and session ids resolve through the
  Timeline to a chat and turn; a handoff's `session_id` too. `left_off.by`
  (rendered today, never sent) gets filled or removed.
- **Server** (`knowledge.rs`): pass the new fields through; `ids_of`
  switches to the snapshot's per-key `span.path`, which also fixes turn
  credit for colliding ids.

## Phases

0. **Now, no code** — update the Mycelium plugin on the HPC host to 0.1.3
   (Extensions): 35 phantom findings disappear.
1. **Honest data, current UI** (small): plugin 0.1.4 with the parsing
   fixes (fields, dates, follow-ups, to-do sections, closed detection,
   handoff newest-wins, backticks, `**Claim:**`, `stated` verbatim); UI:
   the legend says the agent sets the status, nothing drawn for an
   unstated one, open-only to-do counts, the To-do layout and `.open`
   collision fixed, decisions sorted by real date. Ships in days and
   removes most of what looks broken.
2. **The redesign** — plugin 0.2.0 (`span`, `refs`, `cites`, `state`,
   keys, conventions, sessions, asks, `id_shapes`, `labels`); core:
   Knowledge only with a provider, Overview, list + reader, id chips in
   Knowledge **and chats** with previews and history, search,
   open-at-line, links to chats.
3. **Tidy up + Ask an agent**, Timeline and dashboard deep links, the
   `knowledge/1` and `references` hand-off to the platform plan.

## Verification

- Fixtures: a synthetic `.living/` that reproduces every reference-project
  shape above (template findings, prose findings, `established`, colliding
  ids, follow-up headings, both decision eras, to-do table + sections,
  three handoffs), checked into the plugin repo; the parser's counts on it
  pinned.
- Live: the isolated daemon with that fixture as a workspace; a chat whose
  fake agent mentions real, colliding and non-existent ids (chips only on
  the first two; hover and click land on the entry); the plugin switched
  off (no tab, no chips). Then, read only, the real reference project
  through the app after that host's plugin update — the header counts, the
  Overview's four cards and ten spot-checked entries compared against the
  files.
- The UI at 1400 / 960 / 820 / 600 px, light and dark; keyboard-only pass.

## Decisions (2026-09-28)

1. **Knowledge belongs to the plugin.** It appears only while Mycelium is
   on; core draws it; ids an agent mentions in a chat can be hovered and
   clicked through to the entry.
2. **No rating by Chimaera.** Status is Mycelium's — shown as the agent
   wrote it, never computed or mapped, by core or by our plugin.
3. **Ask an agent** from *Tidy up*: yes.
4. **Handoffs**: Mycelium's own handoff files only, newest wins, older ones
   behind a disclosure. Project-named files (the reference project's
   `.claude/RESUME_STATE.md`) are not read: newest-wins across the run
   folders already surfaces the hand-written handoff that file was created
   to protect. If one is still missed, a plugin setting can name extra
   handoff files later.
5. **No upstream proposals** to Mycelium; everything here works with
   Mycelium as it is.
