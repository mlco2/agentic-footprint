<script lang="ts">
  // Timeline rail "Collectors" section (SCREENS.md §1 Layout A): status dot
  // + name + "N ev · N/s · N rejected". Dot state is display-only (already
  // classed by selectRail); this component just renders it.
  import type { CollectorRailRow } from "../selectors/timeline";
  import EmptyState from "./EmptyState.svelte";

  let { collectors }: { collectors: readonly CollectorRailRow[] } = $props();
</script>

<div class="rail-section">
  <div class="rail-section__heading">Collectors</div>
  {#if collectors.length === 0}
    <EmptyState label="Collectors" message="no collectors connected yet" />
  {:else}
    {#each collectors as c (c.name)}
      <div class="collector-row">
        <span class="status-dot {c.dotClass}" aria-hidden="true"></span>
        <span class="collector-row__name">{c.name}</span>
        <span class="collector-row__meta">{c.evCount} ev · {c.eventsPerSLabel} · {c.rejectedCount} rejected</span>
      </div>
    {/each}
  {/if}
</div>

<style>
  .collector-row {
    display: flex;
    align-items: baseline;
    gap: var(--space-1);
    padding: 2px 0;
  }
  .collector-row__name {
    font-size: 12.5px;
  }
  .collector-row__meta {
    font-size: 11px;
    color: var(--color-neutral-600);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
