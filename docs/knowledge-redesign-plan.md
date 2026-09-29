# Knowledge redesign — a real UI on top of Mycelium

Status: **plan** (2026-09-28, nothing built). Items marked **[decide]** are
the maintainer's call. It supersedes the Knowledge parts of
[timeline-knowledge-plugins-plan.md](timeline-knowledge-plugins-plan.md) §5
and the Knowledge rows of its §9 experience bar where they conflict (named
below). It is written to fit the plugin platform plan on
`origin/claude/brave-hamilton-afhtt3` (`docs/plugin-platform-plan.md`, not
yet on main) — see [Fit with the plugin platform](#fit-with-the-plugin-platform).

The ask: the Knowledge tab should be a good, intuitive UI on top of *regular*
Mycelium — it should be obvious where every rating, piece of evidence,
to-do and question comes from, and it should represent all of Mycelium,
not a template-shaped subset of it.

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

1. **The host runs plugin 0.1.2.** It counts 35 `### F-NNN addendum:` headings as
   findings of their own — every one unrated and without evidence. 0.1.3
   (already locked on main) folds them: 258 findings. *Fix: update the
   plugin on that host from Extensions — no code.*
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
4. **Status is hand-written by the agent, not derived.** Mycelium states a
   rule (one ledger row = preliminary; 2+ agreeing = supported; 3+ across
   datasets or projects = robust; any contradicting row = contradicted), but
   no Mycelium code applies it — the agent types the word. Of the 30
   "supported" findings, 10 have no ledger and 12 have one row (the rule
   says preliminary). Our legend says the rating is "set by mycelium from
   each finding's evidence, never by hand" — **that is false**, and the §5
   design line "derived by mycelium from the evidence ledger, never by us"
   rests on the same misreading.

Two presentation choices then turn "not stated" into "failed": the plugin
defaults a missing Status to `unknown` (upstream's own registry builder
defaults it to `preliminary`), and the UI draws three grey dots plus "no
evidence yet" for it — a grade against a template nobody follows.

### Where to-dos, questions and "where we left off" come from today

- **To-dos** are the rows of the table in `todo/TODO_REGISTRY.md` — and only
  those. The project also keeps 16 to-dos as `##` sections *below* the table
  (`## #50 — … ✅ DONE`, `## T-<Name>`, …),
  including all six newest; the parser stops at the first heading, so the
  UI never sees them. The header's "44 to do" counts **every** row,
  complete and wont-do included; `done 2026-09-23 (…)` isn't recognised as
  closed. 33 of 44 item cells are whole paragraphs (the squashed layout),
  and 33 File cells are free text the UI turns into broken `todo/…` links.
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
  wouldn't match anyway. `.claude/RESUME_STATE.md`, which the project's
  `MYCELIUM.md` names as the authoritative hand-off, isn't read at all.
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
  but not the entry, and 772 entries render at once.
- 15 finding ids are reused across topic files for unrelated findings
  (one id three times; another names two unrelated findings in two topic
  files), so a bare id anywhere is ambiguous. Six decision ids are duplicated too.

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
  conventions (`.living/conventions.md`, free-form `##` sections — the reference
  project numbers them `## C-N`); to-dos (`todo/TODO_REGISTRY.md` + item files, at
  the repo root); session logs (`.living/log/*.md` + `LOG_REGISTRY.md`) and
  data lineage (`.living/log/data-lineage/<sid>.json`: scripts, inputs,
  outputs, sha256s per action); the handoff (`.mycelium/last-session.md`,
  gitignored — local to one machine); manifests (`data/DATA_MANIFEST.md`,
  `analysis/ANALYSIS_MANIFEST.md`, …); `MYCELIUM.md` and the managed blocks
  in `AGENTS.md`/`CLAUDE.md`.
- **Links are conventions, not fields.** The ledger's Run/Session cell is
  meant to hold a session id (the key of the log and lineage) — nothing
  enforces it. Upstream has no addenda, no supersession field (the
  project's `MYCELIUM.md` defines `⛔ SUPERSEDED BY` / `⚠️ SUSPECT`
  markers), no questions file, no finding↔decision link. In practice agents
  cross-reference by writing ids in prose — which is exactly the graph a UI
  can show.
- **Real projects drift from the templates** (the reference project above), and upstream
  has legacy shapes a reader must tolerate (`##` entries before 0.7.0,
  `.claude/last-session.md`, numbered and explicit-id headings).

## Principles

1. **Parse the envelope, show the body.** The plugin extracts only what
   the UI navigates by — kind, id, title, date, stated status, tags,
   state markers, references, and the entry's span in its file. The reader
   renders the entry's own markdown, as written, with our markdown renderer
   (math, callouts, tables). A prose finding then reads exactly as the
   agent wrote it; the template is no longer a precondition for a good
   view.
2. **Say where every signal comes from; never fake a rating.** Show the
   status the agent *stated*, in its own word ("supported",
   "established"), marked as stated. Where a ledger exists, show what
   Mycelium's rule gives, and flag it only when it disagrees. No stated
   status means no ladder — a quiet "not rated" and the *What backs it*
   line instead. The legend explains both, truthfully.
3. **Everything is linked.** `F-`/`D-`/`C-`/`L-`/`T-` ids in any text become
   chips: hover previews the entry, click opens it in the reader (with
   back/forward). Every entry lists what references it. Paths open the file
   at the line; a `recorded_by` or session id opens the chat turn that
   wrote it. Colliding ids are qualified by topic (`F-177 · topic-a`).
4. **State over status.** Corrected, superseded, retracted and resolved
   come from what agents actually write (`⛔ SUPERSEDED BY D-157`,
   `RETRACTED`, `### F-073 CORRECTION`, `**This CORRECTS F-171.**`,
   `RESOLVED`). A superseded entry dims and points at its replacement; a
   correction threads under its finding. **Bad news leads** — corrections
   and contradictions head *What changed*.
5. **The first screen answers "where are we, what needs me, what
   changed".** Everything else is browsing.
6. **Browse is a list and a reader, not an accordion.** One dense line per
   entry; the selected entry reads in a pane beside the list (full width on
   narrow panes). Keyboard: `j`/`k`, `Enter`, `/`, `[`/`]` back/forward.
7. **One search, everywhere** — every kind, snippet under each hit, an id
   jumps straight to the entry.
8. **Health, quietly; action through agents.** Things that look off
   (colliding ids, a stated status the ledger doesn't support, to-dos
   another entry says are done, answered questions, a clobbered handoff)
   collect in one *Tidy up* list; each offers **Ask an agent**, which drafts
   the request into a chat. Knowledge itself never writes `.living/`.
9. **All of Mycelium.** Conventions (cited by 52 findings in the reference project) and
   Sessions (the log registry + handoffs, linked to chats) join the view.
   Manifests and lineage come later as evidence behind entries, not as
   their own sections.
10. **Provider words live in one adapter.** "mycelium", `.living/` and the
    ladder vocabulary sit in one module; components take semantic props
    matching the platform plan's `ui/1` (list rows with badges, key–value,
    callout, empty-with-one-action, file cards, tones neutral / accent /
    good / warn / bad). The platform plan's P10 then becomes a swap.

