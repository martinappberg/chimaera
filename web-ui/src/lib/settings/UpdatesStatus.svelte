<script lang="ts">
  /**
   * The Updates section's status block — the one place that answers "is
   * there an update?" for everything that can go stale: the app (native),
   * the daemon serving this window, and the agent CLIs. Each line says
   * plainly whether a newer release exists, when that was last checked,
   * and — the part that used to be invisible — why a check failed if it
   * did. "check now" asks every source at once. The switch below it is the
   * ordinary `update.autoCheck` schema row.
   */
  import { onMount } from "svelte";
  import { beginUpdate, connectHost, isNativeShell, updateLocalDaemon } from "../net/native";
  import { pageVisible } from "../shared/visibility";
  import { openInSystemBrowser } from "../shared/urlOpen";
  import { listAgents, relativeAge, type AgentInfo } from "../workspace/launcher";
  import { checkForUpdates, updateState } from "../workspace/update.svelte";
  import { getSetting } from "./store.svelte";

  let { onJump }: { onJump?: (section: string) => void } = $props();

  const native = isNativeShell();

  /** How a line reads at a glance: new = an update exists, warn = the check
   *  failed, ok = checked and current, idle = nothing known yet. */
  type Tone = "new" | "warn" | "ok" | "idle";

  interface Line {
    key: string;
    name: string;
    version: string | null;
    verdict: string;
    tone: Tone;
    sub: string;
    action?: { label: string; run: () => Promise<unknown> };
    link?: { label: string; url?: string; section?: string };
  }

  let agents = $state<AgentInfo[]>([]);
  let checking = $state(false);
  let busy = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let now = $state(Date.now());

  // "checked 2h ago" stays true while someone is looking; a hidden window
  // doesn't tick, and this effect's re-run on return is the catch-up.
  $effect(() => {
    if (!$pageVisible) return;
    now = Date.now();
    const timer = setInterval(() => (now = Date.now()), 30_000);
    return () => clearInterval(timer);
  });

  onMount(() => {
    void listAgents().then(
      (list) => (agents = list),
      () => {},
    );
  });

  function ago(secs: number | null): string | null {
    if (secs === null) return null;
    const age = relativeAge(secs, now);
    return age === "now" ? "just now" : `${age} ago`;
  }

  function every(secs: number): string {
    const h = Math.round(secs / 3600);
    return h === 1 ? "every hour" : `every ${h} hours`;
  }

  const autoCheck = $derived(getSetting("update.autoCheck"));

  const appLine = $derived.by((): Line | null => {
    if (!native) return null;
    const a = updateState.app;
    const base = { key: "app", name: "chimaera app" };
    if (a === null) {
      return { ...base, version: null, verdict: "asking…", tone: "idle", sub: "waiting for the app's answer" };
    }
    const version = a.dev ? "dev" : a.current;
    const checked = ago(a.checked_at);
    if (a.dev) {
      return {
        ...base,
        version,
        verdict: "development build",
        tone: "idle",
        sub: "release updates don't apply to a dev build",
      };
    }
    if (a.available !== null) {
      const notes = updateState.daemon?.latest;
      return {
        ...base,
        version,
        verdict: `${a.available} available`,
        tone: "new",
        sub: "updates the app, then its local daemon — windows, tabs and sessions come back",
        action: { label: "update now", run: beginUpdate },
        link: notes?.version === a.available ? { label: "release notes", url: notes.url } : undefined,
      };
    }
    if (a.error !== null) {
      return {
        ...base,
        version,
        verdict: "couldn't check",
        tone: "warn",
        sub: checked === null ? a.error : `${a.error} · tried ${checked}`,
      };
    }
    if (checked === null) {
      return {
        ...base,
        version,
        verdict: "not checked yet",
        tone: "idle",
        sub: `the app checks shortly after launch, then ${every(a.interval_secs)}`,
      };
    }
    return {
      ...base,
      version,
      verdict: "up to date",
      tone: "ok",
      sub: `checked ${checked} · checks ${every(a.interval_secs)}`,
    };
  });

  const daemonLine = $derived.by((): Line => {
    const scope = updateState.scope;
    const where = scope === null ? null : scope.split("#")[0];
    // In a browser the daemon IS chimaera; in the app it sits beside the app line.
    const name = where !== null ? `daemon on ${where}` : native ? "local daemon" : "chimaera";
    const base = { key: "daemon", name };
    const d = updateState.daemon;
    if (d === null) {
      return { ...base, version: null, verdict: "asking…", tone: "idle", sub: "waiting for the daemon's answer" };
    }
    const version = d.dev ? `dev·${(d.build ?? "unknown").split(".")[0]}` : d.current;
    if (native && updateState.buildSkew) {
      return {
        ...base,
        version,
        verdict: "older build than this app",
        tone: "new",
        sub: "restart it into the app's build — layouts and sessions come back; running terminal programs restart",
        action:
          scope === null
            ? { label: "restart daemon", run: updateLocalDaemon }
            : { label: `update ${where}`, run: () => connectHost(scope, true) },
      };
    }
    if (d.dev) {
      return {
        ...base,
        version,
        verdict: "development build",
        tone: "idle",
        sub: "release checks don't apply to a dev build",
      };
    }
    const checked = ago(d.checked_at);
    const cadence = autoCheck ? `checks ${every(d.interval_secs)}` : "automatic checks are off";
    if (d.state === "available" && d.latest !== null) {
      return {
        ...base,
        version,
        verdict: `${d.latest.version} available`,
        tone: "new",
        sub:
          (native
            ? "arrives with the app update"
            : "update from the chimaera app, or rerun chimaera connect from your machine") +
          // The release stays known when a later re-check fails; say so.
          (d.error !== null && checked !== null ? ` · the last re-check failed ${checked}` : ""),
        link: { label: "release notes", url: d.latest.url },
      };
    }
    if (d.state === "failed") {
      const good = ago(d.succeeded_at);
      const parts = [d.error ?? "the check failed"];
      if (checked !== null) parts.push(`tried ${checked}`);
      if (good !== null) parts.push(`last good check ${good}`);
      return { ...base, version, verdict: "couldn't check", tone: "warn", sub: parts.join(" · ") };
    }
    if (d.state === "unchecked") {
      return {
        ...base,
        version,
        verdict: "not checked yet",
        tone: "idle",
        sub: autoCheck
          ? `the first check runs a minute after the daemon starts, then ${every(d.interval_secs)}`
          : "automatic checks are off — check now to ask",
      };
    }
    return {
      ...base,
      version,
      verdict: "up to date",
      tone: "ok",
      sub: checked === null ? cadence : `checked ${checked} · ${cadence}`,
    };
  });

  const versionNumber = (v: string): string =>
    v.split(" ").find((t) => /^\d/.test(t)) ?? v.split(" ")[0];

  /**
   * The agent CLIs as one line: their details live in the Agents section.
   * "Up to date" only when every installed agent's version AND its latest
   * release are known — an unparseable version is never read as current
   * (the agent-updates core bet: honest or absent).
   */
  const agentsLine = $derived.by((): Line | null => {
    const installed = agents.filter((a) => a.installed);
    if (installed.length === 0) return null;
    const newer = installed.filter((a) => a.updateAvailable && a.latestVersion !== null);
    const known = installed.filter((a) => a.version !== null && a.latestVersion !== null);
    const sub = installed
      .map((a) => {
        if (a.version === null) return `${a.name} (version unknown)`;
        const ver = versionNumber(a.version);
        if (a.updateAvailable && a.latestVersion !== null) return `${a.name} ${ver} → ${a.latestVersion}`;
        return a.latestVersion === null ? `${a.name} ${ver} (latest unknown)` : `${a.name} ${ver}`;
      })
      .join(" · ");
    const checks = installed.map((a) => a.latestCheckedAt).filter((t): t is number => t !== null);
    const checked = checks.length > 0 ? ago(Math.min(...checks)) : null;
    const base = {
      key: "agents",
      name: "agents",
      version: null,
      link: { label: "Agents", section: "Agents" },
    };
    const withChecked = checked === null ? sub : `${sub} · checked ${checked}`;
    if (newer.length > 0) {
      return {
        ...base,
        verdict: newer.length === 1 ? "1 update available" : `${newer.length} updates available`,
        tone: "new",
        sub: withChecked,
      };
    }
    if (known.length === installed.length) {
      return { ...base, verdict: "up to date", tone: "ok", sub: withChecked };
    }
    return {
      ...base,
      verdict: known.length === 0 ? "latest not known yet" : "no updates found",
      tone: "idle",
      sub: withChecked,
    };
  });

  const lines = $derived(
    [appLine, daemonLine, agentsLine].filter((l): l is Line => l !== null),
  );

  async function checkNow(): Promise<void> {
    if (checking) return;
    checking = true;
    actionError = null;
    try {
      const [, list] = await Promise.all([
        checkForUpdates(false),
        listAgents(false, true).catch(() => null),
      ]);
      if (list !== null) agents = list;
    } finally {
      checking = false;
    }
  }

  async function act(line: Line): Promise<void> {
    if (line.action === undefined || busy !== null) return;
    busy = line.key;
    actionError = null;
    try {
      await line.action.run();
    } catch (e) {
      actionError = e instanceof Error ? e.message : String(e);
    } finally {
      busy = null;
    }
  }

  function follow(link: NonNullable<Line["link"]>): void {
    if (link.section !== undefined) onJump?.(link.section);
    else if (link.url !== undefined) openInSystemBrowser(link.url);
  }
