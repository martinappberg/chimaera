/**
 * The rail's Recents section: the active workspace's ended agent
 * conversations, how many rows the collapsed section fits, expand/collapse,
 * archive (one, or all with a ~6 s Undo), and reopening a row.
 *
 * App constructs one `RecentsRail` and calls `trackFit()` and
 * `trackWorkspace()` where those effects used to be declared (the fit
 * observer early, the workspace-switch refetch later), so the window's effect
 * order is unchanged; App's teardown calls `dispose()`. Requests and their
 * order are as before: an archive POSTs and, only on failure, refetches; Undo
 * un-archives and then refetches.
 */
import { tick } from "svelte";
import { archiveRecents, unarchiveRecents } from "./history";
import { listRecents, type RecentConvo } from "./launcher";
import { displayName, worktreeInUse, type AgentSpawn, type Session } from "./sessions";

export interface RecentsDeps {
  activeWsId: () => string | null;
  sessions: () => Session[];
  /** Start an agent (App's spawn path). */
  startAgent: (spawn: AgentSpawn) => void;
  /** An archived row's managed worktree is no longer in use. */
  offerWorktreeRemoval: (ws: string, path: string, branch: string | null) => void;
}

/** The collapsed section's floor, mirrored by `.recents-list`'s min-height
 *  (3 rows): below it the sessions column scrolls rather than hiding history. */
const RECENTS_MIN_ROWS = 3;
/** How long "Archived N · Undo" stays after Archive all. */
const UNDO_MS = 6000;

function archiveEntry(r: RecentConvo) {
  return { key: r.key, kind: r.kind, title: r.title, resume: r.resume, ui: r.ui };
}

export class RecentsRail {
  recents = $state<RecentConvo[]>([]);
  expanded = $state(false);
  /** Recents rows the collapsed section can show without clipping. The
   *  section fills the height the sessions column has left (`.recents` is
   *  the column's flex: 1), so a short roster on a tall window shows a long
   *  history and a full roster falls back to the CSS floor (3 rows) and the
   *  column scrolls. Measured, never guessed: a ResizeObserver on the list
   *  reports its box; the first two rows give the row pitch. Only the count
   *  is state, so a resize that doesn't change it costs nothing. */
  fit = $state(RECENTS_MIN_ROWS);
  /** The list element; App's markup binds it (`bind:this`), the one write
   *  to this class's state from outside its methods. */
  listEl = $state<HTMLElement | null>(null);
  /** "Archived 12 · Undo" after Archive all, for ~6 s. */
  archivedUndo = $state<{ ws: string; keys: string[] } | null>(null);

  /** Recents rows glide up when one is archived (the dashboard roster's
   *  flip idiom); zero under reduced motion. */
  readonly flipMs =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches
      ? 0
      : 220;

  /** Recents to SHOW: drop any whose conversation is currently LIVE (a live
   *  agent with the same title). A running conversation belongs under AGENTS,
   *  not RECENT — and the server's live-exclusion can briefly miss a fresh
   *  session whose transcript id isn't recorded yet, so a row would otherwise
   *  appear in both sections at once. */
  readonly visible = $derived.by(() => {
    const liveTitles = new Set(
      this.#deps
        .sessions()
        .filter((s) => s.kind === "agent" && s.alive)
        .map((s) => displayName(s)),
    );
    return this.recents.filter((r) => r.title === "" || !liveTitles.has(r.title));
  });

  /** Monotone fetch counter: a slow early /recents response must not
   *  clobber a newer one. */
  #seq = 0;
  #undoTimer: ReturnType<typeof setTimeout> | null = null;
  readonly #deps: RecentsDeps;

  constructor(deps: RecentsDeps) {
    this.#deps = deps;
  }

  measure = (): void => {
    const el = this.listEl;
    // The expanded list is taller by design (it scrolls); measuring it would
    // inflate the fit and hide the "show less" toggle.
    if (el === null || this.expanded) return;
    const first = el.firstElementChild;
    if (!(first instanceof HTMLElement)) return;
    const second = first.nextElementSibling;
    const pitch =
      second instanceof HTMLElement ? second.offsetTop - first.offsetTop : first.offsetHeight + 1;
    if (pitch <= 0) return;
    // The last row needs no trailing gap: a box exactly N rows tall fits N.
    const gap = pitch - first.offsetHeight;
    const fit = Math.max(RECENTS_MIN_ROWS, Math.floor((el.clientHeight + gap) / pitch));
    if (fit !== this.fit) this.fit = fit;
  };

  /** Declares the fit observer's effect on the list element. */
  trackFit(): void {
    $effect(() => {
      const el = this.listEl;
      if (el === null) return;
      const ro = new ResizeObserver(this.measure);
      ro.observe(el);
      return () => ro.disconnect();
    });
  }