## Information architecture

| Section | The question | Source |
|---|---|---|
| **Overview** | "Where are we, what needs me, what changed?" | the four cards below |
| **Findings** | "What do we know — how sure, and on what?" | `.living/findings/*.md` |
| **Decisions** | "Why did we do X — is it still in force?" | `.living/decisions.md` |
| **Watch out for** | "What will bite me?" | `.living/learnings.md` |
| **Conventions** | "What rules do the agents follow here?" | `.living/conventions.md` (+ generated conventions) |
| **To do** | "What's left, and what's blocked?" | `todo/TODO_REGISTRY.md` table **and** sections, item files |
| **Sessions** | "What did each session do?" | `.living/log/LOG_REGISTRY.md`, handoffs, the Timeline |
| **Guidance & memory** | "What are the agents told?" | `MYCELIUM.md`, `AGENTS.md`, `CLAUDE.md`, claude memory |
| *Tidy up* (muted, with a count) | "Is the knowledge itself healthy?" | derived |

Counts in the nav are live counts (open to-dos, not rows). Empty sections
don't render (§9's rule stands).

### Overview

Four cards, top to bottom; each collapses to one honest line when empty.

1. **Where we left off** — the *newest* handoff among
   `.mycelium/last-session.md`, `.mycelium/run/*/*/last-session.md` and
   (**[decide]**) handoff files `MYCELIUM.md` points at. Its own headings,
   rendered (Goal · Done · In flight · Next), with its age, the session that
   wrote it (→ chat), and "2 older handoffs" behind a disclosure.
