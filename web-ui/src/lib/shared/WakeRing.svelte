<script lang="ts">
  /**
   * A session's next self-started run as a still ring (the rail row, the
   * dashboard card): it drains toward the wakeup when its start is known,
   * and is a quiet full ring for an interval job. The time and what it will
   * do are in the tooltip.
   */
  import CountdownRing from "./CountdownRing.svelte";
  import { liveClock } from "./liveClock.svelte";
  import { ringFraction, untilLabel } from "../chat/ahead";

  interface Props {
    at: number | null;
    from?: number | null;
    label?: string;
    size?: number;
  }
  let { at, from = null, label = "", size = 10 }: Props = $props();

  const clock = liveClock(() => at !== null);
  const now = $derived(at !== null ? clock.ms : 0);
  const fraction = $derived(at !== null && from !== null ? ringFraction(at, from, now) : 1);
  const title = $derived(
    `${at !== null ? `next ${untilLabel(at, now)}` : "scheduled"}${label !== "" ? ` · ${label}` : ""}`,
  );
</script>

<span class="wake-ring" {title}>
  <CountdownRing {fraction} soon={at !== null && at - now < 60_000} {size} />
</span>

<style>
  .wake-ring {
    flex: none;
    display: inline-flex;
    align-items: center;
  }
</style>
