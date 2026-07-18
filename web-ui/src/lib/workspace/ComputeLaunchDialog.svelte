<script lang="ts">
  import {
    launchComputeSession,
    type ComputeLaunchSpec,
  } from "./computeSessions";
  import {
    formatSlurmDuration,
    parseSlurmTimeLeft,
    type ComputePartition,
  } from "./compute";
  import { modalFocus } from "../shared/modalFocus";

  interface Props {
    /** The host the job submits on — display only; the submission goes to
     *  this window's own (login-node) daemon. */
    alias: string;
    /** Partitions from the host's compute snapshot (empty → free-text field). */
    partitions: ComputePartition[];
    /** The job was accepted — the caller closes this dialog and refetches. */
    onLaunched: () => void;
    onClose: () => void;
  }

  let { alias, partitions, onLaunched, onClose }: Props = $props();

  let name = $state("session");
  // sinfo's `*` partition is the cluster default — preselect it. Initial
  // capture is intentional: the dialog mounts fresh per open.
  // svelte-ignore state_referenced_locally
  let partition = $state(
    partitions.find((p) => p.default)?.name ?? partitions[0]?.name ?? "",
  );
  // Walltime as separate d/h/m boxes (maintainer ask — "easier to fix" than
  // one Slurm string); the composed Slurm walltime rides the wire. Numeric
  // inputs bind empty as null → read as 0.
  let days = $state<number | null>(0);
  let hours = $state<number | null>(2);
  let mins = $state<number | null>(0);
  const time = $derived.by(() => {
    const d = Math.max(0, Math.floor(days ?? 0));
    const h = Math.max(0, Math.floor(hours ?? 0));
    const m = Math.max(0, Math.floor(mins ?? 0));
    // The empty-guard is load-bearing: formatSlurmDuration(0) is "00:00", a
    // real walltime string that would slip past the canSubmit gate.
    if (d + h + m === 0) return "";
    return formatSlurmDuration(((d * 24 + h) * 60 + m) * 60);
  });
  /** Numeric input: Svelte binds an empty field as null. */
  let cpus = $state<number | null>(1);
  let mem = $state("4G");
  let gres = $state("");
  let prelude = $state("");
  let advancedOpen = $state(false);
  let routable = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const canSubmit = $derived(!busy && time !== "");

  // --- per-partition ceilings (sinfo %l/%c/%m) — hints + a soft pre-flight.
  // Slurm remains the authority (QOS/account rules can differ); the launch
  // is never blocked, but a request already over the published limit gets
  // told BEFORE the round-trip.
  const selected = $derived(partitions.find((p) => p.name === partition.trim()) ?? null);

  /** sinfo %m is MB ("128000+") — read as "128G+ mem/node". */
  function fmtMemPerNode(raw: string): string {
    const m = raw.match(/^(\d+)(\+?)$/);
    if (m === null) return raw;
    return `${Math.round(Number(m[1]) / 1000)}G${m[2]}`;
  }

  const partitionHint = $derived.by(() => {
    if (selected === null) return "";
    const parts: string[] = [];
    if (selected.time_limit !== "") parts.push(`up to ${selected.time_limit}`);
    if (selected.cpus_per_node !== "") parts.push(`${selected.cpus_per_node} cpus/node`);
    if (selected.mem_per_node !== "") parts.push(`${fmtMemPerNode(selected.mem_per_node)}/node`);
    return parts.join(" · ");
  });

  const timeWarning = $derived.by(() => {
    if (selected === null || selected.time_limit === "") return null;
    const limit = parseSlurmTimeLeft(selected.time_limit);
    const asked = parseSlurmTimeLeft(time);
    if (limit === null || asked === null || asked <= limit) return null;
    return `${selected.name} allows at most ${selected.time_limit}`;
  });

  async function submit(): Promise<void> {
    if (!canSubmit) return;
    error = null;
    busy = true;
    // Empty optional fields are OMITTED, not sent as "" — the daemon composes
    // the srun argv from what's present.
    const spec: ComputeLaunchSpec = {
      name: name.trim() === "" ? "session" : name.trim(),
      time,
    };
    if (partition.trim() !== "") spec.partition = partition.trim();
    if (cpus !== null && Number.isFinite(cpus) && cpus >= 1) spec.cpus = Math.floor(cpus);
    if (mem.trim() !== "") spec.mem = mem.trim();
    if (gres.trim() !== "") spec.gres = gres.trim();
    if (prelude.trim() !== "") spec.prelude = prelude;
    if (routable) spec.routable = true;
    try {
      await launchComputeSession(spec);
      onLaunched();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      busy = false;
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  }

  function focusOnMount(node: HTMLElement): void {
    node.focus();
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="overlay">
  <button class="scrim" aria-label="close" tabindex="-1" onclick={onClose}></button>
  <div
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label="new compute session"
    tabindex="-1"
    use:modalFocus
  >
    <form
      class="body"
      onsubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div class="head">
        <span class="title">new compute session</span>
        <span class="host">srun on {alias}</span>
      </div>
      <div class="fields">
        <label class="field">
          <span class="lab">name</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="in"
            bind:value={name}
            spellcheck="false"
            autocomplete="off"
            use:focusOnMount
          />
        </label>
        <label class="field">
          <span class="lab">partition</span>
          {#if partitions.length > 0}
            <select class="in" bind:value={partition}>
              {#each partitions as p (p.name)}
                <option value={p.name}>
                  {p.name}{p.default ? " (default)" : ""}{p.avail ? "" : " — down"}
                </option>
              {/each}
            </select>
          {:else}
            <input
              class="in mono"
              bind:value={partition}
              placeholder="cluster default"
              spellcheck="false"
              autocomplete="off"
            />
          {/if}
          {#if partitionHint !== ""}
            <span class="hint mono">{partitionHint}</span>
          {/if}
        </label>
        <div class="triple">
          <div class="field">
            <span class="lab">time</span>
            <div class="dur" class:over={timeWarning !== null} role="group" aria-label="walltime">
              <label class="seg" title="days">
                <input type="number" min="0" max="99" step="1" bind:value={days} aria-label="days" />
                <span>d</span>
              </label>
              <label class="seg" title="hours">
                <input type="number" min="0" max="23" step="1" bind:value={hours} aria-label="hours" />
                <span>h</span>
              </label>
              <label class="seg" title="minutes">
                <input type="number" min="0" max="59" step="1" bind:value={mins} aria-label="minutes" />
                <span>m</span>
              </label>
            </div>
          </div>
          <label class="field">
            <span class="lab">cpus</span>
            <input class="in mono" type="number" min="1" step="1" bind:value={cpus} />
          </label>
          <label class="field">
            <span class="lab">mem</span>
            <input
              class="in mono"
              bind:value={mem}
              placeholder="4G"
              spellcheck="false"
              autocomplete="off"
            />
          </label>
        </div>
        {#if timeWarning !== null}
          <div class="preflight">{timeWarning} — Slurm will likely refuse this walltime</div>
        {/if}
        <label class="field">
          <span class="lab">gres</span>
          <input
            class="in mono"
            bind:value={gres}
            placeholder="gpu:1 — empty for none"
            spellcheck="false"
            autocomplete="off"
          />
        </label>
        <label class="field">
          <span class="lab">startup commands (prelude)</span>
          <textarea
            class="in mono prelude"
            bind:value={prelude}
            rows="3"
            spellcheck="false"
            placeholder={"module load …\nconda activate …"}
          ></textarea>
          <span class="hint">runs before the session starts — host and workspace preludes also apply</span>
        </label>
        <div class="advanced">
          <button
            type="button"
            class="adv-toggle"
            aria-expanded={advancedOpen}
            onclick={() => (advancedOpen = !advancedOpen)}
          >
            <svg
              class="chev"
              class:open={advancedOpen}
              viewBox="0 0 16 16"
              width="10"
              height="10"
              aria-hidden="true"
            >
              <path
                d="M6 4l4 4-4 4"
                fill="none"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            advanced
          </button>
          {#if advancedOpen}
            <label class="adv-row">
              <input type="checkbox" bind:checked={routable} />
              <span>routable bind</span>
            </label>
            <p class="adv-warn">exposes the daemon's port on the cluster network (token-gated)</p>
          {/if}
        </div>
        {#if error !== null}
          <div class="error">{error}</div>
        {/if}
      </div>
      <div class="acts">
        <button type="button" class="quiet" onclick={onClose}>cancel</button>
        <button type="submit" class="cta" disabled={!canSubmit}>
          {busy ? "submitting…" : "launch"}
        </button>
      </div>
    </form>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    animation: fade 0.1s ease-out;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }

  .scrim {
    position: absolute;
    inset: 0;
    appearance: none;
    border: none;
    padding: 0;
    background: var(--scrim);
    cursor: default;
  }

  .panel {
    position: relative;
    width: min(460px, calc(100vw - 2rem));
    max-height: 74vh;
    margin: 13vh auto 0;
    display: flex;
    flex-direction: column;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
    overflow: hidden;
  }

  /* The role="dialog" wrapper is the panel; the form just relays the flex
     column so `.fields` can scroll under the pinned head/actions. */
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .head {
    flex: none;
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 10px;
    padding: 14px 16px 8px;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  .host {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fields {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 4px 16px 12px;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .lab {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .in {
    min-width: 0;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 6px 10px;
    outline: none;
  }

  .in:focus {
    border-color: var(--focus-ring);
  }

  .in::placeholder {
    color: var(--muted);
    opacity: 0.7;
  }

  .in.mono {
    font-family: var(--mono);
  }

  .prelude {
    resize: vertical;
    min-height: 3.6em;
    line-height: 1.45;
  }

  .triple {
    display: grid;
    grid-template-columns: 1.4fr 0.8fr 0.8fr;
    gap: 10px;
  }

  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .hint.mono {
    font-family: var(--mono);
  }

  /* Walltime as d/h/m segments: one bordered pill per unit, the unit letter
     living inside the box — adjustable without Slurm-string surgery. */
  .dur {
    display: flex;
    gap: 6px;
    min-width: 0;
  }

  .seg {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--bg);
    cursor: text;
  }

  .seg:focus-within {
    border-color: var(--focus-ring);
  }

  .seg input {
    min-width: 0;
    width: 100%;
    border: none;
    background: none;
    color: var(--fg);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    padding: 6px 0 6px 8px;
    outline: none;
    text-align: right;
  }

  /* Spinner chrome crowds a 3ch box; arrow keys still step the value. */
  .seg input::-webkit-outer-spin-button,
  .seg input::-webkit-inner-spin-button {
    -webkit-appearance: none;
    margin: 0;
  }

  .seg span {
    flex: none;
    padding: 0 7px 0 3px;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* Over the partition's published ceiling: caution, not a block — Slurm
     stays the authority (QOS/accounts can differ), but say so up front. */
  .dur.over .seg {
    border-color: color-mix(in srgb, var(--warn) 55%, var(--edge));
  }

  .preflight {
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .adv-toggle {
    appearance: none;
    border: none;
    background: none;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 2px 0;
  }

  .adv-toggle:hover {
    color: var(--fg);
  }

  .chev {
    flex: none;
    transition: transform 0.12s ease;
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .adv-row {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: var(--text-sm);
    padding: 6px 0 0;
    cursor: pointer;
  }

  .adv-row input {
    accent-color: var(--accent);
    margin: 0;
  }

  /* The exposure warning wears the caution tone — routable means co-tenants
     can reach the port (the token stays the only gate). */
  .adv-warn {
    margin: 4px 0 0 21px;
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .error {
    font-size: var(--text-sm);
    color: var(--err);
    white-space: pre-wrap;
  }

  .acts {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    padding: 10px 16px 14px;
    border-top: 1px solid var(--edge);
  }

  .quiet {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    padding: 2px 8px;
    border-radius: 4px;
  }

  .quiet:hover {
    color: var(--fg);
  }

  .cta {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 5px 12px;
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }

  .cta:hover {
    border-color: var(--accent);
  }

  .cta:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
