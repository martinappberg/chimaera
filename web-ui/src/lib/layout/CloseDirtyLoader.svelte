<script lang="ts">
  import type { ComponentProps } from "svelte";
  import type CloseDirtyDialog from "./CloseDirtyDialog.svelte";
  import LazyDialogLoader from "./LazyDialogLoader.svelte";

  let props: ComponentProps<typeof CloseDirtyDialog> = $props();
</script>

<LazyDialogLoader kind="closeDirty" dialogProps={props} onCancel={props.onCancel} />
