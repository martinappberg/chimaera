<script lang="ts">
  /**
   * The session-type glyph set, one component everywhere a session shows a
   * type mark (pane tabs, quick-open, the launcher). `agent_kind` from the
   * server drives the choice; the marks are original geometry in the app's
   * stroke language — deliberately NOT vendor logos:
   *
   *   shell  — prompt mark (chevron + underscore)
   *   claude — four-point spark
   *   codex  — paired code brackets
   *   gemini — triangle
   *
   * The optional `state` class carries the session-state color (same
   * modifier names as the dots); default is the muted chrome tone.
   */

  interface Props {
    kind: "shell" | "agent";
    /** Server-reported agent_kind; unset/unknown agents read as claude. */
    agentKind?: string | null;
    /** Session-state modifier class ("alive", "attn", ...), or "". */
    state?: string;
    size?: number;
    /** Accessible name / hover title for the mark. */
    title?: string;
  }

  let { kind, agentKind = null, state = "", size = 10, title }: Props = $props();

  const PATHS: Record<string, string> = {
    shell: "M3 4.5L6.5 8 3 11.5M8.5 12h4.5",
    claude: "M8 2.5l1.4 3.6 3.6 1.4-3.6 1.4L8 12.5 6.6 8.9 3 7.5l3.6-1.4z",
    codex: "M6.4 4.8L3.6 8l2.8 3.2M9.6 4.8L12.4 8l-2.8 3.2",
    gemini: "M8 3.2l4.8 8.6H3.2z",
  };

  const which = $derived(
    kind === "shell" ? "shell" : (agentKind !== null && agentKind in PATHS ? agentKind : "claude"),
  );
</script>

<svg class="sglyph {state}" viewBox="0 0 16 16" width={size} height={size} aria-hidden={title === undefined}>
  {#if title !== undefined}
    <title>{title}</title>
  {/if}
  <path
    d={PATHS[which]}
    fill="none"
    stroke="currentColor"
    stroke-width={which === "claude" ? 1.3 : 1.5}
    stroke-linecap="round"
    stroke-linejoin="round"
  />
</svg>

<style>
  /* Muted by default; the state modifiers reuse the dot palette so type
     marks double as state color wherever they replace a dot. */
  .sglyph {
    flex: none;
    color: var(--muted);
  }

  .sglyph.alive {
    color: var(--accent);
  }

  .sglyph.attn {
    color: var(--warn);
  }

  .sglyph.err {
    color: var(--err);
  }

  .sglyph.rate {
    color: var(--rate);
  }

  .sglyph.done {
    color: color-mix(in srgb, var(--fg) 60%, transparent);
  }

  .sglyph.starting,
  .sglyph.unk {
    color: var(--muted);
    opacity: 0.75;
  }
</style>
