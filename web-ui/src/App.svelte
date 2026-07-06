<script lang="ts">
  import { ApiError, pollHealth, type Health } from "./lib/api";

  type Status = "connecting" | "connected" | "unauthorized" | "unreachable";

  let status = $state<Status>("connecting");
  let info = $state<Health | null>(null);

  $effect(() =>
    pollHealth(
      (h) => {
        info = h;
        status = "connected";
      },
      (e) => {
        status = e instanceof ApiError && e.status === 401 ? "unauthorized" : "unreachable";
      },
    ),
  );

  function formatUptime(secs: number): string {
    const d = Math.floor(secs / 86400);
    const h = Math.floor((secs % 86400) / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = Math.floor(secs % 60);
    if (d > 0) return `${d}d ${h}h`;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
  }

  const dotClass = $derived(
    status === "connected" ? "ok" : status === "connecting" ? "wait" : "err",
  );
</script>

<main>
  <header>
    <h1>chimaera</h1>
    <span class="dot {dotClass}" role="status" aria-label={status}></span>
  </header>

  <dl class="rows">
    <div class="row">
      <dt>host</dt>
      <dd>{info ? info.hostname : "—"}</dd>
    </div>
    <div class="row">
      <dt>version</dt>
      <dd>{info ? info.version : "—"}</dd>
    </div>
    <div class="row">
      <dt>uptime</dt>
      <dd>{info ? formatUptime(info.uptime_secs) : "—"}</dd>
    </div>
  </dl>

  {#if status === "unauthorized"}
    <p class="hint">open the URL printed by the daemon (it carries the access token)</p>
  {/if}

  <p class="footnote">M0 walking skeleton — sessions land in M1.</p>
</main>
