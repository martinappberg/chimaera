<script lang="ts">
  /**
   * One settings row: title + description on the left, the typed control on
   * the right. A modified row (value differs from default) carries a quiet
   * accent bar and a reset affordance — the VS Code gutter language, muted.
   */
  import { onDestroy } from "svelte";
  import { ACTION_BY_ID, captureChord, displayChord, isBrowserReserved } from "../shared/keys";
  import { modifierSetting, setCapturing } from "../shared/keybindings";
  import { isNativeShell } from "../net/native";
  import type { SettingDef, SettingId, SettingValue } from "./schema";
  import { activeTheme, getSetting, isModified, resetSetting, setSetting } from "./store.svelte";

  interface Props {
    def: SettingDef;
  }

  let { def }: Props = $props();

  const id = $derived(def.id as SettingId);
  const modified = $derived(isModified(def.id));
  const value = $derived(getSetting(id) as SettingValue);

  function set(v: SettingValue): void {
    setSetting(id, v as never);
  }

  // --- keybinding --------------------------------------------------------------

  /** Arrow-set actions record any arrow as the four-arrow wildcard. */
  const arrowSet = $derived(ACTION_BY_ID.get(def.id.replace(/^keys\./, ""))?.arrowSet === true);

  let listening = $state(false);

  function stopCapture(): void {
    if (!listening) return;
    listening = false;
    setCapturing(false);
    window.removeEventListener("keydown", onCaptureKey, true);
  }

  function onCaptureKey(e: KeyboardEvent): void {
    // The press belongs to the recorder — it must not scroll, type, or fire
    // the action it happens to collide with (App's handler checks the flag).
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Escape") {
      stopCapture();
      return;
    }
    const chord = captureChord(e, arrowSet);
    if (chord === null) return; // modifier-only or uncapturable — keep listening
    set(chord);
    stopCapture();
  }

  function startCapture(): void {
    if (listening) {
      stopCapture();
      return;
    }
    listening = true;
    setCapturing(true);
    window.addEventListener("keydown", onCaptureKey, true);
  }

  onDestroy(stopCapture);

  const chordLabel = $derived.by(() => {
    if (def.type !== "keybinding") return "";
    const v = value as string;
    return v === "" ? "" : displayChord(v, modifierSetting());
  });

  /** The bound chord never reaches a browser page — only the app sees it. */
  const reservedInBrowser = $derived(
    def.type === "keybinding" &&
      (value as string) !== "" &&
      !isNativeShell() &&
      isBrowserReserved(value as string, modifierSetting()),
  );

  // --- number ----------------------------------------------------------------

  /** Local text while typing, so "1" en route to "14" never clamps mid-keystroke. */
  let numDraft = $state<string | null>(null);

  function commitNumber(raw: string): void {
    numDraft = null;
    const n = Number(raw);
    if (raw.trim() === "" || !Number.isFinite(n)) return; // keep current value
    set(n);
  }

  // --- string-list (chips) ----------------------------------------------------

  let chipDraft = $state("");

  function addChip(): void {
    const v = chipDraft.trim();
    chipDraft = "";
    if (v === "") return;
    const list = value as string[];
    if (list.includes(v)) return;
    set([...list, v]);
  }

  function removeChip(chip: string): void {
    set((value as string[]).filter((c) => c !== chip));
  }

  function onChipKeydown(e: KeyboardEvent): void {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      addChip();
    } else if (e.key === "Backspace" && chipDraft === "") {
      const list = value as string[];
      if (list.length > 0) removeChip(list[list.length - 1]);
    }
  }

  // --- color -------------------------------------------------------------------

  /** The swatch the native picker shows for "" (theme default): the active
   *  theme's own accent, tracked so it follows theme switches live. */
  const effectiveColor = $derived.by(() => {
    const v = value as string;
    return v !== "" ? v : activeTheme().tokens["--accent"];
  });
</script>

