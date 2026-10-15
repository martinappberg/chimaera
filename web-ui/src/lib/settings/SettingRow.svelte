<script lang="ts">
  /**
   * One settings row: title + description on the left, the typed control on
   * the right, centred on the title's line. A modified row (value differs
   * from default) carries a quiet accent bar and a reset button in a slot
   * of its own — the VS Code gutter language, muted.
   */
  import { onDestroy } from "svelte";
  import { ACTION_BY_ID, captureChord, displayChord, isBrowserReserved } from "../shared/keys";
  import { modifierSetting, setCapturing } from "../shared/keybindings";
  import { isNativeShell } from "../net/native";
  import type { SettingDef, SettingId, SettingValue } from "./schema";
  import { activeTheme, getSetting, isModified, resetSetting, setSetting } from "./store.svelte";
  import Segmented from "../shared/Segmented.svelte";
  import Switch from "../shared/Switch.svelte";

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

  /** A slider only where a drag can land on any value: a few hundred stops
   *  (scrollback, delays) make the knob a random-number picker. */
  const sliderFits = $derived(
    def.min !== undefined &&
      def.max !== undefined &&
      (def.max - def.min) / (def.step ?? 1) <= 120,
  );

  /** Characters the number field must show: the widest bound's integer
   *  digits plus the step's decimals (and the separator), so 200000 or 1.57
   *  sits inside the border rather than scrolling under it. */
  const numChars = $derived.by(() => {
    const intDigits = (n: number | undefined) =>
      n === undefined ? 0 : String(Math.trunc(Math.abs(n))).length + (n < 0 ? 1 : 0);
    const decimals = (String(def.step ?? 1).split(".")[1] ?? "").length;
    const digits = Math.max(intDigits(def.min), intDigits(def.max), 2) + (decimals > 0 ? decimals + 1 : 0);
    return Math.max(3, digits) + 0.5;
  });

  /** The default, as the reset button's tooltip states it. */
  const defaultLabel = $derived.by(() => {
    const d = def.default;
    if (def.type === "keybinding") return d === "" ? "disabled" : displayChord(d as string, modifierSetting());
    if (def.type === "enum") return def.options?.find((o) => o.value === d)?.label ?? String(d);
    if (def.type === "boolean") return d ? "on" : "off";
    if (Array.isArray(d) || d === "") return "";
    return def.unit ? `${d} ${def.unit}` : String(d);
  });

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

