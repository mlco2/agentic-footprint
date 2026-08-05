<script lang="ts">
  // Attribution allocation table (SCREENS.md §3): span flex:1 · locus 58 ·
  // overlap 68 · cpuΔ 78 · l2 share 122 (bar+%) · l2 joules 70 · l1 joules 70.
  // Mirrors EventTable.svelte's geometry conventions (sticky uppercase
  // header, ink rule under it, neutral-200 rules between rows). Every field
  // here is already formatted by selectors/attribution.ts — this component
  // does no formatting/computation of its own, only column layout.
  import type { AllocTableRow } from "../selectors/attribution";
  import ShareBar from "./ShareBar.svelte";

  let { rows }: { rows: readonly AllocTableRow[] } = $props();
</script>

<div class="alloc-table">
  <div class="alloc-table__head rule-table-head">
    <span class="col col--span">span</span>
    <span class="col col--locus">locus</span>
    <span class="col col--overlap">overlap</span>
    <span class="col col--cpu">cpu Δ</span>
    <span class="col col--share">l2 share</span>
    <span class="col col--l2 col--right">l2 joules</span>
    <span class="col col--l1 col--right">l1 joules</span>
  </div>
  {#each rows as row (row.key)}
    <div class="alloc-table__row rule-table-row" class:is-agent={row.kind === "agent"} class:is-baseline={row.kind === "baseline"}>
      <span class="col col--span">
        {#if row.kind === "agent"}
          <span class="alloc-table__swatch" aria-hidden="true"></span>
        {/if}
        {row.label}
        {#if row.noteLabel}
          <div class="alloc-table__note">{row.noteLabel}</div>
        {/if}
      </span>
      <span class="col col--locus">{row.locusLabel}</span>
      <span class="col col--overlap">{row.overlapLabel}</span>
      <span class="col col--cpu">{row.cpuDeltaLabel}</span>
      <span class="col col--share">
        {#if row.shareSegments.length > 0}
          <ShareBar segments={row.shareSegments} variant="compact" />
        {/if}
        <span class="alloc-table__share-pct">{row.shareLabel}</span>
      </span>
      <span class="col col--l2 col--right">{row.l2JoulesLabel}</span>
      <span class="col col--l1 col--right">{row.l1JoulesLabel}</span>
    </div>
  {/each}
</div>

<style>
  .alloc-table {
    font-size: 12px;
  }
  .alloc-table__head {
    display: flex;
    gap: var(--space-2);
    padding: 3px 0;
    font-size: 10.5px;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--color-neutral-600);
  }
  .alloc-table__row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 3px 0;
  }
  /* Agent's own process (DESIGN-SYSTEM §3): accent-300 is a FILL value (see
     `.alloc-table__swatch` below, mirroring the file_op tool-kind fill/border
     pair — `--tool-fill-file_op`/`--tool-border-file_op` — the established
     accent-300-as-fill precedent), never a text color: accent-300 on this
     row's own light background reads at ~1.3:1 contrast, effectively
     illegible. Row text stays ink, same as every other row. */
  .alloc-table__row.is-agent {
    color: var(--color-text);
  }
  .alloc-table__swatch {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-right: 6px;
    vertical-align: middle;
    box-sizing: border-box;
    background: var(--color-accent-300);
    border: 1px solid var(--color-accent-700);
  }
  /* Baseline/idle remainder: hatched bar carries the semantic (DESIGN-SYSTEM
     §3); the row text itself just reads slightly muted so it doesn't compete
     with real span rows. */
  .alloc-table__row.is-baseline {
    color: var(--color-neutral-700);
    font-style: italic;
  }
  .alloc-table__note {
    font-size: 11px;
    color: var(--color-neutral-600);
    font-style: italic;
  }
  .col--span {
    flex: 1;
    min-width: 0;
  }
  .col--locus {
    flex: 0 0 58px;
    color: var(--color-neutral-600);
  }
  .col--overlap {
    flex: 0 0 68px;
    color: var(--color-neutral-600);
  }
  .col--cpu {
    flex: 0 0 78px;
    color: var(--color-neutral-600);
  }
  .col--share {
    flex: 0 0 122px;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .alloc-table__share-pct {
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .col--l2 {
    flex: 0 0 70px;
  }
  .col--l1 {
    flex: 0 0 70px;
    color: var(--color-neutral-600);
  }
  .col--right {
    text-align: right;
  }
</style>