2. **Waiting on you** — decisions put to the user: findings or handoff
   sections that say so ("Put to the user", "PARKED USER DECISION", "Not yet
   decided — options, for the user"), blocked to-dos, and open questions
   that no later entry answers. Each row leads with the question, then the
   source chip and age. This is the card a scientist opens the tab for.
3. **What changed** — the last 7 days, by day: new and edited findings,
   decisions and learnings, with who recorded them (Timeline credit → the
   chat turn). Corrections, supersessions, retractions and contradictions
   first, in the err/warn tone *with* the word.
4. **Open work** — in progress and blocked, then critical and high open
   to-dos; "23 more" opens To do.

### Browse: list + reader

The list is one line per entry: mono id · title (the heading, one line,
ellipsized) · state chips (`corrects F-171`, `superseded → D-157`,
`resolved`) · date · a small *backs* mark (ledger strip when there is a
ledger; otherwise a count of cited files, scripts and jobs). Findings group
by topic (topic description as the group's subtitle); decisions and
learnings are newest first; filter chips above the list (*Changed this
week*, *Corrected*, *Stated: supported / established / none*, a tag).

The reader shows, in order:

- breadcrumb (`topic-slug › F-228`), the title, and one meta line:
  date · recorded by *Main Agent* (→ chat) · **open at line 4152**;
- **How sure** — the stated status word and, when there is a ledger, the
  evidence strip (one mark per row: filled supports, half refines, ring
  contradicts) and Mycelium's rule result when it differs ("stated
  *supported* · ledger has 1 run → *preliminary* by Mycelium's rule");
  omitted entirely when nothing is stated and there's no ledger;
- **What backs it** — chips for everything the entry cites: data files,
  scripts, jobs, commits, figures (open at the path), and the entries it
  references;
- the **body**, rendered as written (ids and paths inside it linked);
- **Follow-ups** — addenda, corrections and resolutions, threaded, newest
  last, each with its own date and status change;
- **Referenced by** — every decision, finding, to-do, convention and handoff
  that cites it.

Hovering an id chip anywhere shows a preview card (title, state, first
lines); the reader keeps a back/forward history so following a chain
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

One list, each row a sentence and an **Ask an agent** button that drafts a
precise request into the workspace's chat (the user sends it): *"Finding
ids F-079, F-126–129, F-175–178, … are used twice in different topic files
— renumber the later ones and update references."* / *"F-007 states
supported with one ledger row; Mycelium's rule says preliminary."* /
*"The Stop hook replaced a hand-written handoff with its fallback stub."*

### Narrow panes and empty states

Below ~820px the reader replaces the list (back returns to the list, at
the same scroll position). A workspace without Mycelium keeps today's
behaviour (Guidance & memory + the attach line).

## Data: the snapshot additions (a draft `knowledge/1`)

All additive — every current field keeps its meaning and today's fixtures
stay byte-identical where their inputs don't use the new shapes.

| Field | On | Meaning |
|---|---|---|
| `span {path, line, end}` | every entry, addendum, to-do, handoff | where the entry's markdown lives; the reader fetches the file (`/fs/file`) and renders the slice — bodies never ride the snapshot (the reference project's `.living/` text is ~1.8 MB) |
| `stated` | finding, decision | the status as written ("established by independent realignment"); `status` stays the normalized word or `unknown` |
| `rule` | finding | Mycelium's rule applied to the ledger (`preliminary`/`supported`/`robust`/`contradicted`), absent without a ledger |
| `refs[] {kind, id, topic?}` | every entry | ids this entry cites (F/D/C/L/T/to-do), in order |
| `cites[] {kind, text}` | every entry | data files, scripts, jobs, commits, figures named in the body |
| `state {kind, by?}` | every entry | `superseded` / `retracted` / `corrected` / `resolved` / `suspect`, with the id that did it |
| `addenda[].kind` | finding | `addendum` / `correction` / `resolution` / `update` |
| `key` | finding | `topic/F-NNN` — unique even when ids collide; `id` stays the bare id |
| `conventions[]` | snapshot | `{id, title, span, refs, cites}` |
| `sessions[]` | snapshot | `{id, date, summary, span, branch, status}` from `LOG_REGISTRY.md` |
| `asks[]` | snapshot | "waiting on you" items: `{text, span, source}` from findings, handoffs, decisions |
| `todos[]` +`closed`, `refs`, `span`, `title` | to-do | closed by the plugin's rules; `title` = the first sentence |
| `left_off.sources[]` | handoff | every handoff found, newest first (`{path, written_ms, session_id?, host?}`) |
| `tidy[]` | snapshot | `{kind, text, refs}` — the Tidy up rows |

Backlinks are computed in the client from `refs` (no extra wire). Deep
links (Timeline, dashboard, chat mentions) address an entry by `key`.

## Plugin work — `chimaera-plugin-mycelium` 0.2.0

Parsing that matches what agents write, all with fixtures cut from real
shapes (anonymized where needed):

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
- `rule` from the ledger; `refutes` as `contradicts`.
- To-dos: the registry table **and** the `##` to-do sections under it;
  closed when the status *starts with* a closed word (`done 2026-09-23 …`);
  File cells as refs + at most one real link; stop trimming backticks.
- Handoff: newest by mtime across the shared file and run dirs; numbered
  headings (`## 1. Goal`) map onto the five slots, unknown sections kept in
  order.
- Conventions (`## C-N` / `##` sections), sessions (`LOG_REGISTRY.md`),
  asks, tidy.
- Keys: `topic/F-NNN`; a collision is a `tidy` row, not a warning line
  above the content.

The 4 MiB snapshot cap stays; the 400-per-kind cap should rise to 1,000
with the snapshot cap as the real bound (the reference project is already at 310
learnings after three months), measured on the fixture.

## Core work

- **UI** (`web-ui/src/lib/knowledge/`): the new IA and screens above; an
  entry reader that fetches and renders `span` slices (cached per file
  mtime); `IdChip` with hover preview and history; a virtualized list;
  search over the envelope and loaded bodies; the provider adapter module;
  the `WhereThingsStand` dashboard card fed by the same Overview model
  (and never printing a raw `unknown`).
- **Open at line**: `revealLines` (`previews/cm.ts`) already exists — wire
  the file open to take a line.
- **Links to chats**: `recorded_by` and session ids resolve through the
  Timeline to a chat and turn; a handoff's `session_id` too. `left_off.by`
  (rendered today, never sent) gets filled or removed.
- **Server** (`knowledge.rs`): pass the new fields through; `ids_of`
  switches to the snapshot's per-key `span.path` (the platform plan's P10
  direction), which also fixes turn credit for colliding ids.