<div class="row" class:modified id={`setting-${def.id}`}>
  <div class="gutter" title={modified ? "modified" : undefined}></div>
  <div class="text">
    <div class="head">
      <span class="crumb">{def.category}</span>
      <span class="title">{def.title}</span>
      {#if def.scope === "daemon"}
        <span class="scope" title="consumed by the daemon on the host">daemon</span>
      {/if}
      {#if modified}
        <button class="reset" title="reset to default" onclick={() => resetSetting(def.id)}>
          reset
        </button>
      {/if}
    </div>
    <p class="desc">{def.description}</p>
    {#if def.note}
      <p class="note">{def.note}</p>
    {/if}
    <code class="id" title="settings.json key">{def.id}</code>
  </div>

  <div class="control">
    {#if def.type === "boolean"}
      <button
        class="toggle"
        class:on={value === true}
        role="switch"
        aria-checked={value === true}
        aria-label={def.title}
        onclick={() => set(!(value as boolean))}
      >
        <span class="knob"></span>
      </button>
    {:else if def.type === "number" || def.type === "integer"}
      <div class="num">
        {#if def.min !== undefined && def.max !== undefined}
          <input
            class="slider"
            type="range"
            min={def.min}
            max={def.max}
            step={def.step ?? 1}
            value={value as number}
            style:--p={`${(((value as number) - def.min) / (def.max - def.min)) * 100}%`}
            aria-label={def.title}
            oninput={(e) => set(Number(e.currentTarget.value))}
          />
        {/if}
        <input
          class="numbox"
          type="number"
          min={def.min}
          max={def.max}
          step={def.step ?? 1}
          value={numDraft ?? (value as number)}
          aria-label={def.title}
          oninput={(e) => (numDraft = e.currentTarget.value)}
          onchange={(e) => commitNumber(e.currentTarget.value)}
          onblur={(e) => commitNumber(e.currentTarget.value)}
        />
      </div>
    {:else if def.type === "enum" && def.control === "theme-cards"}
      <!-- Mini window previews: pane sheet + rail strip + text lines + the
           accent dot — enough of the palette to choose by eye. -->
      <div class="cards" role="radiogroup" aria-label={def.title}>
        {#each def.options ?? [] as opt (opt.value)}
          {@const [pane, rail, text, accent] = opt.swatch ?? []}
          <button
            class="card"
            class:on={value === opt.value}
            role="radio"
            aria-checked={value === opt.value}
            title={opt.label}
            onclick={() => set(opt.value)}
          >
            <span class="card-window" style:background={pane}>
              <span class="card-rail" style:background={rail}></span>
              <span class="card-page">
                <span class="card-line" style:background={text} style:opacity={0.85}></span>
                <span class="card-line short" style:background={text} style:opacity={0.45}></span>
                <span class="card-dot" style:background={accent}></span>
              </span>
            </span>
            <span class="card-label">{opt.label}</span>
          </button>
        {/each}
      </div>
    {:else if def.type === "enum"}
      {#if (def.options ?? []).length <= 4}
        <div class="seg" role="radiogroup" aria-label={def.title}>
          {#each def.options ?? [] as opt (opt.value)}
            <button
              class="seg-btn"
              class:on={value === opt.value}
              role="radio"
              aria-checked={value === opt.value}
              onclick={() => set(opt.value)}
            >
              {opt.label}
            </button>
          {/each}
        </div>
      {:else}
        <select
          class="select"
          value={value as string}
          aria-label={def.title}
          onchange={(e) => set(e.currentTarget.value)}
        >
          {#each def.options ?? [] as opt (opt.value)}
            <option value={opt.value}>{opt.label}</option>
          {/each}
        </select>
      {/if}
    {:else if def.type === "keybinding"}
      <div class="keybind">
        <div class="kb-row">
          <button
            class="kb-btn"
            class:listening
            class:unbound={(value as string) === "" && !listening}
            title={listening ? "press a chord — Esc cancels" : "click, then press the new chord"}
            onclick={startCapture}
          >
            {#if listening}
              recording…
            {:else if (value as string) === ""}
              disabled
            {:else}
              <kbd>{chordLabel}</kbd>
            {/if}
          </button>
          {#if (value as string) !== "" && !listening}
            <button class="kb-clear" title="disable this chord" onclick={() => set("")}>
              &times;
            </button>
          {/if}
        </div>
        {#if reservedInBrowser}
          <p class="kb-warn">browser-reserved — fires only in the chimaera app</p>
        {/if}
      </div>
    {:else if def.type === "color"}
      <div class="color">
        <input
          class="swatch"
          type="color"
          value={effectiveColor}
          aria-label={def.title}
          oninput={(e) => set(e.currentTarget.value)}
        />
        <input
          class="hex"
          type="text"
          placeholder="theme default"
          value={value as string}
          aria-label="{def.title} hex"
          onchange={(e) => {
            const v = e.currentTarget.value.trim();
            if (v === "" || /^#[0-9a-fA-F]{6}$/.test(v)) set(v);
            else e.currentTarget.value = value as string;
          }}
        />
      </div>
    {:else if def.type === "string-list"}
      <div class="chips" role="group" aria-label={def.title}>
        {#each value as string[] as chip (chip)}
          <span class="chip">
            {chip}
            <button class="chip-x" aria-label="remove {chip}" onclick={() => removeChip(chip)}
              >&times;</button
            >
          </span>
        {/each}
        <input
          class="chip-input"
          type="text"
          placeholder={(value as string[]).length === 0 ? (def.placeholder ?? "add…") : "add…"}
          bind:value={chipDraft}
          onkeydown={onChipKeydown}
          onblur={addChip}
          aria-label="add to {def.title}"
        />
      </div>
    {:else}
      <input
        class="textbox"
        type="text"
        placeholder={def.placeholder ?? ""}
        value={value as string}
        aria-label={def.title}
        onchange={(e) => set(e.currentTarget.value)}
      />
    {/if}
  </div>
</div>

<style>
  .row {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 24px;
    /* Left padding reserves the absolutely-positioned gutter's lane (14 + 3 + 24). */
    padding: 14px 18px 14px 41px;
    border-radius: 8px;
    transition: background-color 0.12s ease;
  }

  .row:hover {
    background: color-mix(in srgb, var(--fg) 3%, transparent);
  }

  /* Modified marker: a quiet accent bar down the left edge (VS Code language).
     Positioned out of flow so the row can switch to a stacked column when the
     pane is narrow without the bar reflowing across the top. */
  .gutter {
    position: absolute;
    left: 14px;
    top: 14px;
    bottom: 14px;
    width: 3px;
    border-radius: 2px;
    background: transparent;
  }

  .row.modified .gutter {
    background: color-mix(in srgb, var(--accent) 70%, transparent);
  }

  .text {
    flex: 1;
    min-width: 0;
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }

  .crumb {
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .crumb::after {
    content: ":";
  }

  .title {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--fg);
  }

  .scope {
    font-family: var(--mono);
    font-size: 10px;
    letter-spacing: 0.06em;
    color: var(--muted);
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 0 5px;
  }

  .reset {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0 4px;
    border-radius: 4px;
    opacity: 0;
    transition:
      opacity 0.12s ease,
      color 0.12s ease;
  }

  .row:hover .reset,
  .reset:focus-visible {
    opacity: 1;
  }

  .reset:hover {
    color: var(--accent);
  }

  .desc {
    margin: 3px 0 0;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 60ch;
  }

  .note {
    margin: 3px 0 0;
    font-size: var(--text-xs);
    color: var(--warn);
    opacity: 0.9;
  }

  .id {
    display: inline-block;
    max-width: 100%;
    overflow-wrap: anywhere;
    margin-top: 5px;
    font-family: var(--mono);
    font-size: 10px;
    color: var(--muted);
    opacity: 0;
    transition: opacity 0.12s ease;
    user-select: all;
  }

  .row:hover .id {
    opacity: 0.75;
  }

  .control {
    flex: none;
    display: flex;
    align-items: center;
    min-height: 24px;
    padding-top: 2px;
  }

  /* --- toggle --- */
  .toggle {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--row-hover);
    width: 34px;
    height: 19px;
    border-radius: 10px;
    padding: 0;
    cursor: pointer;
    position: relative;
    transition:
      background-color 0.14s ease,
      border-color 0.14s ease;
  }

  .toggle .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    background: var(--muted);
    transition:
      transform 0.14s ease,
      background-color 0.14s ease;
  }

  .toggle.on {
    background: color-mix(in srgb, var(--accent) 28%, var(--row-hover));
    border-color: color-mix(in srgb, var(--accent) 55%, var(--edge));
  }

  .toggle.on .knob {
    transform: translateX(15px);
    background: var(--accent);
  }

  /* --- number --- */
  .num {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  /* Quiet slider: a hairline track with a soft accent fill up to the thumb
     (--p, set inline) and a small bordered knob — the native accent-color
     widget reads as a loud foreign control next to the rest of the chrome. */
  .slider {
    appearance: none;
    width: 110px;
    height: 16px;
    margin: 0;
    background: none;
    cursor: pointer;
  }

  .slider::-webkit-slider-runnable-track {
    height: 3px;
    border-radius: 2px;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--accent) 55%, var(--row-active)) 0 var(--p, 50%),
      var(--row-active) var(--p, 50%)
    );
  }

  .slider::-webkit-slider-thumb {
    appearance: none;
    width: 12px;
    height: 12px;
    margin-top: -4.5px;
    border-radius: 50%;
    background: var(--overlay-bg);
    border: 1.5px solid var(--muted);
    transition: border-color 0.12s ease;
  }

  .slider:hover::-webkit-slider-thumb,
  .slider:active::-webkit-slider-thumb {
    border-color: var(--accent);
  }

  .slider::-moz-range-track {
    height: 3px;
    border-radius: 2px;
    background: var(--row-active);
  }

  .slider::-moz-range-progress {
    height: 3px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 55%, var(--row-active));
  }

  .slider::-moz-range-thumb {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--overlay-bg);
    border: 1.5px solid var(--muted);
    transition: border-color 0.12s ease;
  }

  .slider:hover::-moz-range-thumb,
  .slider:active::-moz-range-thumb {
    border-color: var(--accent);
  }

  .numbox {
    width: 74px;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 6px;
  }

  /* --- theme cards --- */
  .cards {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }

  .card {
    appearance: none;
    border: none;
    background: none;
    padding: 0;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 5px;
    cursor: pointer;
  }

  .card-window {
    display: flex;
    width: 92px;
    height: 56px;
    border: 1px solid var(--edge);
    border-radius: 7px;
    overflow: hidden;
    transition:
      border-color 0.12s ease,
      box-shadow 0.12s ease;
  }

  .card:hover .card-window {
    border-color: color-mix(in srgb, var(--fg) 25%, var(--edge));
  }

  .card.on .card-window {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }

  .card-rail {
    flex: none;
    width: 22px;
  }

  .card-page {
    flex: 1;
    position: relative;
    padding: 9px 9px 0;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .card-line {
    height: 4px;
    border-radius: 2px;
  }

  .card-line.short {
    width: 62%;
  }

  .card-dot {
    position: absolute;
    left: 9px;
    bottom: 8px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }

  .card-label {
    font-size: var(--text-xs);
    color: var(--muted);
    text-align: center;
    transition: color 0.12s ease;
  }

  .card:hover .card-label,
  .card.on .card-label {
    color: var(--fg);
  }

  .card.on .card-label {
    font-weight: 600;
  }

  /* --- enum --- */
  .seg {
    display: flex;
    border: 1px solid var(--edge);
    border-radius: 7px;
    overflow: hidden;
  }

  .seg-btn {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--muted);
    padding: 3px 11px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      color 0.12s ease;
  }

  .seg-btn + .seg-btn {
    border-left: 1px solid var(--edge);
  }

  .seg-btn:hover {
    color: var(--fg);
    background: var(--row-hover);
  }

  .seg-btn.on {
    color: var(--fg);
    font-weight: 600;
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }

  .select {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 6px;
  }

  /* --- keybinding --- */
  .keybind {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }

  .kb-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .kb-btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    border-radius: 6px;
    min-width: 96px;
    padding: 3px 10px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      color 0.12s ease;
  }

  .kb-btn kbd {
    font-family: var(--mono);
    font-size: var(--text-sm);
  }

  .kb-btn:hover {
    border-color: color-mix(in srgb, var(--fg) 25%, var(--edge));
  }

  .kb-btn.listening {
    border-color: var(--accent);
    color: var(--accent);
  }

  .kb-btn.unbound {
    color: var(--muted);
    font-style: italic;
  }

  .kb-clear {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-md);
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 4px;
  }

  .kb-clear:hover {
    color: var(--err);
  }

  .kb-warn {
    margin: 0;
    font-size: var(--text-xs);
    color: var(--warn);
  }

  /* --- color --- */
  .color {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .swatch {
    appearance: none;
    width: 26px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: none;
    cursor: pointer;
  }

  .swatch::-webkit-color-swatch-wrapper {
    padding: 2px;
  }

  .swatch::-webkit-color-swatch {
    border: none;
    border-radius: 4px;
  }

  .hex {
    width: 110px;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 6px;
  }

  /* --- string / list --- */
  .textbox {
    width: 200px;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px 8px;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    max-width: 300px;
    padding: 4px;
    border: 1px solid var(--edge);
    border-radius: 7px;
    background: var(--term-bg);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--fg);
    background: var(--row-hover);
    border-radius: 5px;
    padding: 1px 3px 1px 7px;
  }

  .chip-x {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    line-height: 1;
    color: var(--muted);
    cursor: pointer;
    padding: 0 3px;
    border-radius: 3px;
  }

  .chip-x:hover {
    color: var(--err);
  }

  .chip-input {
    flex: 1;
    min-width: 70px;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--fg);
    background: none;
    border: none;
    outline: none;
    padding: 2px 4px;
  }

  /* --- responsive: keyed off the `settings` named container declared on the
     .settings root in SettingsView (a child component can only query an
     ancestor's container, never its own). --- */

  /* Narrow: stack the control beneath the text, left-aligned under it. The
     segmented control also breaks into wrap-friendly pills here. */
  @container settings (max-width: 640px) {
    .row {
      flex-direction: column;
      align-items: stretch;
      gap: 10px;
      padding-left: 22px;
      padding-right: 14px;
    }

    .gutter {
      left: 12px;
    }

    .control {
      padding-top: 0;
      min-height: 0;
    }

    .textbox {
      width: 100%;
    }

    .chips {
      max-width: 100%;
    }

    .select {
      max-width: 100%;
    }
  }

  /* Very narrow: guarantee nothing overflows. The slider yields to its numbox,
     the hex field shrinks to fit, and the segmented bar — which can't shrink —
     wraps as separated pills. */
  @container settings (max-width: 360px) {
    .seg {
      flex-wrap: wrap;
      gap: 5px;
      border: none;
      border-radius: 0;
      overflow: visible;
    }

    .seg-btn {
      border: 1px solid var(--edge);
      border-radius: 6px;
    }

    .num {
      gap: 8px;
    }

    .slider {
      display: none;
    }

    .color {
      width: 100%;
    }

    .hex {
      width: auto;
      flex: 1 1 0;
      min-width: 0;
    }
  }
</style>
