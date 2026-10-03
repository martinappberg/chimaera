<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import ModNode from "./ModNode.svelte";
  import ModClient from "./ModClient.svelte";
  import type { ModsController, ModRender, ModSite } from "./mods.svelte";
  import { isRecord, type UiNode, type UiRecord } from "./nativeUi";
  let { mods, component, instanceId, props = {}, active = true, children }: { mods: ModsController; component: string; instanceId: string; props?: UiRecord; active?: boolean; children?: Snippet<[UiRecord]> } = $props();
  let render = $state<ModRender>({ tree: null, error: null, clientModules: {}, props: {} });
  let mount: ReturnType<ModsController["mount"]> | null = null;
  let root = $state<HTMLDivElement>();
  const scrollable = $derived(component === "Pane" || component === "AbovePrompt");
  let following = false;
  let expectedScroll: number | null = null;
  let lastOffset = 0;
  let lastSpec = "";
  let layoutFrame: number | null = null;
  let scrollFrame: number | null = null;
  let scrollPending = false;
  let scrollDirty = false;
  let focusRevision = 0;
  let siteHeld = false;
  let scrollRevision = 0;

  function metrics(): { columns: number; rows: number; line: number; content: number; offset: number } {
    if (!root) return { columns: 80, rows: 1, line: 20, content: 1, offset: 0 };
    const style = getComputedStyle(root), fontSize = Number.parseFloat(style.fontSize) || 14;
    const line = Number.parseFloat(style.lineHeight) || fontSize * 1.45;
    const context = document.createElement("canvas").getContext("2d");
    if (context) context.font = `${fontSize}px ${style.getPropertyValue("--mono") || "monospace"}`;
    const column = context?.measureText("0").width || fontSize * .6;
    return { columns: Math.max(1, Math.min(512, Math.floor(root.clientWidth / column))), rows: Math.max(1, Math.min(256, Math.floor(root.clientHeight / line))), line, content: Math.min(1_000_000, Math.ceil(root.scrollHeight / line)), offset: Math.max(0, Math.floor(root.scrollTop / line)) };
  }
  function site(): ModSite {
    if (!scrollable || !root) return { component, instance_id: instanceId, props };
    const size = metrics(), bounds = root.getBoundingClientRect();
    const keyed: NonNullable<ModSite["keyed"]> = [];
    for (const element of [...root.querySelectorAll<HTMLElement>("[data-mod-key][data-mod-plugin]")].slice(0, 256)) {
      const key = element.dataset.modKey, plugin = element.dataset.modPlugin;
      if (!key || !plugin) continue;
      const box = element.getBoundingClientRect();
      keyed.push({ plugin, key, top: Math.max(0, Math.floor((box.top - bounds.top + root.scrollTop) / size.line)), bottom: Math.max(1, Math.ceil((box.bottom - bounds.top + root.scrollTop) / size.line)) });
    }
    const defaults = component === "Pane" ? { isFocused: root.contains(document.activeElement), placement: "inline" } : { maxRows: size.rows };
    return { component, instance_id: instanceId, props: { ...defaults, view: {}, ...props, bodyColumns: size.columns, scroll: { offset: size.offset, bodyRows: size.rows } }, viewport: { columns: size.columns, rows: size.rows, isFullscreen: false }, content_rows: size.content, keyed };
  }
  function updateLayout(): void {
    if (!active || !mount || !root) return;
    if (following) setScroll(root.scrollHeight);
    const next = site(), encoded = JSON.stringify(next);
    if (encoded !== lastSpec) { lastSpec = encoded; mount.update(next); }
  }
  function queueLayout(): void {
    if (layoutFrame === null) layoutFrame = requestAnimationFrame(() => { layoutFrame = null; updateLayout(); });
  }
  function setScroll(value: number): void {
    if (!root) return;
    const next = Math.max(0, Math.min(root.scrollHeight - root.clientHeight, value));
    if (Math.abs(next - root.scrollTop) > .5) { expectedScroll = next; root.scrollTop = next; }
    lastOffset = metrics().offset;
  }
  $effect(() => {
    if (!active) return;
    const registration = mods.mount(untrack(site), (next) => { render = next; });
    mount = registration;
    return () => {
      registration.dispose(); mount = null; lastSpec = "";
      ++focusRevision; ++scrollRevision;
      if (layoutFrame !== null) { cancelAnimationFrame(layoutFrame); layoutFrame = null; }
      if (scrollFrame !== null) { cancelAnimationFrame(scrollFrame); scrollFrame = null; }
      if (scrollable && siteHeld && mods.attached) void mods.transport.request({ subtype: "ui_focus", component, instance_id: instanceId, is_held: false, by: "person" }).catch(() => {});
      siteHeld = false;
    };
  });
  $effect(() => { void props; untrack(updateLayout); });
  $effect(() => {
    if (!active || !scrollable || !root) return;
    void render.tree;
    const observer = new ResizeObserver(queueLayout);
    observer.observe(root);
    if (root.firstElementChild) observer.observe(root.firstElementChild);
    untrack(queueLayout);
    return () => observer.disconnect();
  });
  function focusElement(element: UiRecord | null): void {
    if (!root || !root.contains(document.activeElement)) return;
    if (!element) { root.focus({ preventScroll: true }); return; }
    const target = [...root.querySelectorAll<HTMLElement>("[data-mod-key][data-mod-plugin]")].find((candidate) => candidate.dataset.modKey === element.key && candidate.dataset.modPlugin === element.plugin);
    const control = target?.matches("input,select,button,a,[tabindex]") ? target : target?.querySelector<HTMLElement>("input,select,button,a,[tabindex]");
    if (control && control !== document.activeElement) control.focus({ preventScroll: true });
  }
  function reportFocus(held: boolean, target: EventTarget | null): void {
    if (!active || !scrollable || !mods.attached) return;
    siteHeld = held;
    const keyed = target instanceof Element ? target.closest<HTMLElement>("[data-mod-key][data-mod-plugin]") : null;
    const element = keyed?.dataset.modKey && keyed.dataset.modPlugin ? { key: keyed.dataset.modKey, plugin: keyed.dataset.modPlugin } : null;
    const revision = ++focusRevision;
    void mods.transport.request({ subtype: "ui_focus", component, instance_id: instanceId, is_held: held, element, by: "person" }).then((reply) => {
      if (revision !== focusRevision || !held || !root?.contains(document.activeElement)) return;
      focusElement(isRecord(reply.element) ? reply.element : null);
    }).catch(() => {});
    queueLayout();
  }
  function onFocusOut(event: FocusEvent): void {
    if (event.relatedTarget instanceof Node && root?.contains(event.relatedTarget)) return;
    reportFocus(false, null);
  }
  function onScroll(): void {
    if (!active || !scrollable || !root || !mods.attached) return;
    if (expectedScroll !== null && Math.abs(root.scrollTop - expectedScroll) < 1) { expectedScroll = null; queueLayout(); return; }
    expectedScroll = null;
    following = false;
    ++scrollRevision;
    if (scrollFrame === null) scrollFrame = requestAnimationFrame(() => { scrollFrame = null; void reportScroll(); });
  }
  async function reportScroll(): Promise<void> {
    if (scrollPending) { scrollDirty = true; return; }
    if (!active || !root || !mods.attached) return;
    const size = metrics(), offset = lastOffset, revision = scrollRevision;
    lastOffset = size.offset;
    if (offset === size.offset) { updateLayout(); return; }
    scrollPending = true; scrollDirty = false;
    try {
      const reply = await mods.transport.request({ subtype: "ui_scroll", component, instance_id: instanceId, offset, by: size.offset - offset, body_rows: size.rows, content_rows: size.content, keyed: site().keyed });
      if (revision === scrollRevision && active && typeof reply.offset === "number") {
        following = reply.follow_end === true;
        setScroll(following ? root.scrollHeight : reply.offset * size.line);
      }
    } catch (error) { render.error = error instanceof Error ? error.message : String(error); }
    finally { scrollPending = false; updateLayout(); if (scrollDirty) void reportScroll(); }
  }
  $effect(() => mods.subscribe((event) => {
    if (!active || event.component !== component || event.instance_id !== instanceId || !root) return;
    const type = event.subtype ?? event.type;
    if (type === "ui_focus" && typeof event.key === "string") focusElement(event);
    if (type === "ui_scroll") {
      following = event.follow_end === true;
      ++scrollRevision;
      setScroll(following ? root.scrollHeight : Math.max(0, Number(event.offset) || 0) * metrics().line);
      queueLayout();
    }
  }));
  async function action(node: UiNode, event: UiRecord): Promise<void> {
    if (!node.press) throw new Error("This Mod control is unavailable");
    const { type, ...detail } = event;
    const result = await mods.transport.request({ subtype: type === "input" ? "ui_input" : type === "select" ? "ui_select" : "ui_press", ...node.press, key: node.props.key, component, instance_id: instanceId, ...detail });
    if (result.handled === false) { mount?.refresh(); throw new Error("This control changed. Try again after it refreshes."); }
  }
