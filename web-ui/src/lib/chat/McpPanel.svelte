<script lang="ts">
  /**
   * The /mcp panel (claude only): MCP server inventory with per-server status,
   * tool counts, and reconnect / enable-disable actions. A floating overlay
   * anchored under the header; the `.menu-host` class marks it "inside" for the
   * outside-dismiss action.
   */
  import type { McpServer } from "./store.svelte";

  interface Props {
    servers: McpServer[] | null;
    onReconnect: (name: string) => void;
    onToggleEnabled: (name: string, enabled: boolean) => void;
  }

  let { servers, onReconnect, onToggleEnabled }: Props = $props();
</script>

<div class="menu-host mcp-host">
  <div class="overlay-surface mcp-panel" role="dialog" aria-label="MCP servers">
    <div class="mcp-title">MCP servers</div>
    {#if servers === null}
      <span class="menu-empty">loading…</span>
    {:else if servers.length === 0}
      <span class="menu-empty">no MCP servers configured</span>
    {:else}
      {#each servers as s (s.name)}
        <div class="mcp-row">
          <span
            class="mcp-glyph"
            class:ok={s.status === "connected"}
            class:bad={s.status === "failed"}
            class:warn={s.status === "needs-auth"}
          >
            {s.status === "connected"
              ? "✓"
              : s.status === "failed"
                ? "✗"
                : s.status === "needs-auth"
                  ? "⚠"
                  : s.status === "pending"
                    ? "◐"
                    : "○"}
          </span>
          <span class="mcp-name" title={s.error ?? s.status}>{s.name}</span>
          {#if s.tools > 0}
            <span class="mcp-tools">{s.tools} tools</span>
          {/if}
          {#if s.status === "failed" || s.status === "needs-auth"}
            <button class="mcp-act" onclick={() => onReconnect(s.name)}>reconnect</button>
          {/if}
          <button
            class="mcp-act"
            onclick={() => onToggleEnabled(s.name, s.status === "disabled")}
          >
            {s.status === "disabled" ? "enable" : "disable"}
          </button>
        </div>
        {#if s.error !== null}
          <div class="mcp-error">{s.error}</div>
        {/if}
      {/each}
    {/if}
  </div>
</div>

<style>
  /* .overlay-surface (the floating-surface recipe) is in app.css. */
  .mcp-host {
    position: absolute;
    top: 28px;
    left: 10px;
    z-index: 25;
  }
  .mcp-panel {
    min-width: 300px;
    max-width: 420px;
  }
  .mcp-title {
    padding: 4px 12px 6px;
    color: var(--muted);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .menu-empty {
    display: block;
    padding: 6px 12px;
    color: var(--muted);
    font-size: var(--text-sm);
  }
  .mcp-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 12px;
    font-size: var(--text-sm);
  }
  .mcp-glyph {
    flex: none;
    width: 14px;
    color: var(--muted);
  }
  .mcp-glyph.ok {
    color: var(--accent);
  }
  .mcp-glyph.bad {
    color: var(--err);
  }
  .mcp-glyph.warn {
    color: var(--warn);
  }
  .mcp-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono, monospace);
  }
  .mcp-tools {
    color: var(--muted);
    font-size: var(--text-xs);
    flex: none;
  }
  .mcp-act {
    background: none;
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-xs);
    padding: 0 6px;
    cursor: pointer;
    flex: none;
    transition:
      color 0.12s ease,
      border-color 0.12s ease;
  }
  .mcp-act:hover {
    color: var(--fg);
    border-color: color-mix(in srgb, var(--accent) 40%, var(--edge));
  }
  .mcp-error {
    padding: 0 12px 4px 34px;
    color: var(--err);
    font-size: var(--text-xs);
    word-break: break-word;
  }
</style>
