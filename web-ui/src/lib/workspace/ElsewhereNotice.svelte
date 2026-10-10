<script lang="ts">
  /**
   * A host view asked to open a project that another machine runs now. The
   * workspace underneath would be that machine's stale copy, so this covers
   * it with one way forward: the project view, which follows the project
   * wherever it runs. The host's own words are generic; an extension may
   * pass the sentence it prefers.
   */
  import { modalFocus } from "../shared/modalFocus";

  let { href, sentence = "This project is open somewhere else." }: {
    /** The project view that follows the project. */
    href: string;
    sentence?: string;
  } = $props();
  const sentenceId = $props.id();
</script>

<div class="elsewhere">
  <div class="panel" role="alertdialog" aria-modal="true" aria-labelledby={sentenceId} tabindex="-1" use:modalFocus>
    <p class="sentence" id={sentenceId}>{sentence}</p>
    <a class="open" {href}>Open</a>
  </div>
</div>

<style>
  .elsewhere {
    position: fixed;
    inset: 0;
    z-index: 190;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    background: var(--scrim);
  }
  .panel {
    margin-top: 20vh;
    width: min(420px, calc(100vw - 2rem));
    padding: 20px;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px var(--scrim);
  }
  .sentence {
    margin: 0 0 12px;
    font-size: var(--text-md);
    line-height: 1.5;
  }
  .open {
    display: inline-block;
    border: 1px solid var(--edge);
    padding: 4px 16px;
    border-radius: 5px;
    font-size: var(--text-md);
    color: var(--fg);
    text-decoration: none;
  }
  .open:hover {
    background: var(--row-hover);
  }
  .open:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>
