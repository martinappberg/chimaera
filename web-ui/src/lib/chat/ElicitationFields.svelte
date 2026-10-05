<script lang="ts">
  import ElicitationFields from "./ElicitationFields.svelte";
  import type { ElicitationField } from "./elicitation";
  let { fields, inputs, errors, setValue, enforceRequired = true }: { fields: ElicitationField[]; inputs: Record<string, unknown>; errors: Record<string, string>; setValue: (name: string, value: unknown) => void; enforceRequired?: boolean } = $props();
  function value(name: string, fallback: unknown): unknown { return Object.hasOwn(inputs, name) ? inputs[name] : fallback; }
</script>
<div class="fields">
      {#each fields as field, index (index)}
        {#if field.kind === "object"}
          <fieldset><legend>{field.title}{field.required ? " *" : ""}</legend>
            {#if field.description}<p class="help">{field.description}</p>{/if}
            <ElicitationFields enforceRequired={enforceRequired && field.required} fields={field.fields} inputs={(value(field.name, field.default) ?? {}) as Record<string, unknown>} errors={Object.fromEntries(Object.entries(errors).filter(([name]) => name.startsWith(`${field.name}.`)).map(([name, error]) => [name.slice(field.name.length + 1), error]))} setValue={(key, next) => setValue(field.name, {...((value(field.name, field.default) ?? {}) as Record<string, unknown>), [key]: next})} />
            {#if errors[field.name]}<p class="problem">{errors[field.name]}</p>{/if}
          </fieldset>
        {:else}
        <label class="field">
          <span class="field-title">{field.title}{field.required ? " *" : ""}</span>
          {#if field.description}<span class="help">{field.description}</span>{/if}
          {#if field.kind === "boolean"}
            <select value={value(field.name, field.default) === true ? "yes" : value(field.name, field.default) === false ? "no" : ""} required={enforceRequired && field.required} onchange={(e) => setValue(field.name, e.currentTarget.value === "" ? null : e.currentTarget.value === "yes")}>
              <option value="">Choose…</option><option value="yes">Yes</option><option value="no">No</option>
            </select>
          {:else if field.kind === "array"}
            <select multiple value={(value(field.name, field.default) ?? []) as string[]} onchange={(e) => setValue(field.name, Array.from(e.currentTarget.selectedOptions, (o) => o.value))}>
              {#each field.options as option, optionIndex (optionIndex)}<option value={option.value}>{option.label}</option>{/each}
            </select>
            <span class="help">Select all that apply.{field.min_items !== null ? ` Minimum ${field.min_items}.` : ""}{field.max_items !== null ? ` Maximum ${field.max_items}.` : ""}</span>
          {:else if field.options.length}
            <select value={String(value(field.name, field.default) ?? "")} required={enforceRequired && field.required} onchange={(e) => setValue(field.name, e.currentTarget.value)}>
              <option value="">Choose…</option>
              {#each field.options as option, optionIndex (optionIndex)}<option value={option.value}>{option.label}</option>{/each}
            </select>
          {:else if field.kind === "number" || field.kind === "integer"}
            <input type="number" value={value(field.name, field.default) as number ?? undefined} min={field.minimum ?? undefined} max={field.maximum ?? undefined} step={field.kind === "integer" ? 1 : "any"} required={enforceRequired && field.required} oninput={(e) => setValue(field.name, e.currentTarget.value)} />
          {:else}
            <input type={field.format === "email" ? "email" : field.format === "date" ? "date" : "text"} value={String(value(field.name, field.default) ?? "")} required={enforceRequired && field.required} placeholder={field.format === "date-time" ? "2026-10-03T12:00:00Z" : field.format === "uri" ? "https://…" : undefined} oninput={(e) => setValue(field.name, e.currentTarget.value)} />
          {/if}
          {#if errors[field.name]}<span class="problem">{errors[field.name]}</span>{/if}
        </label>
        {/if}
      {/each}
</div>
<style>
 .fields { display: grid; gap: 14px; }
 .field { display: flex; flex-direction: column; gap: 5px; min-width: 0; }
 .field-title, legend { font-weight: 500; font-size: var(--text-sm); }
 .help { color: var(--muted); font-size: var(--text-xs); line-height: 1.5; overflow-wrap: anywhere; }
 .problem { color: var(--err); font-size: var(--text-sm); }
 fieldset { border: 1px solid var(--edge); border-radius: 6px; padding: 12px; min-width: 0; }
 input, select { font: inherit; color: var(--fg); background: var(--bg); border: 1px solid var(--edge); border-radius: 5px; padding: 8px; width: 100%; box-sizing: border-box; }
 select[multiple] { min-height: 90px; }
</style>
