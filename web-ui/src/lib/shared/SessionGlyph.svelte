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
   *   agy    — dot floating over an upward arc (antigravity)
   *   gemini — triangle (legacy sessions/recents only)
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
    /** Opt-in gentle breathing while the mark is "alive" (green) — the
     *  activity cue in surfaces that show only the glyph (the rail rows, the
     *  focus-strip chips), where a working agent has no separate pulsing dot.
     *  Off by default so tabs / quick-open / the dashboard (which carries its
     *  own dot) stay still. */
    pulse?: boolean;
  }

  let { kind, agentKind = null, state = "", size = 10, title, pulse = false }: Props = $props();

  const PATHS: Record<string, string> = {
    shell: "M3 4.5L6.5 8 3 11.5M8.5 12h4.5",
    claude: "M8 2.5l1.4 3.6 3.6 1.4-3.6 1.4L8 12.5 6.6 8.9 3 7.5l3.6-1.4z",
    codex: "M6.4 4.8L3.6 8l2.8 3.2M9.6 4.8L12.4 8l-2.8 3.2",
    agy: "M8 5.1m-1.5 0a1.5 1.5 0 1 0 3 0a1.5 1.5 0 1 0-3 0M3.7 11.4q4.3-3.1 8.6 0",
    gemini: "M8 3.2l4.8 8.6H3.2z",
  };

  const which = $derived(
    kind === "shell" ? "shell" : (agentKind !== null && agentKind in PATHS ? agentKind : "claude"),
  );
</script>

<svg
  class="sglyph {state}"
  class:pulse={pulse && state === "alive"}
  viewBox="0 0 16 16"
  width={size}
  height={size}
  aria-hidden={title === undefined}
>
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
  /* Opt-in breathing for glyph-only surfaces (the `pulse` prop): the same
     2.4s rhythm as the dashboard card's alive dot, so a working agent in the
     rail reads as active without an extra icon. `pulse` is the global
     keyframe in app.css. */
  .sglyph.pulse.alive {
    animation: pulse 2.4s ease-in-out infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .sglyph.pulse.alive {
      animation: none;
    }
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

  /* An idle shell (alive, at the prompt): a calm, present dot — distinctly not
     the "active" accent, and a touch stronger than the exited/unknown muting. */
  .sglyph.idle {
    color: var(--muted);
  }
</style>
