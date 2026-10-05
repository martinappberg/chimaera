<script lang="ts">
  import { untrack } from "svelte";
  import ModNode from "./ModNode.svelte";
  import { CLIENT_FRAME, CLIENT_WORKER, clientBundle, reserveClientWorker, clientKeyEvent, clientPointerEvent, clientViewport } from "./modClient";
  import { isRecord, parseUiTree, type UiNode, type UiRecord } from "./nativeUi";
  import type { ModsController } from "./mods.svelte";
  import { pageVisible } from "../shared/visibility";
  let { node, mods, component, instanceId, hash, suspended = false }: { node: UiNode; mods: ModsController; component: string; instanceId: string; hash?: unknown; suspended?: boolean } = $props();
  let frame = $state<HTMLIFrameElement>();
  let tree = $state<UiNode | null>(null);
  let error = $state<string | null>(null);
  let ready = $state(false);
  let loadedFrame = $state<HTMLIFrameElement>();
  let body = $state<HTMLDivElement>();
  let keyListener = $state(false);
  let pointerListener = $state(false);
  let pointerFrame: number | null = null;
  let nextPointer: UiRecord | null = null;
  let lastPointer = { x: 0, y: 0 };
  let executionTimer: ReturnType<typeof setTimeout> | null = null;
  let port: MessagePort | null = null;
  let serial = 0;
  const pending = new Map<number, { resolve: () => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  const target = $derived({ plugin: node.client?.plugin, component, instance_id: instanceId, client: node.props.key, module: node.props.module });
  const moduleName = $derived(String(node.props.module ?? ""));
  const pluginName = $derived(node.client?.plugin);
  const clientKey = $derived(String(node.props.key ?? ""));
  const paused = $derived(suspended || !$pageVisible);
  function viewport(): { columns: number; rows: number } {
    if (!body) return { columns: 80, rows: 24 };
    const styles = getComputedStyle(body);
    const fontSize = Number.parseFloat(styles.fontSize) || 14;
    const line = Number.parseFloat(styles.lineHeight) || fontSize * 1.45;
    const site = body.closest(".mod-site");
    return clientViewport(body.clientWidth, site?.clientHeight || body.clientHeight || 24 * line, fontSize, line);
  }
  function stop(reason = "Claude Mod surface closed"): void {
    ready = false;
    port?.postMessage({ type: "stop" }); port?.close(); port = null;
    for (const entry of pending.values()) { clearTimeout(entry.timer); entry.reject(new Error(reason)); }
    pending.clear();
    if (executionTimer) { clearTimeout(executionTimer); executionTimer = null; }
    if (pointerFrame !== null) { cancelAnimationFrame(pointerFrame); pointerFrame = null; }
    nextPointer = null; keyListener = false; pointerListener = false;
  }
  function fail(reason: string): void { error = reason.slice(0, 1000); stop(reason); }
  function send(message: UiRecord, timeout = 1500): Promise<void> {
    if (!port) return Promise.reject(new Error("Claude Mod surface disconnected"));
    if (pending.size >= 32) return Promise.reject(new Error("Claude Mod surface is busy"));
    const id = ++serial;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject, timer: setTimeout(() => fail("Claude Mod surface stopped responding"), timeout) });
      // Svelte wraps render props in proxies; only plain JSON crosses realms.
      port!.postMessage(JSON.parse(JSON.stringify({ ...message, id })));
    });
  }
  $effect(() => {
    const window = frame?.contentWindow;
    const version = hash;
    const module = moduleName;
    const plugin = pluginName;
    const key = clientKey;
    if (!window || !plugin || loadedFrame !== frame) return;
    void key;
    if (typeof version !== "string") { error = "This Mod has no loaded surface module"; return; }
    const releaseWorker = reserveClientWorker();
    if (!releaseWorker) { error = "Claude Mod surface limit reached (16 active surfaces)"; return; }
    let canceled = false;
    let startup: ReturnType<typeof setTimeout> | null = null;
    let count = 0;
    let windowStart = Date.now();
    const props = untrack(() => node.props.props ?? {});
    tree = null; error = null; ready = false;
    void clientBundle(mods.transport, plugin, version, module).then((bundle) => {
      if (canceled) return;
      const channel = new MessageChannel(); port = channel.port1;
      port.onmessage = ({ data }) => {
        if (!isRecord(data)) return;
        const now = Date.now(); if (now - windowStart > 1000) { windowStart = now; count = 0; }
        if (++count > 512) { fail("Claude Mod surface exceeded its update limit"); return; }
        if (data.type === "execution") {
          if (executionTimer) clearTimeout(executionTimer);
          executionTimer = data.active ? setTimeout(() => fail("Claude Mod surface exceeded its one-second execution limit"), 1000) : null;
        }
        if (data.type === "error") { fail(String(data.message ?? "Claude Mod surface failed")); return; }
        if (data.type === "ack" && typeof data.id === "number") { const request = pending.get(data.id); if (request) { clearTimeout(request.timer); pending.delete(data.id); request.resolve(); } }
        if (data.type === "response_error" && typeof data.id === "number") { const request = pending.get(data.id); if (request) { clearTimeout(request.timer); pending.delete(data.id); request.reject(new Error(String(data.message))); } }
        if (data.type === "ready") { ready = true; if (startup) { clearTimeout(startup); startup = null; } }
        if (data.type === "tree") {
          try { tree = parseUiTree(data.tree); ready = true; keyListener = isRecord(data.listeners) && data.listeners.key === true; pointerListener = isRecord(data.listeners) && data.listeners.pointer === true; if (startup) { clearTimeout(startup); startup = null; } } catch (reason) { fail(String(reason)); }
        }
        if (data.type === "post") void mods.transport.request({ subtype: "ui_message", ...target, data: data.data }).then((reply) => { if (isRecord(reply.props)) void send({ type: "props", props: reply.props }).catch(() => {}); }).catch((reason) => fail(String(reason)));
      };
      window.postMessage({ worker: CLIENT_WORKER, initial: JSON.parse(JSON.stringify({ type: "init", id: 0, bundle, props, ...viewport(), suspended: untrack(() => paused) })) }, "*", [channel.port2]);
      startup = setTimeout(() => { if (!ready && !canceled) fail("Claude Mod surface did not start"); }, 5000);
    }).catch((reason) => { if (!canceled) fail(String(reason)); });
    return () => { canceled = true; if (startup) clearTimeout(startup); stop(); releaseWorker(); };
  });
  $effect(() => { const props = node.props.props ?? {}; if (ready && !paused) untrack(() => { void send({ type: "props", props }).catch(() => {}); }); });
  $effect(() => {
    if (!ready) return;
    const suspended = paused;
    if (suspended) {
      if (pointerFrame !== null) { cancelAnimationFrame(pointerFrame); pointerFrame = null; }
      nextPointer = null;
    }
    untrack(() => { void send({ type: "suspend", suspended, ...(!suspended ? { props: node.props.props ?? {}, ...viewport() } : {}) }).catch(() => {}); });
  });
  $effect(() => {
    if (!ready || paused) return;
    const heartbeat = setInterval(() => { void send({ type: "ping" }).catch(() => {}); }, 2000);
    return () => clearInterval(heartbeat);
  });
  $effect(() => {
    if (!ready || paused || !body) return;
    let last = "";
    const resize = () => { const size = viewport(), key = `${size.columns}:${size.rows}`; if (key !== last) { last = key; void send({ type: "resize", ...size }).catch(() => {}); } };
    const observer = new ResizeObserver(resize); observer.observe(body);
    const site = body.closest(".mod-site");
    if (site) observer.observe(site);
    return () => observer.disconnect();
  });
  async function action(inner: UiNode, event: UiRecord): Promise<void> {
    if (paused) throw new Error("This Mod surface is paused");
    if (inner.held === undefined) throw new Error("Claude Mod control is unavailable");
    await send({ type: "pin", handle: inner.held });
    try {
      const reply = await mods.transport.request({ subtype: "ui_client_press", ...target, element: inner.props.key, event });
      if (reply.handled === false) throw new Error("This Mod control changed; refresh the pane");
      if (reply.reached !== undefined) await send({ type: "held", handle: inner.held, event: reply.reached });
    } finally { if (port) void send({ type: "unpin", handle: inner.held }).catch(() => {}); }
  }
  function keydown(event: KeyboardEvent): void {
    // Inputs, selects, links, and buttons retain their browser keyboard behavior.
    if (!ready || paused || event.target !== body || event.isComposing) return;
    if (event.key === "Escape") {
      event.preventDefault(); event.stopPropagation();
      (body?.closest("[role='tabpanel']") as HTMLElement | null)?.focus({ preventScroll: true });
      return;
    }
    if (!keyListener) return;
    // Tab always leaves the custom region and remains usable for keyboard navigation.
    if (event.key !== "Tab") { event.preventDefault(); event.stopPropagation(); }
    void send({ type: "key", event: clientKeyEvent(event) }).catch(() => {});
  }
  function pointer(type: "down" | "move" | "up" | "enter" | "leave", event: PointerEvent): void {
    if (!ready || paused || !pointerListener || !body) return;
    if ((event.target as Element).closest("button,input,textarea,select,a,[contenteditable='true']")) return;
    const rect = body.getBoundingClientRect(), style = getComputedStyle(body);
    const fontSize = Number.parseFloat(style.fontSize) || 14;
    const payload = clientPointerEvent(type, event, { left: rect.left, top: rect.top, cellWidth: fontSize * .6, cellHeight: Number.parseFloat(style.lineHeight) || fontSize * 1.45 }, lastPointer);
    lastPointer = { x: payload.x as number, y: payload.y as number };
    if (type === "down") { body.focus({ preventScroll: true }); body.setPointerCapture(event.pointerId); event.preventDefault(); }
    if (type === "move") {
      nextPointer = payload;
      if (pointerFrame === null) pointerFrame = requestAnimationFrame(() => { pointerFrame = null; if (nextPointer) { void send({ type: "pointer", event: nextPointer }).catch(() => {}); nextPointer = null; } });
      if (body.hasPointerCapture(event.pointerId)) event.preventDefault();
    } else {
      if (nextPointer) { void send({ type: "pointer", event: nextPointer }).catch(() => {}); nextPointer = null; }
      void send({ type: "pointer", event: payload }).catch(() => {});
      if (type === "up" && body.hasPointerCapture(event.pointerId)) body.releasePointerCapture(event.pointerId);
    }
  }
