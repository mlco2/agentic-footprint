<script lang="ts">
  // SCREENS.md §1: 76px prefix (colour per kind, 600 weight for [orphan]) ·
  // 76px timestamp · text. Clickable when `ref` is present. Auto-scrolls to
  // the newest row unless the user has scrolled up — `rows[0]` is the
  // newest (selectDecisionLog walks newest-first), so "pinned to newest"
  // means keeping the container scrolled to the TOP, not the bottom.
  import { tick } from "svelte";
  import type { DecisionRow } from "../selectors/timeline";
  import { pressable } from "../actions/pressable";

  let {
    rows,
    onSelect,
  }: {
    rows: readonly DecisionRow[];
    onSelect: (id: string) => void;
  } = $props();

  let container: HTMLDivElement | undefined;
  let pinnedToNewest = $state(true);

  function onScroll(): void {
    if (!container) return;
    pinnedToNewest = container.scrollTop <= 2;
  }

  $effect(() => {
    void rows; // re-run whenever the row set changes
    if (!pinnedToNewest || !container) return;
    void tick().then(() => {
      if (container) container.scrollTop = 0;
    });
  });
</script>

{#snippet rowBody(row: DecisionRow)}
  <span class="decision-log__prefix decision-prefix-{row.kind}">{row.prefixLabel}</span>
  <span class="decision-log__ts">{row.ts}</span>
  <span class="decision-log__text">{row.text}</span>
{/snippet}

<div class="decision-log" bind:this={container} onscroll={onScroll}>
  {#each rows as row (row.key)}
    {#if row.ref !== undefined}
      {@const ref = row.ref}
      <div class="decision-log__row decision-log__row--clickable" use:pressable={() => onSelect(ref)}>
        {@render rowBody(row)}
      </div>
    {:else}
      <div class="decision-log__row">
        {@render rowBody(row)}
      </div>
    {/if}
  {/each}
</div>

<style>
  .decision-log {
    height: 100%;
    overflow-y: auto;
  }
  .decision-log__row {
    display: flex;
    gap: var(--space-2);
    padding: 1px 0;
    font-size: 12px;
  }
  .decision-log__row--clickable {
    cursor: pointer;
  }
  .decision-log__row--clickable:hover {
    background: var(--color-neutral-100);
  }
  .decision-log__prefix {
    flex: 0 0 76px;
  }
  .decision-log__ts {
    flex: 0 0 76px;
    color: var(--color-neutral-600);
  }
  .decision-log__text {
    flex: 1;
    min-width: 0;
    word-break: break-word;
  }
</style>