## Fit with the plugin platform

The platform plan (branch only, docs only, nothing shipped) keeps
**Knowledge as a data surface core draws** (its §4): today's snapshot
becomes the documented `knowledge/1` schema, the `knowledge` and `query`
exports are unchanged, and its P10 removes Mycelium's names from core.
This redesign fits that: it stays core Svelte, keeps the contract additive,
confines provider words to one adapter (principle 10), and hands the
field table above to that plan as the `knowledge/1` draft. Nothing in its
first phase (P6, trust for existing plugins) touches Knowledge; the new
dashboard `panel` slot sits beside *Where things stand*, so the dashboard
card must leave room for it.

**[decide] One conflict.** On 2026-09-28 the direction agreed was
"Knowledge becomes Mycelium's *declarative plugin view*" (a general
`provides.views`, view documents via `query`, drawn by core), with turn
credit fed by a provider-neutral entry list. The platform plan instead
keeps Knowledge a fixed core surface and gives generic plugin screens
(`ui/1`) to everything else. **Recommendation: take the platform plan's
route for Knowledge.** A knowledge base with hover previews, backlinks, a
reader and keyboard history is exactly what that plan calls "too
important or too interactive to be free-form"; a generic `ui/1` tree would
cap it. Turn credit stays core either way, now keyed by `span.path`.

