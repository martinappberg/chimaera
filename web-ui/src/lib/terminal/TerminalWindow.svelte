<script lang="ts">
  /**
   * A terminal-only window (the `term=` hash param): one daemon session and
   * none of the workbench around it — no rail, tabs, dashboard or file tree.
   * The cluster page's login-node terminal (an `ssh <cluster>` shell in the
   * app's hidden workspace) opens here; the native app ends that session
   * when this window closes. Links stay off: paths and addresses printed on
   * the login node mean nothing on this machine.
   */
  import { onMount } from "svelte";
  import * as pool from "./termPool";
  import Terminal from "./Terminal.svelte";
  import { listSessions } from "../workspace/sessions";
  import { loadSettings } from "../settings/store.svelte";
  import { closeThisWindow, isNativeShell } from "../net/native";
  import { isMac } from "../shared/keys";
  import { focusOnMount } from "../shared/focusOnMount";

  let { sessionId }: { sessionId: string } = $props();

  /** The session's name ("<cluster> · login node"), once listed. */
  let name = $state("");
  /** The session is gone: its shell exited (ssh closed) or never existed. */
  let ended = $state(false);
  /** The macOS overlay titlebar needs its own drag lane beside the lights. */
  const overlayChrome = isNativeShell() && isMac;

  onMount(() => {
    pool.initPool({
      onTitle: () => {},
      onExited: (id) => {
        if (id === sessionId) ended = true;
      },
      onSocketError: (id, message) => {
        if (id === sessionId && message === "unauthorized") ended = true;
      },
      onSelection: () => {},
      onPaste: () => {},
      linkContext: () => ({ cwd: null, root: null, workspaceId: null }),
      onOpenPath: () => {},
      onOpenUrl: () => {},
      onUrlMenu: () => {},
      plainText: true,
    });
    // Theme and terminal font from the daemon's settings, like any window.
    void loadSettings();
    listSessions()
      .then((list) => {
        const session = list.find((s) => s.id === sessionId);
        if (session === undefined || !session.alive) ended = true;
        else name = session.name;
      })
      .catch(() => {
        // An unreachable daemon: the terminal's own socket reports it.
      });
    return () => pool.disposePool();
  });

  $effect(() => {
    document.title = name !== "" ? name : "chimaera";
  });

  function close(): void {
    if (isNativeShell()) closeThisWindow();
    else window.close();
  }
</script>

<div class="term-window" class:overlay-chrome={overlayChrome}>
  {#if overlayChrome}
    <div class="lane" data-tauri-drag-region>
      <span class="title" data-tauri-drag-region>{name}</span>
    </div>
  {/if}
  <div class="stage">
    <Terminal {sessionId} focused={!ended} />
  </div>
  {#if ended}
    <div class="ended" role="status">
      <span class="copy">This session ended.</span>
      <button class="opt primary" use:focusOnMount onclick={close}>Close window</button>
    </div>
  {/if}
</div>

<style>
  .term-window {
    position: fixed;
    inset: 0;
    display: flex;
    flex-direction: column;
    background: var(--term-bg);
    color: var(--fg);
  }

  .lane {
    flex: 0 0 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    /* Clear the traffic lights on the left; keep the title centred. */
    padding: 0 82px;
    user-select: none;
  }

  .title {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    font-size: var(--text-sm);
    color: var(--muted);
  }

  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    margin: 4px 6px 6px 10px;
  }

  .overlay-chrome .stage {
    margin-top: 0;
  }

  .ended {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 12px;
    border-top: 1px solid var(--edge);
    background: var(--bg);
    font-size: var(--text-sm);
  }

  .copy {
    color: var(--muted);
  }
</style>
