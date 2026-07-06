<script lang="ts">
  import { fsRawUrl } from "./files";

  interface Props {
    path: string;
  }

  let { path }: Props = $props();

  let url = $state<string | null>(null);
  let error = $state<string | null>(null);

  // Ticketed /raw/ URL: the daemon serves HTML under CSP "sandbox
  // allow-scripts" and the iframe repeats the sandbox — no same-origin
  // access, no top-navigation, and the bearer token never appears in a URL.
  $effect(() => {
    const p = path;
    url = null;
    error = null;
    let stale = false;
    fsRawUrl(p)
      .then((u) => {
        if (!stale) url = u;
      })
      .catch((e) => {
        if (!stale) error = e instanceof Error ? e.message : "failed to load page";
      });
    return () => {
      stale = true;
    };
  });
</script>

<div class="html-view">
  {#if error !== null}
    <div class="file-error">{error}</div>
  {:else if url !== null}
    <iframe src={url} title={path} sandbox="allow-scripts"></iframe>
  {/if}
</div>

<style>
  .html-view {
    position: absolute;
    inset: 0;
    display: flex;
  }

  iframe {
    flex: 1;
    border: none;
    background: #ffffff; /* pages assume a white canvas regardless of theme */
  }

  .file-error {
    margin: auto;
    color: var(--muted);
    font-size: 0.8rem;
    padding: 1rem;
    text-align: center;
  }
</style>
