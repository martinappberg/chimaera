<script lang="ts">
  /**
   * The one-line question an interrupting action asks when it would also end
   * subagents or workflows ("Also stops 2 subagents · Click again to stop"),
   * shown above the control that asked. An overlay (the `.overlay-surface`
   * recipe, like the slash popover): it never moves anything. The host gives
   * a `position: relative` box whose right edge lines up with the control,
   * and may set `--stop-note-right` / `--stop-note-left` / `--stop-note-max`
   * (default 0 / auto / none).
   * Clicking it is the second press. ChatView owns the state and the polite
   * announcement; `control` is the asking control's `data-stop-control` key,
   * so a press here is never read as a press outside.
   */
  interface Props {
    text: string;
    how: string;
    control: string;
    onConfirm: () => void;
  }
  let { text, how, control, onConfirm }: Props = $props();
</script>

<button
  type="button"
  class="overlay-surface stop-note"
  data-stop-control={control}
  tabindex="-1"
  onmousedown={(e) => e.preventDefault()}
  onclick={onConfirm}
>
  <span class="mark" aria-hidden="true"></span>
  <span class="what">{text}</span>
  <span class="how">{how}</span>
</button>

<style>
  .stop-note {
    bottom: 100%;
    right: var(--stop-note-right, 0);
    left: var(--stop-note-left, auto);
    max-width: var(--stop-note-max, none);
    margin-bottom: 6px;
    z-index: 11;
    box-sizing: border-box;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 5px 10px;
    font: inherit;
    font-size: var(--text-sm);
    font-weight: normal;
    line-height: 1.35;
    color: var(--fg);
    text-align: left;
    white-space: nowrap;
    cursor: pointer;
    animation: rise 0.14s ease-out;
  }
  .stop-note:hover {
    background: color-mix(in srgb, var(--fg) 5%, var(--overlay-bg));
  }
  .mark {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 1.5px;
    background: var(--warn);
  }
  .what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .how {
    flex: none;
    color: var(--muted);
  }
  .how::before {
    content: "·";
    margin-right: 7px;
  }
  @media (prefers-reduced-motion: reduce) {
    .stop-note {
      animation: none;
    }
  }
</style>
