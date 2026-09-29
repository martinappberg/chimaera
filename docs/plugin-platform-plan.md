# The plugin platform: screens, programs, tools and trust

Dated 2026-09-29. A plan, and from phase P6 on a record: **P6 (trust) is built**
([the feature page](features/plugins.md#trust-what-a-plugin-can-do-and-who-approved-it));
P7 onward is still plan. It grows the WASM
plugin host of the [plugin system plan](plugin-system-plan.md) (shipped as the WIT
world `chimaera:plugin@0.1.0`) into a platform: a plugin can draw screens in
Chimaera's own format, run the programs it declares, install the side programs it
needs, and is trusted in proportion to what it can do. The LaTeX and Typst plugins
are the first built on it ([latex-reports-plan.md](latex-reports-plan.md)); Agent
notes and Mycelium inherit it. It was built from a code map of the tree at
`4b2f9a1` (the [appendix](#appendix-what-the-code-does-today) traces every claim
about today's code) and a survey of how Zed, VS Code, browser extensions, JetBrains,
Raycast, Adaptive Cards, Slack Block Kit, Deno, download-and-verify installers, GitHub
artifact attestations and Sigstore handle the same problems ([prior art](#prior-art)).

## Decisions (maintainer, 2026-09-29)

1. **The platform route.** Capabilities built on other people's tools, or optional
   ones, grow as plugins. Core grows only generic pieces that every plugin can use.
2. **Plugins draw screens in the Chimaera format.** A screen is data that core renders
   with Chimaera's own components and styles, so plugins look like Chimaera. There is
   no plugin HTML, script or stylesheet anywhere.
3. **Plugins may run programs, and install the side programs they need**, through
   the host, declared in the manifest, bounded by the host.
4. **Verified plugins install freely. Anything else needs the user's explicit trust**
   in everything it may do.
5. **Today's plugins inherit the design** (Agent notes, Mycelium, the test fixture).
6. **LaTeX and Typst are the first plugins on it.** This reverses the 2026-09-28
   decision to build them into core ([LaTeX plan](latex-reports-plan.md)).

Proposed in the same discussion and accepted with the platform route: an update that
asks for more needs the user's OK again; a kill switch can block a bad version
everywhere; an admin can forbid unverified plugins on a host. Later the same day the
maintainer accepted every recommendation in [open decisions](#17-open-decisions) and
asked for the platform to be built, starting with the trust phase (P6).

## The short version

- **One list decides everything a plugin can do.** Its manifest declares its
  capabilities. That list is what the card shows before install, what a trust prompt
  asks about, and what the host enforces. Nothing outside it is possible.
- **Two tiers, named honestly.** A *sandboxed* plugin cannot leave its WebAssembly
  sandbox except through bounded host calls: it installs from the verified list with a
  click, as today. A *privileged* plugin runs programs or downloads them, and no
  sandbox can bound what a program does, so it needs verification or explicit trust.
- **Verified means reviewed.** A verified plugin is one whose exact release the
  maintainers pinned in `plugins/plugins.lock`, with its capabilities recorded there.
  An update that grows its capabilities, or any release of a privileged plugin the
  lock has not pinned, is no longer covered and asks the user.
- **Screens as data.** A plugin returns a tree of Chimaera components (panels, lists,
  tables, buttons, forms, and rich pieces such as the code editor and the PDF viewer);
  core draws it. Tabs, dashboard panels, file views, status chips and card sections are
  the places a plugin can fill.
- **Programs as jobs.** A plugin asks the host to run one of its declared programs;
  the host runs it in the background with a time limit, a memory cap, low priority and
  a queue, and tells the plugin when it finished.
- **Side programs as pinned downloads.** A plugin lists each download with its URL and
  sha256 in its manifest; the host fetches, checks, unpacks and places it. The lock
  pins the manifest, so it pins the downloads too. The user's own copy of a program
  always wins.
- **Files, settings, outputs.** A plugin can claim file kinds (`.tex` opens in its
  view), add actions on files, declare settings that the Settings page draws, and
  keep build output in a private folder outside the repository.
- **A kill switch.** A signed revocation list, checked daily, stops a bad version at
  once on every host that can reach it; offline hosts get it with the next chimaera
  release.
- **Nothing changes for today's plugins until they opt in.** The host serves the 0.1
  world beside the new one. Everything that is host-side (the trust model, the card's
  capability list, the kill switch, the activity log, the fixes below) applies to them
  from day one.

## Principles

1. **Plugins decide; the host does.** Every pixel, process, download, file write and
   limit belongs to the host. A plugin returns data and decisions.
2. **Declared, shown, enforced: one list.** A capability exists only if the manifest
   declares it; the card shows exactly that list; the host refuses everything else.
3. **The tier follows the capabilities**, never the author's word. Running or
   downloading a program makes a plugin privileged, whoever wrote it.
4. **Generic or it does not ship.** No core code names a plugin id, a file format or a
   vendor. A third-party plugin with the same trust can do everything a first-party
   one can. Today's exceptions are listed and removed
   ([generalizing core](#generalizing-core-what-names-a-plugin-today)).
5. **Versioned contracts, additive by default.** The WIT world, the screen format and
   each data surface carry a version. Additions are minor; an unknown node or field
   degrades visibly, never breaks.
6. **Login-node discipline.** Lazy, bounded, nothing polls, a quota per plugin, one
   daemon-wide queue for programs ([daemon rules](../.claude/rules/daemon.md)).
7. **Off means off.** Where no plugin is active, what agents see is byte-identical
   (the `agent_view` fixtures) and every file opens in its default viewer.
8. **Today's plugins keep working.** Adopting anything new is opt-in and never
   required.

## What a plugin can contribute

| Contribution | Manifest | Where it shows | Tier |
|---|---|---|---|
| Tools and a paragraph for agents | `provides.mcp_tools` (today) | the chimaera MCP server, where active | sandboxed |
| A line in an agent's hook answer | `provides.events = ["hook"]` (today) | claude's hook context | sandboxed |
| Knowledge | `provides.knowledge` (today) | the Knowledge view, "Where things stand" | sandboxed |
| Timeline notes | `[access] timeline = "notes"` | the Timeline | sandboxed |
| Screens | `[[views]]` | a tab, a dashboard panel, a card section | sandboxed |
| File kinds and file actions | `[[files]]`, `[[actions]]` | how a file opens; its toolbar and menu | sandboxed |
| Data surfaces | `provides.surfaces` | editor marks, problems lists, a file's output, jumps | sandboxed |
| Settings | `[[settings]]` | Settings → Plugins, and the card | sandboxed |
| An output folder | implied by `[[programs]]` or `[[files]]` | build output outside the repository | sandboxed |
| Programs | `[[programs]]` | the card's Runs line; the activity log | **privileged** |
| Side programs | `[[tools]]` | the card's Downloads line; the Tools section | **privileged** |

What stays out: network access from the component itself, writes into the workspace
without a user's click, reads outside the workspace (other than the plugin's own
output and tools), and plugin code in the UI. Each is a
[non-goal](#out-of-scope) or a later plan.

## 1. Capabilities

### The manifest declares them

The 0.2 manifest adds sections; everything in the 0.1 manifest keeps its meaning.

```toml
api = "0.2"

[access]                          # what the plugin may read through the host
files = "read"                    # "read" | "none"   (workspace files)
timeline = "notes"                # "none" | "read" | "notes" (read + post notes)
sessions = "read"                 # "none" | "read"

[[programs]]                      # privileged: the only programs it can run
name = "latexmk"
version = ["-v"]                  # how to read its version (default ["--version"])

[[programs]]
name = "tlmgr"
network = "CTAN mirrors (TeX Live packages)"   # disclosed on the card, not enforced

[[tools]]                         # privileged: side programs it may install
id = "tinytex"
# … artifacts per platform with url + sha256 (section 8)
```

`files`, `timeline` and `sessions` are the three reads a 0.1 plugin could make
without saying so. A 0.1 manifest has no `[access]` section and is read as
`files = "read"`, `timeline = "notes"`, `sessions = "read"`: exactly what it can do
today, now shown on the card. The host enforces each: a `read` from a plugin with
`files = "none"` fails, as does `timeline-recent` with `timeline = "none"`.

### The card says them in words

Every card, verified or not, gains a **Can** list generated from the manifest. It is
the same text the trust prompt shows:

- "Reads files in this workspace" · "Reads the Timeline" · "Posts notes to the
  Timeline"
- "Gives agents 2 tools: knowledge_search, knowledge_get" · "Adds a line to claude's
  context when a session starts"
- "Shows a tab: Notes" · "Opens .tex and .ltx files"
- "Sends a setup prompt to an agent you choose" · "Recommends an agent-side plugin
  from arjunrajlaboratory/mycelium"
- "**Runs on this host:** latexmk, pdflatex, xelatex, lualatex, bibtex, biber, tlmgr"
  · "tlmgr uses the network: CTAN mirrors"
- "**Downloads:** TinyTeX 2026.09 (152 MB) from github.com/rstudio/tinytex-releases"

The last two only for privileged plugins, with the tier named beside them.

### Indirect paths count

The list is only honest if every way a plugin can cause something is on it. Zed checks
the commands an extension runs itself, but the commands an extension hands back for
the editor to start take another path ([prior art](#prior-art)). Here:

- **One door for programs.** A program runs only as a job ([section 6](#6-programs)),
  whoever asks: the plugin, a tool's setup step, an action. A later feature that runs
  something for a plugin goes through the same runner and the same declared list, or
  it does not ship.
- **Agent-side plugins** (`agent_plugins` under `[requires]` or `[recommends]`) are
  listed with their marketplace. Installing one stays a separate click, and it then
  runs inside the agents with their permissions: claude runs its hooks, and codex asks
  per hook, which the card's hook trust keeps per hook and bound to the hook's hash, as
  today.
- **A setup prompt** (`[setup]`) is shown in full before it is sent, to a session the
  user picks.
- **A program's network use** is declared (`network` on its `[[programs]]` entry) so
  the card can say it. That is disclosure, reviewed for a verified plugin, not
  enforcement ([the honest limit](#the-honest-limit)).

### The capability digest

The host normalizes the declared capabilities (sorted, defaults filled in, download
URLs and hashes included) and hashes them: the **capability digest**. It is how the
lock records what the maintainers approved, how a trust record says what the user
approved, and how an update is compared ([growth](#an-update-that-asks-for-more)).

## 2. Trust and verification

### Verified, tightened

Today `first_party` (the check badge, "Verified by the Chimaera maintainers") means:
the id is in the lock, the manifest names the lock's repository, and the copy came
from that repository or has the pinned bytes. An update past the pin keeps the badge
although the lock never pinned those bytes, and a lock bump auto-merges after
checking only hashes and names. That is acceptable for a sandboxed plugin, which the
sandbox bounds; it is not for a privileged one, where new code can pass new arguments
to the same program.

The platform's rule:

- **Each lock entry gains `tier` and `caps`** (the capability digest of the pinned
  release). CI's lock check recomputes both from the release's `plugin.toml`.
- **A sandboxed plugin** keeps today's behaviour: verified at the pinned version, and
  an update from its own repository keeps the badge **when its capability digest is
  unchanged**. A digest that grows asks the user ([below](#an-update-that-asks-for-more)).
- **A privileged plugin** is verified only at a version the lock pins. Its updates
  reach users through the lock: the plugin's release, then a lock bump, then a
  chimaera release. A user can still install a newer release early, through the trust
  prompt, as an unverified version.
- **Lock bumps** (the `plugin-lock` workflow): auto-merge only when the tier is
  sandboxed and the digest is unchanged. Any privileged bump, and any digest change,
  opens the pull request without auto-merge and with a label asking for review.
- **Provenance** for privileged plugins: the plugin repository's release workflow
  attests that `plugin.wasm` was built from the tagged source (`actions/attest`, which
  takes the release's existing `SHA256SUMS` as its subjects), and the lock check runs
  `gh attestation verify` pinned to that repository and its release workflow before it
  proposes the bump. Review then covers the code that actually ships. The daemon does
  not verify attestations itself: it trusts the lock, which pins the hashes, so the
  check runs once, in CI, where people can see it.
- **Rebuildable.** A plugin repository's CI builds its component twice, in different
  folders, and compares the sha256, so a reviewer can rebuild what they read. Rust to
  WebAssembly builds are reproducible in practice with path remapping, not by
  guarantee, so this is a check, not an assumption.

### Install policy

- **Verified** plugins install with one click, as today; the card lists what they can
  do.
- **Unverified** plugins (any other repository, a version past a privileged pin, a
  local build without a matching lock entry) install only through a **trust prompt**:
  the source, the version and its sha256, the tier, and the full **Can** list. For a
  privileged plugin the user types the plugin's name to confirm. The record lands in
  `~/.chimaera/plugins/trust.json` (small, capped, rewritten atomically):
  `{id, repo, version, caps, granted_ms}`.
- **The CLI** asks the same question (`chimaera plugin add owner/repo` prints the list
  and asks), or takes `--trust` for scripts.
- **Local builds** (`chimaera plugin add --path`) ask once per id and capability
  digest, so the development loop (rebuild, add again) does not nag.
- **Admin policy.** A host setting, `plugins.allowUnverified` (default `true`), turns
  unverified installs off: the trust prompt is replaced by "This host only allows
  verified plugins". A machine-wide file, `/etc/chimaera/policy.json`, can set the
  same key plus `allowPrivileged` (`all`, `verified` or `none`) and `blocked` (plugin
  ids), and wins over the user's settings. It can only tighten. A file that does not
  parse **fails closed**: unverified and privileged plugins are refused, and Settings
  says why (VS Code's extension policy is ignored when it has a syntax error; this one
  must not be). It is a guardrail for shared and managed machines, not a lock against
  a user who runs their own build.
- **Trust is per host** (per daemon). The switch per workspace stays the user's click
  and still decides where a plugin is active.

### An update that asks for more

When an update would run a build whose capability digest is not covered (by the lock
for a verified plugin, by a trust record otherwise), the host downloads it but **keeps
running the previous build**, and the card says what the new one wants: "Mycelium 0.3
would also: run `git`". **Allow** records the new trust and switches; **Skip this
version** keeps the previous build until a later release. Nothing stops working while
the user decides. This is Firefox's model; Chrome installs such an update and disables
the extension until the user agrees, which leaves them with nothing
([prior art](#prior-art)). A digest that only shrinks is covered. When the running
build is itself blocked ([below](#the-kill-switch)), there is nothing to keep: the
plugin stays off until the user allows the update or removes the plugin.

### The kill switch

- **A revocation list**, `revoked.json`, published by the chimaera repository at a
  fixed URL and signed with an Ed25519 key the maintainers hold offline, whose public
  half the binary embeds. Each entry names a plugin id, the versions or sha256s it
  covers, a level and a reason. Publishing an entry takes two maintainers.
- **Two levels.** A **hard** block is for a malicious or dangerous build: it never
  loads, and there is no override. A **soft** block is for a build that is broken or
  misbehaves: it is switched off, and the user can switch it back on after reading why
  (Firefox's hard and soft blocks).
- **The daemon fetches it** with the plugin release checker (once after boot, then
  daily, under `update.autoCheck`), verifies the signature, and keeps the last good
  copy on disk, so a host that goes offline keeps the list it had.
- **It acts at once**, not at the next restart: a newly blocked build's instances are
  dropped, its running jobs are killed, its switches go off, and the card says
  "Chimaera blocked this version: <reason>. Update or remove it." Its tools stay on
  disk, unused (nothing else puts them on a PATH), until the plugin is updated or
  removed.
- **The lock carries revocations too**, so a host that never reaches the list is
  covered by its next chimaera release (JetBrains bakes a snapshot of its list into
  every build the same way).

### The activity log

Every privileged action is logged per plugin: each program run (program, the first
arguments, working folder, exit, duration), each download (URL, sha256, size), and
each trust grant or revocation. The log is append-only JSONL under
`~/.chimaera/plugins/activity/`, size-capped and rotated; the card's **Activity**
section shows the last 50 entries.

### Closing today's gaps

The code map found these; the platform closes them in its first phase:

| Gap today | Fix |
|---|---|
| A third-party plugin installs and switches on with no consent; its tools are then pre-allowed for new agent sessions | the trust prompt; pre-allow only for covered builds |
| `spawn_allow` pre-allows the declared tool names even of a plugin whose build was refused or faulted | pre-allow only what the loaded build offers |
| `detect.any` paths are not validated at parse time; an absolute or `..` path probes outside the workspace (only an existence answer) | validate at parse: relative, no `..`, no absolute path |
| `emit` frames go to every connected client, not only that workspace's | scope them to the workspace; the UI finally consumes them (screens) |
| The codex TUI silently skips tool names with `.`, `-` or capitals, which the manifest allows | quote them in the codex config, or refuse them at validation; never skip silently |
| Plugin state survives a Remove | clear it on Remove |
| `timeline-recent` returns entries of any kind to any plugin | gated by `[access] timeline` |
| An update past the pin keeps the badge whatever it changed | the digest rule above |
| Lock bumps auto-merge after checking only hashes and names | the tier and digest rule above |

## 3. Screens in the Chimaera format

### Where a plugin can draw

| Slot | Declared as | Opens |
|---|---|---|
| **Tab** | `[[views]] id, title, slot = "tab"` | from quick-open, the plugin's card, or one of its actions |
| **Panel** | `slot = "panel"` | on the workspace dashboard, beside "Where things stand" |
| **File view** | `slot = "file"`, named by a `[[files]]` entry's `view` | when a claimed file opens |
| **Status** | `slot = "status"` | a chip in a file view's toolbar, or on the rail |
| **Card section** | `slot = "card"` | inside the plugin's own Extensions card |

Settings are not a free-form slot: they are declared ([section 9](#9-settings)), so
Settings stays uniform and searchable.

### The format, `ui/1`

A screen is JSON: `{"ui": "1", "root": <node>}`, where a node is
`{"type": …, props…, "children": […]}`. Version 1 has four families of nodes:

- **Layout:** `stack`, `row`, `grid`, `split` (two panes and a ratio), `tabs`,
  `section` (a titled group), `card`, `divider`.
- **Content:** `text` (tone and size), `heading`, `markdown` (through the same
  sanitizer as chat), `code`, `keyvalue`, `badge`, `icon` (from Chimaera's icon set),
  `progress`, `empty` (an empty state with one action), `callout`, `list` (rows with a
  title, a subtitle, badges and row actions), `table` (columns and rows, paged),
  `file` (the embed card for a workspace or output file), `link`.
- **Inputs:** `button`, `toggle`, `select`, `textfield`, `form` (fields and a submit).
- **Rich components**, the pieces core already has, bound to data instead of code:
  - `editor`: a workspace file in the shared editor (its buffer, saves and merges as
    today), with the plugin's diagnostics as marks and optional change bars against a
    base (`head`, `index`, `rev:<ref>`, or `output:<path>`, a snapshot the plugin
    kept). The change bars compare words within paragraphs for prose, so a rewrapped
    paragraph is not marked whole.
  - `pdf`: an output or workspace PDF, swapped in place when it changes, with jumps
    both ways from a `sourcemap`, or from text matching when there is none, and change
    marks beside the lines the editor's change bars mark.
  - `diagnostics` (a problems list), `diff` (two texts, with the same prose mode), and
    `log` (a job's output, read from the tail like `LogView`).

Props are semantic, never visual: a `tone` (neutral, accent, good, warn, bad), a
`size`, an `icon` name, an `emphasis`. There are no colors, fonts or CSS, so light and
dark, the brand and accessibility come from the components, not the plugin.

### How a screen lives

- **Render.** The UI asks the host for a view; the host calls the plugin's
  `render(view, context)` export (context: the workspace, the file for a file view,
  the viewer's width class) and returns the tree.
- **Act.** A node's `action` (a name and a small payload) goes to the plugin's
  `on-action(view, action, payload, form)` export, which returns the new tree. The UI
  applies it with Svelte's keyed updates, so focus and scroll hold.
- **Update.** A plugin that knows something changed (a job finished) calls the
  `invalidate(view)` import; clients showing that view in that workspace re-render.
  At most 4 renders a second per view; a burst coalesces.
- **Interactive bits never round-trip.** Jumps between source and PDF, hover cards,
  scrolling and selection work from data the plugin published up front (section 4),
  so a click does not wait on the tunnel or on the plugin.
- **Caps.** A tree is at most 256 KiB and 5,000 nodes; a `list` or `table` longer than
  200 rows pages through the plugin's `query` export (already in 0.1, finally
  reachable).
- **Safety.** Markdown is sanitized like agent prose in chat. Links: `http(s)` opens
  outside the app, a file link goes through the file resolver and opens at its locator.
  Images and embeds come only from the workspace or the plugin's output folder, through
  short-lived tickets; nothing loads from the network.
- **Versioning.** `ui` names the version a tree needs. Adding node types or props is a
  minor version. A node a client does not know is replaced by its `fallback` (another
  node, or `"drop"`), else by a quiet "needs a newer chimaera" placeholder whose
  children still render; an unknown prop is ignored (Adaptive Cards' rules). Removing
  or changing a node is a major version, served beside the old one for a transition.
- **Validated on arrival.** The host checks every tree against the schema before a
  client sees it. A bad tree is not drawn: the view says "this plugin sent a screen
  chimaera could not draw", and the plugin's log gets each error with its JSON path
  (as Slack's `invalid_blocks` does), so an author sees exactly what to fix.
- **Accessible by construction.** Every view has a title and every input a label, both
  required; icons carry names. Screen readers read Chimaera's components, never a
  plugin's markup.

### Why data and not code

Webviews (VS Code) and iframes (Figma) let a plugin draw anything, and pay for it with
a second security boundary, an inconsistent look and no theming. Declarative formats
(Raycast's components, Slack Block Kit, Adaptive Cards) give up arbitrary drawing for
a consistent look, safety by construction and a schema that can evolve with fallback
([prior art](#prior-art)). For a workbench that must look like one product and run
over a tunnel, data wins. The rich components are what make it enough: a plugin
composes the editor and the PDF viewer rather than drawing them. And the components
stay in core: VS Code's host-styled toolkit for webviews was a library extensions
bundled, and when it was archived in 2025 every extension using it was left holding it.

## 4. Data surfaces

Some screens are too important or too interactive to be free-form. For those, a
plugin publishes data in a versioned shape, and core draws a view it owns. Knowledge
is the first such surface today; the platform generalizes the idea.

| Surface | Shape | Core draws it as |
|---|---|---|
| `knowledge/1` | today's snapshot, now a documented schema: every entry has a unique `key` and a `span {path, line, end_line}`; the snapshot carries `labels` (every word core shows) and `id_shapes` ([coordination](#coordination-the-knowledge-redesign-2026-09-29)) | the Knowledge view, "Where things stand", id chips |
| `references/1` | id shapes and the ids they name: `{shapes: [{kind, pattern}], ids: [{id, key, kind, title, span?, view?}]}`, at most 5,000 ids | chips for those ids in chats, previews and the Timeline, a hover preview from `span`, a click that opens `span` or the plugin's `view` |
| `diagnostics/1` | per file: `{severity, line, column?, end_line?, message, context?, source}`, at most 200 | editor marks, the problems list, an **Ask agent** action |
| `output/1` | per source file: the output file, its state (`building`, `ok`, `errors`, `failed`), a label, when it finished, and the pages that changed since the previous output | the file view's result pane and status chip; a "3 pages changed" chip on the Timeline turn that wrote the source |
| `sourcemap/1` | per output: source line ↔ output page and box, at most 4 MiB | jumps both ways, selections as source references, change marks |

- A plugin publishes with the `publish(surface, key, data)` import; the host
  validates the shape, applies the cap, keeps the latest per (plugin, workspace, key)
  in memory (and a `sourcemap` in the output folder, served by ticket), and sends a
  small `{"type":"surface", …}` frame to that workspace's clients.
- The same data reaches agents where it helps: a plugin's own tool can return its
  diagnostics, and **Ask agent** types one precise reference.
- `knowledge/1` loses its Mycelium-shaped assumptions in core
  ([generalizing core](#generalizing-core-what-names-a-plugin-today)): the snapshot
  itself says where each entry lives (`span`), what core calls things (`labels`)
  and which ids it answers for (`id_shapes`). It still arrives through the
  `knowledge` export, unchanged in 0.2; `publish("knowledge/1", …)` is an
  optional push a provider may use later.
- **Ids become links through one client registry.** Core's chip layer asks
  reference sources, never a plugin: the Knowledge snapshot (its `id_shapes`
  and keys) is the first source, and any plugin that publishes `references/1`
  is another (a LaTeX plugin's `\label` and `\cite` keys, an issue tracker's
  numbers). A token becomes a chip only if a source has that exact id;
  a plugin switched off takes its chips with it.

## 5. Files: kinds, actions and events

- **File kinds.** `[[files]] match = ["*.tex", "*.ltx"]`, `view = "document"`,
  `label = "LaTeX"`. Where the plugin is active, a matching file opens in its view;
  **Open as text** is always one click away. Two active plugins claiming the same kind
  get an **Open with** choice, remembered per workspace.
- **File actions.** `[[actions]] match = ["*.md"], label = "Export PDF", action =
  "export-pdf"` adds an item to the file's toolbar and context menu, which calls
  `on-action`.
- **Events** the host now delivers, to plugins that declare them: `file-saved`
  (the editor saved a claimed file), `file-changed` (the disk watcher saw a claimed
  file, or one in the plugin's watch set, change; an agent's write arrives at once
  through the existing hooks), `job-finished` (section 6), `settings-changed`, and
  `switched-on` / `switched-off` (declared since 0.1, now delivered).
- **Debounced by the host.** A component has no timers, so the host coalesces file
  events per file for the `debounce_ms` its `[[files]]` entry declares (default 300,
  at most 5,000): an agent writing five chapters in a burst is one event per file,
  delivered once the burst settles.
- **The watch set.** `watch(paths)` registers up to 256 workspace paths per plugin
  and workspace (a build's inputs, say). The host stats them with its existing sweep
  every 5 s while one of the plugin's views is open in that workspace, and for 10
  minutes after; otherwise not at all.

## 6. Programs

### Declared, then run by the host

```toml
[[programs]]
name = "latexmk"
version = ["-v"]
```

Only declared names can run. The host resolves each name on the PATH the user's
terminals get (the host and workspace environment prelude, captured once per prelude
change), then in the plugin's own tools (section 8). **The user's copy wins** unless a
job asks for the plugin's copy by name (`"prefer": "tool:tinytex"`), which a setting
exposes to the user. A plugin never passes a path to a binary.

### The import

```wit
job-start: func(cx: context, spec: json) -> result<string, string>;   // a job id
job-status: func(cx: context, id: string) -> json;
job-cancel: func(cx: context, id: string);
```

A **spec**: `program`, `args` (a list; there is no shell), `cwd` (a workspace path,
or `output:` inside the plugin's output folder), `env` (added variables; the host's
own, such as `PATH`, `HOME`, `LD_*` and the limits, cannot be set), `stdin` (up to
4 MiB), `wall_s` (clipped to the host's cap), `label` (shown in the UI), `priority`
(`user`, `agent`, `background`).

Arguments are passed as given. A plugin that must name a folder in an argument
(`-outdir=…`) builds it from the `roots` import, which returns the absolute workspace
root and output folder; the same import lets it turn paths a program prints (a log's
file names, SyncTeX's inputs) back into workspace paths. Knowing a path grants
nothing: every read and write still goes through the host.

### What the host enforces

| Limit | Default | How |
|---|---|---|
| Queue | at most 2 jobs running daemon-wide, 1 per plugin, 8 waiting per plugin | a semaphore and a queue, by priority |
| Time | 60 s, at most 600 s | a timer, then SIGTERM and SIGKILL to the job's process group |
| Priority | nice 10, idle I/O where allowed | `setpriority`, `ioprio_set` before exec |
| Memory | 4 GB of address space | `RLIMIT_AS`; for a heavily threaded program that reserves more than it uses, a user cgroup (`systemd-run --user --scope -p MemoryMax=…`) where the host has user systemd |
| CPU | the wall time plus slack | `RLIMIT_CPU` |
| File size | 256 MB per written file | `RLIMIT_FSIZE` |
| Output | stdout and stderr to files in the output folder, 16 MB each | never into daemon memory |
| Environment | the prelude's, minus the daemon's own variables and secrets | the existing spawn scrubbing |
| stdin | closed unless given | an error prompt never waits |

### Finishing

When a job ends, the host delivers `job-finished {id, exit, timed_out, duration_ms}`
to the plugin. That call gets the 30 s budget `knowledge` already has, because it is
where a plugin digests outputs (a thesis's SyncTeX file is several MB); every other
event keeps 5 s. The plugin reads what it needs from the output folder, publishes
surfaces, and invalidates views. The instance is free while the job runs, so the
one-call-at-a-time rule stays as it is.

### Long tools for agents

An agent tool that starts a job returns `tool-result` with `wait: some(job-id)`. The
host holds the agent's call until the job ends (at most about 45 s, under the agents'
MCP timeouts), then calls the new `tool-resume(name, job-id)` export for the final
answer. A longer job answers "still running (job …)"; the agent's next identical call
joins it.

### The honest limit

A declared program can do anything its arguments allow: `latexmk` runs Perl, `bash`
runs anything, and a program may start others (latexmk starts the engines, `tlmgr`
runs `gpg` and a downloader). The host's limits cover the whole process group, but no
WebAssembly sandbox reaches inside a program; Deno says the same of its `--allow-run`
([prior art](#prior-art)). That is why programs make a plugin privileged, why the card
names every program, and why a plugin that declares a shell gets the strongest wording
on its card ("Runs bash: this plugin can run any command on this host"). Two rules
follow:

- **A plugin never writes where its programs live.** The component writes only to its
  output folder; its tools folder changes only through the tools' own programs, run as
  jobs. Write access plus run access is everything.
- **Operating-system confinement is later, and opt-in.** Where a host allows it
  (bubblewrap with user namespaces, or Landlock), a job could be confined to the
  workspace, its output folder and its tools, as Codex and Anthropic's sandbox runtime
  confine agents' commands. Many HPC kernels allow neither, so it cannot be the safety
  model ([open decisions](#17-open-decisions)).

## 7. Output folders

- Each (plugin, workspace) gets `$XDG_CACHE_HOME/chimaera/plugins/<id>/<workspace-key>/`
  (else `~/.cache/…`): outside the repository, surviving restarts, never night-scrubbed.
- The plugin addresses it as `output:<path>` with `output-read` (from an offset, up to
  8 MiB a call, so a large log or SyncTeX file streams), `output-list`, `output-write`
  (files up to 8 MiB, such as a generated source) and `output-remove`. Jobs may use it
  as their working folder.
- A quota per plugin (1 GB by default, a setting) with least-recently-used eviction of
  top-level entries; Settings shows the use and a **Clear**.
- **Durable state.** Today's `state` is memory only and a restart clears it. A plugin
  that must remember something (a trust decision about a project file, a chosen main
  file) writes it with the 0.2 `state-keep` import; the host keeps durable keys in a
  capped JSON per (plugin, workspace) under `~/.chimaera/plugins/`, within the same
  64 KiB.
- The UI reads output files through `/raw` tickets. **Save to workspace** is a host
  action the user clicks (a `button` with the built-in `save-to-workspace` action);
  a plugin cannot write into the workspace on its own.

## 8. Tools: side programs a plugin installs

### Declared downloads, not install scripts

```toml
[[tools]]
id = "tinytex"
name = "TeX Live (TinyTeX)"
version = "2026.09"
programs = ["latexmk", "pdflatex", "xelatex", "lualatex", "biber", "tlmgr"]
home = "https://github.com/rstudio/tinytex-releases"

[[tools.artifacts]]
platform = "linux-x86_64"
url = "https://github.com/rstudio/tinytex-releases/releases/download/v2026.09/…"  # a fixed tag
sha256 = "…"                      # required
size = 159_000_000
unpack = "tar.xz"
bin = "TinyTeX/bin/x86_64-linux"

[[tools.setup]]                   # run once after unpacking, as ordinary jobs
program = "tlmgr"
args = ["install", "latexmk"]
```

- **The host installs**, on the user's click (from the card, a file view's empty
  state, or a screen's `install-tool` action): download over HTTPS only, from the
  declared URL (a fixed release, never a moving tag such as `daily` or `latest`);
  check the sha256 while streaming (a manifest without one does not validate; the host
  refuses a size above the declared one, and anything above 2 GB); unpack; then run the
  `setup` steps as jobs whose programs must be declared by the tool. It runs in the
  visible install terminal the agent installs already use.
- **The unpacker is trusted core**, because a bad archive is how Zed's download path
  became a sandbox escape (CVE-2026-27976, [prior art](#prior-art)). It extracts into
  a fresh, empty folder; creates every entry relative to that folder's descriptor,
  never following a link on the way (the plugin host's `O_NOFOLLOW` rule); refuses
  absolute paths, `..`, device files and hard links; allows a symbolic link only when
  it is relative and resolves inside the folder (TeX Live's `bin` folder is mostly such
  links), and never writes through one; and stops at 4 GB unpacked or 200,000 entries.
  A corpus of hostile archives tests each rule.
- **Nothing outside the folder changes.** No PATH edit, no shell rc file, no links in
  `~/bin`. (TinyTeX's own installer runs `tlmgr path add`; the platform never does.)
  A tool's `bin` joins the PATH of that plugin's jobs only.
- **Layout:** `~/.chimaera/tools/<plugin>/<tool>/<version>/` behind an atomic `current`
  link, the `runtimes.rs` idiom. At most two versions stay.
- **The chain of trust.** The lock pins the manifest's sha256; the manifest pins every
  artifact's sha256. A verified plugin's downloads are therefore verified by the same
  review, with no separate list, the way uv embeds its downloads' hashes. Upstreams
  often publish no checksums (TinyTeX's releases have none), and GitHub's per-asset
  digest comes from the same server as the file, so the reviewer takes each sha256 from
  the artifact itself when the manifest changes, and the lock check downloads every
  artifact and compares.
- **Updates** come with the plugin: a new tool version is a new plugin release (and,
  for a privileged plugin, a lock bump). A tool that manages itself (`tlmgr install`)
  changes only its own folder.
- **Where users see them:** the card's **Downloads** line before install, a **Tools**
  section after (version, size, **Update**, **Remove**), and Settings → Plugins. The
  Environment page lists what the prelude puts in reach for the active plugins'
  programs ("found through this prelude: TeX Live 2025"), so the user can see which
  copy wins.
- **Remove** of a plugin offers to remove its tools; tools are per plugin in this
  version (two plugins may download the same thing; sharing by sha256 is later).
- **Agent CLI installs stay as they are** (`runtimes.rs`'s curated scripts); the
  downloader and unpacker are shared code.

## 9. Settings

```toml
[[settings]]
key = "build_on_open"
type = "bool"                     # bool | enum | string | number | path
default = true
label = "Build when a document opens"
scope = "workspace"               # host | workspace
```

- Settings → **Plugins** gains one section per installed plugin, drawn from its
  declarations with the same `SettingRow` controls as core settings, and searchable. The
  plugin's card links to it.
- Host-scoped values live in `settings.json` under `plugins.<id>.<key>`;
  workspace-scoped ones in a small capped JSON per workspace. The plugin reads them
  with `setting-get(key)` and hears `settings-changed`.
- `schema.ts` stays the single source of truth for core settings; plugin settings are
  a second, declared source, documented as an exception the way Environment and
  Documents are ([settings map](../web-ui/src/lib/settings/AGENTS.md)).

## 10. Agents

Unchanged: tools and the instruction paragraph only where active, the call gate,
pre-allow at spawn, hooks for claude, the `agent_view` invariant. Added: long tools
(section 6), and pre-allow limited to what the loaded build offers. A privileged
plugin's tools are pre-allowed like any other (the user trusted the plugin); its jobs
run under the host's limits whether a person or an agent asked.

## 11. The interface: WIT 0.2

```wit
package chimaera:plugin@0.2.0;

// types, host and plugin as in 0.1, plus:

interface platform {                       // imports
    job-start: func(cx: context, spec: json) -> result<string, string>;
    job-status: func(cx: context, id: string) -> json;
    job-cancel: func(cx: context, id: string);
    output-read: func(cx: context, path: string, offset: u64, cap: u32) -> result<list<u8>, string>;
    output-list: func(cx: context, path: string, cap: u32) -> result<list<entry>, string>;
    output-write: func(cx: context, path: string, bytes: list<u8>) -> result<_, string>;
    output-remove: func(cx: context, path: string) -> result<_, string>;
    publish: func(cx: context, surface: string, key: string, data: json) -> result<_, string>;
    invalidate: func(cx: context, view: string);
    watch: func(cx: context, paths: list<string>) -> result<_, string>;
    setting-get: func(cx: context, key: string) -> option<json>;
    state-keep: func(cx: context, key: string, value: json) -> result<_, string>;  // durable
    tool-state: func(cx: context, tool: string) -> json;     // installed? version? found on PATH?
    roots: func(cx: context) -> json;                        // {workspace, output}: absolute paths
}

interface screens {                        // exports
    render: func(cx: context, view: string, args: json) -> result<json, string>;
    on-action: func(cx: context, view: string, action: string, payload: json) -> result<json, string>;
    tool-resume: func(cx: context, name: string, job: string) -> tool-result;
}

world chimaera-plugin {                    // 0.2
    import host;
    import platform;
    export plugin;
    export screens;
}
```

- `event` gains `file-saved`, `file-changed`, `job-finished`, `settings-changed`
  (and `switched-on` / `switched-off` are now delivered); `tool-result` gains `wait`.
- **Both worlds are served.** A component built against 0.1 instantiates through the
  0.1 bindings, unchanged; a 0.2 component through the new ones. The `api` gate
  accepts `0.1` and `0.2` (`SERVED_APIS`).
- **The Rust side:** `chimaera-plugin-api` 0.2 adds default methods for the new exports
  (`render` returns "no such view", `on_action` does nothing, `tool_resume` an error),
  so a 0.1 plugin moves to 0.2 by bumping the dependency and `api`, with no code change.
- **0.1's support window:** served for at least two minor chimaera releases after
  both first-party plugins move to 0.2, then kept loading with a card note ("built for
  an older chimaera; its author should rebuild it").

## 12. Today's plugins inherit it

| | Agent notes | Mycelium | The test fixture |
|---|---|---|---|
| Tier | sandboxed | sandboxed | sandboxed (a second, privileged fixture joins) |
| Can (derived from today's manifest; a 0.1 manifest gets 0.1's implicit reads) | reads files, reads the Timeline and posts notes, reads sessions; 2 agent tools; a hook line | reads files, reads the Timeline and posts notes, reads sessions; 2 agent tools; fills Knowledge | the same reads; one tool per host limit |
| Day one, with no plugin change | the Can list on its card, the trust rules (it is verified, so no prompt), the kill switch, the activity log, the gap fixes | the same | the same, and tests for each |
| Worth adopting on 0.2 | `[access]` narrowed to what it uses; an inbox **panel** and a **deliver** action, replacing the bespoke inbox chip in the Mastermind dock that names `agent-notes` today | `[access] timeline = "none"`; its guidance files and id-to-file map declared in the snapshot, removing Mycelium names from core; a setting for its root folder | a 0.2 variant covering every new import, export and node type |
| Required work | none | none | the new fixture |

### Generalizing core: what names a plugin today

"No core code names a plugin" is a principle only once these go:

| Where | What | Replaced by |
|---|---|---|
| `knowledge.rs` `ids_of` | maps decisions and learnings to `.living/decisions.md` and `.living/learnings.md` | each entry's `key` and `span.path` (the Knowledge redesign does it) |
| `knowledge.rs` guidance | lists `MYCELIUM.md` and looks for `MYCELIUM:BEGIN` | Guidance & memory moves to the dashboard (the Knowledge redesign); a provider's own guidance file is named in its snapshot |
| `knowledge.rs` `empty_body` | Mycelium's keys (`topics`, `decisions`, …) | `knowledge/1`'s documented empty shape |
| `web-ui/…/workspace/knowledge.ts` | the wire types are Mycelium's schema | `knowledge/1`'s types |
| `web-ui/…/plugins/store.ts` | `myceliumPlugin`, `openAttachSheet("mycelium")` | the active plugin with `provides.knowledge`, whichever it is (the Knowledge redesign does it) |
| `AttachSheet.svelte` | Mycelium-specific strings | the manifest's `setup` and `recommends` text |
| Knowledge's words | section names and status words written in core | `knowledge/1`'s `labels` |
| `installCopy.ts` | hard-coded tile letters | derived from the name |
| `MastermindDock.svelte`, `DashboardView.svelte` | the `agent-notes` id | a panel slot and a surface |
| `agent_probe.rs` hook trust | uses only the first codex agent plugin | every one the manifest names |

## 13. Host limits, new and old

| Limit | Value |
|---|---|
| Calls into a plugin | 5 s (`render`, `on-action`, tools, events, queries); 30 s for `knowledge` and `job-finished` |
| State | 64 KiB per (plugin, workspace), durable keys included |
| File events | coalesced per file for 300 ms by default, at most 5,000 |
| Screen tree | 256 KiB, 5,000 nodes; 4 renders a second per view |
| Surfaces | diagnostics 200 per file; a source map 4 MiB; a knowledge snapshot 4 MiB (as today) |
| Jobs | 2 running daemon-wide, 1 per plugin, 8 waiting per plugin; 60 s default, 600 s at most; the rest in [section 6](#6-programs) |
| Output folder | 1 GB per plugin by default; files written by the plugin itself ≤ 8 MiB |
| Watch set | 256 paths per plugin and workspace; swept only while a view is open, and 10 minutes after |
| Tools | download ≤ 2 GB and ≤ the declared size; unpacked ≤ 4 GB and 200,000 entries; two versions kept |
| Activity log | size-capped JSONL, rotated; 50 entries on the card |
| Trust and revocation files | small, capped JSON, rewritten atomically |

## 14. What changes where

| Where | What |
|---|---|
| `crates/chimaera-plugin-api` | the 0.2 WIT (both worlds), the Rust traits with defaults, native stubs |
| `crates/chimaera-server/src/plugins/` | `capabilities.rs` (derive, digest, diff), `trust.rs` (records, prompts, the admin policy and `/etc/chimaera/policy.json`), `revoke.rs` (feed, signature, levels, cache), `jobs.rs` (the runner and its limits), `output.rs`, `toolchain.rs` (downloads, unpacking, layout; shared with `runtimes.rs`), `surfaces.rs`, `views.rs` (render and action routes, invalidation), `files.rs` (kinds, actions, events, watch set), `settings.rs`; `runtime.rs` binds both worlds and the new exports and events; `hostfns.rs` the new imports |
| routes (bearer-authed, additive) | `GET /workspaces/{id}/plugins/{pid}/views/{view}`, `POST …/views/{view}/actions`, `GET …/surfaces/{surface}?key=`, `GET …/query/{name}` (0.1's promised route), `POST /plugins/{pid}/trust`, `DELETE /plugins/{pid}/trust`, `GET /plugins/{pid}/activity`, `POST /plugins/{pid}/tools/{tool}/install`, `DELETE …/tools/{tool}`, `GET /workspaces/{id}/jobs/{job}` (status and log ticket), `GET` and `DELETE …/plugins/{pid}/output` (use and Clear) |
| `/ws/events` | `plugin` frames scoped to their workspace; new `surface`, `view` and `job` frames, small, invalidate-and-refetch |
| `crates/chimaera-server/src/git/` | `rev=` on `GET /git/diff` (a ref checked with `check-ref-format` and resolved with `rev-parse --verify`) and `GET /git/log?path=` (at most 50), for the editor's change-bar bases |
| `web-ui/src/lib/plugins/` | `ui/` (one component per node type, the renderer, the fallback node), the trust dialog, the card's Can, Activity and Tools sections, the growth callout |
| `web-ui/src/lib/layout/`, `previews/` | the plugin tab kind, file-kind dispatch in `FileView`, file actions, the status slot |
| `web-ui/src/lib/settings/` | Settings → Plugins sections from declarations |
| `.github/scripts/plugin-lock.mjs` and the workflow | `tier` and `caps` in the lock, the no-auto-merge rule, the provenance check, revocations |
| `plugins/` | `plugins.lock` gains `tier`, `caps` and revocations; the second fixture |
| docs | this plan; the authoring guide rewritten for 0.2; the feature page as each phase ships |

## 15. Phases

### P6: trust, for the plugins that exist (host only)

**Built (2026-09-29).** One deviation: the revocation list needs `threshold` of the
keys in `plugins/revocation-keys.txt` (1 while there is one maintainer; "two
maintainers" is then a process rule, and the file can raise it), and it ships with
no key, so only the list embedded in each build counts until a maintainer makes one
(`node scripts/revocations.mjs keygen`).

The capability model for 0.1 manifests and the card's **Can** list; the install policy,
the trust prompt and trust records; the admin policy; the growth check; `tier` and
`caps` in the lock and the lock-bump rule; the revocation list, its signature and its
two levels; the activity log; every gap in [closing today's gaps](#closing-todays-gaps),
and the indirect paths on the card. No plugin changes.

**Verification.** Tests: a third-party install refused without trust and accepted with
it; a digest growth keeping the previous build running; a hard-blocked sha dropped at
once and refused, then an update restoring the plugin; a soft block switched back on;
a policy file that does not parse refusing unverified installs; pre-allow limited to
loaded builds; `detect` validation; emit scoping.
Live: install a fixture from a fake releases server through the prompt; revoke it
with a locally signed list; watch the card.

### P7: screens, surfaces, files, settings, outputs (0.2 world)

The 0.2 world beside 0.1; `ui/1` and its renderer; the five slots; `render`,
`on-action`, `invalidate` and the query route; the four surfaces; file kinds,
actions and events; the watch set; output folders; declared settings; `switched-on`
/ `switched-off`; the new fixture. Still sandboxed only.

**Verification.** The fixture draws every node type in light and dark, on Chromium and
WebKit, over a real tunnel; an unknown node renders its placeholder; a burst of
invalidations coalesces; a 0.1 plugin loads unchanged beside it.

### P8: programs and tools (the privileged tier)

Jobs and their limits, `job-finished`, long agent tools, tool downloads and setup,
the Tools section, the privileged fixture, provenance in the lock check.

**Verification.** The privileged fixture runs a stand-in program and a stand-in tool
download against local servers: a timeout kills the process group; a download whose
sha256 is wrong is refused; the hostile-archive corpus (a `..` entry, an absolute
path, a hard link, a link pointing out, an entry written through an earlier link) is
refused; a job past its queue cap waits; an agent tool waits for its job; a hard block
kills a running job. Live on a real login node.

### P9: LaTeX and Typst

The first privileged plugins, in their own repositories
([latex-reports-plan.md](latex-reports-plan.md)).

### P10: today's plugins adopt, and core stops naming them

Agent notes and Mycelium on 0.2 with the pieces in [section 12](#12-todays-plugins-inherit-it);
the [generalizing core](#generalizing-core-what-names-a-plugin-today) table emptied.
The Knowledge rows empty through the redesign's fields (`key`, `span`, `labels`,
`id_shapes`), not a rewrite; P10 then adds `references/1` as the registry's
second source ([coordination](#coordination-the-knowledge-redesign-2026-09-29)).

| Phase | What | Size |
|---|---|---|
| P6 | Trust for existing plugins | medium |
| P7 | Screens, surfaces, files, settings, outputs | large |
| P8 | Programs and tools | large |
| P9 | LaTeX and Typst plugins | large |
| P10 | Current plugins adopt; core de-named | medium |

P6 first, because it protects users today. P7 and P8 are independent after P6 and can
run in parallel. P9 needs both. P10 needs P7.

## Coordination: the Knowledge redesign (2026-09-29)

The Knowledge redesign (branch `claude/knowledge-redesign`,
`docs/knowledge-redesign-plan.md`) and this platform meet in four places. What
the redesign should build so that the two fit, decided here for both:

1. **Knowledge is the `knowledge/1` surface: core draws it, the plugin owns its
   data and words.** Not a `ui/1` screen (a reader, virtualized lists and
   previews inside chats need core's components). Core never branches on a
   plugin's id, its file names or its status words: which plugin is the
   provider is "the active plugin with `provides.knowledge`"; every visible
   word (section names, status vocabulary, the legend, the source chip) comes
   from `labels`; tones use the `ui/1` set (`neutral`, `accent`, `good`, `warn`,
   `bad`). The lists stay the documented kinds (findings, decisions,
   learnings, conventions, to-dos, questions, sessions, asks, tidy, where we
   left off): a provider fills the ones it has. Status is shown as written
   (`stated`); rank and tone only when the plugin's `labels` give them.
2. **Transport stays the `knowledge` export**, unchanged in WIT 0.2 (stamp in,
   snapshot out, 30 s, 4 MiB). A provider may stay on `api = "0.1"`. One that
   moves to `api = "0.2"` must declare `[access] files = "read"` (0.2 grants
   nothing unsaid), which changes its capability digest: its lock bump then
   waits for a maintainer, who sets `caps` from `chimaera plugin caps`.
3. **Spans are one shape everywhere:** `{path, line, end_line}`,
   workspace-relative, 1-based, inclusive (as `diagnostics/1`). Core reads the
   slice through the ordinary file routes; bodies never ride a surface.
4. **Ids to chips go through a client registry** (`shared/references.ts`): a
   source is `{shapes, lookup(id) → targets}`, a target `{key, kind, title,
   span?, open}`. The Knowledge snapshot registers first (its `id_shapes` and
   keys; a click opens Knowledge at the key); `references/1` publishers
   register later (P10) with no change to the chip layer. Chat, markdown
   previews and the Timeline ask the registry, never the Knowledge store.

Shared pieces:

- **Ask an agent** is one function (`shared/askAgent.ts`, `{text, file?,
  line?, end_line?}` → a draft in a chat's composer). The redesign's Tidy up
  and `ui/1`'s built-in `ask-agent` action both call it.
- **Rows, badges, key–value, callouts and file cards** the redesign builds go
  in `web-ui/src/lib/shared/`, with `ui/1`'s prop names (`title`, `subtitle`,
  `badges`, `tone`, `text`), so the `ui/1` renderer draws with them too.
- **The dashboard**: core cards first (Knowledge's card from `knowledge/1`,
  Guidance & memory), then active plugins' `panel` views
  (`dashboard/PluginPanels.svelte`, one insert in `DashboardView.svelte`).

Who edits what until both land: the redesign owns
`web-ui/src/lib/knowledge/*`, `web-ui/src/lib/workspace/knowledge.ts`,
`crates/chimaera-server/src/knowledge.rs`, `AttachSheet.svelte` and the
attach-sheet and `myceliumPlugin` part of `plugins/store.ts`; this platform
does not touch them. This platform adds to `plugins/store.ts` (trust fields,
`platform` on `WorkspacePlugin`), `net/events.ts` (platform frames),
`App.svelte` (their handler), the layout (a plugin tab), `FileView`,
`PluginCard` and Settings, and one insert in `DashboardView.svelte`. Whichever
lands second merges the other in; the overlaps are additive.

## 16. Risks

- **The screen format becomes a promise.** Mitigation: a small v1, fallback for unknown
  nodes, semantic props only, and versions served side by side.
- **Privileged means trusted.** A verified plugin that runs programs is as safe as its
  review. Mitigation: human review for every privileged bump, provenance, the digest
  rule, the kill switch, the activity log, and an admin setting.
- **Login nodes.** Programs and downloads cost CPU, memory and disk on shared hosts.
  Mitigation: the daemon-wide queue, low priority, per-plugin quotas, nothing in the
  background without a view or a user's action.
- **Review load.** Mitigation: sandboxed bumps with unchanged digests still merge on
  their own; only privileged or growing ones wait for a person.
- **Latency over a tunnel.** Mitigation: coarse renders, paging, and interactive work
  done in the browser from published data.
- **Core size.** The renderer and the job and tool host are real work. None of it is
  about a format, so it is paid once for every plugin that follows.

## 17. Open decisions

All six were decided on 2026-09-29, each as recommended:

1. **The revocation list is signed** with an Ed25519 key the binary embeds, so a
   compromised GitHub account alone cannot block or unblock plugins.
2. **Build provenance is required** for privileged verified plugins.
3. **Privileged updates come only through the lock.**
4. **Unverified plugins are allowed by default**, through the trust prompt;
   `plugins.allowUnverified` defaults to `true`.
5. **0.1 stays served** for two minor releases after both first-party plugins move,
   then loads with a card note, never a silent refusal.
6. **Operating-system confinement of jobs** is later and opt-in, because many login
   nodes allow neither bubblewrap nor Landlock, so it can never be what safety rests
   on.

## Out of scope

- **Plugin code in the UI** (HTML, script, stylesheets, webviews) and **native plugins**.
- **Network access from the component itself.** A program a privileged plugin runs may
  use the network (TeX Live's `tlmgr` does); the component may not. A declared,
  host-mediated fetch is a later plan if a plugin needs one.
- **Writes into the workspace** without a user's click.
- **Plugins calling other plugins.**
- **A marketplace.** Browse, ratings and search are the Extensions tab's later work; this
  plan only decides what an install may do.

## Prior art

Checked 2026-09-29. Most official documentation sites were unreachable from the
research environment, so the facts come from the projects' source and documentation
repositories ([sources](#sources)).

| System | What it does | What this plan takes |
|---|---|---|
| **Zed extensions** (the closest precedent) | WebAssembly components. Running a program must be declared in the manifest (command and argument patterns) and allowed by a global user setting that allows everything by default. `download_file` takes no expected hash. No extension UI yet (an open request). Every registry submission and update is reviewed; extensions must download language servers, never bundle them. A tar-extraction bug in the download path was a sandbox escape (CVE-2026-27976). | Declared programs checked by the host, made stricter: per plugin, shown on the card, consented. Hashes required. Review of the manifest that pins the downloads. The unpacker as trusted core, tested against hostile archives. |
| **VS Code** | Declarative contribution points, plus webviews whose content policy the extension writes itself. No per-extension permissions; a publisher-trust dialog on first install from a new publisher. A marketplace block list that uninstalls malicious extensions; signature checks since 1.75. An admin allow-list that is ignored when it has a syntax error. Its host-styled webview toolkit was archived in 2025. | No webviews. The component catalog lives in core, not in a library plugins bundle. A block list that acts on installed copies. A policy that fails closed. |
| **Chrome and Firefox** | A new warning-level permission disables a Chrome extension until the user accepts; Firefox holds the update and keeps the old version running. Remote disable through the update check (Chrome) or a per-version block list with hard and soft blocks (Firefox). Admins can block by permission. | Growth re-consent in Firefox's shape. Hard and soft blocks per version. Admin policy by capability, not only by id. |
| **JetBrains** | A broken-plugins list fetched by the IDE and baked into each build; plugins signed by their author and the marketplace. In June 2026 it disabled 15 plugins that stole AI API keys, at the next restart. | A snapshot in every release. Act at once, not at the next restart. |
| **Raycast** | Extensions are React components rendered as native views, with no HTML or CSS. Every store extension is open source and reviewed; extensions are not further sandboxed. | Screens as a fixed component vocabulary, sent as data. |
| **Adaptive Cards** | A declared schema version; unknown elements dropped; a `fallback` and `requires` per element; the host's config owns styling. | The `ui` version, per-node fallback, semantic props. |
| **Slack Block Kit** | Strict validation with JSON-pointer errors, block limits, and a text fallback for notifications and screen readers. | Validation on arrival with exact paths, caps, required titles and labels. |
| **Figma** | A sandboxed plugin thread and an iframe UI; declared network domains with a reason, shown to users and admins. | The contrast for screens; disclosure of a program's network use on the card. |
| **Deno, WASI, Extism** | Deny by default. Deno's docs say a subprocess started under `--allow-run` runs outside the sandbox, and that write plus run access equals full access. No WASI proposal spawns processes; Extism exposes no process API. | Programs are a custom host import and a trust decision, never "sandboxed". A plugin never writes where its programs live. |
| **Codex, Anthropic's sandbox runtime** | Confine agents' commands with bubblewrap (Linux) or `sandbox-exec` (macOS), plus a network filter or proxy. | Operating-system confinement as later, opt-in hardening where a host allows it. |
| **uv, mise, Homebrew, rustup** | uv embeds the URL and sha256 of every Python it can download; mise's lockfile records checksums and checks attestations where present; Homebrew pins a sha256 per platform. | The manifest pins every artifact's sha256; the lock pins the manifest. |
| **TinyTeX and tlmgr** | TinyTeX publishes no checksum files; its installer downloads the moving `daily` tag and edits PATH. `tlmgr` checks package hashes, and TeX Live's signature only when `gpg` is present. | Pin a fixed release, hash it at review, never touch PATH, ask `tlmgr` for signatures ([LaTeX plan](latex-reports-plan.md#missing-latex-packages)). |
| **GitHub attestations, SLSA, Sigstore** | `actions/attest` signs build provenance (SLSA Build L2, L3 from a reusable workflow) and accepts a `SHA256SUMS` file; `gh attestation verify` pins the repository and workflow; releases can be made immutable; Rust crates can verify bundles in process. | Provenance for privileged releases, verified in the lock's CI; the daemon trusts the lock. |

## Appendix: what the code does today

From a code map of the tree at `4b2f9a1` (file and line references are to that tree).

- **The world** (`crates/chimaera-plugin-api/wit/chimaera.wit`): package
  `chimaera:plugin@0.1.0`; imports `read`, `stat`, `list`, `state-get`, `state-put`,
  `sessions`, `timeline-append`, `timeline-recent`, `emit`, `now-ms`, `log`; exports
  `tools`, `instructions`, `call-tool`, `knowledge`, `query`, `on-event`. The Rust
  `Plugin` trait gives every export a default (`src/lib.rs:54-92`); built natively,
  every host import aborts (`lib.rs:27-29`).
- **The manifest** (`plugins/mod.rs:163-201`): `deny_unknown_fields` everywhere; `id`,
  `name`, `version`, `summary`, `description`, `homepage`, `api`, `[detect]`,
  `[requires]`, `[recommends]`, `[setup]`, `[provides]` (`knowledge`, `mcp_tools`,
  `views`, `events`), `[adds]`, `[release]`. `provides.views` is only echoed on the wire;
  `switched-on` / `switched-off` are declarable and never delivered; `detect` paths are
  not validated at parse (`:848-858`).
- **Gates and activity:** `SERVED_APIS = ["0.1"]` (`mod.rs:65-68`); active = on, footprint
  present and no load fault (`:887-906`); `spawn_allow` returns declared tool names of
  active plugins (`:920-936`).
- **Runtime** (`plugins/runtime.rs`): one instance per (plugin, workspace), one call at a
  time, at most 64 (`:78`, `:504-507`); `CALL_BUDGET` 5 s, `KNOWLEDGE_BUDGET` 30 s,
  `HOST_GRACE` 2 s (`:66-72`); 64 MiB of memory (`:216-287`); five traps in a minute mark
  a fault (`:723-741`); no `Query` call exists (`:562-568`); `emit` frames go to every
  client (`ws.rs:977`, `:1139-1149`) and the UI ignores them (`web-ui/src/lib/net/events.ts:209-265`).
- **Host functions** (`plugins/hostfns.rs`): workspace-relative paths, every component
  opened without following links (`:127-191`); reads up to 8 MiB, listings 4,096; state
  64 KiB per (plugin, workspace), in memory, cleared only when the workspace is deleted;
  Timeline notes only, 10 a minute per session; `timeline-recent` of any kind.
- **Installs** (`plugins/installed.rs`, `releases.rs`, `preview.rs`): first-party pinned,
  third-party release and local path; `SHA256SUMS` beside each copy, re-hashed at load;
  `first_party` = id in the lock, repository match, and a source marker or pinned bytes
  (`installed.rs:180-190`); an update past the pin keeps it; a third-party install and
  switch-on need no consent (`InstalledView.svelte:102-120`, `PluginCard.svelte:182-192`).
- **Lock bumps** (`.github/workflows/plugin-lock.yml`, `.github/scripts/plugin-lock.mjs`):
  hourly and on a release dispatch; check the tag, `SHA256SUMS`, id, version and
  repository; open a pull request with squash auto-merge. Changes to `api`, `provides`,
  `detect` or `requires` are not checked.
- **Agents** (`plugins/tools.rs`, `mcp.rs`): plugin tools and paragraphs only where active;
  pre-allow at spawn; the codex TUI skips tool names with `.`, `-` or capitals
  (`launcher.rs:836-843`); the plugin-free view pinned by `src/tests/agent_view.rs`.
- **Knowledge** (`knowledge.rs`): the first active provider; snapshot ≤ 4 MiB; the last
  good snapshot served with a daemon-owned `error`; Mycelium names in core at `:343-346`,
  `:593-630`, `:668-682`.
- **Agent CLI installs** (`runtimes.rs`): curated scripts, official sources with checksums,
  a visible terminal, `~/.chimaera/agents/<agent>/<version>/` behind an atomic link.
- **UI pieces a screen kit reuses:** `shared/` (`Switch`, `Segmented`, `ConfirmDialog`,
  `ContextMenuHost`, `EmbedCard` and its bodies, `ReferenceChip`, `WorkTray`), `previews/`
  (`CodeView`, `PdfView`, `DiffView`, `TableView`, `LogView`, `MarkdownView`,
  `SplitEditPreview`), and chat's sanitized `Markdown.svelte`.

## Sources

Checked 2026-09-29. Where an official site was unreachable, the same text was read from
its source repository, which is what is cited.

- **Zed**: the host API ([extension.wit](https://github.com/zed-industries/zed/blob/main/crates/extension_api/wit/since_v0.8.0/extension.wit),
  [process.wit](https://github.com/zed-industries/zed/blob/main/crates/extension_api/wit/since_v0.8.0/process.wit),
  [github.wit](https://github.com/zed-industries/zed/blob/main/crates/extension_api/wit/since_v0.8.0/github.wit)),
  [capability_granter.rs](https://github.com/zed-industries/zed/blob/main/crates/extension_host/src/capability_granter.rs),
  [extension_manifest.rs](https://github.com/zed-industries/zed/blob/main/crates/extension/src/extension_manifest.rs),
  [capabilities.md](https://github.com/zed-industries/zed/blob/main/docs/src/extensions/capabilities.md),
  [default settings](https://github.com/zed-industries/zed/blob/main/assets/settings/default.json),
  [GHSA-59p4-3mhm-qm3r](https://github.com/zed-industries/zed/security/advisories/GHSA-59p4-3mhm-qm3r)
  (CVE-2026-27976),
  [publishing guide](https://github.com/zed-industries/zed/blob/main/docs/src/extensions/publishing/publishing-guide.md),
  [prerequisites](https://github.com/zed-industries/zed/blob/main/docs/src/extensions/publishing/prerequisites.md),
  [package-extensions.js](https://github.com/zed-industries/extensions/blob/main/src/package-extensions.js),
  [custom UI request](https://github.com/zed-industries/extensions/issues/1288),
  [auto-update setting](https://github.com/zed-industries/zed/blob/main/crates/settings_content/src/extension.rs).
- **VS Code**: [webviews](https://github.com/microsoft/vscode-docs/blob/main/api/extension-guides/webview.md),
  [webview toolkit deprecation](https://github.com/microsoft/vscode-webview-ui-toolkit/issues/561),
  [extension runtime security](https://github.com/microsoft/vscode-docs/blob/main/docs/configure/extensions/extension-runtime-security.md),
  [workspace trust](https://github.com/microsoft/vscode-docs/blob/main/api/extension-guides/workspace-trust.md),
  [the control manifest](https://github.com/microsoft/vscode/blob/main/src/vs/platform/extensionManagement/common/extensionManagement.ts),
  signing in [1.75](https://github.com/microsoft/vscode-docs/blob/main/release-notes/v1_75.md)
  and [1.77](https://github.com/microsoft/vscode-docs/blob/main/release-notes/v1_77.md),
  [enterprise extension policy](https://github.com/microsoft/vscode-docs/blob/main/docs/enterprise/extensions.md).
- **Browsers**: Chrome's [permission warnings](https://github.com/GoogleChrome/developer.chrome.com/blob/main/site/en/docs/extensions/mv3/permission_warnings/index.md),
  [remote disable](https://github.com/chromium/chromium/blob/main/chrome/browser/extensions/omaha_attributes_handler.cc)
  and [ExtensionSettings policy](https://github.com/chromium/chromium/blob/main/components/policy/resources/templates/policy_definitions/Extensions/ExtensionSettings.yaml);
  Firefox's [permission updates](https://github.com/mozilla/extension-workshop/blob/master/src/content/documentation/develop/request-the-right-permissions.md)
  and [block list](https://github.com/mozilla/addons-server/blob/master/docs/topics/blocklist.md).
- **JetBrains**: [broken-plugins list](https://github.com/JetBrains/intellij-community/blob/master/platform/platform-impl/src/com/intellij/ide/plugins/marketplace/MarketplaceRequests.kt),
  [plugin signing](https://github.com/JetBrains/intellij-sdk-docs/blob/main/topics/basics/plugin_signing.md),
  [the June 2026 removals](https://blog.jetbrains.com/platform/2026/06/marketplace-ecosystem-security-update-malicious-ai-plugins/)
  (read from search results; the blog was unreachable).
- **Declarative UI**: Raycast's [FAQ](https://github.com/raycast/extensions/blob/gh-pages/faq.md),
  [security](https://github.com/raycast/extensions/blob/gh-pages/information/security.md)
  and [how extensions work](https://www.raycast.com/blog/how-raycast-api-extensions-work);
  Adaptive Cards' [renderer rules](https://github.com/MicrosoftDocs/AdaptiveCards/blob/main/AdaptiveCards/rendering-cards/implement-a-renderer.md)
  and [fallback](https://github.com/MicrosoftDocs/AdaptiveCards/blob/main/AdaptiveCards/schema-explorer/text-block.md);
  Slack's [Block Kit guide](https://github.com/slackapi/slack-skills-plugin/blob/main/skills/block-kit/SKILL.md)
  and [chat.postMessage](https://docs.slack.dev/reference/methods/chat.postMessage/);
  Figma's [how plugins run](https://developers.figma.com/docs/plugins/how-plugins-run).
- **Supply chain**: [actions/attest](https://github.com/actions/attest),
  [attest-build-provenance](https://github.com/actions/attest-build-provenance),
  [`gh attestation verify`](https://github.com/cli/cli/blob/trunk/pkg/cmd/attestation/verify/verify.go),
  [offline verification](https://github.com/github/docs/blob/main/content/actions/how-tos/secure-your-work/use-artifact-attestations/verify-attestations-offline.md),
  [artifact attestations and SLSA](https://github.com/github/docs/blob/main/content/actions/concepts/security/artifact-attestations.md),
  [SLSA build track](https://github.com/slsa-framework/slsa/blob/main/spec/build-track-basics.md),
  [immutable releases](https://github.blog/changelog/2025-10-28-immutable-releases-are-now-generally-available/),
  [`gh release verify-asset`](https://github.com/cli/cli/blob/trunk/pkg/cmd/release/verify-asset/verify_asset.go),
  [release asset digests](https://github.blog/changelog/2025-06-03-releases-now-expose-digests-for-release-assets/),
  [cosign verify-blob](https://github.com/sigstore/cosign/blob/main/doc/cosign_verify-blob.md),
  [sigstore-rs](https://github.com/sigstore/sigstore-rs),
  [sigstore-rust](https://github.com/sigstore/sigstore-rust),
  [cargo trim-paths](https://github.com/rust-lang/cargo/pull/17488).
- **Runtimes and sandboxes**: [Deno security](https://github.com/denoland/docs/blob/main/runtime/fundamentals/security.md),
  [WASI proposals](https://github.com/WebAssembly/WASI/blob/main/docs/Proposals.md),
  [wasmtime's WASI defaults](https://github.com/bytecodealliance/wasmtime/blob/main/crates/wasi/src/ctx.rs),
  [Extism manifest](https://github.com/extism/extism/blob/main/manifest/src/lib.rs),
  [Codex's Linux sandbox](https://github.com/openai/codex/blob/main/codex-rs/linux-sandbox/README.md),
  [sandbox-runtime](https://github.com/anthropic-experimental/sandbox-runtime).
- **Installers**: [rustup security](https://github.com/rust-lang/rustup/blob/main/doc/user-guide/src/security.md),
  [uv's download metadata](https://github.com/astral-sh/uv/blob/main/crates/uv-python/download-metadata.json),
  mise's [aqua backend](https://github.com/jdx/mise/blob/main/docs/dev-tools/backends/aqua.md)
  and [plugins](https://github.com/jdx/mise/blob/main/docs/plugins.md),
  [asdf plugin security](https://github.com/asdf-vm/asdf-plugins#security),
  Homebrew's [bottles](https://github.com/Homebrew/brew/blob/main/docs/Bottles.md)
  and [manpage](https://github.com/Homebrew/brew/blob/main/docs/Manpage.md),
  [tinytex-releases](https://github.com/rstudio/tinytex-releases),
  [TinyTeX's install script](https://github.com/rstudio/tinytex/blob/main/tools/install-bin-unix.sh),
  [tlmgr](https://github.com/TeX-Live/installer/blob/master/texmf-dist/scripts/texlive/tlmgr.pl)
  (package hashes, `--verify-repo`).

**Not verified**: whether Zed checks the commands an extension returns for the editor
to start (code search found no check); whether Zed can pull a version from installed
clients; VS Code's publisher signing status; whether HPC login nodes allow the
unprivileged user namespaces bubblewrap needs (it varies by site).
