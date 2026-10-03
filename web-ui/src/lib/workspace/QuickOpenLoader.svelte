<script lang="ts">
  import type { ComponentProps } from "svelte";
  import type QuickOpen from "./QuickOpen.svelte";
  import LazyDialogLoader from "../layout/LazyDialogLoader.svelte";

  let props: ComponentProps<typeof QuickOpen> = $props();
</script>

<LazyDialogLoader kind="quickOpen" dialogProps={props} onCancel={props.onClose} />
