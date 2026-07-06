<script lang="ts">
  import { fsMarkdown } from "./files";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  let html = $state<string | null>(null);
  let error = $state<string | null>(null);

  // Server-rendered (comrak, GFM) and sanitized (ammonia) — safe to inject.
  $effect(() => {
    const p = path;
    html = null;
    error = null;
    let stale = false;
    fsMarkdown(p)
      .then((h) => {
        if (!stale) html = h;
      })
      .catch((e) => {
        if (!stale) error = e instanceof Error ? e.message : "failed to render markdown";
      });
    return () => {
      stale = true;
    };
  });
</script>

<div class="md-scroll">
  {#if error !== null}
    <div class="file-error">{error}</div>
  {:else if html !== null}
    <article class="md-body">
      <!-- eslint-disable-next-line svelte/no-at-html-tags — sanitized server-side -->
      {@html html}
    </article>
  {/if}
</div>

<style>
  .md-scroll {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    overflow-x: hidden;
  }

  .file-error {
    padding: 2rem;
    color: var(--muted);
    font-size: 0.8rem;
    text-align: center;
  }

  /* Readable measure, styled by the app's tokens; content is server HTML,
     hence the :global rules scoped under .md-body. */
  .md-body {
    max-width: 70ch;
    margin: 0 auto;
    padding: 2.2rem 2rem 3.5rem;
    font-size: 0.92rem;
    line-height: 1.65;
    color: var(--fg);
    overflow-wrap: break-word;
  }

  .md-body :global(h1),
  .md-body :global(h2),
  .md-body :global(h3),
  .md-body :global(h4),
  .md-body :global(h5),
  .md-body :global(h6) {
    line-height: 1.25;
    margin: 1.6em 0 0.55em;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .md-body :global(h1) {
    font-size: 1.45rem;
    margin-top: 0.2em;
    padding-bottom: 0.35em;
    border-bottom: 1px solid var(--edge);
  }

  .md-body :global(h2) {
    font-size: 1.15rem;
    padding-bottom: 0.25em;
    border-bottom: 1px solid var(--edge);
  }

  .md-body :global(h3) {
    font-size: 1rem;
  }

  .md-body :global(h4),
  .md-body :global(h5),
  .md-body :global(h6) {
    font-size: 0.92rem;
  }

  .md-body :global(p) {
    margin: 0.7em 0;
  }

  .md-body :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .md-body :global(a:hover) {
    text-decoration: underline;
  }

  .md-body :global(code) {
    font-family: var(--mono);
    font-size: 0.82em;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    border-radius: 4px;
    padding: 0.12em 0.34em;
  }

  .md-body :global(pre) {
    background: color-mix(in srgb, var(--fg) 4.5%, transparent);
    border: 1px solid var(--edge);
    border-radius: 8px;
    padding: 0.8em 1em;
    overflow-x: auto;
    line-height: 1.5;
  }

  .md-body :global(pre code) {
    background: none;
    padding: 0;
    font-size: 0.78rem;
  }

  .md-body :global(blockquote) {
    margin: 0.8em 0;
    padding: 0.1em 1em;
    border-left: 3px solid var(--edge);
    color: var(--muted);
  }

  .md-body :global(ul),
  .md-body :global(ol) {
    padding-left: 1.6em;
    margin: 0.6em 0;
  }

  .md-body :global(li) {
    margin: 0.2em 0;
  }

  .md-body :global(hr) {
    border: none;
    border-top: 1px solid var(--edge);
    margin: 1.8em 0;
  }

  .md-body :global(img) {
    max-width: 100%;
  }

  .md-body :global(table) {
    border-collapse: collapse;
    margin: 1em 0;
    display: block;
    overflow-x: auto;
    font-size: 0.85rem;
  }

  .md-body :global(th),
  .md-body :global(td) {
    border: 1px solid var(--edge);
    padding: 0.35em 0.7em;
    text-align: left;
  }

  .md-body :global(th) {
    font-weight: 600;
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }

  .md-body :global(input[type="checkbox"]) {
    accent-color: var(--accent);
    margin-right: 0.4em;
  }
</style>
