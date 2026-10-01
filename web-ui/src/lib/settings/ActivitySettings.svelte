<script lang="ts">
  /**
   * Settings → Activity: what the agents did across every workspace, from
   * the session records (`GET /activity`). This week's sessions and tokens
   * as the headline with the time agents spent working, one bar chart of
   * sessions per day for the last 14 days, a table by agent and model and
   * one by workspace, and a CSV export (where the estimated cost lives — no
   * dollars on this page). A value an agent doesn't report reads "—" with a
   * tooltip saying why, never zero. Fetched when Settings shows and on the
   * history nudge — never polled.
   */
  import { untrack } from "svelte";
  import { ApiError } from "../net/api";
  import {
    aggTokens,
    downloadActivityCsv,
    fetchActivity,
    formatDuration,
    historyNudge,
    type ActivityAgg,
    type ActivityReport,
  } from "../workspace/history";

  let { visible = true }: { visible?: boolean } = $props();

  let report = $state<ActivityReport | null>(null);
  let error = $state<string | null>(null);
  let exporting = $state(false);
  let seq = 0;

  function load(): void {
    const mine = ++seq;
    void fetchActivity({ days: 14, weeks: 1 }).then(
      (r) => {
        if (mine !== seq) return;
        report = r;
        error = null;
      },
      (e: unknown) => {
        if (mine !== seq) return;
        error =
          e instanceof ApiError && e.status === 404
            ? "This daemon doesn't keep session records yet — update chimaera to see activity here."
            : e instanceof Error
              ? e.message
              : String(e);
      },
    );
  }

  $effect(() => {
    if (!visible) return;
    void $historyNudge;
    untrack(load);
  });

  async function exportCsv(): Promise<void> {
    exporting = true;
    try {
      await downloadActivityCsv();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      exporting = false;
    }
  }

  /** Why tokens read "—" (or are partial): the agents that report none. */
  function tokensWhy(a: ActivityAgg, agent?: string): string | undefined {
    const missing = a.sessions - a.token_sessions;
    if (missing <= 0) return undefined;
    if (agent === "codex") return "Codex terminals don't report tokens";
    if (agent !== undefined) return `${agent} doesn't report tokens here`;
    return `${missing} session${missing === 1 ? "" : "s"} without reported token counts`;
  }

  const dayMax = $derived(Math.max(1, ...(report?.days ?? []).map((d) => d.sessions)));

  function dayLabel(day: string): string {
    const [y, m, d] = day.split("-").map(Number);
    return new Date(y, m - 1, d).toLocaleDateString([], { month: "short", day: "numeric" });
  }
</script>

<div class="h2row">
  <h2 class="cat">Activity</h2>
</div>
<div class="activity">
  {#if error !== null}
    <p class="note err">{error}</p>
  {/if}

  {#if report !== null}
    <div class="top">
      <div class="big">
        <span class="num">{report.week.sessions}</span>
        <span class="unit">session{report.week.sessions === 1 ? "" : "s"}</span>
        <span class="num" title={tokensWhy(report.week)}>{aggTokens(report.week)}</span>
        <span class="unit">tokens</span>
        <span class="side">this week{report.week.duration_ms > 0 ? ` · agents worked ${formatDuration(report.week.duration_ms)}` : ""}</span>
      </div>
      <button class="opt quiet" disabled={exporting} onclick={() => void exportCsv()}>
        {exporting ? "exporting…" : "Export CSV"}
      </button>
    </div>

    <div class="bars" role="img" aria-label="sessions per day, last 14 days">
      {#each report.days as d (d.day)}
        <div class="bar" title="{dayLabel(d.day)}: {d.sessions} session{d.sessions === 1 ? '' : 's'} · {aggTokens(d)} tokens">
          <span class="fill" style:height="{d.sessions === 0 ? 0 : Math.max(4, (d.sessions / dayMax) * 100)}%"></span>
        </div>
      {/each}
    </div>
    <div class="axis">
      <span>{report.days.length > 0 ? dayLabel(report.days[0].day) : ""}</span>
      <span>today</span>
    </div>

    {#if report.by_agent_model.length > 0}
      <table>
        <thead><tr><th>agent</th><th>model</th><th class="n">sessions</th><th class="n">tokens</th></tr></thead>
        <tbody>
          {#each report.by_agent_model as r (`${r.agent}:${r.model ?? ""}`)}
            <tr>
              <td>{r.agent}</td>
              <td class="mono">{r.model ?? "—"}</td>
              <td class="n">{r.sessions}</td>
              <td class="n" title={tokensWhy(r, r.agent)}>{aggTokens(r)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if report.workspaces.some((w) => w.sessions > 0)}
      <table>
        <thead><tr><th>workspace</th><th class="n">sessions</th><th class="n">tokens</th></tr></thead>
        <tbody>
          {#each report.workspaces.filter((w) => w.sessions > 0) as w (w.id)}
            <tr>
              <td>{w.name}</td>
              <td class="n">{w.sessions}</td>
              <td class="n" title={tokensWhy(w)}>{aggTokens(w)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}
</div>

<style>
  .h2row {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin: 18px 0 4px;
    padding: 0 14px;
  }
  .cat {
    margin: 0;
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .activity {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 14px 12px;
    max-width: 640px;
  }
  .note {
    margin: 0;
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .err {
    color: var(--err);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .big {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    flex-wrap: wrap;
  }
  .num {
    font-size: 26px;
    font-weight: 600;
    letter-spacing: -0.01em;
    font-variant-numeric: tabular-nums;
  }
  .unit {
    font-size: var(--text-sm);
    color: var(--muted);
    margin-right: 8px;
  }
  .side {
    font-size: var(--text-sm);
    color: var(--muted);
  }
  .top .opt {
    margin-left: auto;
    flex: none;
  }
  .bars {
    display: grid;
    grid-template-columns: repeat(14, minmax(0, 1fr));
    gap: 4px;
    height: 64px;
    align-items: end;
  }
  .bar {
    height: 100%;
    display: flex;
    align-items: flex-end;
  }
  .fill {
    display: block;
    width: 100%;
    border-radius: 2px 2px 0 0;
    background: color-mix(in srgb, var(--accent) 65%, transparent);
  }
  .axis {
    display: flex;
    justify-content: space-between;
    margin-top: -6px;
    font-size: var(--text-xs);
    color: var(--muted);
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--text-sm);
  }
  th {
    text-align: left;
    font-weight: 500;
    color: var(--muted);
    font-size: var(--text-xs);
    padding: 4px 8px 4px 0;
    border-bottom: 1px solid var(--edge);
  }
  td {
    padding: 4px 8px 4px 0;
    border-bottom: 1px solid var(--edge);
  }
  .n {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .mono {
    font-family: var(--mono, monospace);
  }
</style>
