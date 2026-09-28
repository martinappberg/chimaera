<script lang="ts">
  /**
   * A document the prose embeds (`![plan](analysis/PLAN.md)`), drawn as a
   * chip where the embed stands — Markdown.svelte mounts it, `embedsAsChip`
   * decides. It resolves as an embed card does (strictly, against the
   * session's directories) and asks again when the pointer arrives or a
   * click lands while the file isn't there yet: an agent often names a file
   * before writing it.
   */
  import { onMount } from "svelte";
  import FileChip, { type ChipState } from "./FileChip.svelte";
  import { isMissing, resolveFile, type TargetInfo, type TargetResult } from "../shared/embed/embed";
  import { fragmentReveal, parseEmbedFragment } from "../shared/embed/fragment";
  import type { Reveal } from "../shared/reveal";
  import type { HoverTargets } from "./hoverTargets";

  interface Props {
    /** The path as written (decoded). */
    shown: string;
    label: string;
    fragment: string | null;
    alt: string;
    resolve?: () => Promise<TargetResult | null>;
    onOpen?: (path: string, reveal: Reveal | undefined, e: MouseEvent) => void;
    hoverTargets?: HoverTargets;
  }

  let { shown, label, fragment, alt, resolve, onOpen, hoverTargets }: Props = $props();

  /** The latest answer; undefined until one arrives (null: unreachable). */
  let answer = $state<TargetResult | undefined>(undefined);
  let alive = true;

  function fileOf(a: TargetResult | null | undefined): TargetInfo | null {
    return a !== null && a !== undefined && !isMissing(a) && a.kind === "file" ? a : null;
  }

  function ask(): Promise<TargetResult | null> {
    const pending = resolve !== undefined ? resolve() : shown.startsWith("/") ? resolveFile(shown) : Promise.resolve(null);
    return pending.then((a) => {
      if (alive && a !== null) answer = a;
      return a;
    });
  }

  onMount(() => {
    void ask();
    return () => {
      alive = false;
    };
  });

  const hit = $derived(fileOf(answer));
  const chipState = $derived<ChipState>(hit !== null ? "present" : answer !== undefined ? "missing" : "pending");
  const reveal = $derived(fragmentReveal(parseEmbedFragment(fragment, hit?.path ?? shown)));

  function open(e: MouseEvent): void {
    if (hit !== null) {
      onOpen?.(hit.path, reveal, e);
      return;
    }
    void ask().then((a) => {
      const f = fileOf(a);
      if (f !== null) onOpen?.(f.path, fragmentReveal(parseEmbedFragment(fragment, f.path)), e);
    });
  }
</script>

<FileChip
  path={hit?.path ?? shown}
  {label}
  state={chipState}
  onOpen={onOpen !== undefined ? open : undefined}
  hover={hit !== null && hoverTargets !== undefined ? { targets: hoverTargets, target: { path: hit.path, fragment } } : null}
  name={alt !== "" ? `open ${alt} (${label})` : undefined}
  title={chipState === "missing" ? `not found: ${shown}` : undefined}
  onEnter={() => {
    if (chipState === "missing") void ask();
  }}
/>
