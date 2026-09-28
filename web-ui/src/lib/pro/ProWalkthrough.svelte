<script lang="ts">
  const steps = [
    { id: "computer", place: "On your computer", title: "Start a session", description: "Work on your project with Claude or Codex. Your files and conversation stay together." },
    { id: "cloud", place: "In the cloud", title: "Keep it moving", description: "Continue a supported agent session in the cloud, so it can work while your computer is offline." },
    { id: "device", place: "On another device", title: "Pick up where you left off", description: "Open your cloud workspace in a browser. Return to the same project, files, and conversation." },
  ] as const;
</script>

<section class="walkthrough" aria-label="Continue your work across devices">
  <ol>
    {#each steps as step, index (step.id)}
      <li>
        <div class="place"><span class="number" aria-hidden="true">0{index + 1}</span>{step.place}</div>
        <div class="illustration" aria-hidden="true">
          <svg viewBox="0 0 260 176" fill="none" focusable="false">
            {#if step.id === "cloud"}
              <path class="connection" d="M130 31v-8" />
              <path class="cloud" d="M116 23h28a7 7 0 0 0 0-14h-1a13 13 0 0 0-24-1 8 8 0 0 0-3 15Z" />
            {/if}
            <rect class="frame" x="20" y="31" width="220" height="124" rx={step.id === "device" ? 12 : 7} />
            <path class="divider" d="M20 56h220" />
            <rect class="project-mark" x="32" y="39" width="10" height="10" rx="3" />
            <path class="project-glyph" d="m35 44 2 2 3-4" />
            <text class="project-name" x="49" y="47">My project</text>
            {#if step.id === "device"}
              <path class="chrome" d="m213 41-3 3 3 3m10-6 3 3-3 3" />
            {:else}
              <circle class="chrome-dot" cx="215" cy="44" r="1.5" />
              <circle class="chrome-dot" cx="222" cy="44" r="1.5" />
              <circle class="chrome-dot" cx="229" cy="44" r="1.5" />
            {/if}
            <rect class="rail" x="21" y="57" width="32" height="97" rx="1" />
            <path class="file" d="M31 69h8m-8 7h12m-12 7h10m-10 19h8m-8 7h12m-12 7h10" />
            <rect class="message" x="67" y="67" width="155" height="24" rx="5" />
            <text class="prompt" x="77" y="82">Let's build on this idea.</text>
            <circle class="agent-dot" cx="73" cy="107" r="3" />
            <text class="reply" x="83" y="110">{step.id === "computer" ? "A good place to start…" : step.id === "cloud" ? "Working on your next step…" : "Here's where we left off."}</text>
            <path class="thread" d="M83 120h120m-120 6h95" />
            {#if step.id === "computer"}
              <path class="laptop" d="M8 155h244l-8 8H16Z" />
              <path class="divider" d="M111 156h38" />
            {:else if step.id === "cloud"}
              <path class="thread" d="M83 132h109" />
            {:else}
              <rect class="phone" x="208" y="82" width="40" height="82" rx="7" />
              <path class="divider" d="M219 88h18" />
              <rect class="project-mark" x="215" y="97" width="9" height="9" rx="2" />
              <path class="project-glyph" d="m217 102 2 2 3-4" />
              <path class="phone-thread" d="M215 114h24m-24 5h19m-19 12h24m-24 5h20" />
              <path class="divider" d="M221 157h14" />
            {/if}
          </svg>
        </div>
        <div class="caption"><h2>{step.title}</h2><p>{step.description}</p></div>
      </li>
    {/each}
  </ol>
  <p class="provider-note">Use your own Claude or Codex account for cloud work.</p>
</section>

<style>
  .walkthrough { margin: 0 0 42px; container-type: inline-size; }
  ol { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 14px; padding: 0; margin: 0; list-style: none; }
  li { min-width: 0; overflow: hidden; border: 1px solid var(--edge); border-radius: 12px; background: color-mix(in srgb, var(--fg) 1%, var(--bg)); }
  .place { display: flex; align-items: center; gap: 9px; padding: 20px 18px 0; color: var(--muted); font-size: var(--text-xs); font-weight: 500; }
  .number { color: color-mix(in srgb, var(--muted) 65%, var(--bg)); font-variant-numeric: tabular-nums; }
  .illustration { margin: 12px 8px 0; }
  svg { display: block; width: 100%; height: auto; }
  svg text { font-family: inherit; font-size: 8.5px; }
  .frame, .phone { fill: var(--bg); stroke: color-mix(in srgb, var(--fg) 24%, var(--edge)); stroke-width: 1.2; }
  .divider, .file, .thread, .phone-thread { stroke: var(--edge); stroke-linecap: round; }
  .thread, .phone-thread { stroke: color-mix(in srgb, var(--muted) 32%, var(--bg)); stroke-width: 2; }
  .file { stroke: color-mix(in srgb, var(--muted) 35%, var(--bg)); stroke-width: 2; }
  .rail, .laptop { fill: color-mix(in srgb, var(--fg) 3%, var(--bg)); }
  .laptop { stroke: color-mix(in srgb, var(--fg) 24%, var(--edge)); stroke-width: 1.2; stroke-linejoin: round; }
  .project-mark { fill: color-mix(in srgb, var(--accent) 13%, var(--bg)); }
  .project-glyph { stroke: var(--accent); stroke-width: 1.2; stroke-linecap: round; stroke-linejoin: round; }
  .project-name { fill: var(--fg); font-weight: 550; }
  .chrome { stroke: var(--muted); stroke-linecap: round; stroke-linejoin: round; }
  .chrome-dot { fill: var(--edge); }
  .message { fill: color-mix(in srgb, var(--fg) 4%, var(--bg)); }
  .prompt { fill: var(--fg); }
  .agent-dot { fill: var(--accent); }
  .reply { fill: var(--muted); }
  .cloud { fill: color-mix(in srgb, var(--accent) 6%, var(--bg)); stroke: color-mix(in srgb, var(--accent) 65%, var(--edge)); stroke-width: 1.2; stroke-linejoin: round; }
  .connection { stroke: color-mix(in srgb, var(--accent) 55%, var(--edge)); stroke-width: 1.2; }
  .caption { padding: 16px 18px 22px; }
  h2 { margin: 0 0 10px; font-size: var(--text-md); font-weight: 560; letter-spacing: -.2px; line-height: 1.4; }
  .caption p { margin: 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.75; }
  .provider-note { margin: 16px 0 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.7; }
  @container (max-width: 680px) {
    ol { grid-template-columns: 1fr; gap: 12px; }
    li { display: grid; grid-template-columns: minmax(150px, 42%) 1fr; align-items: center; }
    .place { grid-column: 1 / -1; padding: 17px 20px 0; }
    .illustration { margin: 4px 8px 12px; }
    .caption { padding: 15px 20px 18px 8px; }
  }
  @container (max-width: 390px) {
    li { display: block; }
    .illustration { max-width: 240px; margin: 8px auto 0; }
    .caption { padding: 14px 20px 22px; }
  }
</style>
