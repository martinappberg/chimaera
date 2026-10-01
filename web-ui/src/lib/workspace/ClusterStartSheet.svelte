<script lang="ts">
  /**
   * Start a job on a cluster (workspaces open inside it), or continue a
   * running job in a new one. Built only from what the cluster reports
   * (`clusterFacts`: partitions, limits, accounts) and what the user chose
   * before (saved setups, the last job's setup) — no invented presets, no
   * site knowledge. Validation is inline and blocks the call; the
   * scheduler's own refusal comes back verbatim under the title, with the
   * one follow-up it allows (start attached, or fill the field it named).
   */
  import { onMount, tick } from "svelte";
  import {
    clusterContinueJob,
    clusterFacts,
    clusterSetStartup,
    clusterStartJob,
    type ClusterConfig,
    type ClusterFacts,
    type ClusterJob,
    type ClusterWorkspaceView,
    type LaunchSpec,
    type StartResult,
  } from "../net/native";
  import {
    accountChoices,
    buildSpec,
    defaultForm,
    formFromSpec,
    isInteractiveOnly,
    limitWords,
    nodeSizeWords,
    partitionTags,
    refusalField,
    walltimeSecs,
    type StartField,
    type StartForm,
  } from "./cluster";
  import { modalFocus } from "../shared/modalFocus";

  interface Props {
    alias: string;
    /** The cluster's config as the overview last read it. */
    config: ClusterConfig;
    /** The cluster-level startup commands (the Environment settings' own). */
    clusterStartup: string;
    /** Every workspace on the cluster (the closed ones can open in the job). */
    workspaces: ClusterWorkspaceView[];
    /** Workspaces ticked to open when it starts. */
    preselect?: string[];
    /** Continue this running job in a new one (its setup, its workspaces). */
    continueJob?: ClusterJob | null;
    /** Start from this setup ("Start again" on an ended job). */
    initialSpec?: LaunchSpec | null;
    /** The job was submitted (or started attached) — the page closes this
     *  sheet and refreshes. */
    onStarted: (result: Exclude<StartResult, { kind: "refused" }>) => void;
    onClose: () => void;
  }

  let {
    alias,
    config,
    clusterStartup,
    workspaces,
    preselect = [],
    continueJob = null,
    initialSpec = null,
    onStarted,
    onClose,
  }: Props = $props();

  /** Closed workspaces: the ones that can open in a new job. */
  const closed = $derived(workspaces.filter((w) => w.state === "closed"));
  /** What a continue moves over. */
  const moving = $derived(
    continueJob === null
      ? []
      : workspaces.filter((w) => w.job === continueJob.id && w.state === "open"),
  );
  // svelte-ignore state_referenced_locally
  let openIds = $state<string[]>([...preselect]);

  function toggleOpen(id: string): void {
    openIds = openIds.includes(id) ? openIds.filter((x) => x !== id) : [...openIds, id];
  }

  let facts = $state<ClusterFacts | null>(null);
  let factsError = $state<string | null>(null);
  let factsLoading = $state(true);

  // The sheet mounts fresh per open: the props' first values are the
  // baseline on purpose.
  // svelte-ignore state_referenced_locally
  let form = $state<StartForm>(defaultForm(null));
  /** The chip whose spec the form holds ("last" or a setup name). */
  let activeChip = $state<string | null>(null);
  /** The workspace's last run is the preselection; it may be gone from the
   *  cluster's list since (said once, under the list). */
  let missingPartition = $state<string | null>(null);

  // svelte-ignore state_referenced_locally
  let startupCluster = $state(clusterStartup);
  // svelte-ignore state_referenced_locally
  let startupRun = $state(continueJob?.startup ?? "");
  /** What the cluster scope holds on the cluster now (moves after a save). */
  // svelte-ignore state_referenced_locally
  let savedCluster = $state(clusterStartup);
  // svelte-ignore state_referenced_locally
  let startupOpen = $state(clusterStartup.trim() !== "" || (continueJob?.startup ?? "").trim() !== "");

  let moreOpen = $state(false);
  let saveAs = $state("");

  let errors = $state<Partial<Record<StartField, string>>>({});
  let refusal = $state<Extract<StartResult, { kind: "refused" }> | null>(null);
  /** A call failed outright (the shell, ssh) — not the scheduler's answer. */
  let startError = $state<string | null>(null);
  let busy = $state(false);

  /** Taught by refusals during this sheet (the shell persists them too). */
  let learnedRequires = $state<string[]>([]);
  let learnedInteractive = $state<string[]>([]);

  const effectiveConfig = $derived<ClusterConfig>({
    ...config,
    learned: {
      ...config.learned,
      interactive_only: [...(config.learned.interactive_only ?? []), ...learnedInteractive],
      requires: [...(config.learned.requires ?? []), ...learnedRequires],
    },
  });
  const requires = $derived(effectiveConfig.learned.requires ?? []);

  const lastSpec = $derived<LaunchSpec | null>(config.last_spec ?? null);
  const selected = $derived(facts?.partitions.find((p) => p.name === form.partition) ?? null);
  const accounts = $derived(accountChoices(facts, selected));
  // One account and nothing requiring it: there is no choice to make, and
  // Slurm uses that account anyway — no field, no --account.
  const showAccount = $derived(accounts.length > 1 || requires.includes("account"));
  const qosRequired = $derived(requires.includes("qos"));
  const constraintRequired = $derived(requires.includes("constraint"));
  const attachedRun = $derived(isInteractiveOnly(effectiveConfig, form.partition));

  const timeHint = $derived.by(() => {
    if (selected === null) return "";
    if (selected.max_time_secs !== null) {
      return `${selected.name} allows up to ${limitWords(selected.max_time_secs)}`;
    }
    return "";
  });
  /** Live pre-flight under the boxes (Start re-checks the same rule). */
  const timeOver = $derived(
    selected !== null &&
      selected.max_time_secs !== null &&
      walltimeSecs(form) > selected.max_time_secs,
  );

  const startupSummary = $derived.by(() => {
    const count = (s: string) => s.split("\n").filter((l) => l.trim() !== "").length;
    const parts: string[] = [];
    const c = count(startupCluster);
    const r = count(startupRun);
    if (c > 0) parts.push(`${c} for every job on ${alias}`);
    if (r > 0) parts.push(`${r} for this job`);
    return parts.length === 0 ? "none" : parts.join(" · ");
  });

  let accountEl = $state<HTMLSelectElement | HTMLInputElement | null>(null);
  let qosEl = $state<HTMLInputElement | null>(null);
  let constraintEl = $state<HTMLInputElement | null>(null);
  let daysEl = $state<HTMLInputElement | null>(null);
  let cpusEl = $state<HTMLInputElement | null>(null);
  let memEl = $state<HTMLInputElement | null>(null);
  let gpusEl = $state<HTMLInputElement | null>(null);
  let fieldsEl = $state<HTMLDivElement | null>(null);

  async function loadFacts(refresh = false): Promise<void> {
    factsLoading = true;
    factsError = null;
    try {
      facts = await clusterFacts(alias, refresh);
    } catch (e) {
      factsError = e instanceof Error ? e.message : String(e);
    } finally {
      factsLoading = false;
    }
  }

  onMount(() => {
    void loadFacts().then(() => {
      if (continueJob !== null) applySpec(continueJob.spec, "job");
      else if (initialSpec !== null) applySpec(initialSpec, "again");
      else if (config.last_spec) applySpec(config.last_spec, "last");
      else {
        form = defaultForm(facts);
        reconcileAccount();
      }
    });
  });

  /** Keep the account on one the select offers (default first). */
  function reconcileAccount(): void {
    if (!showAccount) {
      form.account = "";
      return;
    }
    if (accounts.length === 0) return;
    if (accounts.includes(form.account)) return;
    const def = facts?.default_account ?? null;
    form.account = def !== null && accounts.includes(def) ? def : accounts[0];
  }

  function applySpec(spec: LaunchSpec, chip: string): void {
    form = formFromSpec(spec, facts);
    activeChip = chip;
    missingPartition = null;
    if (facts !== null && form.partition !== "" && !facts.partitions.some((p) => p.name === form.partition)) {
      missingPartition = form.partition;
      form.partition = defaultForm(facts).partition;
    }
    if (form.qos !== "" || form.constraint !== "") moreOpen = true;
    errors = {};
    reconcileAccount();
  }

  function pickPartition(name: string): void {
    form.partition = name;
    activeChip = null;
    missingPartition = null;
    if (errors.time !== undefined) errors = { ...errors, time: undefined };
    reconcileAccount();
  }

  function edited(field: StartField): void {
    activeChip = null;
    if (errors[field] !== undefined) errors = { ...errors, [field]: undefined };
  }

  function fieldEl(f: StartField): HTMLElement | null {
    switch (f) {
      case "account":
        return accountEl;
      case "qos":
        return qosEl;
      case "constraint":
        return constraintEl;
      case "time":
        return daysEl;
      case "cpus":
        return cpusEl;
      case "mem":
        return memEl;
      case "gpus":
        return gpusEl;
      default:
        return null;
    }
  }

  async function focusField(f: StartField): Promise<void> {
    if ((f === "qos" && !qosRequired) || (f === "constraint" && !constraintRequired)) moreOpen = true;
    await tick();
    fieldEl(f)?.focus();
  }

  /** Save edited cluster-level startup commands before the job reads them. */
  async function saveStartup(): Promise<void> {
    if (startupCluster !== savedCluster) {
      await clusterSetStartup(alias, null, startupCluster);
      savedCluster = startupCluster;
    }
  }

  async function start(forceAttached = false): Promise<void> {
    if (busy) return;
    const built = buildSpec(form, { partition: selected, requires });
    errors = built.errors;
    if (built.spec === null) {
      const order: StartField[] = ["time", "cpus", "mem", "gpus", "account", "qos", "constraint"];
      const first = order.find((f) => built.errors[f] !== undefined);
      if (first !== undefined) void focusField(first);
      return;
    }
    busy = true;
    refusal = null;
    startError = null;
    try {
      await saveStartup();
    } catch (e) {
      startError = `Couldn't save the startup commands: ${e instanceof Error ? e.message : String(e)}`;
      busy = false;
      return;
    }
    const name = saveAs.trim();
    let result: StartResult;
    try {
      result =
        continueJob !== null
          ? await clusterContinueJob(alias, { jobId: continueJob.id }, built.spec, startupRun)
          : await clusterStartJob(
              alias,
              built.spec,
              openIds.filter((id) => closed.some((w) => w.id === id)),
              startupRun,
              null,
              name === "" ? null : name,
              forceAttached || attachedRun,
            );
    } catch (e) {
      startError = e instanceof Error ? e.message : String(e);
      busy = false;
      return;
    }
    busy = false;
    if (result.kind !== "refused") {
      onStarted(result);
      return;
    }
    refusal = result;
    if (result.refusal === "batch_not_allowed" && form.partition !== "") {
      if (!learnedInteractive.includes(form.partition)) {
        learnedInteractive = [...learnedInteractive, form.partition];
      }
    }
    const field = refusalField(result.refusal);
    if (field !== null) {
      if (!requires.includes(field)) learnedRequires = [...learnedRequires, field];
      void focusField(field);
    } else {
      await tick();
      fieldsEl?.scrollTo({ top: 0 });
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape" && !busy) {
      e.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="overlay">
  <button class="scrim" aria-label="Close" tabindex="-1" onclick={() => !busy && onClose()}></button>
  <div
    class="panel"
    role="dialog"
    aria-modal="true"
    aria-label={continueJob !== null ? `Continue ${continueJob.name} in a new job` : `Start a job on ${alias}`}
    tabindex="-1"
    use:modalFocus
  >
    <form
      class="body"
      onsubmit={(e) => {
        e.preventDefault();
        void start();
      }}
    >
      <div class="head">
        <div class="title-line">
          {#if continueJob !== null}
            <span class="title">Continue {continueJob.name} in a new job</span>
          {:else}
            <span class="title">Start a job on {alias}</span>
          {/if}
        </div>
        {#if continueJob !== null}
          <span class="sub">
            {moving.length === 0
              ? "Nothing is open in it."
              : `Moves: ${moving.map((w) => w.name).join(", ")}.`}
            When the new job starts, {moving.length === 0 ? "it" : "they"} move over and {continueJob.name} stops.
          </span>
        {:else}
          <span class="sub">Workspaces open inside it. Slurm queues it; you'll get a notification when it's ready.</span>
        {/if}
        {#if refusal !== null}
          <div class="refusal" role="alert">
            <span class="refusal-lead">Slurm didn't take it:</span>
            <span class="refusal-text">{refusal.message}</span>
            {#if refusal.refusal === "batch_not_allowed"}
              <button type="button" class="refusal-act" disabled={busy} onclick={() => void start(true)}
                >Start it attached instead — it stops if this app disconnects</button
              >
            {/if}
          </div>
        {/if}
        {#if startError !== null}
          <div class="refusal" role="alert"><span class="refusal-text">{startError}</span></div>
        {/if}
      </div>

      <div class="fields" bind:this={fieldsEl}>
        {#if factsLoading && facts === null}
          <div class="loading" role="status">Reading the cluster's partitions…</div>
        {:else}
          <!-- Saved setups: the user's own, never invented presets. -->
          <div class="block">
            <span class="lab">Setup</span>
            {#if continueJob === null && lastSpec === null && config.setups.length === 0}
              <span class="hint">Your choices are remembered for next time.</span>
            {:else}
              <div class="chips">
                {#if continueJob !== null}
                  <button
                    type="button"
                    class="chip"
                    class:on={activeChip === "job"}
                    onclick={() => continueJob !== null && applySpec(continueJob.spec, "job")}
                    >{continueJob.name}'s</button
                  >
                {:else if lastSpec !== null}
                  <button
                    type="button"
                    class="chip"
                    class:on={activeChip === "last"}
                    onclick={() => lastSpec !== null && applySpec(lastSpec, "last")}>Last used</button
                  >
                {/if}
                {#each config.setups as s, i (`${i}:${s.name}`)}
                  <button
                    type="button"
                    class="chip"
                    class:on={activeChip === `setup:${s.name}`}
                    onclick={() => applySpec(s.spec, `setup:${s.name}`)}>{s.name}</button
                  >
                {/each}
              </div>
            {/if}
          </div>

          <div class="block">
            <div class="lab-line">
              <span class="lab">Partition</span>
              <button
                type="button"
                class="link"
                disabled={factsLoading}
                onclick={() => void loadFacts(true)}>{factsLoading ? "Reading…" : "Refresh"}</button
              >
            </div>
            {#if factsError !== null}
              <div class="err">Couldn't read the partitions: {factsError}</div>
              <input
                class="in mono"
                bind:value={form.partition}
                placeholder="partition name — blank for the cluster's default"
                spellcheck="false"
                autocomplete="off"
                oninput={() => edited("partition")}
              />
            {:else if facts !== null && facts.partitions.length === 0}
              <span class="hint">The cluster listed no partitions; the job goes to its default.</span>
            {:else if facts !== null}
              <div class="plist" role="radiogroup" aria-label="Partition">
                {#each facts.partitions as p (p.name)}
                  {@const tags = partitionTags(p, effectiveConfig)}
                  <button
                    type="button"
                    class="prow"
                    class:on={form.partition === p.name}
                    class:down={!p.up}
                    role="radio"
                    aria-checked={form.partition === p.name}
                    onclick={() => pickPartition(p.name)}
                  >
                    <span class="radio" aria-hidden="true"></span>
                    <span class="pname">{p.name}</span>
                    <span class="tags">
                      {#each tags as t, i (i)}
                        <span class="tag {t.tone}">{t.text}</span>
                      {/each}
                    </span>
                  </button>
                {/each}
              </div>
              {#if missingPartition !== null}
                <span class="hint warn">{missingPartition} isn't listed any more — pick another.</span>
              {/if}
              {#if selected !== null && nodeSizeWords(selected) !== ""}
                <span class="hint">{nodeSizeWords(selected)}</span>
              {/if}
            {/if}
          </div>

          <div class="block">
            <span class="lab" id="start-time-lab">Time limit</span>
            <div class="dur" class:over={timeOver || errors.time !== undefined} role="group" aria-labelledby="start-time-lab">
              <label class="seg" title="days">
                <input
                  type="number"
                  min="0"
                  max="365"
                  step="1"
                  bind:value={form.days}
                  bind:this={daysEl}
                  aria-label="days"
                  oninput={() => edited("time")}
                />
                <span>d</span>
              </label>
              <label class="seg" title="hours">
                <input
                  type="number"
                  min="0"
                  max="23"
                  step="1"
                  bind:value={form.hours}
                  aria-label="hours"
                  oninput={() => edited("time")}
                />
                <span>h</span>
              </label>
              <label class="seg" title="minutes">
                <input
                  type="number"
                  min="0"
                  max="59"
                  step="1"
                  bind:value={form.mins}
                  aria-label="minutes"
                  oninput={() => edited("time")}
                />
                <span>m</span>
              </label>
            </div>
            {#if errors.time !== undefined}
              <span class="err">{errors.time}</span>
            {:else if timeOver}
              <span class="err">{timeHint}.</span>
            {:else if timeHint !== ""}
              <span class="hint">{timeHint}.</span>
            {/if}
          </div>

          <div class="block">
            <span class="lab">Resources <span class="aside">· blank uses the cluster's default</span></span>
            <span class="hint">Each workspace you open in the job uses about 100 MB of its memory.</span>
            <div class="triple">
              <label class="field">
                <span class="sublab">CPUs</span>
                <input
                  class="in mono"
                  bind:value={form.cpus}
                  bind:this={cpusEl}
                  inputmode="numeric"
                  placeholder="default"
                  spellcheck="false"
                  autocomplete="off"
                  oninput={() => edited("cpus")}
                />
                {#if errors.cpus !== undefined}<span class="err">{errors.cpus}</span>{/if}
              </label>
              <label class="field">
                <span class="sublab">Memory</span>
                <input
                  class="in mono"
                  bind:value={form.mem}
                  bind:this={memEl}
                  placeholder="e.g. 16G"
                  spellcheck="false"
                  autocomplete="off"
                  oninput={() => edited("mem")}
                />
                {#if errors.mem !== undefined}<span class="err">{errors.mem}</span>{/if}
              </label>
              <label class="field">
                <span class="sublab">GPUs</span>
                <input
                  class="in mono"
                  bind:value={form.gpus}
                  bind:this={gpusEl}
                  inputmode="numeric"
                  placeholder="none"
                  spellcheck="false"
                  autocomplete="off"
                  oninput={() => edited("gpus")}
                />
                {#if errors.gpus !== undefined}
                  <span class="err">{errors.gpus}</span>
                {:else if selected !== null && !selected.gpus && form.gpus.trim() !== "" && form.gpus.trim() !== "0"}
                  <span class="hint warn">{selected.name} reports no GPUs</span>
                {/if}
              </label>
            </div>
          </div>

          {#if showAccount}
            <label class="block">
              <span class="lab">Account</span>
              {#if accounts.length > 0}
                <select
                  class="in mono"
                  bind:value={form.account}
                  bind:this={accountEl}
                  onchange={() => edited("account")}
                >
                  {#each accounts as a (a)}
                    <option value={a}>{a}{a === facts?.default_account ? " (default)" : ""}</option>
                  {/each}
                </select>
              {:else}
                <input
                  class="in mono"
                  bind:value={form.account}
                  bind:this={accountEl}
                  placeholder="your Slurm account"
                  spellcheck="false"
                  autocomplete="off"
                  oninput={() => edited("account")}
                />
              {/if}
              {#if errors.account !== undefined}<span class="err">{errors.account}</span>{/if}
            </label>
          {/if}

          {#snippet qosField()}
            <label class="field">
              <span class="sublab">QOS{qosRequired ? "" : " (optional)"}</span>
              <input
                class="in mono"
                bind:value={form.qos}
                bind:this={qosEl}
                placeholder={qosRequired ? "required here" : "cluster default"}
                spellcheck="false"
                autocomplete="off"
                oninput={() => edited("qos")}
              />
              {#if errors.qos !== undefined}<span class="err">{errors.qos}</span>{/if}
            </label>
          {/snippet}
          {#snippet constraintField()}
            <label class="field">
              <span class="sublab">Constraint{constraintRequired ? "" : " (optional)"}</span>
              <input
                class="in mono"
                bind:value={form.constraint}
                bind:this={constraintEl}
                placeholder={constraintRequired ? "required here" : "node features, e.g. a CPU type"}
                spellcheck="false"
                autocomplete="off"
                oninput={() => edited("constraint")}
              />
              {#if errors.constraint !== undefined}<span class="err">{errors.constraint}</span>{/if}
            </label>
          {/snippet}

          {#if qosRequired || constraintRequired}
            <div class="pair">
              {#if qosRequired}{@render qosField()}{/if}
              {#if constraintRequired}{@render constraintField()}{/if}
            </div>
          {/if}
          {#if !qosRequired || !constraintRequired}
            <div class="block">
              <button
                type="button"
                class="disclose"
                aria-expanded={moreOpen}
                onclick={() => (moreOpen = !moreOpen)}
              >
                <svg class="chev" class:open={moreOpen} viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
                  <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                </svg>
                More
              </button>
              {#if moreOpen}
                <div class="pair">
                  {#if !qosRequired}{@render qosField()}{/if}
                  {#if !constraintRequired}{@render constraintField()}{/if}
                </div>
              {/if}
            </div>
          {/if}

          <div class="block">
            <button
              type="button"
              class="disclose"
              aria-expanded={startupOpen}
              onclick={() => (startupOpen = !startupOpen)}
            >
              <svg class="chev" class:open={startupOpen} viewBox="0 0 16 16" width="10" height="10" aria-hidden="true">
                <path d="M6 4l4 4-4 4" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
              Startup commands
              {#if !startupOpen}<span class="aside">· {startupSummary}</span>{/if}
            </button>
            {#if startupOpen}
              <span class="hint"
                >Run before every chat and terminal: {alias}'s first, then each workspace's own, then this job's.</span
              >
              <label class="field">
                <span class="sublab">Every job on {alias}</span>
                <textarea
                  class="in mono startup"
                  bind:value={startupCluster}
                  rows="2"
                  spellcheck="false"
                  placeholder={"module load …"}
                ></textarea>
              </label>
              <label class="field">
                <span class="sublab">This job</span>
                <textarea
                  class="in mono startup"
                  bind:value={startupRun}
                  rows="2"
                  spellcheck="false"
                  placeholder={"export …"}
                ></textarea>
              </label>
            {/if}
          </div>

          {#if continueJob === null}
            <div class="block">
              <span class="lab">Open when it starts</span>
              {#if closed.length === 0}
                <span class="hint">
                  {workspaces.length === 0
                    ? "No workspaces yet — add one, then open it in this job."
                    : "Every workspace is already open in a job."}
                </span>
              {:else}
                <div class="opens">
                  {#each closed as w (w.id)}
                    <label class="open-row">
                      <input
                        type="checkbox"
                        checked={openIds.includes(w.id)}
                        onchange={() => toggleOpen(w.id)}
                      />
                      <span class="open-name">{w.name}</span>
                      <span class="open-path" title={w.path}>{w.path}</span>
                    </label>
                  {/each}
                </div>
              {/if}
            </div>
          {/if}

          {#if continueJob === null}
          <label class="block">
            <span class="lab">Save as a setup <span class="aside">· optional</span></span>
            <input
              class="in"
              bind:value={saveAs}
              placeholder="a name, to start a job this way again"
              spellcheck="false"
              autocomplete="off"
            />
          </label>
          {/if}
        {/if}
      </div>

      <div class="acts">
        {#if attachedRun}
          <span class="acts-note">Interactive only — stops if this app disconnects</span>
        {/if}
        <button type="button" class="quiet" disabled={busy} onclick={onClose}>Cancel</button>
        <button type="submit" class="cta" disabled={busy || (factsLoading && facts === null)}>
          {busy ? "Starting…" : continueJob !== null ? "Start new job" : "Start job"}
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
    width: min(540px, calc(100vw - 2rem));
    max-height: 84vh;
    margin: 8vh auto 0;
    display: flex;
    flex-direction: column;
    background: var(--overlay-bg);
    border: 1px solid var(--edge);
    border-radius: 9px;
    box-shadow: 0 12px 36px rgba(0, 0, 0, 0.22);
    overflow: hidden;
  }

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
    flex-direction: column;
    gap: 3px;
    padding: 16px 18px 10px;
  }

  .title-line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }

  .title {
    font-size: var(--text-md);
    font-weight: 600;
  }

  .host {
    font-size: var(--text-xs);
    color: var(--muted);
    white-space: nowrap;
  }

  .sub {
    font-size: var(--text-sm);
    color: var(--muted);
    line-height: 1.45;
  }

  /* Open when it starts: one quiet checkbox row per closed workspace. */
  .opens {
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: 168px;
    overflow-y: auto;
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px;
  }

  .open-row {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
    padding: 5px 8px;
    border-radius: 4px;
    cursor: pointer;
  }

  .open-row:hover {
    background: var(--row-hover);
  }

  .open-row input {
    flex: none;
    margin: 0;
    accent-color: var(--accent);
  }

  .open-name {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
  }

  .open-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* The scheduler's own words, verbatim, in the danger wash. */
  .refusal {
    margin-top: 8px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    padding: 9px 11px;
    border-radius: 7px;
    background: color-mix(in srgb, var(--err) 9%, transparent);
    font-size: var(--text-sm);
    line-height: 1.45;
  }

  .refusal-lead {
    font-weight: 600;
    color: var(--err);
  }

  .refusal-text {
    font-family: var(--mono);
    font-size: var(--text-xs);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--fg);
  }

  .refusal-act {
    appearance: none;
    font: inherit;
    font-size: var(--text-sm);
    padding: 4px 10px;
    border-radius: 6px;
    border: 1px solid var(--edge);
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }

  .refusal-act:hover:enabled {
    border-color: var(--accent);
  }

  .fields {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 4px 18px 16px;
    scrollbar-width: thin;
    scrollbar-color: color-mix(in srgb, var(--fg) 22%, transparent) transparent;
  }

  .loading {
    padding: 18px 0;
    font-size: var(--text-sm);
    color: var(--muted);
    animation: breathe 1.4s ease-in-out infinite;
  }

  @keyframes breathe {
    50% {
      opacity: 0.45;
    }
  }

  :global(html.app-hidden) .loading {
    animation-play-state: paused;
  }

  .block {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }

  .lab-line {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }

  .lab {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--muted);
  }

  .sublab {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .aside {
    font-weight: 400;
    opacity: 0.85;
  }

  .hint {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .hint.warn,
  .err {
    font-size: var(--text-xs);
  }

  .hint.warn {
    color: var(--warn);
  }

  .err {
    color: var(--err);
  }

  .link {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
  }

  .link:hover:enabled {
    color: var(--fg);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .chip {
    appearance: none;
    font: inherit;
    font-size: var(--text-sm);
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid var(--edge);
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }

  .chip:hover {
    border-color: color-mix(in srgb, var(--accent) 60%, var(--edge));
  }

  .chip.on {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, var(--bg));
    color: var(--accent);
  }

  .plist {
    display: flex;
    flex-direction: column;
    max-height: 210px;
    overflow-y: auto;
    border: 1px solid var(--edge);
    border-radius: 7px;
    background: var(--bg);
    scrollbar-width: thin;
  }

  .prow {
    appearance: none;
    border: none;
    border-bottom: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
    background: none;
    font: inherit;
    color: var(--fg);
    text-align: left;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 10px;
    cursor: pointer;
  }

  .prow:last-child {
    border-bottom: none;
  }

  .prow:hover {
    background: var(--row-hover);
  }

  .prow.on {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .prow.down .pname {
    color: var(--muted);
  }

  .radio {
    flex: none;
    width: 11px;
    height: 11px;
    border-radius: 50%;
    border: 1.5px solid color-mix(in srgb, var(--muted) 70%, transparent);
    box-sizing: border-box;
  }

  .prow.on .radio {
    border-color: var(--accent);
    background: radial-gradient(circle, var(--accent) 0 3px, transparent 3.5px);
  }

  .pname {
    flex: none;
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    min-width: 0;
  }

  .tag {
    font-size: 11px;
    line-height: 16px;
    padding: 0 6px;
    border-radius: 4px;
    white-space: nowrap;
    color: var(--muted);
    background: color-mix(in srgb, var(--fg) 6%, transparent);
  }

  .tag.accent {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 11%, transparent);
  }

  .tag.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 13%, transparent);
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

  .startup {
    resize: vertical;
    min-height: 2.9em;
    line-height: 1.45;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }

  .triple {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 10px;
  }

  .pair {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
  }

  /* Walltime as d/h/m segments: one bordered pill per unit, the unit letter
     inside the box — adjustable without Slurm-string surgery. */
  .dur {
    display: flex;
    gap: 6px;
    max-width: 260px;
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

  .dur.over .seg {
    border-color: color-mix(in srgb, var(--err) 55%, var(--edge));
  }

  .disclose {
    appearance: none;
    border: none;
    background: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    align-self: flex-start;
    font: inherit;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 0;
  }

  .disclose:hover {
    color: var(--fg);
  }

  .chev {
    flex: none;
    transition: transform 0.12s ease;
  }

  .chev.open {
    transform: rotate(90deg);
  }

  .acts {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    padding: 11px 18px 14px;
    border-top: 1px solid var(--edge);
  }

  .acts-note {
    margin-right: auto;
    font-size: var(--text-xs);
    color: var(--warn);
  }

  .quiet {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
  }

  .quiet:hover:enabled {
    color: var(--fg);
  }

  .cta {
    appearance: none;
    border: 1px solid color-mix(in srgb, var(--accent) 55%, var(--edge));
    background: color-mix(in srgb, var(--accent) 12%, var(--bg));
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    padding: 5px 16px;
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.12s ease;
  }

  .cta:hover:enabled {
    border-color: var(--accent);
  }

  .cta:disabled {
    opacity: 0.55;
    cursor: default;
  }
</style>
