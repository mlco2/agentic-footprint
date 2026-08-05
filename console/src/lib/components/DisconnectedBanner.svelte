<script lang="ts">
  import type { AfClientStatus } from "../client/afClient.svelte";

  let { status = "connecting" }: { status?: AfClientStatus } = $props();
</script>

{#if status === "reconnecting"}
  <div class="disconnected-banner">
    SSE reconnecting — no live events until the connection is restored.
  </div>
{:else if status === "offline"}
  <div class="disconnected-banner">
    SSE offline — no live events until the connection is restored. Still retrying.
  </div>
{/if}

<style>
  .disconnected-banner {
    flex: 0 0 auto;
    width: 100%;
    padding: var(--space-1) var(--space-3);
    font-size: 12px;
    font-style: italic;
    color: var(--color-accent-2);
  }
</style>
