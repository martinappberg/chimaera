<script lang="ts">
  /**
   * A wake request's Wake (or Continue) with its "from now on" chevron, as
   * one control — shared by the idle agent's wake bar (chat/WakeRequestBar)
   * and its Needs-you card (dashboard/WakeRequestCard). The chevron's menu
   * lists `loosenChoices`: Within limits or Always under Ask me, Always past
   * a limit. A pick answers the request the menu was opened for, even if the
   * owner has moved on to the next one meanwhile.
   */
  import Chevron from "../shared/Chevron.svelte";
  import { contextMenu } from "../shared/contextMenu.svelte";
  import { loosenChoices, wakeAsk, type WakeRequest } from "./comms.svelte";

  interface Props {
    request: WakeRequest;
    /** The button's words (the owner's busy wording included). */
    label: string;
    title: string;
    disabled: boolean;
    /** The card's pill shape; the bar's is the `.opt` box. */
    pill?: boolean;
    /** Hang the menu from the chevron's right edge (the bar sits at the right). */
    alignRight?: boolean;
    onWake: () => void;
    onPick: (wid: string, policy: "auto" | "always") => void;
  }

  let { request, label, title, disabled, pill = false, alignRight = false, onWake, onPick }: Props = $props();

  const cont = $derived(wakeAsk(request.reason).cont);

  function openChoices(e: MouseEvent): void {
    const wid = request.id;
    const r = (e.currentTarget as Element).getBoundingClientRect();
    contextMenu.openAtPoint(
      alignRight ? r.right : r.left,
      r.bottom,
      loosenChoices(request.reason).map((c) => ({
        label: c.label,
        detail: c.detail,
        onSelect: () => onPick(wid, c.policy),
      })),
      { alignRight },
    );
  }
</script>

<span class="split" class:pill>
  <button class="opt primary" {disabled} {title} onclick={onWake}>{label}</button><button
    class="opt primary more"
    {disabled}
    aria-haspopup="menu"
    aria-label={cont ? "Continue from now on" : "Wake from now on"}
    title={cont ? "continue, and stop asking" : "wake, and stop asking every time"}
    onclick={openChoices}><Chevron open size={9} /></button
  >
</span>

<style>
  /* The `.opt` recipe (app.css), joined into one control. */
  .split {
    display: inline-flex;
  }
  .split > .opt:first-child {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }
  .more {
    display: inline-flex;
    align-items: center;
    padding: 3px 5px;
    border-left-width: 0;
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
  }
  .more :global(.chev) {
    color: inherit;
  }
  /* The Needs-you card's pill buttons. */
  .pill > .opt {
    font-size: var(--text-xs);
    font-weight: 500;
    padding: 2px 10px;
  }
  /* Dimmed like the card's other buttons while busy. */
  .pill > .opt:disabled {
    opacity: 0.55;
  }
  .pill > .opt:first-child {
    border-radius: 999px 0 0 999px;
  }
  .pill > .more {
    padding: 2px 7px 2px 5px;
    border-radius: 0 999px 999px 0;
  }
</style>