</script>

<div class="cat-row">
  <h2 class="cat">Updates</h2>
  <button
    class="recheck"
    title="ask GitHub (and the agents' release feeds) for the newest versions now"
    onclick={() => void checkNow()}
    disabled={checking}
  >
    {checking ? "checking…" : "check now"}
  </button>
</div>

<div class="status" aria-live="polite">
  {#each lines as line (line.key)}
    <div class="line" data-tone={line.tone}>
      <span class="dot" aria-hidden="true"></span>
      <div class="text">
        <div class="head">
          <span class="name">{line.name}</span>
          {#if line.version !== null}<span class="ver">{line.version}</span>{/if}
          <span class="verdict">{line.verdict}</span>
        </div>
        <p class="sub">{line.sub}</p>
      </div>
      {#if line.action !== undefined || line.link !== undefined}
        <div class="actions">
          {#if line.link !== undefined}
            {@const link = line.link}
            <button class="link" onclick={() => follow(link)}>
              {link.section !== undefined ? `${link.label} ↓` : `${link.label} ↗`}
            </button>
          {/if}
          {#if line.action !== undefined}
            <button class="btn primary" disabled={busy !== null} onclick={() => void act(line)}>
              {busy === line.key ? "updating…" : line.action.label}
            </button>
          {/if}
        </div>
      {/if}
    </div>
  {/each}
  {#if actionError !== null}
    <p class="err" role="alert">{actionError}</p>
  {/if}
</div>

<style>
  /* The shared settings grammar (AgentsSettings' header row, the
     NotificationStatus card): an uppercase category header with a quiet
     action, then a bordered status card. */
  .cat-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
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

  .recheck {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--muted);
    cursor: pointer;
    padding: 0 4px;
    border-radius: 4px;
    transition: color 0.12s ease;
  }

  .recheck:hover:not(:disabled) {
    color: var(--fg);
  }

  .recheck:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .status {
    display: flex;
    flex-direction: column;
    margin: 4px 18px 10px 18px;
    border: 1px solid var(--edge);
    border-radius: 8px;
    background: color-mix(in srgb, var(--fg) 2%, transparent);
    container-type: inline-size;
  }

  .line {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 11px 14px;
  }

  .line + .line {
    border-top: 1px solid color-mix(in srgb, var(--edge) 70%, transparent);
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    margin-top: 6px;
    border-radius: 50%;
    background: var(--muted);
    opacity: 0.55;
  }

  .line[data-tone="ok"] .dot {
    opacity: 1;
  }

  .line[data-tone="new"] .dot {
    background: var(--accent);
    opacity: 1;
  }

  .line[data-tone="warn"] .dot {
    background: var(--warn);
    opacity: 1;
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .head {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 4px 8px;
    min-width: 0;
  }

  .name {
    font-size: var(--text-md);
    color: var(--fg);
  }

  .ver {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .verdict {
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .line[data-tone="new"] .verdict {
    color: var(--accent);
  }

  .line[data-tone="warn"] .verdict {
    color: var(--warn);
  }

  .sub {
    margin: 0;
    font-size: var(--text-sm);
    line-height: 1.45;
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .actions {
    flex: none;
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .link {
    appearance: none;
    border: none;
    background: none;
    font: inherit;
    font-size: var(--text-xs);
    color: var(--accent);
    cursor: pointer;
    padding: 0 2px;
  }

  .link:hover {
    text-decoration: underline;
  }

  .btn {
    appearance: none;
    border: 1px solid var(--edge);
    background: var(--term-bg);
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
    padding: 3px 9px;
    border-radius: 6px;
    white-space: nowrap;
    transition:
      color 0.12s ease,
      background-color 0.12s ease;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .btn.primary {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--edge));
  }

  .btn.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .err {
    margin: 0;
    padding: 0 14px 11px 34px;
    font-size: var(--text-sm);
    color: var(--err);
    overflow-wrap: anywhere;
  }

  @container (max-width: 480px) {
    .line {
      flex-wrap: wrap;
    }

    .actions {
      width: 100%;
      padding-left: 20px;
    }
  }
</style>