  /** Expanded, the section wants more height than the column may have left
   *  (the live roster comes first), so the column scrolls it into view —
   *  else "show all" can change nothing visible. Collapse scrolls back. */
  toggle = (): void => {
    this.expanded = !this.expanded;
    const section = this.listEl?.parentElement;
    const column = section?.parentElement;
    if (section == null || column == null) return;
    void tick().then(() => {
      // Scroll the sessions column itself — never scrollIntoView, which
      // would also shift the overflow-hidden rail around it.
      const top = section.getBoundingClientRect().top - column.getBoundingClientRect().top;
      column.scrollTo({ top: this.expanded ? column.scrollTop + top : 0 });
      // Collapsing back to a box the same size as before fires no resize;
      // the column may have changed meanwhile, so re-fit explicitly.
      if (!this.expanded) this.measure();
    });
  };

  /** Reload the workspace's ended agent conversations. Retire timing needs
   *  no guessing: the daemon pushes a `recents` epoch frame the moment a
   *  conversation lands in the store, and this refetches on it. This path
   *  also runs on agent-set changes — a conversation resumed into a live
   *  session must drop off the list even though the store didn't change. */
  refresh = (): void => {
    const ws = this.#deps.activeWsId();
    if (ws === null) {
      this.recents = [];
      return;
    }
    const seq = ++this.#seq;
    listRecents(ws)
      .then((r) => {
        if (this.#deps.activeWsId() === ws && seq === this.#seq) this.recents = r;
      })
      .catch(() => {
        // rail stays on its last list; the next snapshot retries
      });
  };

  /** Declares the workspace-switch effect: collapse and refetch. */
  trackWorkspace(): void {
    $effect(() => {
      void this.#deps.activeWsId();
      this.expanded = false;
      this.refresh();
    });
  }

  /** Archive hides a conversation from Recents — never deletes it; it stays
   *  in All sessions under "Archived". Optimistic: the row animates out now,
   *  and a failure refetches the truth. No confirm, no toast. */
  async archive(r: RecentConvo): Promise<void> {
    const ws = this.#deps.activeWsId();
    if (ws === null) return;
    this.recents = this.recents.filter((x) => x.key !== r.key);
    if (r.worktree !== null && !worktreeInUse(this.#deps.sessions(), r.worktree)) {
      this.#deps.offerWorktreeRemoval(ws, r.worktree, null);
    }
    try {
      await archiveRecents(ws, [archiveEntry(r)]);
    } catch {
      this.refresh();
    }
  }

  async archiveAll(): Promise<void> {
    const ws = this.#deps.activeWsId();
    const rows = this.visible;
    if (ws === null || rows.length === 0) return;
    const gone = new Set(rows.map((r) => r.key));
    this.recents = this.recents.filter((x) => !gone.has(x.key));
    try {
      const keys = await archiveRecents(ws, rows.map(archiveEntry));
      if (this.#undoTimer !== null) clearTimeout(this.#undoTimer);
      this.archivedUndo = { ws, keys };
      this.#undoTimer = setTimeout(() => {
        this.archivedUndo = null;
        this.#undoTimer = null;
      }, UNDO_MS);
    } catch {
      this.refresh();
    }
  }

  /** Undo restores exactly the set Archive all hid. */
  async undoArchiveAll(): Promise<void> {
    const undo = this.archivedUndo;
    if (undo === null) return;
    this.archivedUndo = null;
    if (this.#undoTimer !== null) clearTimeout(this.#undoTimer);
    this.#undoTimer = null;
    try {
      await unarchiveRecents(undo.ws, undo.keys);
    } finally {
      this.refresh();
    }
  }

  /** A Recents row: resume when the daemon captured a native conversation
   *  handle, else an honest fresh start (the tooltip says why). The row's
   *  title rides along so the restored conversation keeps its name instead of
   *  showing a bare "claude" until a new turn regenerates one. */
  open = (r: RecentConvo): void => {
    const titleHint = r.title !== "" ? r.title : undefined;
    // Reopening is independent of the preference for NEW conversations.
    // Legacy/scanned CLI histories predate an explicit chat choice.
    // Preserve their terminal surface instead of migrating them on an update.
    const ui = r.ui ?? "term";
    this.#deps.startAgent(
      r.resume !== null
        ? { agent: r.kind, resume: r.resume, titleHint, ui }
        : { agent: r.kind, titleHint, ui },
    );
  };

  tooltip(r: RecentConvo): string {
    return r.resume !== null
      ? `resume “${r.title}”`
      : `no saved conversation handle was recorded — starts a fresh ${r.kind} session`;
  }

  /** App teardown: the Undo timer. */
  dispose(): void {
    if (this.#undoTimer !== null) clearTimeout(this.#undoTimer);
  }
}
