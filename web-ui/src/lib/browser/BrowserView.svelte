<script lang="ts">
  /**
   * The browser pane: a live web app (Jupyter, marimo, Streamlit…) in an
   * iframe on the daemon's reverse proxy — same origin, so it works through
   * the ssh tunnel exactly like the rest of the workbench (localhost on the
   * remote is one click away from the laptop).
   *
   * The tab persists only the TARGET; the proxy session is minted here on
   * demand and re-minted whenever it expires, so daemon restarts heal
   * invisibly. The chrome stays quiet: back/reload/external + an address
   * field, with honest overlay states instead of raw browser error pages.
   *
   * In the app the page is not an iframe but a web view of its own laid over
   * the stage (`shared/paneView.ts`): its own web process, so a heavy app
   * can't freeze the window. Its location, title and focus arrive as events
   * instead of same-origin reads, history goes through the shell, and a
   * text selection in it can be referenced in an agent. The iframe stays the
   * browser's path and the fallback for an app build without pane views.
   *
   * The agent browser (#273): a pane shared with an agent (the chain link)
   * registers its view so the agent's browser tools can address it by tab,
   * shows who holds it at the trailing end of the bar ("driving" while the
   * agent acts, "Paused · Resume" after the person took over), and reports
   * the person's own input in the page as a take-over. A hand-over tab
   * (`adopt`) takes on the agent's live page instead of loading it again.
   */
  import { onDestroy, onMount, untrack } from "svelte";
  import { get } from "svelte/store";
  import Spinner from "../previews/Spinner.svelte";
  import { pageVisible } from "../shared/visibility";
  import { computeStatus } from "../workspace/compute";
  import { isWebUrl, openInSystemBrowser } from "../shared/urlOpen";
  import SessionGlyph from "../shared/SessionGlyph.svelte";
  import { agentHue } from "../workspace/agentLinks";
  import type { BrowserOpener } from "./agentOpen";
  import { agentBrowser } from "./agentBrowserRuntime";
  import { PaneViewControl, paneView, paneViewsPossible } from "../shared/paneView";
  import { referencePill } from "../shared/paneViewBridge";
  import {
    activeSelection,
    clearSelection,
    referenceTarget,
    requestReference,
    setSelection,
    type FileSelection,
  } from "../shared/reference";
  import { PINNED } from "../shared/keys";
  import { inAppPath, onRootForm, pageAddress, proxySessionOf } from "./frameLocation";
  import {
    ConfirmRequired,
    isLoopbackHost,
    mintProxy,
    parseAddress,
    probeNodes,
    proxyHealth,
    proxyTarget,
    setBrowserTitle,
    targetLabel,
  } from "./proxy";

  interface Props {
    /** The BrowserTab instance id (title-store key). */
    tabId: string;
    host: string;
    port: number;
    /** App-internal path+query to open at (persisted navigation state). */
    path: string;
    /** False while this view is a hidden keep-alive layer. */
    visible: boolean;
    /** Persist the current in-app path onto the tab. */
    onNavigate: (path: string) => void;
    /** Re-point this tab: the address bar named a different target
     *  (`found` false), or the compute-node hunt found this app on the node
     *  it runs on (`found` true). */
    onRetarget: (host: string, port: number, path: string, found: boolean) => void;
    /** A click landed inside the iframe: focus this pane. */
    onFocusRequest: () => void;
    /** The agent session that opened this pane (`open_browser`), as the
     *  roster knows it now; null for a pane the user opened. */
    opener?: BrowserOpener | null;
    /** Show the opening agent's session (its tab, revealed beside). */
    onRevealOpener?: () => void;
    /** A hand-over: the live pane view this tab takes on (no reload). */
    adopt?: string;
    /** The agent this pane is shared with (the chain link), as the roster
     *  knows it now; null when not shared. */
    sharedWith?: BrowserOpener | null;
    /** Show the agent it is shared with. */
    onRevealShared?: () => void;
  }

  let {
    tabId,
    host,
    port,
    path,
    visible,
    onNavigate,
    onRetarget,
    onFocusRequest,
    opener = null,
    onRevealOpener,
    adopt,
    sharedWith = null,
    onRevealShared,
  }: Props = $props();

  type Phase =
    | { kind: "blank" }
    | { kind: "connecting" }
    | { kind: "confirm"; detail: string }
    | { kind: "unreachable"; detail: string }
    | { kind: "ready" };

  let phase = $state<Phase>({ kind: "blank" });
  /** `/proxy/{id}` once minted (the iframe URL base). */
  let base = $state<string | null>(null);
  let proxyId = $state<string | null>(null);
  let iframeSrc = $state<string | null>(null);
  let iframeEl = $state<HTMLIFrameElement | null>(null);
  /** The live in-app location (address display); falls back to the tab's. */
  let livePath = $state<string | null>(null);
  /** Health went bad while the app was on screen (quiet banner, not a wipe). */
  let degraded = $state(false);
  /** The user confirmed this non-allowlisted target (survives re-mints). */
  let confirmed = false;

  let draft = $state<string | null>(null);
  let addressEl = $state<HTMLInputElement | null>(null);

  /** The page shows in a native pane view; false after the shell declined. */
  let native = $state(paneViewsPossible());
  /** The native view has yet to show `iframeSrc`'s page. */
  let viewLoading = $state(true);
  /** The native view's page process died for good (until a reload). */
  let viewCrashed = $state(false);
  const control = new PaneViewControl();
  /** The native view's page sits on a root-form (rescued) path. */
  let viewRootForm = false;

  // --- the agent browser ------------------------------------------------------

  /** The hand-over view this mount may still adopt (with the URL it was
   *  last shown at): read once, so a later view of this tab (a new target,
   *  a remount) makes its own. Gone already (the page closed, another
   *  window): this tab just loads its target. */
  let adoptable = $state<{ id: string; url: string } | undefined>(
    untrack(() => (adopt !== undefined ? (agentBrowser.adoption(adopt) ?? undefined) : undefined)),
  );
  /** The id of this tab's live pane view, as registered. */
  let viewId: string | null = null;

  function onViewId(id: string | null): void {
    if (id === null) {
      if (viewId !== null) agentBrowser.registry.unregister(tabId, viewId);
      viewId = null;
      return;
    }
    viewId = id;
    if (id === adoptable?.id) {
      adoptable = undefined;
      // The tab's `adopt` stays as it is: changing the layout here, mid
      // hand-over, must not disturb the view that just took the page on. A
      // later mount finds the adoption done and makes its own view.
      agentBrowser.adopted(tabId, id);
    } else {
      agentBrowser.registry.register(tabId, id);
    }
  }

  // A hand-over whose page is already gone (or already adopted by an
  // earlier mount): nothing to adopt, so this view loads its target.
  onMount(() => {
    if (adopt !== undefined && adoptable === undefined) agentBrowser.registry.dropAdoption(tabId);
  });

  const driveState = agentBrowser.driveState;
  const drive = $derived(sharedWith !== null ? $driveState.get(tabId) : undefined);
  const driving = $derived(drive?.driving === true && !drive.paused);
  const paused = $derived(drive?.paused === true);

  function resume(): void {
    agentBrowser.resume(tabId);
  }

  // --- the compute-node hunt --------------------------------------------------
  //
  // Apps started inside an allocation print `localhost` URLs, but their
  // localhost is the COMPUTE NODE's — the daemon's own loopback has nothing
  // there. So when a loopback target is unreachable on a Slurm host, probe
  // the same port on the user's running jobs' nodes (already allowlisted;
  // a hit's proven route stays cached): one answer moves the pane there with
  // a visible note, several become one-click choices. Probing is honest
  // spend — a few bounded dials — and silence stays silence.
  /** Nodes that answered on the target port (multi-hit choice chips). */
  let nodeHits = $state<string[]>([]);
  let probing = $state(false);
  /** One probe per unreachable episode per target. */
  let probedFor: string | null = null;
  /** The "moved from localhost:PORT" note shown after an auto-retarget — set
   *  the instant before we re-point (so it names where we came FROM), and
   *  deliberately NOT cleared by the target-change reset below. Cleared only
   *  by an explicit new address, or a click on the chip. */
  let moved = $state<string | null>(null);

  /** Single-node RUNNING allocations from the compute snapshot, deduped. */
  function nodeCandidates(): string[] {
    const snap = get(computeStatus);
    if (snap === null || snap.scheduler !== "slurm") return [];
    const out: string[] = [];
    for (const job of snap.jobs) {
      if (job.state !== "RUNNING") continue;
      const node = job.nodes.trim();
      if (node === "" || node.includes("[") || node.includes(",")) continue;
      if (node.toLowerCase() === host.toLowerCase()) continue;
      if (!out.includes(node)) out.push(node);
      if (out.length >= 4) break;
    }
    return out;
  }

  $effect(() => {
    if (phase.kind !== "unreachable" || !visible) return;
    const key = `${host}:${port}`;
    if (!isLoopbackHost(host) || probedFor === key) return;
    const candidates = nodeCandidates();
    // Mark probed ONLY once there are candidates: if the compute snapshot
    // hasn't populated yet (the common click-a-Jupyter-URL-at-startup race),
    // leave the one-shot armed so the next 5s reconnect re-probes once the
    // snapshot arrives — otherwise the pane would stay stuck on localhost.
    if (candidates.length === 0) return;
    probedFor = key;
    probing = true;
    void probeNodes(candidates, port).then((hits) => {
      if (probedFor !== key) return;
      probing = false;
      nodeHits = hits;
      if (hits.length === 1) {
        moved = targetLabel(host, port);
        onRetarget(hits[0], port, path, true);
      }
    });
  });

  function chooseNode(node: string): void {
    moved = targetLabel(host, port);
    onRetarget(node, port, path, true);
  }

  const address = $derived(
    draft ?? (host === "" ? "" : `${targetLabel(host, port)}${livePath ?? path}`),
  );

  /** Serial number guarding async flows against target changes mid-flight. */
  let epoch = 0;

  async function connect(): Promise<void> {
    const mine = ++epoch;
    if (host === "") {
      phase = { kind: "blank" };
      return;
    }
    phase = { kind: "connecting" };
    degraded = false;
    try {
      const session = await mintProxy(host, port, confirmed);
      if (mine !== epoch) return;
      proxyId = session.id;
      base = session.base;
      const health = await proxyHealth(session.id);
      if (mine !== epoch) return;
      if (!health.ok) {
        phase = {
          kind: "unreachable",
          detail: health.detail ?? `nothing is listening on ${targetLabel(host, port)}`,
        };
        return;
      }
      iframeSrc = `${session.base}${path.startsWith("/") ? path : `/${path}`}`;
      phase = { kind: "ready" };
    } catch (e) {
      if (mine !== epoch) return;
      if (e instanceof ConfirmRequired) {
        phase = { kind: "confirm", detail: e.message };
      } else {
        phase = { kind: "unreachable", detail: e instanceof Error ? e.message : "proxy error" };
      }
    }
  }

  // --- following the page to another target -----------------------------------
  //
  // An agent driving a shared pane navigates it with `location.assign` to
  // `/proxy/{id}…` — possibly another app (another session id). The pane
  // follows: the tab is re-pointed at that target so its title, address and
  // saved layout stay true, while the live view stays (no reload).

  /** A re-point this view asked for itself: the target effect takes the
   *  page as it is instead of starting over. */
  let following: { host: string; port: number; id: string; path: string; title: string } | null = null;
  let followSeq = 0;

  function followPage(u: URL, title: string): void {
    // A load this pane started is still settling: a late report from the
    // page it left is not a navigation.
    if (viewLoading) return;
    const session = proxySessionOf(u);
    if (session === null || session.id === proxyId) return;
    const mine = ++followSeq;
    void proxyTarget(session.id).then((target) => {
      if (mine !== followSeq || target === null || base === null) return;
      if (target.host === host && target.port === port) {
        // The same app on a fresh session: just its id changes.
        proxyId = session.id;
        base = `/proxy/${session.id}`;
        notePage(session.path, title);
        return;
      }
      following = { ...target, id: session.id, path: session.path, title };
      onRetarget(target.host, target.port, session.path, true);
    });
  }

  // (Re)connect whenever the target identity changes.
  $effect(() => {
    void host;
    void port;
    untrack(() => {
      const f = following;
      following = null;
      followSeq += 1;
      if (f !== null && f.host === host && f.port === port && phase.kind === "ready") {
        proxyId = f.id;
        base = `/proxy/${f.id}`;
        livePath = f.path;
        draft = null;
        nodeHits = [];
        degraded = false;
        pageSel = null;
        unpublish();
        // The view is already there: `paneView` sees the page's own URL and
        // only takes note of it.
        iframeSrc = `${base}${f.path}`;
        const t = f.title.trim();
        setBrowserTitle(tabId, t.length > 0 ? t : null);
        return;
      }
      livePath = null;
      draft = null;
      confirmed = false;
      nodeHits = [];
      probing = false;
      probedFor = null;
      // Drop the OLD target's iframe immediately. Leaving base/iframeSrc live
      // would keep the previous page mounted through the new `connecting`
      // phase, and a late onload — or the slow location poll — could read it
      // against the now-stale base and persist the wrong location onto the NEW target.
      // connect() sets them fresh once the new session is minted (codex R4).
      base = null;
      proxyId = null;
      iframeSrc = null;
      pageSel = null;
      unpublish();
      // `moved` is intentionally NOT reset here — an auto-move sets it right
      // before it re-points, and this reset runs as a consequence of that
      // very re-point; clearing it would erase the note we just set.
      setBrowserTitle(tabId, null);
      void connect();
    });
  });

  // An EXTERNAL navigation of the SAME target: openBrowser re-pointed this
  // pane's `path` at a freshly clicked URL (a new Jupyter `?token=`, a
  // different notebook route on the same host:port). Navigate the live iframe
  // there. Only `path` is tracked: reading livePath/phase/base untracked is
  // what breaks the feedback loop — the iframe reporting its OWN location sets
  // livePath and then converges `path` to it via onNavigate, and re-firing on
  // that would re-load the pre-redirect URL forever.
  $effect(() => {
    const p = path;
    untrack(() => {
      if (phase.kind !== "ready" || base === null || livePath === null) return;
      if (p === livePath) return;
      iframeSrc = `${base}${p.startsWith("/") ? p : `/${p}`}`;
    });
  });

  function confirmTarget(): void {
    confirmed = true;
    void connect();
  }

  // While unreachable and on screen, retry quietly — "I clicked the URL a
  // beat before Jupyter finished booting" must fix itself. Gated on document
  // visibility too: each probe is 2 HTTP round-trips, and a hidden window at
  // a dead URL must not keep paying them; the effect re-runs on return, so
  // the retry resumes (and connect() fires within 5s) once someone looks.
  $effect(() => {
    if (!visible || !$pageVisible || phase.kind !== "unreachable") return;
    const t = setInterval(() => void connect(), 5000);
    return () => clearInterval(t);
  });

  // Keep-alive + liveness while showing the app: refreshes the proxy
  // session's idle clock and notices a died server (banner) or an expired
  // session (transparent re-mint). Deliberately NOT gated on document
  // visibility (rules/web-ui.md exemption): the keep-alive is what stops the
  // daemon expiring the proxy session, and a hidden window must come back to
  // a working app, not an expired ticket. 1 request/min is the accepted cost.
  $effect(() => {
    if (!visible || phase.kind !== "ready") return;
    const t = setInterval(() => {
      const id = proxyId;
      if (id === null) return;
      void proxyHealth(id).then((h) => {
        if (proxyId !== id || phase.kind !== "ready") return;
        if (h.ok) {
          degraded = false;
        } else if (h.error === "expired") {
          // Daemon restarted or the session idled out: re-mint and re-point
          // the iframe (same target, fresh ticket).
          void connect();
        } else {
          degraded = true;
        }
      });
    }, 60_000);
    return () => clearInterval(t);
  });

  // --- iframe navigation tracking -------------------------------------------

  let titleObserver: MutationObserver | null = null;

  /**
   * Pull the iframe's in-app location into `livePath` + the address bar. Cheap
   * (a synchronous same-origin location read) and idempotent, so it is safe to
   * poll: same-document navigations — a hash-router route, JupyterLab's
   * History-API notebook switch — never fire `onload`, and the poll below is
   * the only thing that keeps the address + persisted path honest for them.
   * Returns the same-origin window when readable, so `readIframeState` can also
   * do its heavier onload-only work (the title observer).
   */
  function syncLocation(): Window | null {
    const el = iframeEl;
    const b = base;
    if (el === null || b === null) return null;
    try {
      const win = el.contentWindow;
      if (win === null) return null; // cross-origin: leave the address as-is
      // Not this app's page (about:blank before the first load commits, or
      // the previous target's page mid-retarget): never record it.
      const inApp = inAppPath(win.location, b);
      if (inApp === null) return null;
      // Keep the tab label honest too: a same-document route change can swap
      // the title, and re-reading it here (idempotent — setBrowserTitle
      // short-circuits an unchanged value) also heals any transient clear.
      notePage(inApp, win.document.title);
      return win;
    } catch {
      // The app navigated somewhere cross-origin; the address keeps the last
      // known in-app location.
      return null;
    }
  }

  /** Record the page's in-app location (persisted onto the tab) and title. */
  function notePage(inApp: string, title: string): void {
    if (inApp !== livePath) {
      livePath = inApp;
      if (inApp !== path) onNavigate(inApp);
    }
    const t = title.trim();
    setBrowserTitle(tabId, t.length > 0 ? t : null);
  }

  /** The native view loaded a page, or its title or same-document URL
   *  changed: the same bookkeeping the iframe's reads do. */
  function onViewPage(page: { url: string; title: string }): void {
    viewCrashed = false;
    const b = base;
    if (b === null) return;
    let u: URL;
    try {
      u = new URL(page.url, location.href);
    } catch {
      return;
    }
    if (u.origin !== location.origin) return;
    const inApp = inAppPath(u, b);
    if (inApp === null) {
      followPage(u, page.title);
      return;
    }
    viewRootForm = onRootForm(u, b);
    notePage(inApp, page.title);
  }

  function readIframeState(): void {
    const win = syncLocation();
    if (win === null) return;
    try {
      // Live title updates BETWEEN polls (an app that renames its own tab
      // without navigating) — the poll re-reads titles too, this just makes it
      // instant.
      const doc = win.document;
      titleObserver?.disconnect();
      const titleEl = doc.querySelector("title");
      if (titleEl !== null) {
        titleObserver = new MutationObserver(() => {
          const t = doc.title.trim();
          setBrowserTitle(tabId, t.length > 0 ? t : null);
        });
        titleObserver.observe(titleEl, { childList: true, characterData: true, subtree: true });
      }
    } catch {
      // went cross-origin between the location read and here — nothing to do
    }
  }

  // Same-document navigations don't reload the iframe, so `onload` never fires
  // for them; a slow poll is what keeps the address bar and persisted path in
  // step for hash-router / History-API apps. Real navigations are event-driven
  // (the iframe's onload → readIframeState), so this is only the fallback:
  // 5s of address-bar staleness on a same-document route change is fine, and
  // it runs only while both the pane AND the document are visible — with a
  // catch-up read when either returns (the effect re-runs; the read is a bare
  // same-origin property access, no I/O). untrack: syncLocation reads AND
  // writes `livePath` — tracking it would loop the effect.
  // A native view reports its own location changes (onViewPage): no poll.
  $effect(() => {
    if (native || !visible || !$pageVisible || phase.kind !== "ready") return;
    untrack(() => syncLocation());
    const t = setInterval(syncLocation, 5000);
    return () => clearInterval(t);
  });

  // Clicking into an iframe never bubbles to the pane: notice the focus move
  // via the window losing focus TO our iframe, and hand the pane focus over.
  $effect(() => {
    const onBlur = () => {
      if (iframeEl !== null && document.activeElement === iframeEl) onFocusRequest();
    };
    window.addEventListener("blur", onBlur);
    return () => window.removeEventListener("blur", onBlur);
  });

  onDestroy(() => {
    // A hand-over not taken on yet stays the channel's: a remount (a zoom,
    // a pane move) adopts it next; App closes it once the tab is gone.
    if (viewId !== null) agentBrowser.registry.unregister(tabId, viewId);
    titleObserver?.disconnect();
    setBrowserTitle(tabId, null);
    unpublish();
  });

  // --- pointing at part of the page (context bridge; native view only) -------
  //
  // A text selection in the page goes to an agent like a file selection,
  // its "file" the app's own address (`@http://localhost:8888/lab (§ Intro)
  // "…"`): the bridge has no kind for a web page, and an agent reads a URL
  // for what it is. Offered by the pill the shell draws in the page.

  const selOwner = {};
  let published: FileSelection | null = null;
  /** The page's text selection as last reported: re-offered by its pill
   *  after a newer selection elsewhere took the bridge. */
  let pageSel: { text: string; context: string | null } | null = null;

  function unpublish(): void {
    if (published === null) return;
    published = null;
    clearSelection(selOwner);
  }

  function publishPageSelection(): void {
    const s = pageSel;
    if (s === null || host === "") return;
    const sel: FileSelection = {
      kind: "file",
      path: pageAddress(host, port, livePath ?? path),
      startLine: null,
      endLine: null,
      text: s.text,
      ...(s.context !== null ? { context: `§ ${s.context}` } : {}),
      label: "selection",
    };
    published = sel;
    setSelection(selOwner, sel);
  }

  function onViewSelection(sel: { text: string; context: string | null }): void {
    if (sel.text.trim() === "") {
      pageSel = null;
      unpublish();
      return;
    }
    pageSel = { text: sel.text, context: sel.context };
    publishPageSelection();
  }

  /** The in-page pill: reference the page's own selection, even when a
   *  newer one elsewhere had taken the bridge since. */
  function onViewReference(): void {
    if (pageSel !== null && published === null) publishPageSelection();
    requestReference();
  }

  // A newer selection elsewhere (another view, a terminal) replaces this one.
  $effect(() => {
    const a = $activeSelection;
    if (published !== null && a !== published) published = null;
  });

  // The in-page pill wears the shared chip's words and follows the target.
  $effect(() => {
    control.setChip(referencePill($referenceTarget, undefined, PINNED.reference));
  });

  // --- chrome actions ---------------------------------------------------------

  function back(): void {
    if (native) {
      control.nav("back");
      return;
    }
    try {
      iframeEl?.contentWindow?.history.back();
    } catch {
      // cross-origin content: the browser refuses; nothing to do
    }
  }

  function forward(): void {
    if (native) {
      control.nav("forward");
      return;
    }
    try {
      iframeEl?.contentWindow?.history.forward();
    } catch {
      // cross-origin content: the browser refuses; nothing to do
    }
  }

  function reload(): void {
    if (phase.kind === "unreachable" || phase.kind === "blank") {
      void connect();
      return;
    }
    if (native) {
      viewCrashed = false;
      // A page on a rescued root-form path can't be reloaded as it is (a
      // top-level document there is the workbench's, never the app's):
      // load the same app location under the proxy prefix instead.
      const prefixed = base !== null && livePath !== null ? `${base}${livePath}` : null;
      if (viewRootForm && prefixed !== null && prefixed !== iframeSrc) iframeSrc = prefixed;
      else control.nav("reload");
      return;
    }
    try {
      iframeEl?.contentWindow?.location.reload();
    } catch {
      // cross-origin content: re-point the iframe at the last app location
      if (base !== null) iframeSrc = `${base}${livePath ?? path}`;
    }
  }

  /** Pop the proxied URL into a real browser tab (it rides the same tunnel,
   *  so it works for remote localhost too). */
  function openExternal(): void {
    if (base === null) return;
    // Through the shell in the native app — a bare window.open goes nowhere
    // there (the navigation guard admits only the daemon origin).
    openInSystemBrowser(`${location.origin}${base}${livePath ?? path}`);
  }

  function commitAddress(): void {
    const entered = (draft ?? "").trim();
    const parsed = parseAddress(entered);
    draft = null;
    addressEl?.blur();
    if (parsed === null) {
      // Not something the pane can serve — an https app (the upstream hop is
      // clear-text) or a plain web URL. Hand it to the real browser rather
      // than swallowing the entry silently.
      if (isWebUrl(entered)) openInSystemBrowser(entered);
      return;
    }
    // A deliberate address is the user moving on — drop any auto-move note.
    moved = null;
    if (parsed.host === host && parsed.port === port) {
      if (base !== null && phase.kind === "ready") {
        iframeSrc = `${base}${parsed.path}`;
      } else {
        onNavigate(parsed.path);
        void connect();
      }
      return;
    }
    onRetarget(parsed.host, parsed.port, parsed.path, false);
  }

  function onAddressKeydown(e: KeyboardEvent): void {
    e.stopPropagation(); // workbench chords stay out of the address field
    if (e.key === "Enter") {
      e.preventDefault();
      commitAddress();
    } else if (e.key === "Escape") {
      e.preventDefault();
      draft = null;
      addressEl?.blur();
    }
  }
