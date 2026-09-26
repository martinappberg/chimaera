<script lang="ts">
  /**
   * The markdown toolbar's "N issues" chip: the daemon's portable-dialect
   * check (`GET /fs/check_document`, the same checker agents call as the MCP
   * `check_document` tool) — broken links and embeds, missing anchors,
   * dangling footnotes, absolute paths, missing alt text, syntax GitHub shows
   * as literal text.
   *
   * Quiet by design: hidden until something is an error or a warning (notes
   * only show inside the popover), no spinner, and a failed check just keeps
   * the chip hidden. It re-checks when the file's on-disk version (`mtime`,
   * read from the shared file store) changes, debounced, and only while it
   * can be seen: a change that lands while the window or this pane's tab is
   * hidden is checked when it shows again.
   */
  import { api } from "../net/api";
  import { dismiss } from "../shared/dismiss";
  import { requestReveal } from "../shared/reveal";
  import { pageVisible } from "../shared/visibility";

  interface Props {
    path: string;
    /** Workspace root: where root-relative `/docs/x.md` links resolve. */
    wsRoot?: string | null;
    /** The file store's on-disk version token; a change means re-check. */
    mtime?: string | null;
  }

  let { path, wsRoot = null, mtime = null }: Props = $props();

  interface DocIssue {
    line: number;
    severity: "error" | "warning" | "info";
    code: string;
    message: string;
    fix: string;
  }

  /** First check after mount: let the document render first. */
  const FIRST_DELAY_MS = 400;
  /** Coalesce a burst of saves (an agent writing in steps). */
  const DEBOUNCE_MS = 800;
  const TIMEOUT_MS = 15_000;
  /** Opening the popover re-checks when the answer is older than this. */
  const FRESH_MS = 5_000;

  let host = $state<HTMLElement | null>(null);
  let issues = $state<DocIssue[]>([]);
  let issuesPath = $state<string | null>(null);
  let truncated = $state(false);
  let open = $state(false);
  // Bookkeeping, not rendered.
  let checkedKey: string | null = null;
  let checkedAt = 0;

  const current = $derived(issuesPath === path ? issues : []);
  const flagged = $derived(current.filter((i) => i.severity !== "info"));
  const notes = $derived(current.filter((i) => i.severity === "info"));
  const errorCount = $derived(flagged.filter((i) => i.severity === "error").length);
  const warningCount = $derived(flagged.length - errorCount);
  const summary = $derived(
    [
      errorCount > 0 ? `${errorCount} ${errorCount === 1 ? "error" : "errors"}` : null,
      warningCount > 0 ? `${warningCount} ${warningCount === 1 ? "warning" : "warnings"}` : null,
    ]
      .filter((s) => s !== null)
      .join(" · "),
  );

  $effect(() => {
    if (flagged.length === 0) open = false;
  });

  /** On screen: the window is visible and this view's pane layer is active. */
  function shown(el: HTMLElement): boolean {
    return (
      document.visibilityState === "visible" &&
      el.closest("[inert]") === null &&
      el.getClientRects().length > 0
    );
  }

  async function check(key: string, signal: AbortSignal): Promise<void> {
    const forPath = path;
    const q = new URLSearchParams({ path: forPath });
    if (wsRoot !== null && wsRoot !== "") q.set("root", wsRoot);
    const ctl = new AbortController();
    const onAbort = (): void => ctl.abort();
    signal.addEventListener("abort", onAbort);
    const timer = window.setTimeout(onAbort, TIMEOUT_MS);
    try {
      const res = await api(`/fs/check_document?${q.toString()}`, { signal: ctl.signal });
      const body = res.ok ? ((await res.json()) as { issues?: unknown; truncated?: unknown }) : null;
      if (signal.aborted || forPath !== path) return;
      // A refused check (too large, unreadable) is not the user's problem
      // to see here: no chip.
      issues = Array.isArray(body?.issues) ? (body.issues as DocIssue[]) : [];
      truncated = body?.truncated === true;
      issuesPath = forPath;
      checkedKey = key;
      checkedAt = Date.now();
    } catch {
      // Aborted, timed out or offline: the next change (or showing) retries.
    } finally {
      window.clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
    }
  }

  $effect(() => {
    const el = host;
    const key = `${path}\n${mtime ?? ""}\n${wsRoot ?? ""}`;
    if (el === null || !$pageVisible || key === checkedKey) return;
    const ctl = new AbortController();
    let timer = 0;
    let observer: MutationObserver | null = null;
    const run = (): void => {
      if (shown(el)) {
        void check(key, ctl.signal);
        return;
      }
      // Parked in a hidden tab: wait for its layer to become active. (A
      // hidden window re-runs this effect through $pageVisible instead.)
      const parked = el.closest("[inert]");
      if (parked === null || observer !== null) return;
      observer = new MutationObserver(() => {
        if (!shown(el)) return;
        observer?.disconnect();
        observer = null;
        schedule();
      });
      observer.observe(parked, { attributes: true, attributeFilter: ["inert"] });
    };
    const schedule = (): void => {
      window.clearTimeout(timer);
      timer = window.setTimeout(run, checkedKey === null ? FIRST_DELAY_MS : DEBOUNCE_MS);
    };
    schedule();
    return () => {
      window.clearTimeout(timer);
      observer?.disconnect();
      ctl.abort();
    };
  });

  function toggle(): void {
    open = !open;
    if (open && checkedKey !== null && Date.now() - checkedAt > FRESH_MS) {
      void check(checkedKey, new AbortController().signal);
    }
  }

  function reveal(issue: DocIssue): void {
    open = false;
    requestReveal(path, { line: issue.line });
  }

  /** Backtick spans as code, the rest as text (never markup). */
  function segments(text: string): { code: boolean; text: string }[] {
    return text
      .split("`")
      .map((part, i) => ({ code: i % 2 === 1, text: part }))
      .filter((s) => s.text !== "");
  }
