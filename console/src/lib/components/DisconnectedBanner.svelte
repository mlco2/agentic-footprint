<script lang="ts">
  import type { AfootClientStatus } from "../client/afootClient.svelte";

  let { status = "connecting" }: { status?: AfootClientStatus } = $props();
</script>

{#if status === "reconnecting"}
  <div class="disconnected-banner">
    Reconnecting to the watcher…
  </div>
{:else if status === "offline"}
  <div class="disconnected-banner">
    Watcher not running — start it with <code>afoot watch --debug</code>. Retrying quietly.
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