<div class="row setting-row" class:modified id={`setting-${def.id}`}>
  <div class="text">
    <!-- The settings.json key (and whether the daemon reads it) rides the
         title's tooltip: the JSON tab's vocabulary without a line per row. -->
    <div class="title" title={`${def.id}${def.scope === "daemon" ? " · read by the daemon" : ""}`}>
      {def.title}
    </div>
    <p class="desc">{def.description}</p>
    {#if def.note}
      <p class="note">{def.note}</p>
    {/if}
  </div>

  <div class="control">
    {#if def.type === "boolean"}
      <Switch on={value === true} label={def.title} onToggle={(next) => set(next)} />
    {:else if def.type === "number" || def.type === "integer"}
      <div class="num">
        {#if sliderFits && def.min !== undefined && def.max !== undefined}
          <!-- Pointer-only: the number field beside it is the one tab stop
               (arrow keys step it) and the one control assistive tech hears,
               so a row costs one Tab, not two. -->
          <input
            class="slider"
            type="range"
            tabindex="-1"
            aria-hidden="true"
            min={def.min}
            max={def.max}
            step={def.step ?? 1}
            value={value as number}
            style:--p={`${(((value as number) - def.min) / (def.max - def.min)) * 100}%`}
            aria-label={def.title}
            oninput={(e) => set(Number(e.currentTarget.value))}
          />
        {/if}
        <label class="numfield">
          <input
            class="numbox"
            style:width={`${numChars}ch`}
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
          {#if def.unit}<span class="unit">{def.unit}</span>{/if}
        </label>
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
        <Segmented
          options={def.options ?? []}
          value={value as string}
          label={def.title}
          onChange={(v) => set(v)}
        />
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

  <!-- A fixed slot, so every control keeps one right edge whether or not
       its row is modified. -->
  <div class="reset-slot">
    {#if modified}
      <button
        class="reset"
        title={`Reset to default${defaultLabel === "" ? "" : ` (${defaultLabel})`}`}
        aria-label={`Reset ${def.title} to default`}
        onclick={() => resetSetting(def.id)}
      >
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path
            d="M3.5 6.5A5 5 0 1 1 3 9.5"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
          />
          <path
            d="M3 3v3.5h3.5"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
    {/if}
  </div>
</div>

<style>
  /* Text | control | reset slot. The control and the reset slot sit on the
     title's line (--line), so every control lines up with its label whether
     the description runs one line or four. */
  .row {
    --line: 28px;
    position: relative;
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto 28px;
    column-gap: 0;
    align-items: start;
    padding: 12px 6px 13px 14px;
    scroll-margin-top: 8px;
  }

  /* Rows of one section part by a hairline inset to the text edge — the
     rhythm comes from spacing; the line only keeps long lists scannable. */
  /* Sibling rows are separate instances, so the left one is matched globally. */
  :global(.setting-row) + .row::before {
    content: "";
    position: absolute;
    top: 0;
    left: 14px;
    right: 34px;
    border-top: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }

  /* Modified marker: a quiet accent bar in the row's left padding. */
  .row.modified::after {
    content: "";
    position: absolute;
    left: 3px;
    top: 17px;
    height: 18px;
    width: 3px;
    border-radius: 2px;
    background: color-mix(in srgb, var(--accent) 70%, transparent);
  }

  .text {
    min-width: 0;
  }

  .title {
    display: flex;
    align-items: center;
    min-height: var(--line);
    font-size: var(--text-md);
    font-weight: 550;
    line-height: 1.3;
    color: var(--fg);
  }

  .reset-slot {
    margin-left: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    height: var(--line);
  }

  .reset {
    appearance: none;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: none;
    border-radius: 5px;
    background: none;
    color: var(--muted);
    opacity: 0.7;
    cursor: pointer;
    transition:
      opacity 0.12s ease,
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .reset:hover,
  .reset:focus-visible {
    opacity: 1;
    color: var(--fg);
    background: var(--row-hover);
  }

  .desc {
    margin: 1px 0 0;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    max-width: 62ch;
  }

  .note {
    margin: 4px 0 0;
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--warn);
    max-width: 62ch;
  }

  .control {
    margin-left: 28px;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    min-height: var(--line);
  }

  /* The theme cards are the one wide control. Above the stacking breakpoint
     their track may shrink below its content (the cards then wrap to three,
     two a row) while the text track keeps a readable floor; every other
     control keeps the auto track. Scoped to the wide layout: the stacked
     query below redraws the grid and must win there. */
  @container settings (width > 640px) {
    .row:has(.cards) {
      grid-template-columns: minmax(240px, 1fr) minmax(0, auto) 28px;
    }
    .control:has(.cards) {
      min-width: 0;
    }
  }

  /* The toggle and the segmented bar are the shared primitives
     (shared/Switch.svelte, shared/Segmented.svelte) — their recipes live there. */

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

  /* The bordered box is the label, so the unit sits inside the field and a
     click on it focuses the number. */
  /* Sized by its content (the input's ch width + the unit), never below a
     common width so short fields still line up as a column. */
  .numfield {
    flex: none;
    justify-content: flex-end;
    min-width: 86px;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 26px;
    box-sizing: border-box;
    padding: 0 8px;
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    cursor: text;
  }

  /* The app's one focus ring (app.css), drawn on the box the input fills. */
  .numfield:focus-within {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  /* Width set inline from the bounds (numChars). */
  .numbox {
    flex: none;
    padding: 0;
    font: inherit;
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    text-align: right;
    color: var(--fg);
    background: none;
    border: none;
    outline: none;
    appearance: textfield;
    -moz-appearance: textfield;
  }

  /* Arrow keys still step the value; the spinner glyphs were noise. */
  .numbox::-webkit-inner-spin-button,
  .numbox::-webkit-outer-spin-button {
    appearance: none;
    margin: 0;
  }

  .unit {
    flex: none;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  /* --- theme cards --- */
  .cards {
    --card-w: 92px;
    --card-gap: 10px;
    display: flex;
    flex-wrap: wrap;
    gap: var(--card-gap);
    /* Four windows to a row: an auto track sizes to its content, so without
       a cap a long list of themes keeps widening instead of wrapping. */
    max-width: calc(4 * var(--card-w) + 3 * var(--card-gap));
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
    width: var(--card-w);
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
  .select {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg);
    background: var(--term-bg);
    border: 1px solid var(--edge);
    border-radius: 6px;
    height: 26px;
    box-sizing: border-box;
    padding: 0 6px;
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
    height: 26px;
    box-sizing: border-box;
    padding: 0 10px;
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
    height: 26px;
    box-sizing: border-box;
    padding: 0 8px;
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
    height: 26px;
    box-sizing: border-box;
    padding: 0 8px;
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
      grid-template-columns: minmax(0, 1fr) 22px;
      grid-template-areas:
        "text reset"
        "control control";
      column-gap: 8px;
      row-gap: 8px;
    }

    :global(.setting-row) + .row::before {
      right: 6px;
    }

    .text {
      grid-area: text;
    }

    .reset-slot {
      grid-area: reset;
      margin-left: 0;
    }

    .control {
      grid-area: control;
      justify-content: flex-start;
      min-height: 0;
      margin-left: 0;
    }

    .textbox {
      width: 100%;
    }

    .keybind {
      align-items: flex-start;
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
    /* The shared Segmented's own `wrap` look, reached through the scope
       boundary — the query lives here because the container is this row's. */
    .control :global(.seg) {
      flex-wrap: wrap;
      gap: 5px;
      border: none;
      border-radius: 0;
      overflow: visible;
    }

    .control :global(.seg-btn) {
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
