<script lang="ts">
  // One chip per event type (8px swatch + live count from eventStore's
  // incremental counters) toggling membership in uiStore.hiddenTypes, a
  // "clear filters" ghost button, and the "N shown of M · newest first"
  // label (SCREENS.md §2). `shown`/`total` come from the caller's
  // selectStreamRows call — this component only renders the label, it
  // doesn't compute it.
  import type { FactEvent } from "../types/contract1";
  import { eventStore } from "../stores/eventStore.svelte";
  import { uiStore } from "../stores/uiStore.svelte";

  const TYPES: FactEvent["type"][] = ["llm_call", "action_span", "energy_sample", "process_sample", "session_meta"];

  let {
    shown,
    total,
  }: {
    shown: number;
    total: number;
  } = $props();

  const label = $derived(`${shown} shown of ${total} · newest first`);
</script>

<div class="filter-chips">
  {#each TYPES as type (type)}
    <button
      type="button"
      class="chip"
      class:is-off={uiStore.hiddenTypes.has(type)}
      onclick={() => uiStore.toggleHiddenType(type)}
    >
      <span class="chip__swatch" style:background={`var(--type-swatch-${type})`}></span>
      {type}
      <span class="chip__count">{eventStore.perType.get(type) ?? 0}</span>
    </button>
  {/each}
  <div class="filter-chips__spacer"></div>
  <button type="button" class="ghost-btn filter-chips__clear" onclick={() => uiStore.hiddenTypes.clear()}>
    clear filters
  </button>
  <span class="filter-chips__label">{label}</span>
</div>

<style>
  .filter-chips {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
    padding-bottom: 3px;
  }
  .filter-chips__spacer {
    flex: 1;
  }
  .filter-chips__clear {
    font-size: 12px;
  }
  .filter-chips__label {
    font-size: 11px;
    color: var(--color-neutral-600);
    font-style: italic;
    white-space: nowrap;
  }
</style>
