# File dragging, references & desktop uploads

Getting a **path** or a **file** into the session you're talking to, by dragging. Three intake
edges share one destination — the live session's input (a chat composer or a PTY):

- **Left-tree drag-to-reference** — drag a file *or folder* from the file tree onto a live-session
  pane's **"@ reference"** band; the path types into that session (agents get an `@mention`, shells
  a shell-quoted path). No bytes move — the file is already on the session's host.
- **A file a chat already shows** — drag an attachment tile, an embedded figure, a written-file
  chip or a mention chip from the transcript onto the composer, or right-click it and choose
  **Reference in chat**. Nothing is opened and nothing is copied: the file is on the session's host.
- **OS-desktop drop / screenshot paste** — drop a file from the desktop (or paste a clipboard
  screenshot) onto a session; the bytes **stream to the daemon that owns the session** (so a
  remote window lands the file on the remote host, over the ssh tunnel), then the returned path
  is referenced exactly like a tree drag. In the native app on the daemon's own computer the file
  is referenced **where it is** instead: no copy is made.

**Where it lives (shared):** UI `web-ui/src/App.svelte` (window drop handlers, `referenceFileDrop`,
`insertUploadedPath`, `dropFilesOnSession`), `web-ui/src/lib/net/uploads.ts` (job chrome + the
callers' entry points), `web-ui/src/lib/net/chunkedUpload.ts` (the transport),
`web-ui/src/lib/chat/images.ts` (downscale/encode), `web-ui/src/lib/layout/dnd.ts`
(`refPath` gate, `paneIdAt`), `web-ui/src/lib/workspace/FileTree.svelte`, `Pane.svelte` (the
overlays), `Terminal.svelte` (paste-capture), `web-ui/src/lib/chat/composerBus.ts`. Daemon:
`crates/chimaera-server/src/upload.rs` + `upload/chunked.rs`. Native shell:
`crates/chimaera-app/src/shell/restore.rs`, `shell/commands.rs::dropped_paths`. Files shown on a
surface: `web-ui/src/lib/shared/fileHandle.ts`; local drops: `web-ui/src/lib/net/localDrop.ts`.
Wire: `/api/v1/uploads` (begin, `PUT` a piece, `GET`
the offset, `finish`, `DELETE`); the one-request `POST /api/v1/sessions/{id}/upload?name=` and
`POST /api/v1/fs/upload` stay for scripts. Reuses `web-ui/src/lib/shared/reference.ts`
(`composeAgentPathReference`/`composeShellPathReference`) — the same path-composition as terminal
selection references (see [terminals.md](terminals.md)).

## Move and copy between folders

- **How it's used.** Drag a file or folder from the FILES tree or a Finder row onto a
  folder, a Finder column/background, or a Finder breadcrumb. File-preview tabs can also
  be dragged onto folders. Release to **move**; hold **Option/Alt** to **copy**. The
  destination lights up and the drag hint names the operation. Hover a closed folder
  for 700 ms to open it; the tree and Finder scroll when held near their edges.
  Escape or losing window focus cancels the gesture. A plain click keeps its usual action.
- **Targets stay distinct.** File-manager targets take priority over pane split zones.
  Other panes and tab bars still open/reposition the surface; a session's reference band
  still inserts the path. Finder *tabs* rearrange the browsing surface rather than moving
  the folder it happens to display.
- **Safety.** Same-parent moves and folder-into-itself drops show a reason and do nothing.
  Moves never overwrite an existing destination; copies use the daemon's unique-name
  policy. Symlink and permission checks remain authoritative on the daemon. Progress and
  errors use the shared file-operation notifications. Open tabs and editor buffers follow
  successful moves through the existing filesystem mutation bus.
- **Where it lives.** `layout/dnd.ts` owns shared `folderTargetAt` hit testing, the `fileOp`
  drop spot, modifier tracking, and edge scrolling. `App.svelte` routes the transfer;
  `workspace/fileTransfer.ts` shares validation and execution with clipboard paste.
  `FileTree.svelte` and `previews/FinderView.svelte` own folder highlights and hover opening.
  Reuses `POST /api/v1/fs/move` and `/api/v1/fs/copy`; no wire change.

## Folder drag-to-reference

- **What & when.** Reference a *directory* (not just a file) in a live session by dragging its row
  from the left file tree or a Finder row — e.g. `@src/lib/` into an agent, or a shell-quoted dir path into a
  terminal.
- **How it's used.** Drag any tree or Finder row over a live-session pane; its lower ~22% grows the
  **"@ reference in ⟨session⟩"** band (named, so two session panes side by side read differently)
  and the drag ghost's hint says the same; release there to type the path. A sub-threshold release (a plain click)
  keeps the tree's own action — open the file, or expand/collapse the folder — so a click never
  also references and a completed drag never also opens.
- **Where it lives.** `FileTree.svelte` starts a pointer drag for **both** kinds, passing the kind
  and the sub-threshold click action. `App.svelte` `onTreeEntryDown` drags a file as a
  `{surface:"file"}` tab and a dir as a **fresh Finder tab** (`freshFinderTab`, `layout.ts`) — so a
  drop on a pane *zone* or tab-bar opens a legitimate Finder browsing surface, never a broken
  file preview. `dnd.ts` arms the reference band on `DragPayload.refPath` (payloads opt in
  explicitly; both file and finder payloads set it) instead of hard-coding a surface.
  `referenceFileDrop` composes the text.
- **Key behaviors.** Directory `@mentions` carry a **trailing slash** (`@src/lib/`) — reads
  unambiguously as "this folder"; shell paths stay bare (what a command expects). A path claude's
  bare `@path` would not read whole — whitespace, or an ending its word boundary cuts (`notes(1)`,
  a non-ASCII last letter) — is mentioned in claude's quoted form, `@"raw data/"` (what claude's
  own completion writes for a spaced path); every agent mention the app composes (drops, uploads,
  selection references, paste provenance, the composer's `@` picks) goes through one writer,
  `reference.ts` `agentMention`, and the chat's link parser reads the quoted form back as one link
  (`extractFileRefs`). Shells get such a path single-quoted. Relativity is
  per-target: workspace-relative for agents, live-cwd-relative for shells, absolute fallback
  outside either root — identical to file references. Each dir drag mints a fresh Finder id, so
  dropping the same folder onto a zone twice opens two Finder tabs (files dedupe by path); a
  reference drop opens nothing. Pure client — no wire change.

## A file a chat already shows

- **What & when.** A picture you sent, a figure the agent embedded, a file on the "Written this
  turn" line or a file mention in a message can be referenced again in the reply you are writing,
  without opening it first.
- **How it's used.** Drag it onto the composer: the pane's **"@ reference in ⟨session⟩"** band
  lights, and releasing types the file's mention (an upload reads as its short name, a workspace
  file as its workspace-relative path). Or right-click it: **Open**, **Open Beside**, **Reference
  in chat**, and **Reveal in Finder** where the files are this computer's. Dropped on another
  pane it opens there, like a FILES-tree row; it is never moved or copied by a drag. A plain
  click keeps its action (open).