## Upstream Mycelium — optional, the maintainer's call

The drift is partly Mycelium's to fix. **[decide]** whether to propose
upstream (via the `martinappberg` fork): allocate finding ids under a lock
so parallel agents can't collide; accept the prose finding shape officially
with a minimal required envelope (Status word, Date, an evidence line) and
say "established" is not a status; have Stop never replace a hand-written
handoff with its stub; let a finding say `**Corrects:** F-171` /
`**Superseded by:** D-157` as fields. The UI must work without any of it.

## Phases

0. **Now, no code** — update the Mycelium plugin on the HPC host to 0.1.3
   (Extensions): 35 phantom findings disappear.
1. **Honest data, current UI** (small): plugin 0.1.4 with the parsing
   fixes (fields, dates, follow-ups, to-do sections, closed detection,
   handoff newest-wins, backticks, `**Claim:**`); UI: truthful legend,
   no grey ladder for an unstated status ("not rated"), open-only to-do
   counts, the To-do layout and `.open` collision fixed, decisions sorted
   by real date. Ships in days and removes most of what looks broken.
2. **The redesign** — plugin 0.2.0 (`span`, `refs`, `cites`, `state`,
   `stated`, `rule`, keys, conventions, sessions, asks); core: Overview,
   list + reader, id chips + backlinks + history, search, open-at-line,
   chat links.
3. **Tidy up + Ask an agent**, Timeline and dashboard deep links, the
   `knowledge/1` hand-off to the platform plan.

## Verification

- Fixtures: a synthetic `.living/` that reproduces every reference-project shape
  above (template findings, prose findings, `established`, colliding ids,
  follow-up headings, both decision eras, to-do table + sections, three
  handoffs), checked into the plugin repo; the parser's counts on it pinned.
- Live: the isolated daemon with that fixture as a workspace; then, read
  only, the real reference project through the app after that host's plugin
  update — the header counts, the Overview's four cards and ten spot-checked
  entries compared against the files.
- The UI at 1400 / 960 / 820 / 600 px, light and dark; keyboard-only pass.

## Decisions for the maintainer

1. **Knowledge as a core surface (`knowledge/1`) vs a Mycelium plugin
   view** — recommend the core surface (above).
2. **How sure** — recommend: show the agent's stated word, add Mycelium's
   rule only where a ledger exists and disagrees, never invent a rating.
   Alternatives: compute from the ledger and ignore the stated word; or
   show stated only.
3. **Ask an agent** from *Tidy up* — recommend yes (it drafts, the user
   sends; Knowledge stays read-only).
4. **Project-named handoff files** (the reference project's `.claude/RESUME_STATE.md`)
   — recommend: read handoff files `MYCELIUM.md` names, newest wins, older
   ones behind a disclosure.
5. **Upstream proposals** — whether to raise any of them with Mycelium.
