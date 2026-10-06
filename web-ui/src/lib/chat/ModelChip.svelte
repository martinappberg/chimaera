<script lang="ts">
  /**
   * The model serving a subagent, as one small chip: the picker's label for
   * it when the catalog knows the id, else the id verbatim (the agent's own
   * vocabulary — never relabelled), with the raw id in the tooltip. One
   * component for the agents tray, the background tray, the agent tool card
   * and the subagent banner, so the chip cannot drift between them.
   */
  interface Props {
    /** The model id, in the agent's own words. */
    model: string;
    /** A model id as the picker labels it. */
    modelName?: (id: string) => string;
    /** The pill treatment (card, banner) over the bare muted text (trays). */
    pill?: boolean;
  }
  let { model, modelName, pill = false }: Props = $props();
</script>

<span class="model" class:pill title={`model: ${model}`}>{modelName?.(model) ?? model}</span>

<style>
  .model {
    flex: none;
    max-width: 24ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-family: var(--mono, monospace);
    font-size: var(--text-xs);
  }
  .pill {
    padding: 0 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg) 6%, transparent);
  }
</style>
