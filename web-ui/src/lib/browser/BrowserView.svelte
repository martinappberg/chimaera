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
   */
  import { onDestroy, untrack } from "svelte";
  import { get } from "svelte/store";
  import Spinner from "../previews/Spinner.svelte";
  import { computeStatus } from "../workspace/compute";
  import { isWebUrl, openInSystemBrowser } from "../shared/urlOpen";
  import {
    ConfirmRequired,
    isLoopbackHost,
    mintProxy,
    parseAddress,
    probeNodes,
    proxyHealth,
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
    /** The address bar named a different target: re-point this tab. */
    onRetarget: (host: string, port: number, path: string) => void;
    /** A click landed inside the iframe: focus this pane. */
    onFocusRequest: () => void;
  }

  let { tabId, host, port, path, visible, onNavigate, onRetarget, onFocusRequest }: Props =
    $props();

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
    probedFor = key;
    const candidates = nodeCandidates();
    if (candidates.length === 0) return;
    probing = true;
    void probeNodes(candidates, port).then((hits) => {
      if (probedFor !== key) return;
      probing = false;
      nodeHits = hits;
      if (hits.length === 1) {
        moved = targetLabel(host, port);
        onRetarget(hits[0], port, path);
      }
    });
  });

  function chooseNode(node: string): void {
    moved = targetLabel(host, port);
    onRetarget(node, port, path);
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

  // (Re)connect whenever the target identity changes.
  $effect(() => {
    void host;
    void port;
    untrack(() => {
      livePath = null;
      draft = null;
      confirmed = false;
      nodeHits = [];
      probing = false;
      probedFor = null;
      // `moved` is intentionally NOT reset here — an auto-move sets it right
      // before it re-points, and this reset runs as a consequence of that
      // very re-point; clearing it would erase the note we just set.
      setBrowserTitle(tabId, null);
      void connect();
    });
  });

  function confirmTarget(): void {
    confirmed = true;
    void connect();
  }

  // While unreachable and on screen, retry quietly — "I clicked the URL a
  // beat before Jupyter finished booting" must fix itself.
  $effect(() => {
    if (!visible || phase.kind !== "unreachable") return;
    const t = setInterval(() => void connect(), 5000);
    return () => clearInterval(t);
  });

  // Keep-alive + liveness while showing the app: refreshes the proxy
  // session's idle clock and notices a died server (banner) or an expired
  // session (transparent re-mint).
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

  function readIframeState(): void {
    const el = iframeEl;
    const b = base;
    if (el === null || b === null) return;
    try {
      const win = el.contentWindow;
      const doc = el.contentDocument;
      if (win === null || doc === null) return; // cross-origin: leave as-is
      const raw = `${win.location.pathname}${win.location.search}`;
      // Prefixed navigations carry /proxy/{id}; rescued (absolute-path) ones
      // land on root-form paths. Both name the same app location. A /proxy/
      // path under a DIFFERENT id is the previous target's iframe still on
      // screen mid-retarget — never record it as this target's location.
      if (raw.startsWith("/proxy/") && !raw.startsWith(b)) return;
      const inApp = raw.startsWith(b) ? raw.slice(b.length) || "/" : raw;
      livePath = inApp;
      if (inApp !== path) onNavigate(inApp);
      const title = doc.title.trim();
      setBrowserTitle(tabId, title.length > 0 ? title : null);
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
      // The app navigated somewhere cross-origin; the address keeps the last
      // known in-app location.
    }
  }

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
    titleObserver?.disconnect();
    setBrowserTitle(tabId, null);
  });

  // --- chrome actions ---------------------------------------------------------

  function back(): void {
    try {
      iframeEl?.contentWindow?.history.back();
    } catch {
      // cross-origin content: the browser refuses; nothing to do
    }
  }

  function forward(): void {
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
    onRetarget(parsed.host, parsed.port, parsed.path);
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
    {#if iframeSrc !== null && (phase.kind === "ready" || phase.kind === "connecting")}
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
