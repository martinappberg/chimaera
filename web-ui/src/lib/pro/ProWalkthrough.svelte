<script lang="ts">
  const steps = [
    { id: "computer", place: "Start here", title: "Start on your computer", description: "Work on your project with Claude or Codex. Your files and conversation stay together." },
    { id: "cloud", place: "Step away", title: "Keep agents working", description: "Let your agents continue in the cloud while your computer is offline." },
    { id: "device", place: "Pick it up", title: "Access anywhere", description: "Pick up your sessions, files, and conversation on another device. Work continues locally when you’re back." },
  ] as const;
</script>

<section class="walkthrough" aria-label="Continue your work across devices">
  <ol>
    {#each steps as step, index (step.id)}
      <li>
        <div class="place"><span class="number" aria-hidden="true">0{index + 1}</span>{step.place}</div>
        <div class="illustration" aria-hidden="true">
          <svg viewBox="0 0 280 184" fill="none" focusable="false">
            {#if step.id === "computer"}
              <rect class="frame" x="20" y="22" width="240" height="137" rx="7" />
              <rect class="screen" x="27" y="29" width="226" height="122" rx="3" />
              <path class="divider" d="M27 54h226" />
              <rect class="project-mark" x="37" y="36" width="12" height="12" rx="3" />
              <path class="project-glyph" d="m40 42 2 2 4-5" />
              <text class="project-name" x="56" y="45">My project</text>
              <path class="file" d="M39 69h18m-18 8h25m-25 8h22m-22 18h17m-17 8h25m-25 8h20" />
              <path class="divider" d="M76 55v96" />
              <rect class="message" x="89" y="66" width="150" height="25" rx="5" />
              <text class="prompt" x="99" y="82">Let's build on this idea.</text>
              <circle class="agent-dot" cx="95" cy="108" r="3" />
              <text class="reply" x="105" y="111">A good place to start…</text>
              <path class="thread" d="M105 123h119m-119 7h96" />
              <path class="hardware" d="M7 159h266l-10 9H17Z" />
              <path class="divider" d="M119 160h42" />
            {:else if step.id === "cloud"}
              <path class="connection dashed" d="M63 148v-22q0-16 16-16h15" />
              <path class="cloud" d="M105 134h124a29 29 0 0 0 7-57 42 42 0 0 0-79-27 29 29 0 0 0-47 21 32 32 0 0 0-5 63Z" />
              <circle class="agent-dot" cx="126" cy="65" r="3" />
              <text class="project-name" x="136" y="68">Agent at work</text>
              <rect class="task" x="110" y="79" width="126" height="22" rx="5" />
              <path class="project-glyph" d="m118 90 3 3 5-6" />
              <text class="task-name" x="134" y="93">Review changes</text>
              <rect class="task" x="110" y="106" width="126" height="22" rx="5" />
              <circle class="task-ring" cx="122" cy="117" r="4" />
              <text class="task-name" x="134" y="120">Build the next step</text>
              <path class="hardware closed-lid" d="m15 153 82-6 19 9-83 6Z" />
              <path class="hardware" d="m15 153 18 9 83-6v5l-83 7-18-9Z" />
              <path class="sleep" d="M36 126a9 9 0 1 1-9-13 8 8 0 0 0 9 13Z" />
            {:else}
              <path class="connection" d="M155 113V97m-5 5 5-5 5 5M107 137H86m111 0h27" />
              <rect class="frame" x="120" y="14" width="146" height="78" rx="5" />
              <rect class="screen" x="126" y="20" width="134" height="60" rx="2" />
              <rect class="project-mark" x="135" y="28" width="10" height="10" rx="2.5" />
              <path class="project-glyph" d="m137 33 2 2 4-5" />
              <text class="project-name" x="151" y="36">My project</text>
              <rect class="message" x="135" y="45" width="115" height="12" rx="3" />
              <path class="thread" d="M141 51h66m-6 16h47m-107 0h11" />
              <path class="hardware" d="M185 92h15l2 11h-19Z" />
              <path class="hardware-line" d="M175 105h35" />
              <rect class="frame" x="13" y="88" width="83" height="81" rx="8" />
              <rect class="screen" x="19" y="96" width="71" height="64" rx="3" />
              <rect class="project-mark" x="27" y="104" width="10" height="10" rx="2.5" />
              <path class="project-glyph" d="m29 109 2 2 4-5" />
              <path class="thread" d="M43 108h37m-53 16h53m-53 6h41m-41 12h53m-53 6h36" />
              <path class="divider" d="M48 165h14" />
              <rect class="frame" x="222" y="111" width="39" height="66" rx="7" />
              <path class="divider" d="M235 116h13" />
              <rect class="project-mark" x="230" y="125" width="9" height="9" rx="2" />
              <path class="project-glyph" d="m232 129 2 2 3-4" />
              <path class="thread" d="M230 142h22m-22 6h17m-17 8h22" />
              <path class="divider" d="M236 172h11" />
              <rect class="shared-project" x="106" y="116" width="98" height="45" rx="7" />
              <rect class="project-mark" x="115" y="125" width="12" height="12" rx="3" />
              <path class="project-glyph" d="m118 131 2 2 4-5" />
              <text class="project-name" x="132" y="133">My project</text>
              <path class="conversation" d="M116 144h9v6h-5l-3 2v-2h-1Z" />
              <path class="thread" d="M132 145h59m-59 6h43" />
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
  svg text { font-family: inherit; font-size: 9.5px; }
  .frame { fill: color-mix(in srgb, var(--fg) 2%, var(--bg)); stroke: color-mix(in srgb, var(--fg) 28%, var(--edge)); stroke-width: 1.2; }
  .screen { fill: var(--bg); stroke: var(--edge); stroke-width: .7; }
  .divider, .file, .thread { stroke: var(--edge); stroke-linecap: round; }
  .thread { stroke: color-mix(in srgb, var(--muted) 37%, var(--bg)); stroke-width: 1.6; }
  .file { stroke: color-mix(in srgb, var(--muted) 40%, var(--bg)); stroke-width: 2; }
  .hardware { fill: color-mix(in srgb, var(--fg) 5%, var(--bg)); stroke: color-mix(in srgb, var(--fg) 28%, var(--edge)); stroke-width: 1.2; stroke-linejoin: round; }
  .closed-lid { fill: color-mix(in srgb, var(--fg) 2%, var(--bg)); }
  .hardware-line { stroke: color-mix(in srgb, var(--fg) 35%, var(--edge)); stroke-width: 1.5; stroke-linecap: round; }
  .project-mark { fill: color-mix(in srgb, var(--accent) 13%, var(--bg)); }
  .project-glyph, .task-ring, .conversation { stroke: var(--accent); stroke-width: 1.2; stroke-linecap: round; stroke-linejoin: round; }
  .project-name { fill: var(--fg); font-weight: 550; }
  .message { fill: color-mix(in srgb, var(--fg) 4%, var(--bg)); }
  .prompt, .task-name { fill: var(--fg); }
  .agent-dot { fill: var(--accent); }
  .reply { fill: var(--muted); }
  .cloud { fill: color-mix(in srgb, var(--accent) 5%, var(--bg)); stroke: color-mix(in srgb, var(--accent) 62%, var(--edge)); stroke-width: 1.3; stroke-linejoin: round; }
  .task { fill: var(--bg); stroke: color-mix(in srgb, var(--accent) 16%, var(--edge)); stroke-width: .8; }
  .shared-project { fill: var(--bg); stroke: color-mix(in srgb, var(--accent) 60%, var(--edge)); stroke-width: 1.2; }
  .connection { stroke: color-mix(in srgb, var(--accent) 62%, var(--edge)); stroke-width: 1.3; stroke-linecap: round; stroke-linejoin: round; }
  .dashed { stroke-dasharray: 3 4; }
  .sleep { fill: color-mix(in srgb, var(--fg) 5%, var(--bg)); stroke: var(--muted); stroke-width: 1; }
  .caption { padding: 16px 18px 22px; }
  h2 { margin: 0 0 10px; font-size: var(--text-md); font-weight: 560; letter-spacing: -.2px; line-height: 1.4; }
  .caption p { margin: 0; color: var(--muted); font-size: var(--text-sm); line-height: 1.75; }
  .provider-note { margin: 16px 0 0; color: var(--muted); font-size: var(--text-xs); line-height: 1.7; }
  @container (max-width: 820px) {
    ol { grid-template-columns: 1fr; gap: 12px; }
    .place { padding: 17px 20px 0; }
    .illustration { max-width: 300px; margin: 8px auto 0; }
    .caption { padding: 14px 20px 22px; }
  }
</style>