</script>

<!-- Negative tabIndex is only for an explicit, idle-composer focus handoff; this container never joins normal Tab navigation. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="mod-site" class:pane={component === "Pane"} bind:this={root} tabindex={scrollable ? -1 : undefined} onfocusin={(event) => reportFocus(true, event.target)} onfocusout={onFocusOut} onscroll={onScroll}>
  {#if render.tree}
    <ModNode node={render.tree} disabled={!mods.attached} onAction={action}>
      {#snippet engine(ordinal)}{#if children}{@render children(ordinal === 0 ? render.props : props)}{/if}{/snippet}
      {#snippet client(node)}{#if active && mods.attached}<ModClient {node} {mods} {component} {instanceId} hash={node.client ? render.clientModules[node.client.plugin] : undefined} />{/if}{/snippet}
    </ModNode>
  {:else if children}{@render children(props)}{/if}
  {#if render.error}<div class="mod-error" role="status">{render.error} <button onclick={() => mount?.refresh()}>Retry</button></div>{/if}
</div>

<style>
  .mod-site { min-width: 0; max-width: 100%; }
  .mod-site.pane { overflow: auto; overscroll-behavior: contain; max-height: var(--mod-pane-height, 34vh); padding: 12px; scrollbar-width: thin; }
  .mod-error { color: var(--muted); font-size: .9em; padding: .5em 0; }
  .mod-error button { font: inherit; color: var(--accent); border: 0; background: none; cursor: pointer; }
</style>