</script>

{#key `${pluginName}:${moduleName}:${clientKey}:${String(hash)}`}
  <iframe title="Isolated Claude Mod runtime" sandbox="allow-scripts" srcdoc={CLIENT_FRAME} bind:this={frame} onload={(event) => (loadedFrame = event.currentTarget as HTMLIFrameElement)} hidden></iframe>
{/key}
<!-- The native Client explicitly registers its keyboard/pointer handlers; only then is this custom region focusable. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="client-body" bind:this={body} data-mod-key={clientKey} data-mod-plugin={pluginName} role={keyListener || pointerListener ? "application" : "group"} aria-label="Claude Mod interactive surface" tabindex={!paused && (keyListener || pointerListener) ? 0 : undefined} onkeydown={keydown} onpointerdown={(event) => pointer("down", event)} onpointermove={(event) => pointer("move", event)} onpointerup={(event) => pointer("up", event)} onpointerenter={(event) => pointer("enter", event)} onpointerleave={(event) => pointer("leave", event)} onpointercancel={(event) => pointer("up", event)}>
  {#if tree && !error}<ModNode node={tree} disabled={!ready || paused || !mods.attached} onAction={action} />{/if}
  {#if error}<div class="client-error" role="status">{error}</div>{/if}
</div>
<style>
  .client-body { min-width: 0; }
  .client-body:focus-visible { outline: 2px solid var(--focus-ring); outline-offset: 3px; border-radius: 3px; }
  .client-error { color: var(--muted); border: 1px solid var(--edge); border-radius: 6px; padding: .6em; font-size: .9em; }
</style>
