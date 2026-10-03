<script lang="ts">
  import type { Snippet } from "svelte";
  import DOMPurify from "dompurify";
  import Markdown from "./Markdown.svelte";
  import ModControl from "./ModControl.svelte";
  import { safeUiHref, uiStyle, type UiNode, type UiRecord } from "./nativeUi";
  let { node, disabled = false, onAction, engine, client }: { node: UiNode; disabled?: boolean; onAction: (node: UiNode, event: UiRecord) => Promise<void>; engine?: Snippet<[number]>; client?: Snippet<[UiNode]> } = $props();
  function svgSource(source: unknown): string {
    const clean = DOMPurify.sanitize(String(source ?? "").slice(0, 128 * 1024), { USE_PROFILES: { svg: true }, FORBID_TAGS: ["foreignObject", "a", "image", "use", "style", "animate", "set"], FORBID_ATTR: ["href", "xlink:href"] });
    return `data:image/svg+xml,${encodeURIComponent(clean)}`;
  }
  function markdownPress(event: MouseEvent, current: UiNode): void {
    const link = (event.target as Element).closest("a");
    const allowed = current.props.pressableLinks;
    if (link && current.press && Array.isArray(allowed) && allowed.includes(link.getAttribute("href"))) {
      event.preventDefault(); event.stopPropagation();
      void onAction(current, { type: "press", href: link.getAttribute("href") });
    }
  }
</script>

{#snippet tree(current: UiNode | string)}
  {#if typeof current === "string"}{current}
  {:else if current.type === "engine"}
    {#if engine}{@render engine(current.engineOrdinal ?? 0)}{/if}
  {:else if ["Button", "Input", "Select"].includes(current.type)}
    {#key `${current.type}:${String(current.props.key ?? "")}`}<ModControl node={current} {disabled} {onAction} />{/key}
  {:else if current.type === "Client"}
    {#if client}{@render client(current)}{/if}
  {:else if current.type === "Markdown"}
    <!-- The listener only intercepts an allowlisted anchor inside sanitized Markdown. -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="mod-markdown" style={uiStyle(current.props, current.type)} onclickcapture={(event) => markdownPress(event, current)}><Markdown text={String(current.props.text ?? "")} /></div>
  {:else if current.type === "Code"}
    <div class="code">
      {#if current.props.path}<div class="code-label">{String(current.props.path)}</div>{/if}
      <pre class:wrap={current.props.wrap === "wrap"}><code>{String(current.props.source ?? "")}</code></pre>
    </div>
  {:else if current.type === "Svg"}
    <img class="mod-svg" src={svgSource(current.props.source)} alt={String(current.props.alt ?? "Mod graphic")} width={typeof current.props.width === "number" ? Math.min(4096, current.props.width) : undefined} height={typeof current.props.height === "number" ? Math.min(4096, current.props.height) : undefined} />
  {:else if current.type === "Link"}
    {@const href = safeUiHref(current.props.href)}
    {#if href}<a {href} target="_blank" rel="noopener noreferrer">{#if current.children.length}{#each current.children as child, i (i)}{@render tree(child)}{/each}{:else}{String(current.props.label ?? current.props.href)}{/if}</a>
    {:else}<span>{String(current.props.label ?? current.props.href ?? "")}</span>{/if}
  {:else}
    <svelte:element this={current.type === "Box" || current.type === "div" ? "div" : "span"} class="mod-element" style={uiStyle(current.props, current.type)} data-mod-key={typeof current.props.key === "string" ? current.props.key : undefined} data-mod-plugin={current.group?.plugin}>
      {#each current.children as child, i (i)}{@render tree(child)}{/each}
    </svelte:element>
  {/if}
{/snippet}
{@render tree(node)}

<style>
  /* Only text preserves whitespace. Inheriting it into layout and controls
     turns template indentation into empty rows and oversized form fields. */
  .mod-element { min-width: 0; max-width: 100%; overflow-wrap: anywhere; white-space: normal; }
  span.mod-element { white-space: pre-wrap; }
  .mod-svg { max-width: 100%; object-fit: contain; }
  a { color: var(--accent); text-underline-offset: 2px; }
  .code { background: var(--bg); border-radius: 5px; overflow: hidden; max-width: 100%; white-space: normal; }
  .code-label { font-size: .85em; color: var(--muted); border-bottom: 1px solid var(--edge); padding: .4em .7em; }
  pre { font-family: var(--mono); font-size: var(--text-sm); line-height: 1.5; white-space: pre; overflow: auto; margin: 0; padding: 8px 10px; }
  pre.wrap { white-space: pre-wrap; overflow-wrap: anywhere; }
  .mod-markdown { min-width: 0; white-space: normal; }
  .mod-markdown :global(.md > :first-child) { margin-top: 0; }
  .mod-markdown :global(.md > :last-child) { margin-bottom: 0; }
</style>