- **Where it lives.** `shared/fileHandle.ts`: the `fileHandle` attachment (a pointer drag and a
  context menu on one element), `fileMenuEntries` (the rows, pure), `referenceScope` (which chat a
  surface belongs to, read from the composer's `data-chat-session`), and the host App registers
  (`shared/fileHandleHost.ts` `setFileHandleHost`, a leaf so the handle's menu and listeners stay
  out of the always-loaded bundle: `beginDrag` with a file tab and no file-transfer source, and
  `referencePathIn`). Attached by `AttachmentStrip` (sent tiles), `FileChip` (the turn-end line and
  prose document chips), `UserText` (resolved mention chips), and `EmbedCard` (its name, and an
  image or PDF body; text bodies stay selectable). A prose file link gets the menu row from
  `Markdown.svelte`'s own link menu.
- **Key behaviors.** The handle is on the element that takes the click: a drag captures the pointer
  on its source, and browsers send a captured press's click to that element. Mouse only, so a
  finger on a figure still scrolls. A gone file is not a handle. Outside a chat (an embed card in a
  document) the menu references into the window's reference target, and has no row when there is
  none. Pure client: no wire change.

## Dropping onto the composer

- **The band is the input area.** The "@ reference" and "link to agent" bands take the pane's
  lower ~22%, and grow to hold the whole composer when it is taller (a long draft, attachments, a
  short pane), up to half the pane (`dnd.ts::bandFraction`, measured from the composer's
  `data-drop-band`; `Pane.svelte` draws the wash with the same share). A release anywhere on the
  composer therefore references; with a fixed share the top of a tall composer was the "add to
  this pane" zone.
- **An agent is a reference too.** An agent's tab or rail row dragged over another agent's pane
  arms the same band there (never on its own pane, never on a shell); the drop types
  `@agent:<session id>` and rearranges nothing
  ([agent-communication.md](agent-communication.md#referencing-an-agent-in-a-message)).
- **A drop is not a click.** The browser makes a click of the release that ends a drag, on the
  element that captured the pointer (Chromium and the native app's WebKit alike). A tab dropped on the band was referenced and then activated by
  its own click handler, and a linked terminal's rail row took the focus the link had handed the
  agent. `dnd.ts` swallows that one click for every drag source (`swallowDragClick`); a release
  before the drag threshold is still a click.

## OS-desktop file drops

- **What & when.** Drop a file from the desktop/Finder onto a live-session pane (uploads into
  the session's uploads dir + references the path) OR onto a **file-manager folder** — a Finder
  column or a FILES-tree directory — which uploads the file **into that folder** on the daemon.
  The folder target wins over the session pane it may sit inside. The daemon that owns the
  session/window receives the bytes, so an agent (or a shell) on a remote host can read a file
  you dragged from your laptop. Folder uploads use the same transport with the folder as the
  destination, take a file of up to 64 GB, then bump the fs epoch so the tree/Finder re-list.
  In-app moves use dragging or copy/cut/paste (above).
  Dragging files **out to the desktop** is punted —
  the pointer-drag stack plus remote-over-tunnel bytes make it not worth it in two of three
  runtimes; the per-row Download menu covers remote→local retrieval.
- **What & when (session pane).** Drop a file onto a live-session pane. The daemon that
  owns the session receives the bytes and the path is referenced in that session — so an agent (or
  a shell) on a remote host can read a file you dragged from your laptop.
- **How it's used.** Drag a file over the app; the exact destination lights **and is named**, so
  a drop is never a whole-pane mystery. A live-session pane lights end to end as the
  **"@ upload & reference in ⟨session⟩"** target (HTML5 dnd has no competing tile gesture, so the
  whole pane is the zone, not a bottom band). Over the file manager only the target lights: a
  Finder **column** rings + washes; a Finder **directory row** under the pointer rings *instead*
  of its column (the row is the target — file rows are not targets themselves, their column is);
  the FILES tree lights the hovered folder the same way (its pinned ancestor rows are targets
  too). The Finder pane itself wears a quiet dashed frame ("this pane is receiving") with one
  `upload into ⟨dir⟩/` chip at its foot — the chip lives on the pane, never inside a column, so
  hovering can't grow a column's content mid-drag — while the column/row is what says *where*.
  Release to upload each file and type its returned path. An
  in-flight upload shows progress plus a cancel action in a quiet chip bottom-center; the
  percentage counts bytes the host has confirmed. If the connection drops, the chip reads
  "Waiting for the connection" and the upload goes on from where the host stands once it is back;
  after two minutes without a byte getting through it fails and says how far it got. A file the
  host cannot take (too big for the destination, no room on its disk) is refused before anything is
  sent, with the reason. A failed or cancelled upload leaves nothing on the host. Dropping an **image**
  onto a *chat* pane that advertises image input additionally attaches its pixels to the composer; the
  uploaded path stays the durable, host-side artifact the agent can re-read later.
- **On this computer, a dropped file is referenced where it is.** In the native app, on a window
  whose daemon runs on the same computer (`native.ts::hasLocalFiles`: the connection the shell
  opened the window on, never a host-name guess), a file or folder dropped on a session pane is
  not uploaded: its own path is typed (workspace-relative inside the workspace, absolute outside
  it), and a folder is referenced with a trailing slash instead of being refused. The wash reads
  **"@ reference in ⟨session⟩"**. A picture dropped on a chat still attaches its pixels. A remote
  window, a browser, a paste of image bytes, and a drop whose path the shell cannot name (a file
  promised by another app) upload as before. A web view never tells the page where a dropped file
  lives, so the page asks the shell (`dropped_paths`: the system drag pasteboard, macOS only) and
  pairs each answer with its dropped `File` by name, kind and size (`net/localDrop.ts`
  `pairDroppedPaths`, `routeDrop`); an entry it cannot pair is ignored. Linux has no such
  report, so there a local drop uploads and the wash says so
  (`native.ts::referencesLocalDrops`). A drop on a
  file-manager folder is still a copy into that folder.
- **In a chat, a dropped file reads as its name, where you referenced it.** The composer shows the
  mention as `@Screenshot-….png` in place of the ~100-character landing-pad path, as a quiet pill
  (every `@` mention in the draft gets one); resting the pointer on an upload's pill shows the path it
  landed at. The short form edits as **one unit**, like a chip: a click inside it lands at its
  nearer edge, the arrow keys step over it, Backspace/Delete at its edge removes all of it, a
  selection or double-click takes it whole, text typed or pasted against its end gets a space first,
  and deleting the space that keeps it apart from the next word does nothing. **Copy and cut carry
  the whole path**, and pasting a whole landing-pad mention shows it short again. The send (and the
  saved draft) puts the whole `@/…/uploads/<session>/<name>` back, so the agent receives exactly the
  text a drop always typed — only what the draft shows changed (`chat/uploadTokens.ts`,
  `chat/uploadChips.ts`, `chat/ComposerMentions.svelte`). Typed and picked mentions stay editable
  text. In the sent bubble **every** `@` file mention reads as a chip in place — icon + name, like a
  document the prose names, widened with its folder only where two share a name, a folder's `/` and a
  `#L…` locator kept — clickable while it resolves, the whole mention in its tooltip, in a copy, and
  in the text the agent got (`chat/UserText.svelte`, `paths.ts::mentionChipLabel`).
- **Where it lives.** `App.svelte` registers **window-level** `dragenter/dragover/dragleave/drop`
  in `onMount` (torn down in the returned cleanup — global listeners must not leak). Every
  `dragover`/`drop` `preventDefault`s **unconditionally** — the browser's default for an unhandled
  file drop is to *navigate away from the app*. `paneIdAt` (`dnd.ts`) hit-tests the drop point
  against registered pane geometry; `osFolderTargetAt` resolves folder targets from the
  attributes the surfaces stamp — a `[data-drop-dir]` row inside the tree, the tree root, or a
  Finder `[data-finder-dir]` column, where a `[data-drop-dir]` **dir row** inside it wins and sets
  `uploadDir.row` (additive DropSpot field) so `FinderView` (`dropDir` + `dropOnRow` props, passed
  down by `Pane.svelte`) lights the row alone, never also a column showing the same dir. `dragover`
  only rewrites `dropSpot` on a logical change (`sameSpot`). `dropFilesOnSession` uploads via
  `uploads.ts::uploadAndInsert` then inserts through `insertUploadedPath`. The bytes go through
  `chunkedUpload.ts::uploadChunked` to the daemon's `upload::chunked` routes (`router.rs`).
- **How the bytes travel.** In pieces, one at a time: the next piece is sent only after the daemon
  answered the last. A remote window's requests all ride one ssh connection, and a whole file
  handed to it at once filled that connection's send queue for the entire transfer — the terminals,
  the chat and the app's own connection check each waited seconds behind it (about 4 s at 1 MB/s,
  16 s at 250 KB/s), long enough for the app to judge the connection dead and rebuild it under the
  upload. With one piece in flight, sized from the link's measured speed to take about half a
  second (16 KB to 8 MB), nothing waits longer than that. A piece that gets no answer is asked about
  (`GET /uploads/{id}` gives the daemon's offset) and sent again from there, so a drop costs one
  piece and no piece is applied twice (the daemon accepts a piece only at its own offset). Every
  piece carries its SHA-256; `finish` carries a running digest of all of them, which the daemon
  compares with its own, then it syncs the file and renames it into place — so a full disk or an
  exceeded quota on a network filesystem is an error, never a short file. The daemon forgets its
  uploads when it restarts: the file is then sent once more from the start, and a temp file an
  interrupted upload left behind is removed by the next daemon from a record under its data
  dir (`uploads-open/`). Open uploads are capped at 32 and expire after 30 idle minutes.
- **Key behaviors.** Each piece **streams** to a hidden tmp sibling, renamed at the end (a partial
  upload is never visible under its final name) — the whole file **never sits in RAM** on either
  side (the daemon lives on shared login nodes; the page holds one piece). Hard caps: **64 MB per
  session file, 256 MB and 256 files per session dir**, checked when the upload opens (`413` with
  the sizes in words, before a byte is sent); completion rechecks the aggregate quotas including
  in-flight temp files, and overflow aborts, deletes the tmp, and answers `413`. Filenames sanitize to a strict
  basename (no separators, no control bytes, no dot-dirs, ≤200 chars) before becoming a path on a
  shared host, then land under a **mention-safe** form (`upload::mention_safe_name`): whitespace
  and the punctuation that ends or splits an `@mention` (claude's `#` line anchor and `:` resource
  form, quotes, brackets, globs, the UI parser's separators, `…`) collapse to one `-`, so
  `Screenshot 2026-09-26 at 12.30.png` lands as `Screenshot-2026-09-26-at-12.30.png` and the typed
  mention names the whole file for claude (TUI and chat), codex, and the UI's own link parser.
  Non-ASCII letters stay (a name that still ends in one is typed in claude's quoted form); a
  leading `-` goes, and a stem of nothing but separators becomes `upload` (`???.png` →
  `upload.png`, never a dotfile). The cases live in one fixture both sides test
  (`web-ui/src/lib/net/uploadNames.fixture.json`: the Rust test pins the rename, the Vitest suite
  reads each result back through `extractFileRefs` and a pinned copy of claude's extractor). A
  taken name dedupes with a short random prefix instead of clobbering. Uploads land
  in `~/.chimaera/uploads/<session-id>/` and are **session-lifetime**: pruned on `DELETE`
  (`sessions.rs` + `recents::retire`), on close-all/shutdown (`shutdown.rs`), and swept at boot for
  sessions that died unwatched (`spawn_boot_prune`). Temp names use exclusive creation and
  exhausted collision retries fail instead of falling back to a known name. Bearer-authed like every REST route; an unknown
  session id `404`s and never mints a directory. A drop caps at **8 files**; **folders** dropped
  from the OS are rejected with a toast (recursive upload is a follow-up). User-chosen Finder/tree
  uploads are distinct: they land outside daemon-owned state, keep the user's name **verbatim**,
  stay constant-memory and atomic, and accept up to **64 GB per file** (refused up front when the
  destination filesystem reports less free space than the file needs). Reading the dropped `Blob`
  piece by piece in the page is deliberately the common transport rather than `rsync`: it works
  without extra host tools and is remote-correct in both the native app and an ordinary browser,
  where the source file's local path is not available. The one-request routes
  (`POST /sessions/{id}/upload`, `POST /fs/upload`, 2 GB) remain for scripts; they refuse an
  over-cap `Content-Length` before reading the body and sync before the rename too.

## Screenshot / clipboard-image paste into a terminal

- **What & when.** Paste a clipboard image (a screenshot) into a **terminal** pane. A PTY can't take
  pixels, so the image uploads to the session's host and its shell-quoted path types at the cursor.
- **How it's used.** `Cmd/Ctrl+V` an image into a focused terminal. Chat-composer image paste
  already worked (base64 over the chat WS — see [chat-mode.md](chat-mode.md)); this adds the
  terminal path.
- **Where it lives.** `Terminal.svelte` `onpastecapture` fires **before** xterm's own paste
  handler, and diverts **only** when the clipboard holds an `image/*` and **no** `text/plain` — a
  normal text paste flows to the PTY untouched. It uploads via `uploads.ts` (`pastedImageName`
  stamps `pasted-<timestamp>.png`) and types the returned path.
- **Key behaviors.** Same upload route, caps, and streaming as OS drops. Only an image-and-no-text
  clipboard is intercepted, so text paste is never swallowed.

## Native app (Tauri)

- **What & when.** The same drop path works in the native shell as in the browser.
- **How it's used / where it lives.** `restore.rs` builds every window with
  `.disable_drag_drop_handler()`. Tauri 2's default drag-drop handler intercepts OS file drops and
  **suppresses the webview's DOM drop events**; disabling it hands HTML5 dnd back to the web UI, so
  one code path runs everywhere.
- **Key behaviors.** Remote-correct **by construction**: the native path a Tauri drag-drop event
  would carry is meaningless on a *remote* host, whereas the upload path streams the bytes to the
  owning daemon regardless. One addition for local windows: `shell/commands.rs::dropped_paths`
  (local-only, `require_local`) reads the file URLs of AppKit's drag pasteboard, which keeps the
  last drag's contents after the drop, so the page can reference a dropped file in place. It
  reports each entry's path, name, size and kind, at most 32, and nothing off macOS. An entry
  the app cannot open is not reported (a file in Downloads, Desktop or Documents before macOS
  was told the app may read that folder: the open is what makes macOS ask), so it uploads
  instead of becoming a reference the agent cannot read.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Drag-to-reference & uploads — why it exists
_Captured 2026-07-11 (from the maintainer, confirming a draft read from code + the open-questions list)._

- **Problem it solves.** Get a **path** or a **file** into the session you're talking to by dragging —
  and do it **remote-transparently**: bytes stream to the daemon that *owns* the session, so an agent
  (or shell) on a remote host can read a file you dragged off your laptop. Two intake edges, one
  destination: a tree drag **references** (paths, no bytes move — the file is already on the host); an
  OS-desktop drop **uploads then references**.
- **How settled it is (resolved with the maintainer).**
  - **Reference-not-copy for tree drags** (paths, never bytes) — **core**: it is the point of a
    workbench that lives *on* the host. Firm.
  - A local-window OS-drop **always streams** through the upload — it never bypasses to insert a raw
    native path — for uniformity and remote-correctness.
  - **Chat image drops keep dual-writing** — the pixels go to the model *now*, and the uploaded
    host-side path stays as the durable artifact the agent can re-read later. Keep both.
  - The **caps** (32 MB/file · 256 MB/session · 8 files/drop) are **provisional** guardrails for
    login-node RSS — free to tune.
  - **Folder OS-drops → recursive upload** is a **deliberate follow-up**, not a non-goal (folders are
    rejected with a toast for now).
- **Do not change:** the **remote-transparent** streaming (bytes to the *owning* daemon, so remote
  windows land files on the remote host) and the **never-in-RAM** streaming (the daemon lives on
  shared login nodes). Everything else here — caps, limits, the folder follow-up — is an improvable
  addition.

### Named drop destinations — why they exist
_Intent pending — drafted from the maintainer's request, 2026-09-06; questionnaire not yet run._

- **Problem it solves (from the request).** "When dropping in files you don't really see where in
  the file manager you are dropping your files. Make sure that all draggable actions are clear
  about where you should drag them so the UI is intuitive." Every drop target now lights the
  exact thing that receives the drop (a Finder column or dir row, a tree folder row, a session
  pane) and names it; pointer-drag ghosts say what releasing will do.
- **Pending.** The label vocabulary ("split right", "add to this pane", "@ reference in ⟨session⟩",
  "link to ⟨agent⟩", "upload into ⟨dir⟩/") and the Finder row-beats-column rule have not been
  confirmed with the maintainer — capture via **capture-feature-intent** when available.

### Dropped files read as their name, and mentions read as chips — why it exists
_Captured 2026-09-28 (from the maintainer, choosing among drafts built from their own words while
the feature was designed with them)._

- **Problem it solves.** A dropped screenshot put ~100 characters of machine-made landing-pad path in
  front of what the user wanted to write, and it looked bad. Chimaera is full of referencing (`@`
  files, drops, quotes), so a reference should read the way a document does in text — a name where
  it was written, the details on hover — without breaking how referencing works.
- **How settled it is.**
  - **Core bets:** the agent receives **exactly the full mention**, and the reference stays
    **where it was written** in the sentence; a copy gives the full path.
  - **Additions (improvable):** everything about the look — the composer pills, the chip editing
    behavior, the bubble chips, the hover tip. Change them freely if something reads better.
- **Deliberately open.** A real **hover preview** of the file (like the chat's document hovers) in
  place of today's name-and-path tip.
- **Do not change:** **what the agent receives** — the whole `@/…/uploads/<session>/<name>` mention
  (or the typed mention), in place. The display may change; the sent text may not.
