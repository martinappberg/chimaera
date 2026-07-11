# Drag-to-reference & desktop uploads

Getting a **path** or a **file** into the session you're talking to, by dragging. Two intake
edges share one destination — the live session's input (a chat composer or a PTY):

- **Left-tree drag-to-reference** — drag a file *or folder* from the file tree onto a live-session
  pane's **"@ reference"** band; the path types into that session (agents get an `@mention`, shells
  a shell-quoted path). No bytes move — the file is already on the session's host.
- **OS-desktop drop / screenshot paste** — drop a file from the desktop (or paste a clipboard
  screenshot) onto a session; the bytes **stream to the daemon that owns the session** (so a
  remote window lands the file on the remote host, over the ssh tunnel), then the returned path
  is referenced exactly like a tree drag.

**Where it lives (shared):** UI `web-ui/src/App.svelte` (window drop handlers, `referenceFileDrop`,
`insertUploadedPath`, `dropFilesOnSession`), `web-ui/src/lib/net/uploads.ts` (upload client +
job chrome), `web-ui/src/lib/chat/images.ts` (downscale/encode), `web-ui/src/lib/layout/dnd.ts`
(`refPath` gate, `paneIdAt`), `web-ui/src/lib/workspace/FileTree.svelte`, `Pane.svelte` (the
overlays), `Terminal.svelte` (paste-capture), `web-ui/src/lib/chat/composerBus.ts`. Daemon:
`crates/chimaera-server/src/upload.rs`. Native shell: `crates/chimaera-app/src/shell/restore.rs`.
Wire: `POST /api/v1/sessions/{id}/upload?name=`. Reuses `web-ui/src/lib/shared/reference.ts`
(`composeAgentPathReference`/`composeShellPathReference`) — the same path-composition as terminal
selection references (see [terminals.md](terminals.md)).

## Folder drag-to-reference

- **What & when.** Reference a *directory* (not just a file) in a live session by dragging its row
  from the left file tree — e.g. `@src/lib/` into an agent, or a shell-quoted dir path into a
  terminal.
- **How it's used.** Drag any tree row over a live-session pane; its lower ~22% grows the
  **"@ reference"** band; release there to type the path. A sub-threshold release (a plain click)
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
  unambiguously as "this folder"; shell paths stay bare (what a command expects). Relativity is
  per-target: workspace-relative for agents, live-cwd-relative for shells, absolute fallback
  outside either root — identical to file references. Each dir drag mints a fresh Finder id, so
  dropping the same folder onto a zone twice opens two Finder tabs (files dedupe by path); a
  reference drop opens nothing. Pure client — no wire change.

## OS-desktop file drops

- **What & when.** Drop a file from the desktop/Finder onto a live-session pane (uploads into
  the session's uploads dir + references the path) OR onto a **file-manager folder** — a Finder
  column or a FILES-tree directory — which uploads the file **into that folder** on the daemon.
  The folder target wins over the session pane it may sit inside. The daemon that owns the
  session/window receives the bytes, so an agent (or a shell) on a remote host can read a file
  you dragged from your laptop. Folder uploads go through `POST /api/v1/fs/upload?dir=&name=`
  (`upload::upload_to_dir`), stream the same way, cap per-file at 32 MB, then bump the fs epoch
  so the tree/Finder re-list. In-app **move between folders** is done via copy/cut/paste (a
  pointer-drag folder-move is a follow-up); dragging files **out to the desktop** is punted —
  the pointer-drag stack plus remote-over-tunnel bytes make it not worth it in two of three
  runtimes; the per-row Download menu covers remote→local retrieval.
- **What & when (session pane).** Drop a file onto a live-session pane. The daemon that
  owns the session receives the bytes and the path is referenced in that session — so an agent (or
  a shell) on a remote host can read a file you dragged from your laptop.
- **How it's used.** Drag a file over the app; a whole live-session pane lights as the
  **"@ drop to upload & reference"** target (HTML5 dnd has no competing tile gesture, so the whole
  pane is the zone, not a bottom band). Release to upload each file and type its returned path. An
  in-flight (or briefly failed) upload shows a quiet chip toast bottom-center. Dropping an **image**
  onto a *chat* pane additionally attaches its pixels to the composer (the model sees it now); the
  uploaded path stays the durable, host-side artifact the agent can re-read later.
- **Where it lives.** `App.svelte` registers **window-level** `dragenter/dragover/dragleave/drop`
  in `onMount` (torn down in the returned cleanup — global listeners must not leak). Every
  `dragover`/`drop` `preventDefault`s **unconditionally** — the browser's default for an unhandled
  file drop is to *navigate away from the app*. `paneIdAt` (`dnd.ts`) hit-tests the drop point
  against registered pane geometry; `dropFilesOnSession` uploads via
  `uploads.ts::uploadAndInsert` then inserts through `insertUploadedPath`. Daemon route
  `upload::upload` (`router.rs`; a per-route `DefaultBodyLimit` override lifts axum's 2 MB buffered
  default).
- **Key behaviors.** The route **streams** the body chunk-by-chunk to a hidden tmp sibling then
  renames (a partial upload is never visible under its final name) — the whole file **never sits in
  RAM** (the daemon lives on shared login nodes). Hard caps: **32 MB per file, 256 MB per session
  dir**; overflow aborts, deletes the tmp, and answers `413`. Filenames sanitize to a strict
  basename (no separators, no control bytes, no dot-dirs, ≤200 chars) before becoming a path on a
  shared host; a taken name dedupes with a short random prefix instead of clobbering. Uploads land
  in `~/.chimaera/uploads/<session-id>/` and are **session-lifetime**: pruned on `DELETE`
  (`sessions.rs` + `recents::retire`), on close-all/shutdown (`shutdown.rs`), and swept at boot for
  sessions that died unwatched (`spawn_boot_prune`). Bearer-authed like every REST route; an unknown
  session id `404`s and never mints a directory. A drop caps at **8 files**; **folders** dropped
  from the OS are rejected with a toast (recursive upload is a follow-up).

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
  owning daemon regardless. One line, no new IPC command, no capability change.

---

## Intent — human-authored ground truth

> Captured from the people who built these features via the **capture-feature-intent**
> skill when a `feat:` ships in this area. **Never** inferred from code. Everything above
> this line is derived and may be regenerated; everything below is deliberate and must not
> be "helpfully" changed without asking.

### Drag-to-reference & uploads — why it exists
_Intent: **pending.** Not yet captured from the maintainer._

Open questions for capture: whether the **reference-not-copy** stance for tree drags (paths, never
bytes) is a firm bet or provisional; whether **OS drops should ever bypass the upload** for a local
window (insert the native path directly) or always stream for uniformity; the per-file/per-session
**caps** (32 MB / 256 MB) and the **8-files-per-drop** limit; whether **folder** OS-drops should
gain recursive upload; and whether chat image drops should keep **dual-writing** (pixels to the
model *and* an uploaded path) or pick one.