</script>

<span
  class="doc-issues"
  bind:this={host}
  use:dismiss={{ enabled: open, onDismiss: () => (open = false) }}
>
  {#if flagged.length > 0}
    <button
      class="chip"
      class:err={errorCount > 0}
      aria-haspopup="dialog"
      aria-expanded={open}
      title="Portable-markdown check: {summary}"
      onclick={toggle}
    >
      <span class="dot" aria-hidden="true"></span>
      {flagged.length}
      {flagged.length === 1 ? "issue" : "issues"}
    </button>
    {#if open}
      <div class="pop" role="dialog" aria-label="document issues">
        <div class="pop-head">
          <span class="pop-title">{summary}</span>
          <span class="pop-sub">portable markdown check</span>
        </div>
        <ul class="list">
          {#each flagged as issue, i (i)}
            {@render row(issue)}
          {/each}
          {#if notes.length > 0}
            <li class="divider" aria-hidden="true">notes</li>
            {#each notes as issue, i (i)}
              {@render row(issue)}
            {/each}
          {/if}
        </ul>
        {#if truncated}
          <div class="more">Only the first 500 are listed.</div>
        {/if}
      </div>
    {/if}
  {/if}
</span>

{#snippet row(issue: DocIssue)}
  <li>
    <button class="row" title="Go to line {issue.line}" onclick={() => reveal(issue)}>
      <span class="ln">{issue.line}</span>
      <span class="sev sev-{issue.severity}" aria-label={issue.severity}></span>
      <span class="txt">
        <span class="msg"
          >{#each segments(issue.message) as s, j (j)}{#if s.code}<code>{s.text}</code
              >{:else}{s.text}{/if}{/each}</span
        >
        <span class="fix"
          >{#each segments(issue.fix) as s, j (j)}{#if s.code}<code>{s.text}</code
              >{:else}{s.text}{/if}{/each}</span
        >
      </span>
    </button>
  </li>
{/snippet}

<style>
  .doc-issues {
    position: relative;
    display: inline-flex;
    align-items: center;
    margin-left: auto;
  }

  .chip {
    --tint: var(--warn);
    appearance: none;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font: inherit;
    font-size: var(--text-xs);
    letter-spacing: 0.02em;
    line-height: 1;
    color: color-mix(in srgb, var(--tint) 70%, var(--fg));
    background: color-mix(in srgb, var(--tint) 9%, transparent);
    border: 1px solid color-mix(in srgb, var(--tint) 35%, var(--edge));
    border-radius: 999px;
    padding: 3px 8px 3px 7px;
    cursor: pointer;
    transition:
      background-color 0.12s ease,
      border-color 0.12s ease;
  }

  .chip.err {
    --tint: var(--err);
  }

  .chip:hover,
  .chip[aria-expanded="true"] {
    background: color-mix(in srgb, var(--tint) 16%, transparent);
    border-color: color-mix(in srgb, var(--tint) 55%, var(--edge));
  }

  .chip:focus-visible {
    outline: 2px solid var(--focus-ring);
    outline-offset: 1px;
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--tint);
  }

  .pop {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 30;
    width: min(440px, calc(100vw - 32px));
    max-height: min(60vh, 420px);
    display: flex;
    flex-direction: column;
    background: var(--bg);
    border: 1px solid var(--edge);
    border-radius: 8px;
    box-shadow: 0 10px 32px rgba(0, 0, 0, 0.28);
    overflow: hidden;
  }

  .pop-head {
    flex: none;
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 8px 12px 7px;
    border-bottom: 1px solid var(--edge);
  }

  .pop-title {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }

  .pop-sub {
    margin-left: auto;
    font-size: var(--text-xs);
    color: var(--muted);
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
    min-height: 0;
  }

  .divider {
    padding: 8px 8px 3px;
    font-size: var(--text-xs);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }

  .row {
    appearance: none;
    width: 100%;
    display: grid;
    grid-template-columns: 2.6em 8px 1fr;
    align-items: baseline;
    font-size: var(--text-sm);
    gap: 8px;
    padding: 6px 8px;
    border: none;
    border-radius: 5px;
    background: none;
    font: inherit;
    text-align: left;
    color: var(--fg);
    cursor: pointer;
  }

  .row:hover,
  .row:focus-visible {
    background: var(--row-hover);
    outline: none;
  }

  .ln {
    font-family: var(--mono);
    font-size: var(--text-xs);
    color: var(--muted);
    text-align: right;
  }

  /* On the message's first line, whatever the row's height. */
  .sev {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    align-self: start;
    margin-top: calc(0.7em - 3px);
    background: var(--muted);
  }

  .sev-error {
    background: var(--err);
  }

  .sev-warning {
    background: var(--warn);
  }

  .txt {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .msg {
    font-size: var(--text-sm);
    line-height: 1.4;
    overflow-wrap: anywhere;
  }

  .fix {
    font-size: var(--text-xs);
    line-height: 1.4;
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  code {
    font-family: var(--mono);
    font-size: 0.92em;
    padding: 0 3px;
    border-radius: 3px;
    background: var(--row-active);
  }

  .more {
    flex: none;
    padding: 6px 12px;
    border-top: 1px solid var(--edge);
    font-size: var(--text-xs);
    color: var(--muted);
  }
</style>
