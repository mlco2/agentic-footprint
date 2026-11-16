<script lang="ts">
  // Masthead: one compact mark per agent, always present next to the SSE
  // dot. The mark is a glance; the same row's full wording is its tooltip
  // and its accessible name, so state never rests on the glyph alone.
  import { statusStore } from "../stores/statusStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";
  import { selectAgentRows } from "../selectors/agents";

  const rows = $derived.by(() => selectAgentRows(statusStore.data, uiStore.nowMs));
</script>

{#if statusStore.unavailable}
  <span class="masthead__meta agent-marks" title="this watcher does not report agent status">agents —</span>
{:else if rows.length > 0}
  <span class="masthead__meta agent-marks">
    {#each rows as row (row.id)}
      <span class="agent-mark" data-agent={row.id} title="{row.name}: {row.label}" aria-label="{row.name}: {row.label}">
        {row.name} <span aria-hidden="true">{row.mark}</span>
      </span>
    {/each}
  </span>
{/if}

<style>
  .agent-marks {
    display: inline-flex;
    align-items: baseline;
    gap: var(--space-2);
    font-size: 12px;
    color: var(--color-neutral-700);
  }
  .agent-mark {
    white-space: nowrap;
  }
</style>
