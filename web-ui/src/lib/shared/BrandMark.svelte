<script module lang="ts">
  let markSeq = 0;
  function nextMarkId(): string {
    markSeq += 1;
    return `m${markSeq}`;
  }
</script>

<script lang="ts">
  /**
   * The chimaera brand mark — a "C" monogram inside a hexagon shell.
   *
   * Two stroked paths (hexagon shell + the inner C) share one metallic
   * gradient. The gradient stops are CSS variables that flip with the theme
   * so the dark end never vanishes into a dark UI: silver-on-dark,
   * charcoal-on-light. The app-icon / favicon keeps the full charcoal→silver
   * range on its light tile (see public/favicon.svg).
   *
   * Motion is restrained and honours prefers-reduced-motion:
   *   - `draw`  one-shot: the shell draws itself, then the C sweeps in.
   *   - hover   a soft lift + brightening (interactive placements).
   *   - `busy`  a slow breath for connecting / loading states.
   */

  interface Props {
    /** Rendered edge length in px (square). */
    size?: number;
    /** Play the one-shot intro draw on mount. */
    draw?: boolean;
    /** Breathe slowly — for connecting / working states. */
    busy?: boolean;
    /** Accessible name; when set the mark is announced, else decorative. */
    title?: string;
  }

  let { size = 24, draw = false, busy = false, title }: Props = $props();

  // Unique per instance so two marks on one page don't share a gradient id.
  const uid = nextMarkId();
</script>

<svg
  class="hexmark"
  class:draw
  class:busy
  viewBox="0 0 240 240"
  width={size}
  height={size}
  role={title ? "img" : "presentation"}
  aria-label={title}
  aria-hidden={title ? undefined : true}
>
  {#if title}<title>{title}</title>{/if}
  <defs>
    <linearGradient
      id="bm-{uid}"
      gradientUnits="userSpaceOnUse"
      x1="54"
      y1="84"
      x2="186"
      y2="156"
    >
      <stop offset="0" stop-color="var(--bm-0)" />
      <stop offset="0.5" stop-color="var(--bm-1)" />
      <stop offset="1" stop-color="var(--bm-2)" />
    </linearGradient>
  </defs>
  <g
    fill="none"
    stroke="url(#bm-{uid})"
    stroke-width="15"
    stroke-linecap="round"
    stroke-linejoin="round"
  >
    <path class="shell" pathLength="100" d="M120 46 L186 84 L186 156 L120 194 L54 156 L54 84 Z" />
    <path class="cee" pathLength="100" d="M151.4 154.9 A 47 47 0 1 1 151.4 85.1" />
  </g>
</svg>

<style>
  .hexmark {
    flex: none;
    overflow: visible;
    /* Charcoal → silver, tuned for a light surface (default / light theme). */
    --bm-0: #2c2c30;
    --bm-1: #5c5c62;
    --bm-2: #9a9aa0;
    transition:
      transform 0.2s ease,
      filter 0.2s ease;
  }

  /* On a dark UI the charcoal end would disappear — lift the whole ramp to
     silver so the mark stays legible. */
  :global(html[data-theme="dark"]) .hexmark {
    --bm-0: #7c7c82;
    --bm-1: #b4b4ba;
    --bm-2: #f2f2f5;
  }

  .hexmark:hover {
    transform: translateY(-0.5px) scale(1.03);
    filter: brightness(1.08);
  }

  @media (prefers-reduced-motion: no-preference) {
    /* One-shot draw: the shell traces itself, then the C sweeps in. */
    .hexmark.draw .shell,
    .hexmark.draw .cee {
      stroke-dasharray: 100;
      stroke-dashoffset: 100;
    }

    .hexmark.draw .shell {
      animation: bm-draw 0.8s cubic-bezier(0.65, 0, 0.35, 1) forwards;
    }

    .hexmark.draw .cee {
      animation: bm-draw 0.5s cubic-bezier(0.65, 0, 0.35, 1) 0.5s forwards;
    }

    .hexmark.busy {
      animation: bm-breathe 2.6s ease-in-out infinite;
    }
  }

  @keyframes bm-draw {
    to {
      stroke-dashoffset: 0;
    }
  }

  @keyframes bm-breathe {
    0%,
    100% {
      opacity: 1;
    }
    50% {
      opacity: 0.55;
    }
  }
</style>
