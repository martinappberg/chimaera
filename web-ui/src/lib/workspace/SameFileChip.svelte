<script lang="ts">
  /**
   * The quiet "same file" chip: this live session and another live session
   * in the workspace both wrote a file (`sameFileOverlaps`, from the
   * `files_touched` lists already on the wire). Informational only — nothing
   * locks; an agent with hooks also hears about it in its own context.
   */
  import { workspaceRelative } from "../shared/reference";
  import type { SameFile } from "./history";

  interface Props {
    overlaps: SameFile[];
    /** Display name of a session id. */
    nameOf: (id: string) => string;
    wsRoot: string | null;
  }

  let { overlaps, nameOf, wsRoot }: Props = $props();

  const title = $derived(
    overlaps
      .map((o) => {
        const p = wsRoot !== null ? workspaceRelative(o.path, wsRoot) : o.path;
        return `“${nameOf(o.other)}” also wrote ${p}`;
      })
      .join("\n"),
  );
</script>

<span class="same-file" title={title} aria-label={title}>same file</span>

<style>
  .same-file {
    display: inline-block;
    vertical-align: 1px;
    margin-left: 5px;
    padding: 0 5px;
    font-family: var(--mono);
    font-size: 9px;
    line-height: 13px;
    letter-spacing: 0.02em;
    color: var(--warn);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--edge));
    border-radius: 999px;
    background: color-mix(in srgb, var(--warn) 7%, transparent);
    white-space: nowrap;
  }
</style>