</script>

<div class="browser">
  <div class="chrome">
    <button class="nav" title="back" aria-label="back" onclick={back} disabled={phase.kind !== "ready"}>
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M10 3 5 8l5 5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    <button class="nav" title="forward" aria-label="forward" onclick={forward} disabled={phase.kind !== "ready"}>
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="m6 3 5 5-5 5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    <button class="nav" title="reload" aria-label="reload" onclick={reload} disabled={phase.kind === "blank"}>
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.8V5h-2.2" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    <input
      class="address"
      type="text"
      spellcheck="false"
      autocomplete="off"
      placeholder="localhost:8888 — or paste a URL"
      value={address}
      bind:this={addressEl}
      oninput={(e) => (draft = e.currentTarget.value)}
      onfocus={(e) => e.currentTarget.select()}
      onkeydown={onAddressKeydown}
      onblur={() => (draft = null)}
    />
    {#if moved !== null}
      <button
        class="moved"
        title="the app printed {moved}, but it answers on this compute node — chimaera connected there (click to dismiss)"
        onclick={() => (moved = null)}
      >
        moved from {moved}
      </button>
    {/if}
    {#if degraded}
      <span class="degraded" title="the server stopped answering — the view stays until you reload">unreachable</span>
    {/if}
    <button
      class="nav"
      title="open in browser tab"
      aria-label="open in browser tab"
      onclick={openExternal}
      disabled={base === null || phase.kind !== "ready"}
    >
      <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
        <path d="M6.5 3H3.8A1.3 1.3 0 0 0 2.5 4.3v7.9a1.3 1.3 0 0 0 1.3 1.3h7.9a1.3 1.3 0 0 0 1.3-1.3V9.5M9.5 2.5H13.5V6.5M13.2 2.8 7.5 8.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" />
      </svg>
    </button>
    <!-- Who holds this page: the agent it is shared with (its hue, its
         name; a breathing dot while it acts, "Paused · Resume" after the
         person took over), else who put it here. Trailing the bar so
         nothing the user aims at moves when it appears. A mousedown never
         takes focus; the name reveals that agent. -->
    {#if sharedWith !== null}
      <span
        class="held"
        class:driving
        class:paused
        class:ended={!sharedWith.live}
        style:--hue={agentHue(sharedWith.id)}
        role="group"
        aria-label="shared with {sharedWith.label ?? 'an agent'}"
      >
        <button
          class="held-main"
          title={paused
            ? `paused — you took over from ${sharedWith.label ?? "the agent"}`
            : driving
              ? `${sharedWith.label ?? "the agent"} is driving this page — act in it to take over`
              : `shared with ${sharedWith.label ?? "an agent"}: its browser tools can read and use this page — click to show it`}
          aria-label={paused
            ? `paused, you took over from ${sharedWith.label ?? "the agent"} — show the agent`
            : driving
              ? `${sharedWith.label ?? "the agent"} is driving — show the agent`
              : `shared with ${sharedWith.label ?? "an agent"} — show the agent`}
          onmousedown={(e) => e.preventDefault()}
          onclick={() => onRevealShared?.()}
        >
          <span class="held-dot" aria-hidden="true"></span>
          <span class="held-text">
            {#if paused}<span class="held-state">Paused</span>{:else}<span class="held-name">{sharedWith.label ?? "agent"}</span>{#if driving}<span class="held-state"> driving</span>{/if}{/if}
          </span>
        </button>
        {#if paused}
          <span class="held-sep" aria-hidden="true">·</span>
          <button
            class="held-resume"
            title="hand the page back to {sharedWith.label ?? 'the agent'}"
            onmousedown={(e) => e.preventDefault()}
            onclick={resume}>Resume</button
          >
        {/if}
      </span>
    {:else if opener !== null}
      {#if opener.live}
        <button
          class="opener"
          style:--hue={agentHue(opener.id)}
          title="opened by {opener.label} — click to show it"
          aria-label="opened by {opener.label} — show this agent"
          onmousedown={(e) => e.preventDefault()}
          onclick={() => onRevealOpener?.()}
        >
          <SessionGlyph kind="agent" agentKind={opener.agentKind} size={10} />
          <span class="opener-text"><span class="opener-by">opened by</span> <span class="opener-name">{opener.label}</span></span>
        </button>
      {:else}
        <span
          class="opener ended"
          title={opener.label !== null
            ? `opened by ${opener.label}, an agent session that has ended`
            : "opened by an agent session that has ended"}
        >
          <SessionGlyph kind="agent" agentKind={opener.agentKind} size={10} />
          <span class="opener-text">{opener.label !== null ? `opened by ${opener.label} · ended` : "opened by an agent that has ended"}</span>
        </span>
      {/if}
    {/if}
  </div>

  <div class="stage">
    {#if phase.kind === "blank"}
      <div class="state">
        <div class="state-title">Open a running web app</div>
        <div class="state-detail">
          Jupyter, marimo, Streamlit, RStudio… — type its address above, like
          <code>localhost:8888</code>, or click a printed URL in any terminal.
        </div>
      </div>
    {:else if phase.kind === "connecting"}
      <div class="state"><Spinner /></div>
    {:else if phase.kind === "confirm"}
      <div class="state">
        <div class="state-title">Proxy to {targetLabel(host, port)}?</div>
        <div class="state-detail">
          {phase.detail !== "" ? phase.detail : "This host isn't this machine or one of your compute nodes."}
          The daemon will forward traffic there on your behalf.
        </div>
        <button class="action" onclick={confirmTarget}>connect</button>
      </div>
    {:else if phase.kind === "unreachable"}
      <div class="state">
        <div class="state-title">Can't reach {targetLabel(host, port)}</div>
        <div class="state-detail">{phase.detail}. Retrying quietly — is the server running?</div>
        {#if probing}
          <div class="state-detail probe">checking your compute nodes…</div>
        {:else if nodeHits.length > 1}
          <div class="state-detail probe">
            something answers on port {port} on your compute nodes:
          </div>
          <div class="node-hits">
            {#each nodeHits as node (node)}
              <button class="action" onclick={() => chooseNode(node)}>{node}</button>
            {/each}
          </div>
        {/if}
        <button class="action" onclick={() => void connect()}>retry now</button>
      </div>
    {/if}
    {#if iframeSrc !== null && (phase.kind === "ready" || phase.kind === "connecting") && native}
      <!-- The loading state sits INSIDE the placeholder: paneView treats a
           positioned status element elsewhere over it as an overlay. -->
      <div
        class="frame native-slot"
        class:hidden={phase.kind !== "ready"}
        use:paneView={{
          url: iframeSrc,
          shown: visible && phase.kind === "ready",
          onFallback: () => (native = false),
          onLoading: (l) => {
            viewLoading = l;
            // A load under way means the page is no longer the crashed one.
            if (l) viewCrashed = false;
          },
          onCrashed: () => (viewCrashed = true),
          onFocus: onFocusRequest,
          onSelection: onViewSelection,
          onReference: onViewReference,
          onPage: onViewPage,
          control,
          adopt: adoptable,
          onViewId,
          onUserInput: () => agentBrowser.userInput(tabId),
        }}
      >
        {#if viewCrashed}
          <div class="state">
            <div class="state-title">This page stopped responding</div>
            <button
              class="action"
              onclick={() => {
                viewCrashed = false;
                control.nav("reload");
              }}>Reload</button
            >
          </div>
        {:else if viewLoading && phase.kind === "ready"}<Spinner />{/if}
      </div>
    {:else if iframeSrc !== null && (phase.kind === "ready" || phase.kind === "connecting")}
      <iframe
        class="frame"
        class:hidden={phase.kind !== "ready"}
        src={iframeSrc}
        title={targetLabel(host, port)}
        allow="clipboard-read; clipboard-write; fullscreen"
        bind:this={iframeEl}
        onload={readIframeState}
      ></iframe>
    {/if}
  </div>
</div>

<style>
  .browser {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
  }

  .chrome {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 3px 6px;
    border-bottom: 1px solid var(--edge);
    background: var(--bg);
    flex: none;
    /* The attribution collapses to its mark in a narrow pane (below). */
    container: browser-chrome / inline-size;
  }

  /* The opening agent: the linked-terminal chip's shape (mono pill) with
     only its mark in the agent's link hue — quieter than the address it
     sits beside; the page title stays the tab's label. */
  .opener {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 18px;
    max-width: 260px;
    min-width: 0;
    margin-left: 4px;
    padding: 0 8px 0 6px;
    border: 1px solid var(--edge);
    border-radius: 9px;
    background: none;
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    line-height: 1;
    cursor: pointer;
    user-select: none;
  }

  .opener :global(.sglyph) {
    color: hsl(var(--hue) 55% 55%);
  }

  /* The ended note is not a control (and has no hue): it never lights up. */
  .opener:not(.ended):hover {
    border-color: hsl(var(--hue) 45% 55% / 0.5);
    background: color-mix(in srgb, var(--fg) 5%, transparent);
  }

  .opener-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .opener-by {
    color: var(--muted);
  }

  /* Its agent has ended: nothing to show, so no control — a muted note. */
  .opener.ended {
    border-style: dashed;
    color: var(--muted);
    cursor: default;
  }

  .opener.ended :global(.sglyph) {
    color: var(--muted);
  }

  /* Below this the address needs the room: the mark alone, words in its
     title (the hover tooltip every chrome control uses). */
  @container browser-chrome (max-width: 520px) {
    .opener {
      padding: 0 5px;
    }

    .opener-text {
      display: none;
    }
  }

  /* The agent holding a shared pane: the opener chip's shape, wearing the
     agent's hue on its border so the bond reads at a glance. The dot says
     what it is doing: a quiet hue disc idle, a breathing one while it
     drives, a hollow ring while paused. */
  .held {
    flex: none;
    display: inline-flex;
    align-items: center;
    height: 18px;
    max-width: 260px;
    min-width: 0;
    margin-left: 4px;
    border: 1px solid hsl(var(--hue) 45% 55% / 0.45);
    border-radius: 9px;
    background: hsl(var(--hue) 50% 55% / 0.08);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    line-height: 1;
    user-select: none;
  }

  .held.ended {
    border-style: dashed;
    border-color: var(--edge);
    background: none;
    color: var(--muted);
  }

  .held-main,
  .held-resume {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    color: inherit;
    cursor: pointer;
    height: 100%;
    display: inline-flex;
    align-items: center;
    min-width: 0;
    border-radius: 9px;
  }

  .held-main {
    gap: 5px;
    padding: 0 8px 0 6px;
  }

  .held.paused .held-main {
    padding-right: 3px;
  }

  .held-main:hover {
    background: hsl(var(--hue) 50% 55% / 0.1);
  }

  .held-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .held-state {
    color: var(--muted);
  }

  .held-dot {
    position: relative;
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: hsl(var(--hue) 40% 55% / 0.6);
  }

  .held.ended .held-dot {
    background: none;
    border: 1px solid var(--muted);
  }

  .held.driving .held-dot {
    background: hsl(var(--hue) 65% 55%);
  }

  /* Breathing via transform/opacity on a static halo (composited), like the
     linked-terminal chip's exec pulse. */
  .held.driving .held-dot::after {
    content: "";
    position: absolute;
    inset: -3.5px;
    pointer-events: none;
    border-radius: 50%;
    background: hsl(var(--hue) 65% 55% / 0.35);
    opacity: 0;
    transform: scale(0.5);
    animation: held-pulse 1.4s ease-in-out infinite;
  }

  @keyframes held-pulse {
    50% {
      opacity: 1;
      transform: scale(1);
    }
  }

  /* Hidden document: nobody sees the pulse (the html.app-hidden contract). */
  :global(html.app-hidden) .held.driving .held-dot::after {
    animation-play-state: paused;
  }

  @media (prefers-reduced-motion: reduce) {
    .held.driving .held-dot::after {
      animation: none;
    }
  }

  .held.paused .held-dot {
    background: none;
    border: 1.5px solid hsl(var(--hue) 60% 55%);
  }

  .held-sep {
    color: var(--muted);
  }

  .held-resume {
    padding: 0 8px 0 4px;
    color: var(--accent-text);
    font-weight: 600;
  }

  .held-resume:hover {
    color: var(--fg);
  }

  .held-main:focus-visible,
  .held-resume:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: -2px;
  }

  /* A narrow pane keeps the dot (and Resume, the one thing to act on). */
  @container browser-chrome (max-width: 520px) {
    .held:not(.paused) .held-text {
      display: none;
    }

    .held:not(.paused) .held-main {
      padding: 0 5px;
    }
  }

  .nav {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border: none;
    border-radius: 5px;
    background: none;
    color: var(--muted);
    cursor: pointer;
    padding: 0;
  }

  .nav:hover:not(:disabled) {
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    color: var(--fg);
  }

  .nav:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .address {
    flex: 1;
    min-width: 0;
    margin: 0 4px;
    padding: 2px 8px;
    height: 20px;
    border: 1px solid transparent;
    border-radius: 5px;
    background: color-mix(in srgb, var(--fg) 5%, transparent);
    color: var(--fg);
    font-family: var(--mono);
    font-size: var(--text-xs);
    outline: none;
  }

  .address:focus {
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
    background: var(--term-bg);
  }

  .address::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .degraded {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--err, #c66);
    padding: 0 6px;
    user-select: none;
  }

  /* The "moved from localhost" note: quiet accent chip, click to dismiss. */
  .moved {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    border: none;
    border-radius: 4px;
    padding: 1px 6px;
    cursor: pointer;
    user-select: none;
  }

  .probe {
    font-size: var(--text-xs);
    opacity: 0.85;
  }


  .node-hits {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
    justify-content: center;
  }

  .stage {
    flex: 1;
    position: relative;
    min-height: 0;
  }

  .frame {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    border: none;
    background: transparent;
  }

  .frame.hidden {
    visibility: hidden;
  }

  .state {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.55rem;
    padding: 0 2rem;
    text-align: center;
    user-select: none;
  }

  .state-title {
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
  }

  .state-detail {
    color: var(--muted);
    font-size: var(--text-sm);
    max-width: 30rem;
    line-height: 1.55;
  }

  .state-detail code {
    font-family: var(--mono);
    font-size: var(--text-xs);
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 0.3rem;
  }

  .action {
    border: 1px solid var(--edge);
    border-radius: 5px;
    padding: 0.25rem 0.7rem;
    color: var(--fg);
    background: var(--bg);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }

  .action:hover {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }
</style>
