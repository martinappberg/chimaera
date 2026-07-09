<script lang="ts">
  /**
   * A delayed loading spinner for the preview surfaces. It renders NOTHING for
   * the first `delay` ms, then fades in — so a fast (local) load never flickers
   * a spinner, but a slow (remote/SSH) file read shows the user that something
   * is happening. Mount it INSIDE a parent's loading branch: its lifetime is the
   * in-flight window, so mount starts the timer and the parent unmounting it on
   * settle tears the timer down. Centered over its positioned parent.
   */
  interface Props {
    /** Grace period before the spinner appears (ms). */
    delay?: number;
    /** Optional caption under the spinner. */
    label?: string | null;
  }

  let { delay = 250, label = null }: Props = $props();

  let visible = $state(false);
  $effect(() => {
    const t = setTimeout(() => (visible = true), delay);
    return () => clearTimeout(t);
  });
</script>

{#if visible}
  <div class="spin-wrap" role="status" aria-live="polite">
    <span class="spinner" aria-hidden="true"></span>
    {#if label !== null}<span class="spin-label">{label}</span>{/if}
    <span class="sr-only">loading…</span>
  </div>
{/if}

<style>
  .spin-wrap {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: var(--muted);
    animation: spin-fade 0.15s ease;
  }

  .spinner {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 2px solid color-mix(in srgb, var(--accent) 30%, transparent);
    border-top-color: var(--accent);
    animation: spin 0.7s linear infinite;
  }

  .spin-label {
    font-size: var(--text-sm);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @keyframes spin-fade {
    from {
      opacity: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation-duration: 1.4s;
    }
    .spin-wrap {
      animation: none;
    }
  }
</style>
